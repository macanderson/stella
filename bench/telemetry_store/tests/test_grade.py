"""A grade must be reproducible, and an absent one must never read as a zero."""

from __future__ import annotations

import json

import pytest
from grade import (
    CONSISTENT,
    DISCORDANT,
    GRADER_VERSION,
    NEUTRAL,
    NO_MEASURABLE_EFFECT,
    PRODUCTIVE,
    UNCLASSIFIED_FLOOR,
    UNPRODUCTIVE,
    grade,
    grade_trial,
    origin_ms,
    split_windows,
    verdict_alignment,
)
from ingest import COST_NORM_PRICED, COST_NORM_UNPRICED_MODEL, connect

TRIAL = "tag:stella:task__abcdefg"
ORIGIN = 1_770_000_000_000


def step_usage(step, *, turn=1, call_seq=0, ts=None, model="anthropic/claude-sonnet-5", **extra):
    """A `step_usage` line, the event that closes a window."""
    event = {
        "type": "step_usage",
        "step": step,
        "turn_instance": turn,
        "call_seq": call_seq,
        "role": "worker",
        "model": model,
        "input_tokens": 1_000_000,
        "cached_input_tokens": 900_000,
        "output_tokens": 10_000,
        "cost_usd": 0.5,
        "duration_ms": 1_200,
        "retries": 0,
        "tool_calls": 0,
        "complete": True,
    }
    if ts is not None:
        event["ts"] = ts
    event.update(extra)
    return event


def tool_start(call_id, command):
    return {
        "type": "tool_start",
        "call": {"call_id": call_id, "name": "bash", "input": {"command": command}},
    }


def tool_result(call_id, *, failed, message="assertion failed: left == right"):
    output = {"error": {"message": message}} if failed else {"ok": {"content": "ok"}}
    return {"type": "tool_result", "call_id": call_id, "output": output, "duration_ms": 5}


def file_change(path, diff):
    return {"type": "file_change", "path": path, "kind": "modified", "diff": diff}


def seed(tmp_path, events, *, passed=1, started_at="2026-08-15T09:00:00Z"):
    """A store holding one run, one trial, and the given event stream."""
    conn = connect(str(tmp_path / "bench.db"))
    conn.execute(
        "INSERT OR REPLACE INTO runs (run_id,tag,kind,model,ingested_at)"
        " VALUES ('tag','tag','smoke','anthropic/claude-sonnet-5','2026-08-15T09:00:00Z')"
    )
    conn.execute(
        "INSERT OR REPLACE INTO trials (trial_id,run_id,arm,agent,task_name,trial_dir,"
        "reward,passed,started_at) VALUES (?,?,?,?,?,?,?,?,?)",
        (TRIAL, "tag", "stella", "stella", "task", "/tmp/task", 1.0, passed, started_at),
    )
    for seq, event in enumerate(events):
        conn.execute(
            "INSERT OR REPLACE INTO events (event_id,trial_id,seq,type,payload_json)"
            " VALUES (?,?,?,?,?)",
            (f"{TRIAL}:{seq}", TRIAL, seq, event["type"], json.dumps(event)),
        )
    conn.commit()
    return conn


def steps(conn):
    """Every graded window as `(turn, step, call_index, direction, reason)`."""
    return conn.execute(
        "SELECT turn_instance, step, call_index, direction, direction_reason"
        " FROM step_grades ORDER BY turn_instance, step, call_index"
    ).fetchall()


# ── The rule chain ───────────────────────────────────────────────────────────


def test_a_loop_the_engine_detected_outranks_everything_else(tmp_path):
    """Rule 1 is highest priority because it is the engine's own verdict: a
    window that also edited a file still grades unproductive."""
    conn = seed(
        tmp_path,
        [
            file_change("a.rs", "@@\n+let x = 1;"),
            {
                "type": "loop_detected",
                "turn_instance": 1,
                "kind": "exact_repeat",
                "pattern": ["search"],
                "repeats": 3,
                "evidence": "same call three times",
                "aborted": False,
            },
            step_usage(1),
        ],
    )
    grade(conn)
    assert steps(conn) == [(1, 1, 0, UNPRODUCTIVE, "loop_detected:exact_repeat")]


def test_a_terminal_error_is_unproductive_and_a_retryable_one_is_not(tmp_path):
    """`retryable: false` is the engine saying another attempt could not have
    helped. A retryable error is an incident, not a verdict on the step."""
    conn = seed(
        tmp_path,
        [
            {"type": "error", "message": "bad credential", "retryable": False},
            step_usage(1),
            {"type": "error", "message": "502 from the gateway", "retryable": True},
            step_usage(2, turn=2),
        ],
    )
    grade(conn)
    graded = {(row[1], row[3], row[4]) for row in steps(conn)}
    assert (1, UNPRODUCTIVE, "fatal_error") in graded
    assert (2, NEUTRAL, NO_MEASURABLE_EFFECT) in graded


