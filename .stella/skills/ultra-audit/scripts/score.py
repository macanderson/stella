#!/usr/bin/env python3
"""Recompute every number in an audit result. No agent's arithmetic is trusted.

Four independent checks, each of which has caught a real error:

  * the weighted overall, recomputed from the synthesis agent's own per-dimension scores
  * the panel median and spread, recomputed from the three panelists' raw votes
  * the PRIOR round's published number, recomputed from its recorded scores — if that does
    not reproduce, the weights drifted and the comparison is invalid
  * the delta, at full precision. 79 -> 80 looks like +1; the real movement was +0.86.
    Subtracting rounded values has already produced one wrong number in a shipped report.

    python3 score.py <result.json> [--record --commit SHA --date YYYY-MM-DD] [--history PATH]
                     [--json out.json]
"""
from __future__ import annotations

import argparse
import json
import os
import statistics
import sys

WEIGHTS = {
    "loop_correctness": 3, "security": 3, "agent_performance": 3, "system_architecture": 3,
    "durability": 2.5, "token_efficiency": 2.5,
    "maintainability": 2, "end_user_experience": 2, "compute_efficiency": 2,
    "data_storage": 1.5, "documentation": 1.5, "file_organization": 1.5,
    "vendor_lock_avoidance": 1.5,
    "formatting": 1, "language": 1, "file_names": 1, "symbol_names": 1,
}
# Heaviest first, so the eye lands on what actually drives the number.
ORDER = sorted(WEIGHTS, key=lambda k: (-WEIGHTS[k], k))
HISTORY_DIR = os.path.expanduser("~/.claude/audits")
CONTESTED = 10          # panel spread at or above which a dimension is contested


def overall(scores: dict) -> float:
    num = den = 0.0
    for d, w in WEIGHTS.items():
        s = scores.get(d)
        if isinstance(s, (int, float)) and s > 0:
            num += s * w
            den += w
    return num / den if den else 0.0


def load(path: str) -> dict:
    raw = json.load(open(path))
    # A backgrounded Workflow writes {summary, result, ...}; unwrap it.
    return raw.get("result", raw) if isinstance(raw, dict) else raw


