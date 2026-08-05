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

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

from stella_harbor.live_feed import (  # noqa: E402 - after importorskip by design
    _EventsCache,
    pair_rows,
    render,
    state,
    trial_view,
)


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


def test_cached_input_split_survives_the_reduction(tmp_path: Path) -> None:
    """#1285: the cached/uncached input split must reach the table.

    Two arms with identical input_tokens but opposite hit rates are different
    harnesses; before the split was carried, ``_reduce`` summed only
    ``input_tokens`` and the difference surfaced nowhere but ``cost_usd``.
    """
    cached_step = {
        **_STEP,
        "input_tokens": 10_000,
        "cached_input_tokens": 8_000,
        "cache_write_tokens": 1_500,
    }
    trial = _write_trial(
        tmp_path,
        "j-armA",
        "regex-log",
        events=[cached_step, cached_step, {"type": "complete"}],
    )
    view = trial_view(trial, _EventsCache())
    assert view["input_tokens"] == 20_000
    assert view["cached_input_tokens"] == 16_000
    assert view["cache_write_tokens"] == 3_000

    snapshot = state(tmp_path, "j-armA", "j-armB", _EventsCache())
    totals = snapshot["totals"]["a"]
    assert totals["cached_input_tokens"] == 16_000
    assert totals["cache_write_tokens"] == 3_000
    # The rendered footer names the split — 16k of 20k is 80%.
    assert "(80% cached)" in render(snapshot)


def test_a_trial_with_no_telemetry_makes_no_cache_claim(tmp_path: Path) -> None:
    # Same discipline as cap_hits_source: absent telemetry is not a
    # truthful zero.
    trial = _write_trial(tmp_path, "j-armA", "regex-log", result={"reward": 1})
    view = trial_view(trial, _EventsCache())
    assert view["cached_input_tokens"] is None
    assert view["cache_write_tokens"] is None


def test_archived_trajectory_supplies_the_cached_total(tmp_path: Path) -> None:
    # An archived bundle with no event stream still carries the split via
    # ATIF's final_metrics (total_cached_tokens), so the trajectory-only
    # path must read it.
    trial = _write_trial(
        tmp_path,
        "j-armB",
        "extract-elf",
        trajectory={
            "steps": [],
            "final_metrics": {
                "total_prompt_tokens": 30_000,
                "total_completion_tokens": 2_000,
                "total_cached_tokens": 24_000,
            },
        },
    )
    view = trial_view(trial, _EventsCache())
    assert view["input_tokens"] == 30_000
    assert view["cached_input_tokens"] == 24_000


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
        events=[_CAP_STEP, {"type": "verdict", "passed": False}, {"type": "complete"}],
    )
    view = trial_view(trial, _EventsCache())
    assert view["status"] == "done"
    assert view["cap_hits"] == 1
    assert view["cap_hits_source"] == "inferred"
    assert view["tools"] == 0
    assert view["verifier_passed"] is False


def test_reported_finish_reason_beats_the_output_size_heuristic(tmp_path: Path) -> None:
    """#1211 §6.5: a long answer is not a cap hit, and the provider says so.

    Both steps here are the exact shape the heuristic fires on — output at
    or past the 16K threshold, no tool call — but the provider reported a
    natural stop. Under the old rule this trial contributed 2 to `cap_hits`;
    that reading, applied across a 64K-ceiling run, is what produced the
    unexplained 106.
    """
    long_answer = {**_CAP_STEP, "output_tokens": 24_000, "finish_reason": "stop"}
    trial = _write_trial(
        tmp_path,
        "j-armA",
        "regex-log",
        events=[long_answer, long_answer, {"type": "complete"}],
    )
    view = trial_view(trial, _EventsCache())
    assert view["cap_hits"] == 0
    assert view["cap_hits_source"] == "reported"


def test_reported_truncation_is_counted(tmp_path: Path) -> None:
    # The other direction: a genuinely truncated step is counted even though
    # its output is far below the heuristic's threshold, which the inference
    # route could never have caught.
    truncated = {
        "type": "step_usage",
        "input_tokens": 900,
        "output_tokens": 128,
        "cost_usd": 0.002,
        "tool_calls": 0,
        "finish_reason": "length",
    }
    trial = _write_trial(
        tmp_path,
        "j-armA",
        "regex-log",
        events=[truncated, {"type": "complete"}],
    )
    view = trial_view(trial, _EventsCache())
    assert view["cap_hits"] == 1
    assert view["cap_hits_source"] == "reported"


def test_legacy_stream_without_finish_reason_still_infers(tmp_path: Path) -> None:
    # Archived bundles predate the field entirely. They must keep classifying
    # exactly as before — the fallback is why the heuristic still exists.
    trial = _write_trial(
        tmp_path,
        "j-armA",
        "regex-log",
        events=[_CAP_STEP, {"type": "complete"}],
    )
    view = trial_view(trial, _EventsCache())
    assert view["cap_hits"] == 1
    assert view["cap_hits_source"] == "inferred"


def test_a_trial_with_no_telemetry_has_no_cap_hit_claim(tmp_path: Path) -> None:
    # Absent is not a truthful zero: with no event stream there is no count
    # by either route, and the source must say so rather than implying one.
    trial = _write_trial(tmp_path, "j-armA", "regex-log", result={"reward": 1})
    view = trial_view(trial, _EventsCache())
    assert view["cap_hits_source"] is None


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


def test_a_summed_cap_hit_count_names_the_route_it_came_from(tmp_path: Path) -> None:
    # One trial reports, one infers. The sum is neither claim, and the
    # footer has to say so instead of presenting a mixed number as measured.
    _write_trial(tmp_path, "j-armA", "legacy", events=[_CAP_STEP, {"type": "complete"}])
    _write_trial(
        tmp_path,
        "j-armA",
        "current",
        events=[
            {**_CAP_STEP, "finish_reason": "length"},
            {"type": "complete"},
        ],
    )
    _write_trial(tmp_path, "j-armB", "legacy", result={"verifier_result": {"reward": 1}})

    snapshot = state(tmp_path, "j-armA", "j-armB", _EventsCache())
    assert snapshot["totals"]["a"]["cap_hits"] == 2
    assert snapshot["totals"]["a"]["cap_hits_source"] == "mixed"
    # Arm B has no event stream at all, so it makes no cap-hit claim.
    assert snapshot["totals"]["b"]["cap_hits_source"] is None
    assert "cap-hits 2 (mixed)" in render(snapshot)


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