def test_an_edit_the_trial_keeps_is_productive_and_one_it_undoes_is_not(tmp_path):
    """Rule 3: a line survives when no later change on the same path removes
    it.

    The third window is the interesting one. It reverts the second, and the
    revert grades *productive* — the trial's final state is the ground truth
    this rule measures against, and the final state does not carry that line.
    The window that gets charged for it is the one that wrote it.
    """
    conn = seed(
        tmp_path,
        [
            file_change("a.rs", "@@\n+let kept = 1;"),
            step_usage(1),
            file_change("b.rs", "@@\n+let doomed = 2;"),
            step_usage(2),
            file_change("b.rs", "@@\n-let doomed = 2;"),
            step_usage(3),
        ],
    )
    grade(conn)
    assert steps(conn) == [
        (1, 1, 0, PRODUCTIVE, "diff_convergent:+1/-0"),
        (1, 2, 0, UNPRODUCTIVE, "diff_divergent:+0/-1"),
        (1, 3, 0, PRODUCTIVE, "diff_convergent:+1/-0"),
    ]


def test_a_check_that_goes_red_to_green_is_productive(tmp_path):
    """Rule 4 reads the `ToolOutput` arm, which is the shell's exit signal —
    the nonzero exit is the `error` arm, never prose to be sniffed."""
    conn = seed(
        tmp_path,
        [
            tool_start("c1", "cargo test -p stella-core"),
            tool_result("c1", failed=True),
            step_usage(1),
            tool_start("c2", "cargo test -p stella-core"),
            tool_result("c2", failed=False),
            step_usage(2),
        ],
    )
    grade(conn)
    assert steps(conn)[1] == (1, 2, 0, PRODUCTIVE, "tool_exit:fixed")


def test_the_same_failure_twice_is_unproductive(tmp_path):
    """Repeating a check that fails the same way is the shape rule 4 exists to
    name: work was spent and the signature did not move."""
    conn = seed(
        tmp_path,
        [
            tool_start("c1", "pytest -q"),
            tool_result("c1", failed=True),
            step_usage(1),
            tool_start("c2", "pytest -q"),
            tool_result("c2", failed=True),
            step_usage(2),
        ],
    )
    grade(conn)
    assert steps(conn)[1] == (1, 2, 0, UNPRODUCTIVE, "tool_exit:repeated_failure")


def test_a_window_that_did_nothing_says_so_rather_than_falling_to_the_floor(tmp_path):
    """`no_measurable_effect` and `unclassified_floor` are different facts: the
    first is settled, the second is the residue a judge is for."""
    conn = seed(
        tmp_path,
        [
            {"type": "text", "text": "restating the plan"},
            step_usage(1),
            tool_start("c1", "ls -la"),
            tool_result("c1", failed=False),
            step_usage(2),
        ],
    )
    grade(conn)
    reasons = [row[4] for row in steps(conn)]
    assert reasons == [NO_MEASURABLE_EFFECT, UNCLASSIFIED_FLOOR]


def test_no_window_is_ever_written_by_the_judge_that_does_not_exist(tmp_path):
    """Phase 1 writes `deterministic` on every row. A row claiming
    `agent_judge` would mean a judge ran, and none is built."""
    conn = seed(tmp_path, [step_usage(1), file_change("a.rs", "@@\n+x"), step_usage(2)])
    grade(conn)
    sources = {
        row[0] for row in conn.execute("SELECT DISTINCT direction_source FROM step_grades")
    }
    assert sources == {"deterministic"}


# ── Windowing and the price clock ────────────────────────────────────────────


def test_two_calls_sharing_one_step_grade_as_two_rows(tmp_path):
    """`call_index` is what stops a step's auxiliary call from overwriting its
    worker call on the identity index."""
    conn = seed(tmp_path, [step_usage(1, call_seq=0), step_usage(1, call_seq=1)])
    grade(conn)
    assert [(row[1], row[2]) for row in steps(conn)] == [(1, 0), (1, 1)]


def test_a_stream_with_no_call_seq_keeps_both_windows(tmp_path):
    """A pre-#4793 stream cannot say which call was the worker. Positional
    indexing orders them without claiming one, and loses neither."""
    events = [step_usage(1), step_usage(1)]
    for event in events:
        del event["call_seq"]
    conn = seed(tmp_path, events)
    grade(conn)
    assert [(row[1], row[2]) for row in steps(conn)] == [(1, 0), (1, 1)]


