# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh
"""Live side-by-side feed for a two-arm Terminal-Bench run.

Watches two Harbor job directories — one per arm — and renders one row per
task with everything a reviewer wants when comparing agents on the same task
set: status, verdict/failure class, steps, tool calls, tokens, cost, wall
clock, and liveness (seconds since the trial last wrote telemetry), plus
Stella-specific integrity signals (output-cap truncations, zero-tool trials,
``TELEMETRY-INCOMPLETE``) from ``stella-events.jsonl``.

Ports, in the hexagonal sense: the aggregation core reads *only* the trial
artifacts every run already produces (``config.json``, ``result.json``, ATIF
``agent/trajectory.json``, ``agent/stella-events.jsonl``) — no agent
cooperation, no sockets into the SUT — so it works identically on a live run
and on an archived bundle. Two presentation ports sit on top:

- **terminal**: ``python3 -m stella_harbor.live_feed <jobs> <jobA> <jobB>``
  redraws a comparison table every ``--interval`` seconds (``--once`` for a
  single snapshot, e.g. over an archived bundle);
- **http**: add ``--port 8907`` to also serve ``/`` (self-contained HTML
  dashboard, auto-refreshing) and ``/state.json`` (the same snapshot as
  machine-readable JSON) — bind a browser through an SSH tunnel and watch
  both arms move.

Stdlib only, read-only, and cheap: event files are re-parsed only when their
size changes, so an idle dashboard costs stat() calls.
"""

from __future__ import annotations

import argparse
import http.server
import json
import sys
import threading
import time
from pathlib import Path
from typing import Any

_EVENTS = "agent/stella-events.jsonl"
_TRAJECTORY = "agent/trajectory.json"

# Signature of a step cut off at the output-token limit (the zero-tool
# failure class): the step spent its whole output allowance. Matches on the
# reported count rather than a config lookup so archived bundles from other
# configurations still classify.
_CAP_HIT_MIN_OUTPUT = 16384


def _load_json(path: Path) -> dict[str, Any] | None:
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return loaded if isinstance(loaded, dict) else None


class _EventsCache:
    """Per-file reduction of ``stella-events.jsonl``, re-read only on growth."""

    def __init__(self) -> None:
        self._by_path: dict[Path, tuple[int, dict[str, Any]]] = {}

    def summary(self, path: Path) -> dict[str, Any] | None:
        try:
            stat = path.stat()
        except OSError:
            return None
        cached = self._by_path.get(path)
        if cached is not None and cached[0] == stat.st_size:
            summary = dict(cached[1])
        else:
            summary = self._reduce(path)
            if summary is None:
                return None
            self._by_path[path] = (stat.st_size, dict(summary))
        summary["age_s"] = max(0, int(time.time() - stat.st_mtime))
        return summary

    @staticmethod
    def _reduce(path: Path) -> dict[str, Any] | None:
        tools = steps = reasoning = cap_hits = 0
        in_tok = out_tok = 0
        cost = 0.0
        judge_passed: bool | None = None
        complete = False
        last_type = ""
        try:
            with path.open(encoding="utf-8") as handle:
                for line in handle:
                    try:
                        event = json.loads(line)
                    except ValueError:
                        continue  # a mid-write tail line is expected on a live file
                    if not isinstance(event, dict):
                        continue
                    kind = str(event.get("type", ""))
                    last_type = kind or last_type
                    if kind == "tool_start":
                        tools += 1
                    elif kind == "reasoning":
                        reasoning += 1
                    elif kind == "step_usage":
                        steps += 1
                        in_tok += int(event.get("input_tokens") or 0)
                        out_tok += int(event.get("output_tokens") or 0)
                        cost += float(event.get("cost_usd") or 0.0)
                        if (
                            int(event.get("output_tokens") or 0) >= _CAP_HIT_MIN_OUTPUT
                            and not event.get("tool_calls")
                        ):
                            cap_hits += 1
                    elif kind == "judge_verdict":
                        passed = event.get("passed")
                        judge_passed = bool(passed) if passed is not None else None
                    elif kind == "complete":
                        complete = True
        except OSError:
            return None
        return {
            "steps": steps,
            "tools": tools,
            "reasoning": reasoning,
            "input_tokens": in_tok,
            "output_tokens": out_tok,
            "cost_usd": round(cost, 4),
            "cap_hits": cap_hits,
            "judge_passed": judge_passed,
            "complete": complete,
            "last_type": last_type,
        }