def band(x: float) -> str:
    return ("RESERVED (Rust itself)" if x >= 100 else
            "Rust-grade process" if x >= 96 else
            "reference-grade" if x >= 90 else
            "strong" if x >= 80 else
            "solid, real gaps" if x >= 70 else
            "mediocre" if x >= 60 else "deficient")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("result")
    ap.add_argument("--history")
    ap.add_argument("--record", action="store_true")
    ap.add_argument("--commit", default="")
    ap.add_argument("--date", default="")
    ap.add_argument("--report", default="", help="path of the HTML report, recorded in history")
    ap.add_argument("--json", default="", help="write the reconciled scoring block here")
    a = ap.parse_args()

    d = load(a.result)
    syn = d.get("synthesis") or {}
    dims = syn.get("dimension_scores") or {}
    scores = {k: v["score"] for k, v in dims.items()
              if isinstance(v, dict) and isinstance(v.get("score"), (int, float))}
    if not scores:
        sys.exit("no dimension_scores in result. Read the workflow's journal.jsonl before "
                 "diagnosing — a null result is more often a parser bug than a dead agent.")

    root = (d.get("run") or {}).get("root") or ""
    hist_path = a.history or os.path.join(
        HISTORY_DIR, (os.path.basename(root.rstrip("/")) or "workspace") + ".json")
    runs = []
    if os.path.exists(hist_path):
        runs = json.load(open(hist_path)).get("runs", [])
    prior = runs[-1]["scores"] if runs else None

    missing = [k for k in WEIGHTS if k not in scores]
    now = overall(scores)
    problems: list[str] = []

    print(f"WORKSPACE  {root or '(unrecorded)'}")
    print(f"COMMIT     {(d.get('run') or {}).get('commit') or a.commit or '(unrecorded)'}")
    print(f"DEPTH      {(d.get('run') or {}).get('depth') or '?'}   "
          f"round {len(runs) + 1}\n")
    print(f"OVERALL    {now:.2f}  ->  {round(now)}   ({band(now)})")

    if prior:
        was = overall(prior)
        print(f"PRIOR      {was:.2f}  ->  {round(was)}   (round {len(runs)})")
        print(f"CHANGE     {now - was:+.2f} weighted   "
              f"[rounded headline would read {round(now) - round(was):+d} — cite the precise "
              f"figure]")
        pub = runs[-1].get("overall_rounded")
        if pub is not None and round(was) != pub:
            problems.append(
                f"WEIGHT DRIFT: recomputing round {len(runs)} from its recorded scores gives "
                f"{round(was)}, but it published {pub}. The comparison is INVALID until "
                f"reconciled — do not report a delta.")
    else:
        print("PRIOR      (none — this is round 1)")

    claimed = syn.get("overall_score")
    if claimed is not None and claimed != round(now):
        problems.append(f"the synthesis agent claimed {claimed}; recomputed is {round(now)}. "
                        f"Report the recomputed value and investigate the difference.")
    if missing:
        problems.append(f"dimensions absent from the synthesis and excluded from the mean: "
                        f"{', '.join(missing)}")

    # ── the panel, recomputed from raw votes ──────────────────────────────────
    panel = d.get("panel") or {}
    votes = panel.get("votes") or []
    recomputed_median, recomputed_spread = {}, {}
    for k in WEIGHTS:
        xs = [v.get("scores", {}).get(k) for v in votes]
        xs = [x for x in xs if isinstance(x, (int, float)) and x > 0]
        if xs:
            recomputed_median[k] = int(round(statistics.median(xs)))
            recomputed_spread[k] = max(xs) - min(xs)
    if votes:
        claimed_med = panel.get("median") or {}
        mism = [k for k in recomputed_median
                if isinstance(claimed_med.get(k), (int, float))
                and claimed_med[k] != recomputed_median[k]]
        if mism:
            problems.append(f"the harness's panel median disagrees with the votes on: "
                            f"{', '.join(mism)}")
        print(f"\nPANEL      {len(votes)} panelist(s): "
              f"{', '.join(v.get('model', '?') for v in votes)}")
        print(f"           panel median overall {overall(recomputed_median):.2f}; "
              f"synthesis landed on {now:.2f}")
        contested = sorted(k for k, s in recomputed_spread.items() if s >= CONTESTED)
        if contested:
            print(f"           CONTESTED (spread >= {CONTESTED}): {', '.join(contested)}")
            for k in contested:
                res = (dims.get(k) or {}).get("panel_resolution")
                if not res:
                    problems.append(f"{k} was contested (spread {recomputed_spread[k]}) but the "
                                    f"synthesis gave no panel_resolution — it was required to "
                                    f"read code and justify the number it chose.")
    else:
        problems.append("no panel votes in the result — the score is one model's opinion, not a "
                        "median. Every cross-model claim in the report must be withdrawn.")

    # ── the table ─────────────────────────────────────────────────────────────
    w = max(len(k) for k in ORDER)
    print(f"\n{'dimension'.ljust(w)}  wt   prev  now  delta   panel(med/spread)  votes")
    for k in ORDER:
        s = scores.get(k)
        if s is None:
            continue
        p = prior.get(k) if prior else None
        dl = f"{s - p:+d}" if isinstance(p, (int, float)) else "—"
        pm = recomputed_median.get(k)
        sp = recomputed_spread.get(k)
        pcol = f"{pm:>3}/{sp:<2}" if pm is not None else "  —   "
        vs = "/".join(str(v.get("scores", {}).get(k, "-")) for v in votes) if votes else ""
        star = " *" if sp is not None and sp >= CONTESTED else ""
        print(f"{k.ljust(w)}  {WEIGHTS[k]:<4} {str(p if p is not None else '—'):>4} "
              f"{s:>4}  {dl:>5}   {pcol}         {vs}{star}")

    # ── coverage and confirmation, the two things that make the number mean something ──
    run = d.get("run") or {}
    f = d.get("findings") or {}
    owned, total = run.get("files_owned"), run.get("total_files")
    if owned and total:
        pct = 100.0 * owned / total
        print(f"\nCOVERAGE   {owned}/{total} source files owned by an auditing agent "
              f"({pct:.1f}%)")
        if pct < 99.5:
            problems.append(f"coverage was {pct:.1f}%, not 100% — the report must name what was "
                            f"not audited.")
    if f:
        print(f"FINDINGS   {f.get('total', '?')} raised · {f.get('severe', '?')} severe · "
              f"{len(f.get('confirmed') or [])} confirmed by cross-model refutation · "
              f"{len(f.get('refuted') or [])} refuted · "
              f"{len(f.get('unverified') or [])} unverified")
        if f.get("unverified"):
            problems.append(f"{len(f['unverified'])} severe findings were never refuted (depth "
                            f"cap). They are UNCONFIRMED and must be labelled so.")
        single = [x for x in (f.get("confirmed") or []) if not x.get("cross_model")]
        if single:
            print(f"           !! {len(single)} confirmed finding(s) had no cross-model verifier")

    if problems:
        print("\n" + "!" * 72)
        for p in problems:
            print(f"!! {p}")
        print("!" * 72)
    else:
        print("\nAll internal checks reconcile.")

    block = {
        "root": root, "commit": (run.get("commit") or a.commit or None),
        "date": a.date or None, "depth": run.get("depth"),
        "round": len(runs) + 1,
        "overall": round(now, 2), "overall_rounded": round(now), "band": band(now),
        "prior_overall": round(overall(prior), 2) if prior else None,
        "delta_precise": round(now - overall(prior), 2) if prior else None,
        "scores": scores, "prior_scores": prior,
        "deltas": {k: scores[k] - prior[k] for k in scores
                   if prior and isinstance(prior.get(k), (int, float))} if prior else None,
        "panel_median": recomputed_median, "panel_spread": recomputed_spread,
        "panel_models": [v.get("model") for v in votes],
        "contested": sorted(k for k, s in recomputed_spread.items() if s >= CONTESTED),
        "agent_claimed": claimed, "weights": WEIGHTS,
        "problems": problems,
    }
    if a.json:
        with open(a.json, "w") as fh:
            json.dump(block, fh, indent=1)
        print(f"\n-> {a.json}")

    if a.record:
        if problems:
            sys.exit("\nREFUSING to record a run with unreconciled problems. Fix them first — a "
                     "bad history entry corrupts every future comparison.")
        os.makedirs(HISTORY_DIR, exist_ok=True)
        # `rubric`/`tool` deliberately keep the "ultraudit" spelling even though
        # the skill is now "ultra-audit": they are the history-lineage keys that
        # make scores comparable across rounds, and this lineage is shared
        # unbroken with /reaudit (see SKILL.md "Relationship to /reaudit").
        # Renaming them would fork the series and silently break comparability.
        blob = json.load(open(hist_path)) if os.path.exists(hist_path) else {
            "repo": os.path.basename(root.rstrip("/")), "rubric": "ultraudit-17d-v1", "runs": []}
        blob.setdefault("runs", []).append({
            "round": len(blob["runs"]) + 1,
            "date": a.date or None, "commit": run.get("commit") or a.commit or None,
            "depth": run.get("depth"), "tool": "ultraudit",
            "overall": round(now, 2), "overall_rounded": round(now),
            "scores": scores,
            "panel_median": recomputed_median, "panel_spread": recomputed_spread,
            "panel_models": [v.get("model") for v in votes],
            "coverage": {"files_owned": owned, "total_files": total},
            "report": a.report or None,
            # Carried into the next run's Phase 0 as "did this actually land?"
            "open_findings": [
                {"severity": r.get("severity"), "title": r.get("title"),
                 "detail": r.get("detail"), "remediation": r.get("remediation"),
                 "file": r.get("file"), "dimension": r.get("dimension")}
                for r in (syn.get("top_risks") or [])
            ],
        })
        with open(hist_path, "w") as fh:
            json.dump(blob, fh, indent=1)
        print(f"\nrecorded round {len(blob['runs'])} -> {hist_path}")

    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
