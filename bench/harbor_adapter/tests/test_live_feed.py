# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh
"""The live feed's aggregation core, pinned over synthetic trial artifacts.

Everything here goes through the same artifact files a real run produces —
no sockets, no agent cooperation — which is the module's whole port
contract: it must read a live run and an archived bundle identically.
"""

from __future__ import annotations

import json
from pathlib import Path

from stella_harbor.live_feed import _EventsCache, pair_rows, render, state, trial_view


def _write_trial(
    root: Path,
    job: str,
    task: str,
    *,
    events: list[dict] | None = None,
    result: dict | None = None,
    trajectory: dict | None = None,
) -> Path:
    trial = root / job / f"{task}__abc1234"
    (trial / "agent").mkdir(parents=True)
    if events is not None:
        (trial / "agent" / "stella-events.jsonl").write_text(
            "".join(json.dumps(e) + "\n" for e in events)
        )
    if result is not None:
        (trial / "result.json").write_text(json.dumps(result))
    if trajectory is not None:
        (trial / "agent" / "trajectory.json").write_text(json.dumps(trajectory))
    return trial


_STEP = {
    "type": "step_usage",
    "input_tokens": 1000,
    "output_tokens": 500,
    "cost_usd": 0.01,
    "tool_calls": 2,
}
_CAP_STEP = {
    "type": "step_usage",
    "input_tokens": 7861,
    "output_tokens": 16384,
    "cost_usd": 0.025,
    "tool_calls": 0,
}


def test_running_trial_reads_from_the_event_stream(tmp_path: Path) -> None:
    trial = _write_trial(
        tmp_path,
        "j-armA",
        "regex-log",
        events=[{"type": "tool_start"}, {"type": "tool_start"}, _STEP],
    )
    view = trial_view(trial, _EventsCache())
    assert view["status"] == "running"
    assert view["tools"] == 2
    assert view["steps"] == 1
    assert view["output_tokens"] == 500
    assert view["cost_usd"] == 0.01
    assert view["age_s"] is not None


def test_cap_hit_and_zero_tool_flags_surface(tmp_path: Path) -> None:
    # The zero-tool failure signature: a step that spent the whole output
    # allowance without a tool call, then a complete event.
    trial = _write_trial(
        tmp_path,
        "j-armA",
        "regex-log",
        events=[_CAP_STEP, {"type": "judge_verdict", "passed": False}, {"type": "complete"}],
    )
    view = trial_view(trial, _EventsCache())
    assert view["status"] == "done"
    assert view["cap_hits"] == 1
    assert view["tools"] == 0
    assert view["judge_passed"] is False


def test_result_json_is_the_verdict_of_record(tmp_path: Path) -> None:
    trial = _write_trial(
        tmp_path,
        "j-armB",
        "extract-elf",
        trajectory={"steps": [{"tool_calls": [{}, {}]}, {"tool_calls": []}]},
        result={
            "verifier_result": {"reward": 1},
            "started_at": "2026-07-31T10:00:00",
            "finished_at": "2026-07-31T10:05:30",
        },
    )
    view = trial_view(trial, _EventsCache())
    assert view["status"] == "done"
    assert view["resolved"] is True
    assert view["steps"] == 2
    assert view["tools"] == 2
    assert view["wall_s"] == 330


def test_exception_class_is_a_failure_and_named(tmp_path: Path) -> None:
    trial = _write_trial(
        tmp_path,
        "j-armA",
        "qemu-startup",
        result={"exception_info": {"exception_type": "NonZeroAgentExitCodeError"}},
    )
    view = trial_view(trial, _EventsCache())
    assert view["resolved"] is False
    assert view["failure"] == "NonZeroAgentExitCodeError"


def test_rows_pair_by_task_across_arms_and_total(tmp_path: Path) -> None:
    _write_trial(tmp_path, "j-armA", "regex-log", events=[_CAP_STEP, {"type": "complete"}])
    _write_trial(
        tmp_path,
        "j-armB",
        "regex-log",
        result={"verifier_result": {"reward": 0}},
        trajectory={"steps": []},
    )
    _write_trial(tmp_path, "j-armA", "only-in-a", events=[_STEP])

    snapshot = state(tmp_path, "j-armA", "j-armB", _EventsCache())
    tasks = [row["task"] for row in snapshot["rows"]]
    assert tasks == ["only-in-a", "regex-log"]
    paired = snapshot["rows"][1]
    assert paired["a"] is not None and paired["b"] is not None
    assert snapshot["totals"]["a"]["trials"] == 2
    assert snapshot["totals"]["a"]["cap_hits"] == 1
    assert snapshot["totals"]["b"]["done"] == 1

    table = render(snapshot)
    assert "regex-log" in table and "only-in-a" in table
    assert "A totals:" in table and "B totals:" in table


def test_growth_cache_reparses_only_on_size_change(tmp_path: Path) -> None:
    trial = _write_trial(tmp_path, "j-armA", "t", events=[_STEP])
    events_file = trial / "agent" / "stella-events.jsonl"
    cache = _EventsCache()
    first = cache.summary(events_file)
    assert first is not None and first["steps"] == 1
    # Same size → served from cache (mutate the cached copy to prove reuse).
    again = cache.summary(events_file)
    assert again is not None and again["steps"] == 1
    with events_file.open("a") as handle:
        handle.write(json.dumps(_STEP) + "\n")
    grown = cache.summary(events_file)
    assert grown is not None and grown["steps"] == 2


def test_mid_write_tail_line_is_tolerated(tmp_path: Path) -> None:
    trial = _write_trial(tmp_path, "j-armA", "t", events=[_STEP])
    events_file = trial / "agent" / "stella-events.jsonl"
    with events_file.open("a") as handle:
        handle.write('{"type":"tool_start"')  # torn write, no newline yet
    view = trial_view(trial, _EventsCache())
    assert view["steps"] == 1
    assert view["tools"] == 0
