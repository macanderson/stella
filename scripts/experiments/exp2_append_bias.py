#!/usr/bin/env python3
"""EXP-2 — Do agents append?

CLAIM (docs/spec/file-budget.md §1.4): "Asked to add a feature, an agent adds
code at the end. It will not restructure." If true, unbounded growth is the
DEFAULT trajectory of agent-authored code rather than an accident — which is the
whole argument for a mechanical forcing function instead of a guideline.

PREDICTION if true: added lines land disproportionately near the END of the
file. Normalized position (hunk start / file length) skews toward 1.0, and the
skew is STRONGER in large files — an agent that cannot hold the file has nowhere
to put code except the end.

PREDICTION if false: additions are spread through the file, mean position ~0.5,
and no interaction with file size.

METHOD: for every non-merge commit modifying a tracked *.rs file, parse
`git diff -U0` hunk headers (@@ -a,b +c,d @@) for hunks that add lines.
Normalize the hunk start by the file's line count AT THAT COMMIT, obtained by
streaming blobs through one `git cat-file --batch`. Bucket by that same length.

WHY THE NULL IS 0.5 AND NOT SOMETHING ELSE: under uniform editing, a hunk is
equally likely to start anywhere in the file, so E[pos] = 0.5 and 10% of added
lines fall in the last 10% of the file. Both are reported against those nulls.

CONFOUNDS HANDLED:
  - Lines are weighted by hunk size, so one 200-line append is not counted the
    same as one 1-line tweak. Weight is capped at 50 so a single giant commit
    cannot dominate a bucket.
  - Files shorter than 50 lines are dropped: "the end" is not meaningful there.
  - Blobs unreadable at that commit (renames, deletions) are skipped, not
    guessed.
  - New-file commits are excluded (--diff-filter=M): creating a file is 100%
    "append" by construction and would fake the result.

Usage:  python3 scripts/experiments/exp2_append_bias.py [max_commits]
"""

import collections
import re
import statistics
import subprocess
import sys

HUNK = re.compile(rb"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")
LIMIT = int(sys.argv[1]) if len(sys.argv) > 1 else 0
BUCKETS = ["<400", "400-1000", "1000-2000", "2000+"]


def bucket(n):
    return "<400" if n < 400 else "400-1000" if n < 1000 else "1000-2000" if n < 2000 else "2000+"


def collect_hunks():
    """Yield (sha, path, [(start, added_count), ...]) for *.rs modifications."""
    cmd = ["git", "log", "--no-merges", "--format=%H", "-U0", "--diff-filter=M", "--", "*.rs"]
    if LIMIT:
        cmd.insert(3, "-n%d" % LIMIT)
    out = subprocess.run(cmd, capture_output=True).stdout
    sha = path = None
    hunks = []
    for line in out.split(b"\n"):
        if re.fullmatch(rb"[0-9a-f]{40}", line):
            if sha and path and hunks:
                yield sha, path, hunks
            sha, path, hunks = line.decode(), None, []
        elif line.startswith(b"+++ b/"):
            if sha and path and hunks:
                yield sha, path, hunks
            path, hunks = line[6:].decode(errors="replace"), []
        elif line.startswith(b"@@") and path and path.endswith(".rs"):
            m = HUNK.match(line)
            if m:
                cnt = int(m.group(2)) if m.group(2) is not None else 1
                if cnt > 0:
                    hunks.append((int(m.group(1)), cnt))
    if sha and path and hunks:
        yield sha, path, hunks


def blob_lines(refs):
    """{sha:path -> line count} via one streaming `git cat-file --batch`."""
    if not refs:
        return {}
    p = subprocess.Popen(["git", "cat-file", "--batch"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    out, _ = p.communicate(b"\n".join(r.encode() for r in refs) + b"\n")
    res, pos = {}, 0
    for ref in refs:
        nl = out.find(b"\n", pos)
        if nl < 0:
            break
        header = out[pos:nl]
        if header.endswith(b"missing"):
            pos = nl + 1
            continue
        try:
            size = int(header.split()[-1])
        except (ValueError, IndexError):
            pos = nl + 1
            continue
        body = out[nl + 1 : nl + 1 + size]
        res[ref] = body.count(b"\n")
        pos = nl + 1 + size + 1
    return res


def main():
    records = list(collect_hunks())
    refs = ["%s:%s" % (sha, path) for sha, path, _ in records]
    lens = blob_lines(refs)

    positions = collections.defaultdict(list)
    tail_hit = collections.Counter()
    tail_all = collections.Counter()
    for sha, path, hunks in records:
        n = lens.get("%s:%s" % (sha, path))
        if not n or n < 50:
            continue
        k = bucket(n)
        for start, cnt in hunks:
            pos = min(start / n, 1.0)
            positions[k].extend([pos] * min(cnt, 50))
            tail_all[k] += cnt
            if pos >= 0.9:
                tail_hit[k] += cnt

    print("EXP-2 — do agents append?")
    print("  commit x file pairs: %d   resolved lengths: %d" % (len(records), len(lens)))
    print()
    print("  %-11s %11s %10s %9s %14s" % ("file size", "added-lines", "mean pos", "median", "in last 10%"))
    for k in BUCKETS:
        v = positions[k]
        if not v:
            continue
        pct = 100 * tail_hit[k] / tail_all[k] if tail_all[k] else 0
        print("  %-11s %11d %10.3f %9.3f %13.1f%%" % (k, tail_all[k], statistics.mean(v), statistics.median(v), pct))

    allv = [x for v in positions.values() for x in v]
    if not allv:
        print("  no data")
        return
    print()
    print("  NULL (uniform editing): mean pos 0.500, 10.0%% of added lines in last 10%%.")
    print("  OBSERVED overall:       mean pos %.3f, %.1f%% in last 10%%."
          % (statistics.mean(allv), 100 * sum(tail_hit.values()) / max(1, sum(tail_all.values()))))

    big = positions["1000-2000"] + positions["2000+"]
    small = positions["<400"]
    print()
    if len(big) < 200 or len(small) < 200:
        print("  VERDICT: UNDERPOWERED (need >=200 weighted samples per arm; have %d big / %d small)"
              % (len(big), len(small)))
        return
    d = statistics.mean(big) - statistics.mean(small)
    if d > 0.05:
        v = "SUPPORTED — append bias is stronger in large files (+%.3f)" % d
    elif d < -0.05:
        v = "REFUTED (reversed) — large files are edited EARLIER than small ones (%.3f)" % d
    else:
        v = "REFUTED — no size interaction (delta %.3f, within +/-0.05)" % d
    print("  VERDICT: %s" % v)


if __name__ == "__main__":
    main()
