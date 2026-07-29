#!/usr/bin/env python3
"""Splice a runnable audit harness from the template.

Workflow scripts have no filesystem access and cannot call Date.now(), so everything the
harness knows about the tree has to be baked in here, by this script, in the parent session.

    python3 build_workflow.py --gate gate.json --units units.json \
            --baseline baseline.txt --out audit.js [--depth deep] [--salvage salvaged.json]

Then launch it with the Workflow tool: Workflow({scriptPath: 'audit.js'}).
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
TEMPLATE = os.path.join(HERE, "workflow.template.js")
HISTORY_DIR = os.path.expanduser("~/.claude/audits")


def history_path(root: str) -> str:
    return os.path.join(HISTORY_DIR, os.path.basename(root.rstrip("/")) + ".json")


def js_literal(obj) -> str:
    """JSON is a subset of JS, but `</` inside any string would close a <script> block if this
    payload is ever embedded in HTML, and U+2028/9 are literal line breaks in JS source."""
    s = json.dumps(obj, ensure_ascii=False)
    return (s.replace("</", "<\\/")
             .replace(" ", "\\u2028")
             .replace(" ", "\\u2029"))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--gate", required=True)
    ap.add_argument("--units", required=True)
    ap.add_argument("--baseline", help="file holding the measured pre-audit gate, as prose")
    ap.add_argument("--depth", default="deep", choices=["standard", "deep", "max"])
    ap.add_argument("--history", help="defaults to ~/.claude/audits/<repo>.json")
    ap.add_argument("--salvage", help="results carried over from an aborted run")
    ap.add_argument("--out", default="audit.js")
    a = ap.parse_args()

    gate = json.load(open(a.gate))
    units_blob = json.load(open(a.units))
    units = units_blob["units"] if isinstance(units_blob, dict) else units_blob
    root = gate["root"]

    proof = units_blob.get("proof", {}) if isinstance(units_blob, dict) else {}
    if proof and not (proof.get("disjoint") and proof.get("complete")):
        sys.exit("REFUSING to build: the partition is not disjoint+complete. Re-run "
                 "partition.py and fix it — overlapping agents clobber each other's work.")

    hist_path = a.history or history_path(root)
    prior, prior_items = None, []
    if os.path.exists(hist_path):
        runs = json.load(open(hist_path)).get("runs", [])
        if runs:
            prior = runs[-1].get("scores")
            prior_items = runs[-1].get("open_findings", [])

    baseline = "(not measured — the Verify phase cannot distinguish regression from " \
               "pre-existing breakage, and must say so)"
    if a.baseline and os.path.exists(a.baseline):
        baseline = open(a.baseline).read().strip()

    head = subprocess.run("git rev-parse HEAD", cwd=root, shell=True,
                          capture_output=True, text=True).stdout.strip()
    run = {
        "date": datetime.date.today().isoformat(),
        "commit": head or None,
        "depth": a.depth,
        "history": hist_path,
        "round": len(json.load(open(hist_path)).get("runs", [])) + 1
                 if os.path.exists(hist_path) else 1,
    }

    # The inventory is thousands of paths and the harness never reads it — partition.py
    # already consumed it. Dropping it keeps the script (and every prompt) small.
    gate_slim = {k: v for k, v in gate.items() if k != "inventory"}

    src = open(TEMPLATE).read()
    subs = {
        "__ROOT__": root,
        "/*__RUN__*/ {}": js_literal(run),
        "/*__GATE__*/ {}": js_literal(gate_slim),
        "/*__UNITS__*/ []": js_literal(units),
        "/*__PRIOR__*/ null": js_literal(prior) if prior else "null",
        "/*__PRIOR_ITEMS__*/ []": js_literal(prior_items),
        "/*__SALVAGE__*/ null": (open(a.salvage).read().strip() if a.salvage else "null"),
    }
    for k, v in subs.items():
        if k not in src:
            sys.exit(f"template is missing the splice point {k!r} — did the template change?")
        src = src.replace(k, v, 1)
    # Backticked, so a stray backtick or ${ in the measured baseline would break the script.
    src = src.replace("__BASELINE__",
                      baseline.replace("\\", "\\\\").replace("`", "\\`").replace("${", "\\${"), 1)

    leftover = re.findall(r"__[A-Z_]+__", src)
    if leftover:
        sys.exit(f"placeholders survived the splice: {sorted(set(leftover))}")

    with open(a.out, "w") as fh:
        fh.write(src)

    # Validate the GENERATED script, not just the template — and do it through
    # tests/check_template.py rather than `node --check <file>.js`.
    #
    # `node --check` is NOT a usable gate for this file shape: the script mixes an ESM
    # `export` with a top-level `return` (legal only inside the harness's async wrapper), and
    # on Node 24 that combination exits 0 even when the file is genuinely unbalanced —
    # verified against a deliberately broken template. check_template.py strips the export,
    # wraps the body, and checks it as .mjs, which does catch it. It also re-runs the
    # model/phase/header invariants against the spliced output.
    checker = os.path.join(HERE, "..", "tests", "check_template.py")
    check = subprocess.run([sys.executable, checker, a.out], capture_output=True, text=True)
    if check.returncode:
        sys.exit(f"generated script failed validation — NOT launching:\n{check.stdout}\n"
                 f"{check.stderr}")

    n_units = len(units)
    lens_jobs = 10           # 8 lenses, 2 of them run twice
    sweep = {"standard": 0, "deep": 0, "max": 6}[a.depth]
    verifiers = {"standard": 1, "deep": 2, "max": 3}[a.depth]
    critics = 3 if a.depth == "max" else 1
    est_refute = min(len(prior_items) * 0 + 48, 64 if a.depth == "deep" else
                     (40 if a.depth == "standard" else 90)) * verifiers
    est = (len(prior_items) + n_units * 2 + lens_jobs + sweep + 1 + est_refute + 3 + critics + 1)
    waves = -(-est // 8)

    print(f"-> {a.out}")
    print(f"round {run['round']} · depth {a.depth} · commit {(head or '?')[:9]}")
    print(f"history: {hist_path}" + ("" if prior else "  (round 1 — no prior scores)"))
    print(f"prior open findings to re-verify: {len(prior_items)}")
    print(f"units: {n_units}   lenses: {lens_jobs}   sweep: {sweep}   "
          f"verifiers/finding: {verifiers}   critics: {critics}")
    print(f"\nESTIMATE  ~{est} agents  ·  ~{waves} waves at concurrency 8  ·  "
          f"~{waves * 6}-{waves * 10} min")
    print("Tell the user this estimate, and tell them that typing in the prompt kills the run.")


if __name__ == "__main__":
    main()
