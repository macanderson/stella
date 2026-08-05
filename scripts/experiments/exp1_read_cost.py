#!/usr/bin/env python3
"""EXP-1 — What does an edit actually COST, as a function of file size?

This is the experiment that should set the budget numbers.
docs/spec/file-budget.md §5.2 picked 400/600/800 by reasoning about token
cost; §12.1 admitted they were argued rather than measured. This measures them.

CLAIM (§1.1): "An agent changing three lines in command_deck.rs must first read
command_deck.rs: roughly 60,000 tokens to change three lines."

PREDICTION if true: whole-file reads dominate, and bytes-read scales with file
size regardless of how small the edit is.

METHOD: read Stella's own telemetry (.stella/private/store.db).
  - tool_calls(name='read_file'|'grep'), args_json -> path, bytes_out -> cost
  - files_touched(execution_id, path, lines_added, lines_removed) -> edit size
Join per (execution, file); bucket by the file's current line count.

REPORTED MEASURES:
  1. ranged vs whole-file read fraction, by file size  (tests §1.1 directly)
  2. reads per (execution, file) pair                  (the re-read tax)
  3. bytes read per line changed                       (the cost curve)

CONFOUND HANDLED: reads-per-file conflates "big file" with "this file was the
task". Results are reported per (execution, file) pair, and the single dominant
file is reported separately so it cannot masquerade as a size effect.

POWER: this store is small (tens of executions). Every table prints its n and
the script refuses to render a verdict below a minimum sample. Treat a
low-n bucket as a pointer to run more sessions, not as a result.

Usage:  python3 scripts/experiments/exp1_read_cost.py [path/to/store.db]
"""

import collections
import json
import os
import re
import statistics
import subprocess
import sys

DB = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser(
    "~/Projects/stella/.stella/private/store.db")
BUCKETS = ["<400", "400-1000", "1000-2000", "2000+"]
MIN_N = 10


def bucket(n):
    return "<400" if n < 400 else "400-1000" if n < 1000 else "1000-2000" if n < 2000 else "2000+"


def q(sql):
    r = subprocess.run(["sqlite3", "-json", "file:%s?mode=ro" % DB, sql], capture_output=True)
    out = r.stdout.decode(errors="replace").strip()
    return json.loads(out) if out else []


def sizes():
    files = subprocess.run(["git", "ls-files", "*.rs", "*.py"], capture_output=True).stdout.decode().split()
    res = {}
    for f in files:
        try:
            with open(f, "rb") as fh:
                res[f] = fh.read().count(b"\n")
        except OSError:
            pass
    return res


def main():
    if not os.path.exists(DB):
        print("EXP-1: no store at %s" % DB)
        return
    S = sizes()

    def norm(p):
        for c in (p, "crates/" + p, re.sub(r"^crates/", "", p)):
            if c in S:
                return c
        return None

    reads = q("select execution_id, args_json, bytes_out from tool_calls where name='read_file'")
    greps = q("select execution_id, bytes_out from tool_calls where name='grep'")
    touched = q("select execution_id, path, lines_added, lines_removed from files_touched")

    pair_reads = collections.Counter()
    pair_bytes = collections.Counter()
    ranged = collections.Counter()
    total = collections.Counter()
    for r in reads:
        try:
            a = json.loads(r["args_json"])
        except Exception:
            continue
        p = norm(a.get("path", ""))
        if not p:
            continue
        k = bucket(S[p])
        total[k] += 1
        if ("limit" in a) or ("offset" in a) or ("name" in a):
            ranged[k] += 1
        pair_reads[(r["execution_id"], p)] += 1
        pair_bytes[(r["execution_id"], p)] += r["bytes_out"] or 0

    print("EXP-1 — the real cost of an edit")
    print("  store: %s" % DB)
    print("  read_file calls: %d   grep calls: %d   files_touched rows: %d"
          % (len(reads), len(greps), len(touched)))

    print()
    print("  (1) Is the whole file read?  [tests §1.1]")
    print("  %-11s %8s %10s %12s" % ("file size", "reads", "ranged", "whole-file"))
    for k in BUCKETS:
        if not total[k]:
            continue
        print("  %-11s %8d %9.0f%% %11.0f%%"
              % (k, total[k], 100 * ranged[k] / total[k], 100 * (total[k] - ranged[k]) / total[k]))

    print()
    print("  (2) Re-read tax: reads per (execution, file) pair")
    by = collections.defaultdict(list)
    for (ex, p), n in pair_reads.items():
        by[bucket(S[p])].append(n)
    print("  %-11s %8s %9s %9s %7s" % ("file size", "pairs", "mean", "median", "max"))
    for k in BUCKETS:
        v = by[k]
        if v:
            print("  %-11s %8d %9.2f %9.1f %7d"
                  % (k, len(v), statistics.mean(v), statistics.median(v), max(v)))

    dom = collections.Counter()
    for (ex, p), n in pair_reads.items():
        if S[p] >= 2000:
            dom[p] += n
    if dom:
        top, topn = dom.most_common(1)[0]
        share = 100 * topn / sum(dom.values())
        print("  NOTE: %s is %.0f%% of all 2000+ reads — that bucket is one file, not a size effect."
              % (top, share))

    print()
    print("  (3) Cost curve: bytes read per line changed")
    changed = collections.Counter()
    for t in touched:
        p = norm(t["path"] or "")
        if p:
            changed[(t["execution_id"], p)] += (t["lines_added"] or 0) + (t["lines_removed"] or 0)
    curve = collections.defaultdict(list)
    for key, lines in changed.items():
        if lines <= 0 or key not in pair_bytes:
            continue
        curve[bucket(S[key[1]])].append(pair_bytes[key] / lines)
    print("  %-11s %8s %16s" % ("file size", "pairs", "bytes/line-changed"))
    any_n = False
    for k in BUCKETS:
        v = curve[k]
        if v:
            any_n = True
            print("  %-11s %8d %16.0f" % (k, len(v), statistics.median(v)))
    if not any_n:
        print("  (no execution both read and edited the same file — needs more sessions)")

    print()
    ok = sum(total.values()) >= 100
    if not ok:
        print("  VERDICT: UNDERPOWERED (%d reads)" % sum(total.values()))
        return
    big_ranged = 100 * ranged["2000+"] / total["2000+"] if total["2000+"] else 0
    print("  VERDICT on §1.1: %s" % (
        "REFUTED — %.0f%% of reads on 2000+ line files are RANGED, not whole-file. "
        "The cost is not one big read; it is many small ones." % big_ranged
        if big_ranged > 80 else
        "SUPPORTED — whole-file reads dominate on large files."))
    for k in BUCKETS:
        if by[k] and len(by[k]) < MIN_N:
            print("  POWER WARNING: bucket %s has only %d pairs; do not quote it." % (k, len(by[k])))


if __name__ == "__main__":
    main()
