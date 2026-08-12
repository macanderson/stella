# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Merge fetched cloud results into one trial (#2100 offline half).

A cloud match is submitted and scored one Batch job per (task, contestant) —
correct, because container isolation needs it — but nothing regrouped the
fetched output: :func:`arenabench.cloud.CloudExecutor.fetch_results` wrote one
``results.json`` per job, so a two-arm, 89-task run landed locally as 178
disconnected single-cell files with no trial-level view at all. This module is
the regrouping step, run automatically by ``cloud fetch``/``cloud run`` on
whatever one call fetched, and separately by ``cloud merge`` across two or
more already-fetched destinations for a match whose arms were submitted as
independent runs (a real workflow: an arm re-run after a credential rotation,
or two arms deliberately launched hours apart).

Same honesty rule as :mod:`.cloud_watch`: every number in the merged trial is
copied or summed from a real per-task ``results.json`` a finished job wrote.
Nothing is inferred, and a task only one arm has is reported, never silently
dropped or padded with a fabricated cell for the other arm.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

__all__ = [
    "MergeError",
    "load_fetched_results",
    "merge_trial",
    "write_merged_trial",
]


class MergeError(ValueError):
    """A fetched results.json set cannot be merged into one trial."""


def load_fetched_results(dest: Path) -> list[dict[str, Any]]:
    """Every ``results.json`` :func:`fetch_results` wrote under ``dest``.

    One file per ``<job-label>/results.json``, sorted by label so a merge is
    reproducible across re-runs of the same fetch.
    """
    return [
        json.loads(path.read_text(encoding="utf-8"))
        for path in sorted(dest.glob("*/results.json"))
    ]


def _totals(cells: list[dict[str, Any]]) -> dict[str, Any]:
    """One contestant's aggregate across the real per-task cells it has.

    Every field is a sum or count over values a finished trial already wrote
    to its own ``results.json`` — combined here, never invented. A task this
    contestant has no cell for (see ``merge_trial``) is excluded from ``n``
    entirely rather than counted as a loss, which is what would happen if a
    missing cell were padded with a zero-reward placeholder.
    """
    judged = [c for c in cells if c.get("reward") is not None]
    passed = sum(1 for c in judged if (c.get("reward") or 0) >= 1.0)

    def total(key: str) -> float:
        return sum((c.get(key) or 0) for c in cells)

    return {
        "trials": len(cells),
        "infrastructure": sum(1 for c in cells if c.get("infrastructure")),
        "running": sum(1 for c in cells if c.get("status") == "running"),
        "done": sum(1 for c in cells if c.get("status") == "done"),
        "judged": len(judged),
        "passed": passed,
        "solve_rate": (passed / len(judged) * 100.0) if judged else 0.0,
        "clock_time": total("clock_time"),
        "tokens_in": total("tokens_in"),
        "tokens_out": total("tokens_out"),
        "cache_read": total("cache_read"),
        "cache_write": total("cache_write"),
        "total_cost": total("total_cost"),
        "priced_cost": total("priced_cost"),
    }


def merge_trial(bodies: list[dict[str, Any]]) -> dict[str, Any]:
    """Regroup N single-task ``results.json`` bodies into one trial.

    Each ``body`` is exactly what a finished cloud job wrote: one task in
    ``match.tasks`` and one contestant in ``match.contestants``/``rows[0].cells``.
    Raises :class:`MergeError` on an empty input or on bodies that do not
    share one dataset — merging across datasets would silently average two
    different benchmarks into one number.

    Returns a dict shaped like a native multi-task, multi-contestant match
    snapshot (``match.tasks``, ``match.contestants`` with recomputed
    ``totals``, and one ``rows`` entry per task carrying whichever
    contestants actually have a cell for it). ``incomplete_tasks`` lists any
    task at least one contestant is missing, so a caller can surface the gap
    instead of it being invisible in an all-arms-present-looking grid.
    """
    if not bodies:
        raise MergeError("no results.json bodies to merge")

    datasets = {body["match"]["dataset"] for body in bodies}
    if len(datasets) > 1:
        raise MergeError(f"refusing to merge across datasets: {sorted(datasets)}")

    # task -> contestant_id -> cell
    grid: dict[str, dict[str, dict[str, Any]]] = {}
    contestants: dict[str, dict[str, Any]] = {}
    for body in bodies:
        match = body["match"]
        tasks = match["tasks"]
        contestant = match["contestants"][0]
        cid = contestant["id"]
        if len(tasks) != 1:
            raise MergeError(
                f"expected one task per fetched job, got {tasks} for {cid}"
            )
        task = tasks[0]
        cell = body["rows"][0]["cells"][cid]
        grid.setdefault(task, {})
        if cid in grid[task]:
            raise MergeError(f"duplicate ({task}, {cid}) — two jobs scored the same cell")
        grid[task][cid] = cell
        contestants.setdefault(cid, contestant)

    contestant_ids = sorted(contestants)
    tasks_sorted = sorted(grid)
    incomplete_tasks = {
        task: sorted(set(contestant_ids) - set(cells))
        for task, cells in grid.items()
        if set(cells) != set(contestant_ids)
    }

    merged_contestants = []
    for cid in contestant_ids:
        cells = [grid[task][cid] for task in tasks_sorted if cid in grid[task]]
        merged_contestants.append({**contestants[cid], "totals": _totals(cells)})

    rows = [
        {"task": task, "cells": dict(grid[task])} for task in tasks_sorted
    ]

    any_body = bodies[0]
    return {
        "match": {
            "id": any_body["match"].get("id", ""),
            "name": any_body["match"].get("name", ""),
            "dataset": any_body["match"]["dataset"],
            "tasks": tasks_sorted,
            "contestants": merged_contestants,
        },
        "dataset": any_body.get("dataset"),
        "status": "finished",
        "rows": rows,
        "contestants": merged_contestants,
        "incomplete_tasks": incomplete_tasks,
    }


def write_merged_trial(dest: Path, merged: dict[str, Any]) -> Path:
    """Write ``merged`` to ``dest/trial.json`` and return the path."""
    target = dest / "trial.json"
    target.write_text(json.dumps(merged, indent=2), encoding="utf-8")
    return target