def trial_view(trial_dir: Path, events_cache: _EventsCache) -> dict[str, Any]:
    """Everything knowable about one trial directory, from artifacts alone."""
    result = _load_json(trial_dir / "result.json")
    trajectory = _load_json(trial_dir / _TRAJECTORY)
    events = events_cache.summary(trial_dir / _EVENTS)

    view: dict[str, Any] = {
        "trial": trial_dir.name,
        "status": "pending",
        "resolved": None,
        "failure": "",
        "steps": None,
        "tools": None,
        "input_tokens": None,
        "output_tokens": None,
        "cost_usd": None,
        "wall_s": None,
        "age_s": None,
        "cap_hits": 0,
        "judge_passed": None,
    }

    if trajectory is not None:
        atif_steps = trajectory.get("steps")
        if isinstance(atif_steps, list):
            view["steps"] = len(atif_steps)
            view["tools"] = sum(
                len(step.get("tool_calls") or [])
                for step in atif_steps
                if isinstance(step, dict)
            )
        metrics = trajectory.get("final_metrics")
        if isinstance(metrics, dict):
            view["input_tokens"] = metrics.get("total_input_tokens", view["input_tokens"])
            view["output_tokens"] = metrics.get("total_output_tokens", view["output_tokens"])
            view["cost_usd"] = metrics.get("total_cost_usd", view["cost_usd"])

    if events is not None:
        # Stella's own stream is the richer, always-current source: prefer it
        # over the trajectory (written once, at the end) wherever both speak.
        view.update(
            steps=events["steps"] or view["steps"],
            tools=events["tools"] if events["tools"] or view["tools"] is None else view["tools"],
            input_tokens=events["input_tokens"] or view["input_tokens"],
            output_tokens=events["output_tokens"] or view["output_tokens"],
            cost_usd=events["cost_usd"] or view["cost_usd"],
            age_s=events["age_s"],
            cap_hits=events["cap_hits"],
            judge_passed=events["judge_passed"],
        )
        view["status"] = "done" if events["complete"] else "running"

    if result is not None:
        view["status"] = "done"
        resolved = result.get("verifier_result") or result
        reward = resolved.get("reward")
        if isinstance(reward, (int, float)):
            view["resolved"] = reward >= 1
        exception = result.get("exception_info") or {}
        if isinstance(exception, dict) and exception.get("exception_type"):
            view["failure"] = str(exception["exception_type"])
            if view["resolved"] is None:
                view["resolved"] = False
        started = result.get("started_at")
        finished = result.get("finished_at")
        try:
            if started and finished:
                from datetime import datetime

                view["wall_s"] = int(
                    (
                        datetime.fromisoformat(str(finished))
                        - datetime.fromisoformat(str(started))
                    ).total_seconds()
                )
        except ValueError:
            pass
    return view


def _trials_by_task(job_dir: Path) -> dict[str, Path]:
    trials: dict[str, Path] = {}
    if not job_dir.is_dir():
        return trials
    for entry in sorted(job_dir.iterdir()):
        if entry.is_dir():
            task = entry.name.rsplit("__", 1)[0]
            trials[task] = entry
    return trials


def pair_rows(
    jobs_root: Path, job_a: str, job_b: str, events_cache: _EventsCache
) -> list[dict[str, Any]]:
    """One row per task present in either arm, alphabetical, both views."""
    arm_a = _trials_by_task(jobs_root / job_a)
    arm_b = _trials_by_task(jobs_root / job_b)
    rows = []
    for task in sorted(set(arm_a) | set(arm_b)):
        rows.append(
            {
                "task": task,
                "a": trial_view(arm_a[task], events_cache) if task in arm_a else None,
                "b": trial_view(arm_b[task], events_cache) if task in arm_b else None,
            }
        )
    return rows


def _totals(rows: list[dict[str, Any]], arm: str) -> dict[str, Any]:
    views = [row[arm] for row in rows if row[arm] is not None]
    return {
        "trials": len(views),
        "done": sum(1 for v in views if v["status"] == "done"),
        "passed": sum(1 for v in views if v["resolved"] is True),
        "tools": sum(v["tools"] or 0 for v in views),
        "input_tokens": sum(v["input_tokens"] or 0 for v in views),
        "output_tokens": sum(v["output_tokens"] or 0 for v in views),
        "cost_usd": round(sum(v["cost_usd"] or 0.0 for v in views), 4),
        "wall_s": sum(v["wall_s"] or 0 for v in views),
        "cap_hits": sum(v["cap_hits"] or 0 for v in views),
    }


def state(jobs_root: Path, job_a: str, job_b: str, events_cache: _EventsCache) -> dict[str, Any]:
    rows = pair_rows(jobs_root, job_a, job_b, events_cache)
    return {
        "generated_at": int(time.time()),
        "jobs_root": str(jobs_root),
        "arms": {"a": job_a, "b": job_b},
        "rows": rows,
        "totals": {"a": _totals(rows, "a"), "b": _totals(rows, "b")},
    }


def _cell(view: dict[str, Any] | None) -> str:
    if view is None:
        return f"{'—':^34}"
    marks = {True: "PASS", False: "fail", None: "…"}
    verdict = marks[view["resolved"]] if view["status"] == "done" else view["status"]
    if view["failure"]:
        verdict = view["failure"][:12]
    live = "" if view["age_s"] is None or view["status"] != "running" else f" {view['age_s']}s"
    flags = ("⚠cap" if view["cap_hits"] else "") + (
        "∅" if view["tools"] == 0 and view["status"] == "done" else ""
    )
    return (
        f"{verdict:>8}{live:>5} s{view['steps'] if view['steps'] is not None else '-':>4}"
        f" t{view['tools'] if view['tools'] is not None else '-':>4}"
        f" {(view['output_tokens'] or 0) // 1000:>4}k"
        f" ${view['cost_usd'] or 0:>6.2f} {flags:<4}"
    )


