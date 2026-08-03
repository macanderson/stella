#!/usr/bin/env python3
"""Reduce a readiness-probe Harbor job directory to the scored table.

Same event-stream semantics as the live-feed reducer
(`bench/harbor_adapter/stella_harbor/live_feed.py::_reduce`): steps are
`step_usage` events, `cap_hits` are steps with >=16384 output tokens and no
tool calls, cost/tokens summed from the stream, reward taken from Harbor's
own `result.json` verifier record. Stdlib only.

    python3 collect.py /path/to/jobs/<job-name> [--json out.json]
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

CAP_HIT_MIN_OUTPUT = 16384


def reduce_trial(trial: Path) -> dict:
    row = {
        "task": trial.name.rsplit("__", 1)[0],
        "trial": trial.name,
        "reward": None,
        "exception": None,
        "steps": 0,
        "tools": 0,
        "in_tokens": 0,
        "out_tokens": 0,
        "cached_in_tokens": 0,
        "cost_usd": 0.0,
        "cap_hits": 0,
        "complete": False,
        "wall_s": None,
    }
    result = trial / "result.json"
    if result.exists():
        d = json.loads(result.read_text())
        vr = d.get("verifier_result") or {}
        row["reward"] = vr.get("reward", d.get("reward"))
        exc = d.get("exception_info") or {}
        row["exception"] = exc.get("exception_type")
        started, finished = d.get("started_at"), d.get("finished_at")
        if started and finished:
            from datetime import datetime

            def parse(ts: str):
                return datetime.fromisoformat(ts.replace("Z", "+00:00"))

            try:
                row["wall_s"] = round(
                    (parse(finished) - parse(started)).total_seconds()
                )
            except ValueError:
                pass
    events = trial / "agent" / "stella-events.jsonl"
    if events.exists():
        for line in events.read_text().splitlines():
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                continue
            t = e.get("type")
            if t == "step_usage":
                row["steps"] += 1
                row["in_tokens"] += int(e.get("input_tokens") or 0)
                row["out_tokens"] += int(e.get("output_tokens") or 0)
                row["cached_in_tokens"] += int(e.get("cached_input_tokens") or 0)
                row["cost_usd"] += float(e.get("cost_usd") or 0.0)
                row["tools"] += int(e.get("tool_calls") or 0)
                if (
                    int(e.get("output_tokens") or 0) >= CAP_HIT_MIN_OUTPUT
                    and not e.get("tool_calls")
                ):
                    row["cap_hits"] += 1
            elif t == "complete":
                row["complete"] = True
    row["cost_usd"] = round(row["cost_usd"], 4)
    return row


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("job_dir", type=Path)
    ap.add_argument("--json", type=Path, default=None)
    args = ap.parse_args()

    rows = [
        reduce_trial(t)
        for t in sorted(args.job_dir.iterdir())
        if t.is_dir() and (t / "result.json").exists() or (t / "agent").is_dir()
    ]
    rows = [r for r in rows if r["steps"] or r["reward"] is not None]

    header = (
        f"{'task':28s} {'reward':>6s} {'steps':>5s} {'tools':>5s} "
        f"{'in_tok':>10s} {'out_tok':>8s} {'cap':>3s} {'cost$':>7s} "
        f"{'wall_s':>6s}  exception"
    )
    print(header)
    print("-" * len(header))
    for r in rows:
        print(
            f"{r['task']:28s} {str(r['reward']):>6s} {r['steps']:>5d} "
            f"{r['tools']:>5d} {r['in_tokens']:>10,d} {r['out_tokens']:>8,d} "
            f"{r['cap_hits']:>3d} {r['cost_usd']:>7.3f} "
            f"{str(r['wall_s'] or '-'):>6s}  {r['exception'] or '-'}"
        )
    passed = sum(1 for r in rows if r["reward"] == 1)
    print(
        f"\npassed {passed}/{len(rows)}; "
        f"total in {sum(r['in_tokens'] for r in rows):,} "
        f"(cached {sum(r['cached_in_tokens'] for r in rows):,}), "
        f"out {sum(r['out_tokens'] for r in rows):,}, "
        f"cost ${sum(r['cost_usd'] for r in rows):.3f}, "
        f"cap hits {sum(r['cap_hits'] for r in rows)}"
    )
    if args.json:
        args.json.write_text(json.dumps(rows, indent=1))
        print(f"wrote {args.json}")


if __name__ == "__main__":
    main()
