#!/usr/bin/env python3
"""EXP-3 — Do files grow monotonically, and do large files grow FASTER?

WHY THIS EXPERIMENT EXISTS: EXP-2 refuted the stated mechanism in
docs/spec/file-budget.md §1.4 ("agents append"). Edits are spread roughly
uniformly through the file. But files clearly DID get large, so some mechanism
produced that. This measures the growth process directly instead of inferring
it.

CLAIM A (the ratchet premise): a file's length is effectively monotonic — edits
add more than they remove, and files are essentially never made smaller. If
true, the "only turns one way" argument for a non-increase rule survives even
though the append explanation did not.

CLAIM B (rich-get-richer): large files grow FASTER than small ones, in lines per
commit. If true, size is self-reinforcing and an early threshold matters more
than a late one — the budget should bite well before the file is a problem.

PREDICTION if A true: the share of commits that shrink a file is small, and net
growth per commit is positive in every size bucket.
PREDICTION if A false: shrinking commits are common and files routinely get
smaller on their own, so no ratchet is needed.

PREDICTION if B true: mean net lines/commit rises with file size.
PREDICTION if B false: net lines/commit is flat or falls with size.

METHOD: `git log --numstat` over all non-merge commits gives (added, removed)
per file per commit. Walk each file's history forward from its creation,
tracking running length, and attribute each delta to the bucket the file was in
BEFORE the commit (so a file is not credited to the bucket its own growth
created — that would be circular).

CONFOUNDS HANDLED:
  - Renames (numstat "=>" form) are skipped rather than double-counted.
  - Binary files (numstat "-") are skipped.
  - The bucket is taken from the PRE-commit length, which is what makes CLAIM B
    a real test rather than a tautology.
  - File creation commits are excluded from the growth stats (a new file is
    100% addition by construction) but do establish the starting length.

Usage:  python3 scripts/experiments/exp3_growth_ratchet.py
"""

import collections
import subprocess

BUCKETS = ["<400", "400-1000", "1000-2000", "2000+"]


def bucket(n):
    return "<400" if n < 400 else "400-1000" if n < 1000 else "1000-2000" if n < 2000 else "2000+"


MASS_MOVE_FILES = 100  # a commit touching more than this is a restructure, not editing

# Excluded-commit tally, reported rather than silently dropped.
EXCLUDED = {"mass_move_commits": 0, "mass_move_rows": 0, "rename_rows": 0}


def history():
    """Oldest-first (sha, [(added, removed, path)]) for *.rs, non-merge.

    `-M -C` turns a rename into a "old => new" row that we skip, instead of a
    delete+create pair that would read as a huge deliberate reduction. Without
    it the crates/ restructure (7df3d73f, 303 files / 121,963 deletions) lands
    entirely in the 2000+ bucket and inverts the result.
    """
    out = subprocess.run(
        ["git", "log", "--no-merges", "--reverse", "-M", "-C", "--format=%H", "--numstat", "--", "*.rs"],
        capture_output=True,
    ).stdout.decode(errors="replace")
    sha, rows = None, []

    def emit(sha, rows):
        if len(rows) > MASS_MOVE_FILES:
            EXCLUDED["mass_move_commits"] += 1
            EXCLUDED["mass_move_rows"] += len(rows)
            return None
        return (sha, rows)

    for line in out.split("\n"):
        line = line.strip()
        if len(line) == 40 and all(c in "0123456789abcdef" for c in line):
            if sha:
                e = emit(sha, rows)
                if e:
                    yield e
            sha, rows = line, []
        elif line and sha:
            parts = line.split("\t")
            if len(parts) != 3:
                continue
            a, r, p = parts
            if a == "-" or r == "-":
                continue
            if "=>" in p:
                EXCLUDED["rename_rows"] += 1
                continue
            if not p.endswith(".rs"):
                continue
            rows.append((int(a), int(r), p))
    if sha:
        e = emit(sha, rows)
        if e:
            yield e


def main():
    length = {}
    grew = collections.Counter()
    shrank = collections.Counter()
    flat = collections.Counter()
    net = collections.Counter()
    commits = collections.Counter()
    big_shrinks = []

    for sha, rows in history():
        for a, r, p in rows:
            if p not in length:
                length[p] = a  # creation establishes the baseline
                continue
            pre = length[p]
            k = bucket(pre)
            delta = a - r
            commits[k] += 1
            net[k] += delta
            if delta > 0:
                grew[k] += 1
            elif delta < 0:
                shrank[k] += 1
                if delta <= -300:
                    big_shrinks.append((delta, pre, p, sha[:8]))
            else:
                flat[k] += 1
            length[p] = max(0, pre + delta)

    print("EXP-3 — growth ratchet and rich-get-richer")
    print("  files tracked: %d   commit x file edits: %d" % (len(length), sum(commits.values())))
    print("  EXCLUDED: %d mass-move commits (>%d files, %d rows) + %d rename rows"
          % (EXCLUDED["mass_move_commits"], MASS_MOVE_FILES,
             EXCLUDED["mass_move_rows"], EXCLUDED["rename_rows"]))
    print()
    print("  CLAIM A — is length effectively monotonic?")
    print("  %-11s %8s %8s %8s %8s" % ("pre-size", "edits", "grew", "shrank", "flat"))
    for k in BUCKETS:
        if not commits[k]:
            continue
        print("  %-11s %8d %7.0f%% %7.0f%% %7.0f%%" % (
            k, commits[k], 100 * grew[k] / commits[k],
            100 * shrank[k] / commits[k], 100 * flat[k] / commits[k]))

    print()
    print("  CLAIM B — do large files grow faster? (net lines per edit)")
    print("  %-11s %8s %14s" % ("pre-size", "edits", "net lines/edit"))
    for k in BUCKETS:
        if commits[k]:
            print("  %-11s %8d %14.2f" % (k, commits[k], net[k] / commits[k]))

    tot = sum(commits.values())
    tot_shrank = sum(shrank.values())
    print()
    print("  Overall: %.1f%% of edits shrink a file; net %+.2f lines per edit."
          % (100 * tot_shrank / tot, sum(net.values()) / tot))
    print("  Large deliberate reductions (<= -300 lines): %d" % len(big_shrinks))
    for d, pre, p, sha in sorted(big_shrinks)[:8]:
        print("     %+6d from %5d  %s  (%s)" % (d, pre, p, sha))

    print()
    a_ok = tot_shrank / tot < 0.35 and all(net[k] > 0 for k in BUCKETS if commits[k])
    print("  VERDICT A: %s" % (
        "SUPPORTED — growth is the strong default; files rarely shrink"
        if a_ok else
        "REFUTED — files shrink often enough that growth is not a one-way process"))

    rates = [(k, net[k] / commits[k]) for k in BUCKETS if commits[k] >= 100]
    if len(rates) < 2:
        print("  VERDICT B: UNDERPOWERED")
    else:
        lo, hi = rates[0][1], rates[-1][1]
        if hi > lo * 1.5:
            print("  VERDICT B: SUPPORTED — growth rate rises %.1fx from smallest to largest bucket" % (hi / lo))
        elif hi < lo * 0.67:
            print("  VERDICT B: REFUTED (reversed) — large files grow SLOWER (%.2f vs %.2f)" % (hi, lo))
        else:
            print("  VERDICT B: REFUTED — growth rate is flat across size (%.2f vs %.2f)" % (lo, hi))


if __name__ == "__main__":
    main()