def render(snapshot: dict[str, Any]) -> str:
    lines = [
        f"jobs: {snapshot['jobs_root']}   A={snapshot['arms']['a']}   B={snapshot['arms']['b']}",
        f"{'task':<34} | {'A: verdict  steps tools out$  ':<36}| B",
        "-" * 110,
    ]
    for row in snapshot["rows"]:
        lines.append(f"{row['task'][:34]:<34} | {_cell(row['a']):<36}| {_cell(row['b'])}")
    lines.append("-" * 110)
    for arm in ("a", "b"):
        totals = snapshot["totals"][arm]
        lines.append(
            f"{arm.upper()} totals: {totals['passed']}/{totals['done']} passed "
            f"(of {totals['trials']}), tools {totals['tools']}, "
            f"out {totals['output_tokens'] // 1000}k tok, ${totals['cost_usd']:.2f}, "
            f"wall {totals['wall_s']}s, cap-hits {totals['cap_hits']}"
        )
    return "\n".join(lines)


_PAGE = """<!doctype html><meta charset="utf-8"><title>TB head-to-head</title>
<style>body{font:13px ui-monospace,monospace;background:#111;color:#ddd;margin:1rem}
table{border-collapse:collapse;width:100%}td,th{padding:2px 8px;border-bottom:1px solid #333;text-align:left}
.p{color:#7c6} .f{color:#e77} .r{color:#dc5}</style>
<h3 id="h">loading…</h3><table id="t"></table><script>
const cell=v=>{if(!v)return "<td>—</td>";
let s=v.status!=="done"?v.status+(v.age_s!=null?" "+v.age_s+"s":""):(v.resolved?"PASS":(v.failure||"fail"));
let c=v.status!=="done"?"r":(v.resolved?"p":"f");
return `<td class=${c}>${s}</td><td>${v.steps??"-"}/${v.tools??"-"}</td>`+
`<td>${((v.output_tokens||0)/1000).toFixed(1)}k</td><td>$${(v.cost_usd||0).toFixed(2)}</td>`+
`<td>${v.wall_s??""}${v.cap_hits?" ⚠cap":""}${v.tools===0&&v.status==="done"?" ∅tools":""}</td>`};
async function tick(){const s=await (await fetch("state.json")).json();
const ta=s.totals.a,tb=s.totals.b;
document.getElementById("h").textContent=
`${s.arms.a}  ${ta.passed}/${ta.done} pass  ${ta.tools} tools  ${(ta.output_tokens/1000).toFixed(0)}k tok  $${ta.cost_usd.toFixed(2)}  ||  `+
`${s.arms.b}  ${tb.passed}/${tb.done} pass  ${tb.tools} tools  ${(tb.output_tokens/1000).toFixed(0)}k tok  $${tb.cost_usd.toFixed(2)}`;
document.getElementById("t").innerHTML=
"<tr><th>task</th><th colspan=5>A: "+s.arms.a+"</th><th colspan=5>B: "+s.arms.b+"</th></tr>"+
s.rows.map(r=>`<tr><td>${r.task}</td>${cell(r.a)}${cell(r.b)}</tr>`).join("")}
tick();setInterval(tick,2000);</script>"""


def serve(jobs_root: Path, job_a: str, job_b: str, port: int, cache: _EventsCache) -> None:
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802 - http.server contract
            if self.path.rstrip("/") in ("", "/index.html"):
                body, ctype = _PAGE.encode(), "text/html; charset=utf-8"
            elif self.path == "/state.json":
                body = json.dumps(state(jobs_root, job_a, job_b, cache)).encode()
                ctype = "application/json"
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_: Any) -> None:  # quiet by design
            return

    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    print(f"live feed: http://127.0.0.1:{port}/  (tunnel: ssh -L {port}:127.0.0.1:{port} …)")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("jobs_root", type=Path, help="Harbor jobs directory ($JOBS)")
    parser.add_argument("job_a", help="arm A job name, e.g. hh9-armA-stella")
    parser.add_argument("job_b", help="arm B job name, e.g. hh9-armB-claudecode")
    parser.add_argument("--interval", type=float, default=2.0)
    parser.add_argument("--once", action="store_true", help="print one snapshot and exit")
    parser.add_argument("--port", type=int, help="also serve the HTML dashboard on 127.0.0.1")
    args = parser.parse_args(argv)

    cache = _EventsCache()
    if args.port:
        serve(args.jobs_root, args.job_a, args.job_b, args.port, cache)
    while True:
        snapshot = state(args.jobs_root, args.job_a, args.job_b, cache)
        if args.once:
            print(render(snapshot))
            return 0
        sys.stdout.write("\x1b[2J\x1b[H" + render(snapshot) + "\n")
        sys.stdout.flush()
        time.sleep(args.interval)


if __name__ == "__main__":
    raise SystemExit(main())
