# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Cross-match outcome series for the trends view (#2068).

One row per match: the apparatus (provenance), a comparability key, and
per-seat outcome aggregates derived from the same :class:`TrialMetrics`
every other surface reads. Nothing here recomputes a verdict — ``resolved``
is Harbor's — and a judged trial with ``resolved is None`` (an
infrastructure void) is excluded from every rate and reported as its own
count instead, so a series built on three surviving trials cannot
masquerade as one built on five.

The unit of comparison is the SUT commit, and comparability is **enforced,
not assumed**: two matches are directly comparable only when everything but
the SUT matches — dataset digest, task list, and every seat's
``(agent, model, pinned roles)`` signature. The client draws a connected
series only within one comparability group and must label anything else as
not directly comparable, because a confounded comparison presented as a
trend is the most misleading thing this feature could do: matches
``4bed110003d8`` (5/5) and ``61e01ca06fc7`` (3/5) differ by ~12 commits,
and reading them as a one-change regression cost real investigation time.

Its own module rather than more length on ``server.py``, per the
repository's file-size ratchet rule: separable new code becomes a module.
"""

from __future__ import annotations

import hashlib
from typing import Any

__all__ = ["match_row", "series_payload"]


def _task_digest(tasks: list[str]) -> str:
    """A stable 8-hex digest of the sorted task list.

    Sorted, because two templates listing the same tasks in a different
    order ran the same measurement; digested, because the grid needs the
    names anyway but the grouping key wants one short token.
    """
    joined = "\n".join(sorted(tasks))
    return hashlib.sha256(joined.encode("utf-8")).hexdigest()[:8]


def _seat_signature(contestant: Any) -> dict[str, Any]:
    """The per-seat half of the comparability key.

    Only *pinned* roles participate: a blank role inherits and pins nothing
    (the posture rule), so including it would split groups on a field that
    changed no behavior.
    """
    engine = contestant.engine
    pinned_roles = {
        name: role.model
        for name, role in sorted(engine.roles.items())
        if getattr(role, "model", "")
    }
    return {
        "name": contestant.name,
        "agent": contestant.agent,
        "model": engine.model,
        "roles": pinned_roles,
        # #2023's vocabulary: an arm with any independently pinned role is a
        # materially different agent from one model wearing every hat.
        "arm": "treatment" if pinned_roles else "control",
    }


def _seat_outcomes(metrics: dict[str, Any]) -> dict[str, Any]:
    """Outcome counts for one seat, from its per-task ``TrialMetrics``.

    The rules mirror the scoreboard's (#2066): ``resolved`` is Harbor's
    verdict and is never recomputed; an exception is an *incident*, counted
    beside the verdict in both directions; a judged trial whose ``resolved``
    is still ``None`` is an infrastructure void, outside every rate.
    Timeouts split by whether the solve had already happened — after-solve
    (did not stop when done) and before-solve (ran out of time) mean
    opposite things and are never merged.
    """
    judged = 0
    solved = 0
    failed = 0
    voids = 0
    incidents = 0
    incidents_on_solved = 0
    timeouts_after_solve = 0
    timeouts_before_solve = 0
    clock_time = 0.0
    priced: float | None = 0.0
    for m in metrics.values():
        if m.status != "done":
            continue
        judged += 1
        clock_time += m.clock_time or 0.0
        if priced is not None:
            priced = None if m.priced_cost is None else priced + m.priced_cost
        if m.resolved is None:
            voids += 1
            continue
        is_timeout = "timeout" in (m.failure or "").lower()
        if m.resolved:
            solved += 1
            if m.failure:
                incidents += 1
                incidents_on_solved += 1
                if is_timeout:
                    timeouts_after_solve += 1
        else:
            failed += 1
            if m.failure:
                incidents += 1
                if is_timeout:
                    timeouts_before_solve += 1
    scored = solved + failed
    return {
        "judged": judged,
        "scored": scored,
        "solved": solved,
        "failed": failed,
        "voids": voids,
        "incidents": incidents,
        "incidents_on_solved": incidents_on_solved,
        "timeouts_after_solve": timeouts_after_solve,
        "timeouts_before_solve": timeouts_before_solve,
        "clock_time": round(clock_time, 2),
        "priced_cost": round(priced, 6) if priced is not None else None,
        # Cost per solve stays null-poisoned like every priced figure: an
        # unpriced model makes the ratio unknown, never smaller.
        "priced_cost_per_solve": (
            round(priced / solved, 6) if priced is not None and solved else None
        ),
    }


def _per_task(metrics: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """The per-task verdicts the task-history grid needs.

    ``outcome`` is a closed vocabulary: ``solved`` / ``failed`` / ``void``
    (judged, no verdict — infrastructure), plus ``incident: bool`` beside
    it, never folded in.
    """
    out: dict[str, dict[str, Any]] = {}
    for task, m in metrics.items():
        if m.status != "done":
            continue
        if m.resolved is None:
            outcome = "void"
        elif m.resolved:
            outcome = "solved"
        else:
            outcome = "failed"
        out[task] = {"outcome": outcome, "incident": bool(m.failure)}
    return out


def match_row(match: Any) -> dict[str, Any]:
    """One series row for one match — apparatus, comparability, outcomes."""
    spec = match.spec
    prov = match.provenance
    seat_signatures = [_seat_signature(c) for c in spec.contestants]
    dataset_digest = (prov.dataset_digest if prov else "") or ""
    sut_commit = (prov.sut_commit if prov else "") or ""
    # Metrics before the task list, because the list may have to come from
    # them: an empty ``spec.tasks`` means "the whole dataset" (MatchSpec's
    # contract), so the comparability key derives the list from the trials
    # that actually exist — the same derivation the live grid uses
    # (``Match.snapshot``) — instead of digesting the empty list, which would
    # stamp every whole-dataset match with one degenerate token and collapse
    # distinct series. A still-running whole-dataset match digests only what
    # has run so far and therefore keys apart from its finished siblings;
    # that is the comparability rule working, not a bug — a 40-task partial
    # charted against an 89-task full run is exactly the confounded trend
    # this key exists to forbid.
    metrics_by_seat = {c.id: match.metrics_for(c.id) for c in spec.contestants}
    if spec.tasks:
        tasks = sorted(spec.tasks)
        tasks_source = "spec"
    else:
        observed: set[str] = set()
        for metrics in metrics_by_seat.values():
            observed.update(metrics)
        tasks = sorted(observed)
        tasks_source = "observed"
    # Sorted, not spec-ordered: an h2h with its seats swapped is the same
    # measurement, and seat order must not split a group.
    seat_sig_token = "&".join(
        sorted(
            f"{s['agent']}={s['model']}({','.join(f'{r}:{m}' for r, m in s['roles'].items())})"
            for s in seat_signatures
        )
    )
    seats = []
    for contestant in spec.contestants:
        metrics = metrics_by_seat[contestant.id]
        seats.append(
            {
                "id": contestant.id,
                # Display only — never part of the comparability signature.
                "color": getattr(contestant, "color", ""),
                **_seat_signature(contestant),
                "outcomes": _seat_outcomes(metrics),
                "per_task": _per_task(metrics),
            }
        )
    return {
        "id": spec.id,
        "name": spec.name,
        "status": match.status,
        "created_at": match.created_at,
        "finished_at": match.finished_at,
        "provenance": prov.to_json() if prov else None,
        "comparability": {
            "dataset": spec.dataset,
            "dataset_digest": dataset_digest,
            "task_digest": _task_digest(tasks),
            "task_count": len(tasks),
            "tasks": tasks,
            # ``spec`` when the template pinned the list; ``observed`` when it
            # was derived from the trials of a whole-dataset match — so the
            # client can label the count as measured rather than declared.
            "tasks_source": tasks_source,
            "sut_commit": sut_commit,
            "seats": seat_signatures,
            # Everything but the SUT: rows sharing this key are one series;
            # rows that differ in it are different measurements, and the
            # client must say so rather than draw a line between them. When
            # provenance never recorded a dataset digest, the registry name
            # stands in — two unprovenanced matches on different datasets are
            # different measurements too, and "unknown" must not join them.
            "group_key": (
                f"{dataset_digest.removeprefix('sha256:')[:8] or spec.dataset or 'unknown'}"
                f"|{_task_digest(tasks)}|{seat_sig_token}"
            ),
        },
        "seats": seats,
    }


def series_payload(matches: list[Any]) -> dict[str, Any]:
    """Every match as a series row, oldest first, so time reads left to
    right without the client re-sorting."""
    ordered = sorted(matches, key=lambda m: m.created_at)
    return {"matches": [match_row(m) for m in ordered]}