def test_timestamps_are_milliseconds_from_the_trials_own_first_event(tmp_path):
    """The price clock: a trial read today and one read six months from now
    must assign the same `ts_start_ms` to the same step."""
    events = [
        {"type": "stage", "ts": ORIGIN, "name": "plan"},
        step_usage(1, ts=ORIGIN + 4_000),
        {"type": "text", "ts": ORIGIN + 5_000, "text": "hello"},
        step_usage(2, ts=ORIGIN + 9_000),
    ]
    assert origin_ms([(i, e["type"], e) for i, e in enumerate(events)]) == ORIGIN
    conn = seed(tmp_path, events)
    grade(conn)
    assert conn.execute(
        "SELECT ts_start_ms, ts_end_ms, wall_clock_ms FROM step_grades"
        " ORDER BY step"
    ).fetchall() == [(0, 4_000, 4_000), (5_000, 9_000, 4_000)]


def test_a_stream_with_no_stamps_leaves_the_timing_columns_null(tmp_path):
    """Absence over invention: a stream recorded before `ts` existed on the
    wire is still graded, and its timing columns stay NULL."""
    conn = seed(tmp_path, [file_change("a.rs", "@@\n+x"), step_usage(1)])
    grade(conn)
    row = conn.execute(
        "SELECT ts_start_ms, ts_end_ms, wall_clock_ms, direction FROM step_grades"
    ).fetchone()
    assert row == (None, None, None, PRODUCTIVE)


def test_a_step_is_priced_off_the_shared_table_and_says_when_it_cannot_be(tmp_path):
    """The step's cost is the same kind of number as `trials.cost_usd_norm` —
    one table, both grains — and an unpriceable model is NULL with a reason."""
    conn = seed(tmp_path, [step_usage(1), step_usage(2, model="made/up-model")])
    grade(conn)
    priced, unpriced = conn.execute(
        "SELECT cost_usd, cost_norm_status FROM step_grades ORDER BY step"
    ).fetchall()
    # 100k fresh @ $2, 900k cached @ $0.20, 10k out @ $10.
    assert priced == (pytest.approx(0.1 * 2.00 + 0.9 * 0.20 + 0.01 * 10.00), COST_NORM_PRICED)
    assert unpriced == (None, COST_NORM_UNPRICED_MODEL)


def test_a_roll_up_over_an_unpriced_step_is_null_not_a_partial_sum(tmp_path):
    """A partial sum published as a total is the failure the status vocabulary
    exists to prevent."""
    conn = seed(tmp_path, [step_usage(1), step_usage(2, model="made/up-model")])
    grade(conn)
    assert conn.execute(
        "SELECT cost_usd, cost_norm_status FROM execution_grades"
    ).fetchone() == (None, COST_NORM_UNPRICED_MODEL)


# ── Aggregation ──────────────────────────────────────────────────────────────


def test_a_turn_that_graded_no_step_has_a_null_ratio_never_zero(tmp_path):
    """The witness for the NULL rule: `0.0` cannot tell "nothing was graded"
    from "every step was unproductive", so the empty case must not produce
    one."""
    conn = seed(tmp_path, [step_usage(1)])
    trial_id, run_id, started_at, passed = conn.execute(
        "SELECT trial_id, run_id, started_at, passed FROM trials"
    ).fetchone()
    # A turn with no windows at all: graded directly, since no event stream can
    # produce one (a turn exists in this table because a step closed it).
    from grade import aggregate_execution, aggregate_turns

    rows = aggregate_turns(conn, trial_id, [], {}, GRADER_VERSION, "2026-08-15T09:00:00Z")
    aggregate_execution(
        conn, trial_id, run_id, rows, passed, GRADER_VERSION, "2026-08-15T09:00:00Z"
    )
    step_count, ratio = conn.execute(
        "SELECT step_count, agent_productive_step_ratio FROM execution_grades"
    ).fetchone()
    assert step_count == 0
    assert ratio is None


def test_the_execution_ratio_is_the_step_weighted_mean_of_its_turns(tmp_path):
    """Section 7 pins this identity so the two aggregation paths cannot drift
    apart silently."""
    conn = seed(
        tmp_path,
        [
            file_change("a.rs", "@@\n+kept"),
            step_usage(1, turn=1),
            step_usage(2, turn=1),
            file_change("b.rs", "@@\n+also kept"),
            step_usage(1, turn=2),
        ],
    )
    grade(conn)
    turns = conn.execute(
        "SELECT step_count, productive_step_ratio FROM turn_grades ORDER BY turn_instance"
    ).fetchall()
    total = sum(count for count, _ratio in turns)
    weighted = sum(count * ratio for count, ratio in turns) / total
    execution = conn.execute(
        "SELECT step_count, agent_productive_step_ratio FROM execution_grades"
    ).fetchone()
    assert execution[0] == total
    assert execution[1] == pytest.approx(weighted)


