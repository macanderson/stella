"""An unfilled placeholder must never be readable as a computed zero."""

from __future__ import annotations

import json
import os

import pytest
from ingest import (
    COST_NORM_NO_MODEL,
    COST_NORM_NO_RUN_DATE,
    COST_NORM_NO_TOKENS,
    COST_NORM_PRICED,
    COST_NORM_UNMIGRATED,
    COST_NORM_UNPRICED_MODEL,
    connect,
    elapsed_seconds,
    ingest_trial,
    price_trial,
    trial_model,
)


def _result(**overrides):
    base = {
        "started_at": "2026-08-15T09:00:00Z",
        "finished_at": "2026-08-15T09:04:00Z",
        "config": {"agent": {"model_name": "anthropic/claude-sonnet-5"}},
        "agent_result": {
            "cost_usd": 2.65,
            "n_input_tokens": 5_000_000,
            "n_cache_tokens": 4_500_000,
            "n_output_tokens": 120_000,
        },
        "verifier_result": {"rewards": {"reward": 1.0}},
    }
    base.update(overrides)
    return base


def test_a_priced_trial_gets_a_number_and_says_so():
    cost, status = price_trial(_result(), None)
    assert status == COST_NORM_PRICED
    # 500k fresh @ $2, 4.5M cached @ $0.20, 120k out @ $10 — the introductory
    # rate, because the trial started before it lapsed.
    assert cost == pytest.approx(0.5 * 2.00 + 4.5 * 0.20 + 0.12 * 10.00)


def test_both_arms_price_the_same_trial_identically():
    """The whole point: Stella reports the routing prefix, the comparator does
    not, and the two must not diverge because of it."""
    prefixed = _result()
    bare = _result(config={"agent": {"model_name": "claude-sonnet-5"}})
    assert price_trial(prefixed, None) == price_trial(bare, None)


def test_the_trials_own_model_beats_the_run_label():
    """`--model` labels the run; `config.agent.model_name` is what the arm was
    actually launched with, and that is what priced the tokens."""
    result = _result()
    assert trial_model(result, "z-ai/glm-5.2") == "anthropic/claude-sonnet-5"
    assert trial_model(_result(config={}), "z-ai/glm-5.2") == "z-ai/glm-5.2"
    assert trial_model(_result(config={}), None) is None


def test_a_number_and_the_priced_status_never_come_apart():
    for result, model in (
        (_result(), None),
        (_result(config={}), None),
        (_result(config={"agent": {"model_name": "made/up-model"}}), None),
    ):
        cost, status = price_trial(result, model)
        assert (cost is None) == (status != COST_NORM_PRICED)


def test_an_unpriceable_trial_is_null_with_a_reason_never_zero():
    cases = {
        COST_NORM_NO_MODEL: (_result(config={}), None),
        COST_NORM_UNPRICED_MODEL: (
            _result(config={"agent": {"model_name": "made/up-model"}}),
            None,
        ),
        COST_NORM_NO_TOKENS: (_result(agent_result={"cost_usd": 1.0}), None),
        # The model's rate changed and the trial carries no readable start
        # time, so no entry can be chosen without guessing.
        COST_NORM_NO_RUN_DATE: (_result(started_at=None), None),
    }
    for expected, (result, model) in cases.items():
        cost, status = price_trial(result, model)
        assert status == expected
        assert cost is None, f"{expected} must not be reported as $0.00"


def test_a_crashed_trial_is_not_free():
    """Charging a trial that never reported usage zero would make a crash look
    like the cheapest possible run."""
    cost, status = price_trial(_result(agent_result={}), None)
    assert cost is None
    assert status == COST_NORM_NO_TOKENS


def test_wall_seconds_is_derived_and_a_backwards_clock_is_unknown():
    assert elapsed_seconds("2026-08-15T09:00:00Z", "2026-08-15T09:04:00Z") == 240.0
    assert elapsed_seconds("2026-08-15T09:04:00Z", "2026-08-15T09:00:00Z") is None
    assert elapsed_seconds(None, "2026-08-15T09:04:00Z") is None
    assert elapsed_seconds("not a stamp", "2026-08-15T09:04:00Z") is None


def _seed_run(conn, run_id="tag"):
    conn.execute(
        "INSERT OR REPLACE INTO runs (run_id,tag,kind,model,ingested_at)"
        " VALUES (?,?,?,?,?)",
        (run_id, run_id, "smoke", "anthropic/claude-sonnet-5", "2026-08-15T09:00:00Z"),
    )


def _write_trial(tmp_path, result):
    tdir = tmp_path / "job" / "task__abcdefg"
    tdir.mkdir(parents=True)
    (tdir / "result.json").write_text(json.dumps(result))
    return str(tdir) + os.sep


def test_ingest_writes_the_computed_cost_not_a_placeholder(tmp_path):
    conn = connect(str(tmp_path / "bench.db"))
    _seed_run(conn)
    ingest_trial(conn, "tag", "stella", "stella", _write_trial(tmp_path, _result()))

    cost, status, wall, started, finished = conn.execute(
        "SELECT cost_usd_norm, cost_norm_status, wall_seconds, started_at,"
        " finished_at FROM trials"
    ).fetchone()
    assert status == COST_NORM_PRICED
    assert cost is not None and cost > 0
    assert wall == 240.0
    assert started == "2026-08-15T09:00:00Z"
    assert finished == "2026-08-15T09:04:00Z"


def test_ingest_records_why_a_trial_is_unpriced(tmp_path):
    conn = connect(str(tmp_path / "bench.db"))
    _seed_run(conn)
    ingest_trial(
        conn,
        "tag",
        "stella",
        "stella",
        _write_trial(tmp_path, _result(config={})),
    )

    cost, status = conn.execute(
        "SELECT cost_usd_norm, cost_norm_status FROM trials"
    ).fetchone()
    assert cost is None
    assert status == COST_NORM_NO_MODEL


def test_migration_is_idempotent_and_marks_pre_existing_rows(tmp_path):
    """A store written before this column existed keeps its rows, and their
    NULL cost is labelled as having no recorded reason rather than borrowing
    one it never had."""
    db = str(tmp_path / "old.db")
    conn = connect(db)
    _seed_run(conn)
    # Simulate the pre-column shape by dropping and rebuilding without it.
    conn.execute("DROP TABLE trials")
    conn.execute(
        "CREATE TABLE trials (trial_id TEXT PRIMARY KEY, run_id TEXT NOT NULL"
        " REFERENCES runs(run_id), arm TEXT NOT NULL, agent TEXT NOT NULL,"
        " task_name TEXT NOT NULL, trial_dir TEXT NOT NULL, reward REAL,"
        " passed INTEGER, exception_type TEXT, exception_message TEXT,"
        " n_input_tokens INTEGER, n_cache_tokens INTEGER, n_output_tokens INTEGER,"
        " cost_usd_self REAL, cost_usd_norm REAL, wall_seconds REAL,"
        " started_at TEXT, finished_at TEXT)"
    )
    conn.execute(
        "INSERT INTO trials (trial_id,run_id,arm,agent,task_name,trial_dir)"
        " VALUES ('old','tag','stella','stella','task','/tmp/task')"
    )
    conn.commit()
    conn.close()

    conn = connect(db)  # runs migrate()
    connect(db).close()  # and again — must not fail
    status = conn.execute(
        "SELECT cost_norm_status FROM trials WHERE trial_id = 'old'"
    ).fetchone()[0]
    assert status == COST_NORM_UNMIGRATED
