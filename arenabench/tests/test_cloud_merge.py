# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Regrouping fetched single-task results.json bodies into one trial."""

from __future__ import annotations

import json

import pytest

from arenabench.cloud_merge import (
    MergeError,
    load_fetched_results,
    merge_trial,
    write_merged_trial,
)


def _body(
    task: str,
    contestant_id: str,
    *,
    reward: float | None,
    dataset: str = "terminal-bench-2.1",
    tokens_in: int = 100,
) -> dict:
    return {
        "match": {
            "id": f"{contestant_id}-{task}",
            "name": "smoke",
            "dataset": dataset,
            "tasks": [task],
            "contestants": [{"id": contestant_id, "name": contestant_id}],
        },
        "dataset": {"key": dataset},
        "rows": [
            {
                "task": task,
                "cells": {
                    contestant_id: {
                        "task": task,
                        "status": "done",
                        "reward": reward,
                        "resolved": None if reward is None else reward >= 1.0,
                        "infrastructure": False,
                        "tokens_in": tokens_in,
                        "tokens_out": tokens_in,
                        "clock_time": 10.0,
                        "total_cost": 0.01,
                        "priced_cost": 0.02,
                        "cache_read": 0,
                        "cache_write": 0,
                    }
                },
            }
        ],
    }


def test_merges_two_arms_across_every_task_into_one_trial() -> None:
    bodies = [
        _body("task-a", "stella", reward=1.0),
        _body("task-b", "stella", reward=0.0),
        _body("task-a", "claude-code", reward=0.0),
        _body("task-b", "claude-code", reward=1.0),
    ]
    merged = merge_trial(bodies)
    assert merged["match"]["tasks"] == ["task-a", "task-b"]
    assert {c["id"] for c in merged["match"]["contestants"]} == {"stella", "claude-code"}
    assert len(merged["rows"]) == 2
    assert set(merged["rows"][0]["cells"]) == {"stella", "claude-code"}
    assert merged["incomplete_tasks"] == {}


def test_this_is_the_bug_being_fixed_flat_files_have_no_trial_view() -> None:
    """The witness: before this module existed, N fetched results.json files
    were the whole story — nothing combined a run's two arms into one trial
    spanning every task. This asserts the fix actually produces that view.
    """
    bodies = [
        _body(f"task-{i}", arm, reward=float(i % 2))
        for i in range(5)
        for arm in ("a", "b")
    ]
    merged = merge_trial(bodies)
    assert len(merged["match"]["tasks"]) == 5
    assert len(merged["match"]["contestants"]) == 2
    # One row per task, not one row per (task, arm) — the defect this fixes.
    assert len(merged["rows"]) == 5


def test_totals_sum_real_recorded_values_not_invented_ones() -> None:
    bodies = [
        _body("task-a", "stella", reward=1.0, tokens_in=100),
        _body("task-b", "stella", reward=0.0, tokens_in=200),
    ]
    merged = merge_trial(bodies)
    totals = merged["match"]["contestants"][0]["totals"]
    assert totals["trials"] == 2
    assert totals["judged"] == 2
    assert totals["passed"] == 1
    assert totals["solve_rate"] == 50.0
    assert totals["tokens_in"] == 300


def test_a_task_missing_one_arms_cell_is_reported_not_hidden_or_faked() -> None:
    bodies = [
        _body("task-a", "stella", reward=1.0),
        _body("task-a", "claude-code", reward=1.0),
        _body("task-b", "stella", reward=1.0),  # claude-code never ran task-b
    ]
    merged = merge_trial(bodies)
    assert merged["incomplete_tasks"] == {"task-b": ["claude-code"]}
    # claude-code's totals only count the one task it actually has.
    cc_totals = next(
        c["totals"] for c in merged["match"]["contestants"] if c["id"] == "claude-code"
    )
    assert cc_totals["trials"] == 1


def test_an_unjudged_trial_is_excluded_from_solve_rate_not_scored_zero() -> None:
    bodies = [
        _body("task-a", "stella", reward=None),
        _body("task-b", "stella", reward=1.0),
    ]
    merged = merge_trial(bodies)
    totals = merged["match"]["contestants"][0]["totals"]
    assert totals["trials"] == 2
    assert totals["judged"] == 1
    assert totals["passed"] == 1
    assert totals["solve_rate"] == 100.0


def test_a_voided_cell_with_a_scored_zero_is_not_a_loss() -> None:
    """#3209: the merged view judges on the classified verdict, not the raw
    reward. An unfunded cell carries `resolved: None` beside a verifier reward
    of 0 — counting the 0 would disagree with the per-cell classification and
    put a dead arm's deaths back in the denominator."""
    voided = _body("task-a", "stella", reward=0.0)
    cell = voided["rows"][0]["cells"]["stella"]
    cell["resolved"] = None
    cell["infrastructure"] = True
    cell["unfunded"] = True
    bodies = [voided, _body("task-b", "stella", reward=1.0)]
    merged = merge_trial(bodies)
    totals = merged["match"]["contestants"][0]["totals"]
    assert totals["judged"] == 1
    assert totals["passed"] == 1
    assert totals["solve_rate"] == 100.0
    assert totals["unfunded"] == 1


def test_a_cell_predating_the_resolved_field_judges_on_its_reward() -> None:
    """Archived cells with no `resolved` key keep their old meaning."""
    legacy = _body("task-a", "stella", reward=0.0)
    del legacy["rows"][0]["cells"]["stella"]["resolved"]
    merged = merge_trial([legacy])
    totals = merged["match"]["contestants"][0]["totals"]
    assert totals["judged"] == 1
    assert totals["passed"] == 0


def test_refuses_to_merge_across_datasets() -> None:
    bodies = [
        _body("task-a", "stella", reward=1.0, dataset="terminal-bench-2.1"),
        _body("task-a", "claude-code", reward=1.0, dataset="terminal-bench-1.0"),
    ]
    with pytest.raises(MergeError, match="dataset"):
        merge_trial(bodies)


def test_refuses_two_jobs_scoring_the_same_cell() -> None:
    bodies = [
        _body("task-a", "stella", reward=1.0),
        _body("task-a", "stella", reward=0.0),
    ]
    with pytest.raises(MergeError, match="duplicate"):
        merge_trial(bodies)


def test_empty_input_is_a_merge_error_not_a_crash() -> None:
    with pytest.raises(MergeError):
        merge_trial([])


def test_load_fetched_results_reads_every_label_dir(tmp_path) -> None:
    (tmp_path / "000-stella-task-a").mkdir()
    (tmp_path / "000-stella-task-a" / "results.json").write_text(
        json.dumps(_body("task-a", "stella", reward=1.0))
    )
    (tmp_path / "001-cc-task-a").mkdir()
    (tmp_path / "001-cc-task-a" / "results.json").write_text(
        json.dumps(_body("task-a", "claude-code", reward=0.0))
    )
    bodies = load_fetched_results(tmp_path)
    assert len(bodies) == 2


def test_write_merged_trial_round_trips(tmp_path) -> None:
    bodies = [_body("task-a", "stella", reward=1.0)]
    merged = merge_trial(bodies)
    target = write_merged_trial(tmp_path, merged)
    assert target == tmp_path / "trial.json"
    assert json.loads(target.read_text())["match"]["tasks"] == ["task-a"]