def test_the_turn_outcome_mirrors_how_the_turn_ended(tmp_path):
    conn = seed(
        tmp_path,
        [
            step_usage(1, turn=1),
            {"type": "turn_complete", "model": "m", "cost_usd": 0.1},
            step_usage(1, turn=2),
            {
                "type": "loop_detected",
                "turn_instance": 2,
                "kind": "stagnation",
                "pattern": ["search"],
                "repeats": 4,
                "evidence": "no progress",
                "aborted": True,
            },
            step_usage(1, turn=3),
        ],
    )
    grade(conn)
    assert [
        row[0]
        for row in conn.execute("SELECT outcome FROM turn_grades ORDER BY turn_instance")
    ] == ["complete", "loop_aborted", "in_progress"]


def test_verdict_alignment_is_null_when_either_half_is_unknown():
    """"We did not grade it" and "the grade disagreed with the verdict" are not
    the same reading."""
    assert verdict_alignment(None, 1) is None
    assert verdict_alignment(0.9, None) is None
    assert verdict_alignment(0.9, 1) == CONSISTENT
    assert verdict_alignment(0.1, 1) == DISCORDANT
    assert verdict_alignment(0.1, 0) == CONSISTENT


# ── Re-runnability ───────────────────────────────────────────────────────────


def test_re_grading_the_same_trial_replaces_rather_than_duplicates(tmp_path):
    conn = seed(tmp_path, [file_change("a.rs", "@@\n+x"), step_usage(1), step_usage(2)])
    grade(conn)
    first = steps(conn)
    grade(conn)
    assert steps(conn) == first
    counts = conn.execute(
        "SELECT (SELECT COUNT(*) FROM step_grades), (SELECT COUNT(*) FROM turn_grades),"
        " (SELECT COUNT(*) FROM execution_grades)"
    ).fetchone()
    assert counts == (2, 1, 1)


def test_a_new_grader_version_adds_rows_beside_the_old_ones(tmp_path):
    """`grader_version` is the re-run key: an improved ruleset must never erase
    what the old one saw."""
    conn = seed(tmp_path, [step_usage(1)])
    grade(conn)
    grade(conn, version="v2-experimental")
    versions = [
        row[0]
        for row in conn.execute(
            "SELECT grader_version FROM step_grades ORDER BY grader_version"
        )
    ]
    assert versions == ["v1-deterministic", "v2-experimental"]


def test_grading_never_writes_back_into_the_benchmarks_own_verdict(tmp_path):
    """"Replay, never perturb": pass/fail stays the benchmark grader's."""
    conn = seed(tmp_path, [step_usage(1)], passed=0)
    before = conn.execute("SELECT reward, passed FROM trials").fetchone()
    grade(conn)
    assert conn.execute("SELECT reward, passed FROM trials").fetchone() == before


def test_a_truncated_payload_is_counted_rather_than_read_as_an_absent_fact(tmp_path):
    """`ingest.py` slices `payload_json` mid-JSON (#5088). An unreadable event
    keeps its place in the stream and is reported, never silently dropped."""
    conn = seed(tmp_path, [file_change("a.rs", "@@\n+x"), step_usage(1)])
    conn.execute(
        "UPDATE events SET payload_json = ? WHERE seq = 0",
        (json.dumps(file_change("a.rs", "@@\n+x"))[:20],),
    )
    trial_id, run_id, started_at, passed = conn.execute(
        "SELECT trial_id, run_id, started_at, passed FROM trials"
    ).fetchone()
    report = grade_trial(conn, trial_id, run_id, started_at, passed)
    assert report["unreadable_payloads"] == 1
    # The window still exists — losing the `step_usage` boundary would take
    # every later window's identity key with it.
    assert report["windows"] == 1


def test_split_windows_closes_on_step_usage_and_leaves_the_tail_ungraded():
    """Events after the last `step_usage` belong to no model call, so they form
    no window — but they are still read for the turn outcome."""
    events = [
        (0, "tool_start", tool_start("c1", "ls")),
        (1, "step_usage", step_usage(1)),
        (2, "turn_complete", {"type": "turn_complete", "model": "m", "cost_usd": 0.1}),
    ]
    windows, outcomes = split_windows(events)
    assert len(windows) == 1
    assert (windows[0].seq_start, windows[0].seq_end) == (0, 1)
    assert outcomes == {1: "complete"}
