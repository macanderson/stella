"""The census reproduces the shape `#3753` measured, and states what it is over.

The number this module exists to produce has already been quoted three ways for
one match — 75% over nine tasks in the issue body, 52% over the twelve trials
that carried events, and 15.5% over every match on the machine. None of those
corrects another, so the required assertion here is not that a share is right.
It is that a share never travels without the trial count it is a share of, and
that a trace with no wall clock cannot silently enter one.

The commands in `a_killed_call_whose_command_comes_back_is_paid_for_twice` are
the ones `pytorch-model-cli` actually sent before it timed out at its budget.

Offline by construction — every trace here is written, never fetched.
"""

from __future__ import annotations

import json
from pathlib import Path

from census import _load, census_of, render, summarize
from fixtures import err, ok, write_run
from run_trace import load_run


def call(call_id: str, command: str, duration_ms: int, output: dict, *, ts: int = 1):
    """A `bash` pair carrying the elapsed time the census reads."""
    return [
        {
            "ts": ts,
            "type": "tool_start",
            "call": {"call_id": call_id, "name": "bash", "input": {"command": command}},
        },
        {
            "ts": ts + duration_ms,
            "type": "tool_result",
            "call_id": call_id,
            "output": output,
            "duration_ms": duration_ms,
        },
    ]


def load(tmp_path: Path, trials: list[dict]):
    return load_run(write_run(tmp_path, trials))


def test_a_trial_without_a_timestamp_stays_out_of_every_share(tmp_path: Path):
    timed = {
        "task": "timed__aaa",
        "events": call("c1", "make", 10_000, ok("built"), ts=1_000)
        + [{"ts": 41_000, "type": "step_usage", "duration_ms": 20_000}],
        "reward": 1.0,
    }
    untimed = {
        "task": "untimed__bbb",
        "events": [
            {
                "type": "tool_start",
                "call": {"call_id": "c2", "name": "bash", "input": {"command": "make"}},
            },
            {
                "type": "tool_result",
                "call_id": "c2",
                "output": ok("built"),
                "duration_ms": 600_000,
            },
        ],
        "reward": 1.0,
    }
    run = load(tmp_path, [timed, untimed])
    census = summarize(census_of(trial) for trial in run.trials)

    assert census.trials == 2
    assert census.untimed_trials == 1
    assert census.timed_trials == 1
    # The 600s the untimed trial spent is real, and it is absent from the share
    # rather than sitting in a denominator no clock supports.
    assert census.tool_ms == 10_000
    assert census.wall_ms == 40_000
    assert census.tool_share == 25.0
    assert census.model_share == 50.0
    # It is still visible where no clock is needed to read it.
    assert census.bash_ms == 610_000


def test_a_killed_call_whose_command_comes_back_is_paid_for_twice(tmp_path: Path):
    events = (
        call(
            "c1",
            "pip install torch --quiet",
            120_000,
            err("command timed out after 120s"),
            ts=0,
        )
        + call(
            "c2",
            "timeout 300 pip install torch --quiet",
            300_000,
            err("command timed out after 300s"),
            ts=200_000,
        )
        + call(
            "c3",
            "pip install torch --quiet",
            60_000,
            ok("Successfully installed torch"),
            ts=600_000,
        )
    )
    run = load(
        tmp_path,
        [{"task": "pytorch-model-cli__mrL9jSa", "events": events, "reward": 0.0}],
    )
    census = summarize(census_of(trial) for trial in run.trials)

    assert census.timed_out_ms == 420_000
    # Both killed attempts came back later, so neither bought anything.
    assert census.reattempted_ms == 420_000
    # Only the first was repeated word for word; the floor is reported beside
    # the similarity reading so a reader can see what the threshold bought.
    assert census.reattempted_exact_ms == 120_000
    assert census.timed_out_share is not None
    assert round(census.timed_out_share, 1) == 87.5


def test_a_run_that_never_timed_out_reports_no_waste(tmp_path: Path):
    events = call("c1", "pytest -q", 30_000, ok("12 passed"), ts=0) + call(
        "c2", "git diff --stat", 500, ok(" 1 file changed"), ts=40_000
    )
    run = load(tmp_path, [{"task": "healthy__ccc", "events": events, "reward": 1.0}])
    census = summarize(census_of(trial) for trial in run.trials)

    assert census.timed_out_ms == 0
    assert census.reattempted_ms == 0
    assert census.timed_out_share == 0.0


def test_the_rendered_census_names_the_basis_of_its_share(tmp_path: Path):
    timed = {
        "task": "a__aaa",
        "events": call("c1", "make", 10_000, ok("ok"), ts=0),
        "reward": 1.0,
    }
    untimed = {
        "task": "b__bbb",
        "events": [
            {
                "type": "tool_start",
                "call": {"call_id": "c2", "name": "bash", "input": {"command": "make"}},
            },
            {
                "type": "tool_result",
                "call_id": "c2",
                "output": ok("ok"),
                "duration_ms": 1_000,
            },
        ],
        "reward": 1.0,
    }
    run = load(tmp_path, [timed, untimed])
    text = render(summarize(census_of(trial) for trial in run.trials), label="panel")

    assert "1 trials with a wall clock, 1 on a schema without one" in text
    assert "tool execution" in text


def test_a_share_with_no_wall_clock_behind_it_is_absent_rather_than_zero(
    tmp_path: Path,
):
    untimed = {
        "task": "b__bbb",
        "events": [
            {
                "type": "tool_start",
                "call": {"call_id": "c2", "name": "bash", "input": {"command": "make"}},
            },
            {
                "type": "tool_result",
                "call_id": "c2",
                "output": ok("ok"),
                "duration_ms": 1_000,
            },
        ],
        "reward": 1.0,
    }
    census = summarize(census_of(trial) for trial in load(tmp_path, [untimed]).trials)

    assert census.tool_share is None
    assert "n/a" in render(census, label="panel")


def test_a_trial_that_recorded_nothing_is_not_reported_as_an_old_schema(tmp_path: Path):
    """A credential that never authenticated writes empty traces.

    Folding those into `untimed_trials` would describe a broken run as an old
    one, which is the distinction `postmortem` puts first for the same reason.
    """
    old_schema = {
        "task": "old__aaa",
        "events": [
            {
                "type": "tool_start",
                "call": {"call_id": "c1", "name": "bash", "input": {"command": "make"}},
            },
            {
                "type": "tool_result",
                "call_id": "c1",
                "output": ok("ok"),
                "duration_ms": 1_000,
            },
        ],
        "reward": 1.0,
    }
    recorded_nothing = {"task": "empty__bbb", "events": [], "reward": 0.0}
    census = summarize(
        census_of(t) for t in load(tmp_path, [old_schema, recorded_nothing]).trials
    )

    assert census.untimed_trials == 1
    assert census.empty_trials == 1
    assert census.timed_trials == 0
    assert "1 on a schema without one, 1 that recorded nothing" in render(
        census, label="panel"
    )


def test_the_other_contestants_seat_is_not_a_trial_that_recorded_nothing(
    tmp_path: Path,
):
    """A match holds both seats, and only one of them writes `stella-events.jsonl`.

    Walking on the `agent/` directory alone counted every opposing trial as a
    trace that recorded nothing, which read as 16 failures on the match `#3753`
    cites and none of them were ours.
    """
    match = tmp_path / "13f7f2bb533d"
    ours = match / "jobs" / "m-stella-low" / "build__aaa" / "agent"
    ours.mkdir(parents=True)
    (ours / "stella-events.jsonl").write_text(
        "\n".join(
            json.dumps(e) for e in call("c1", "make", 10_000, ok("built"), ts=1_000)
        )
    )
    theirs = match / "jobs" / "m-claude-code-low" / "build__aaa" / "agent"
    theirs.mkdir(parents=True)
    (theirs / "transcript.jsonl").write_text("{}\n")

    census = summarize(census_of(trial) for trial in _load(match))

    assert census.trials == 1
    assert census.empty_trials == 0
