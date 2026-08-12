# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Stopping a cloud run's remaining trials — and nothing else in the account.

When the banned-behavior gate (:mod:`.gate`) trips on a finished trial, the
money still to be spent is in the trials that have not started. Nothing in
ArenaBench could stop them before this module: there was no ``cancel_job`` or
``terminate_job`` call anywhere in the tree, so an operator who saw a run go
wrong killed the terminal and watched Batch bill the rest.

**The selector is the run's own ``jobs.json``, never a queue sweep and never a
job-name prefix.** This is the load-bearing sentence in the file, and it is
enforced structurally: :func:`cancel_run` reads
``s3://<bucket>/runs/<run-id>/jobs.json`` through
``executor.load_jobs(run_id)``, takes the ids in it, and signals exactly those.
It never calls ``list_jobs``, and it never matches on ``job_name``'s
``<run-id>-<label>`` shape.

The reason is that ``arenabench-measure`` and ``arenabench-burst`` are **shared
by every run and every operator in the account**. A cancel scoped to a queue —
or to a name prefix another run's ids happen to share — reaches into a
stranger's benchmark and destroys hours of paid compute, and the destruction is
indistinguishable at the scoreboard from an agent losing. This project has
already paid for the general lesson twice, one layer down each time:

* ``scripts/arena-kill.sh`` selected servers by argv pattern machine-wide and
  killed a neighbouring session's on 2026-08-08; its header now records the
  incident.
* :mod:`.reap` reaps by trial id rather than by container-name prefix, for the
  reason stated at ``reap.py:100-103`` — "a match owns exactly the containers
  its own trial ids name, so a sweep can be precise instead of guessing from a
  prefix that another match may share."

Same rule, one layer up, where the blast radius is an AWS account instead of a
host.

The split is the usual one: :func:`plan_cancellation` is **pure** and decides
which id gets which API call, so the dangerous decision is unit-testable with
dicts and no boto3; :func:`cancel_run` is the thin impure half that makes the
calls. A single failed call never abandons the rest — a partially cancelled run
is the worst available outcome, because the operator believes it stopped and
Batch keeps billing.
"""

from __future__ import annotations

import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from typing import Any

from .cloud import _QUEUED_STATES, _RUNNING_STATES, _SETTLED_STATES

__all__ = [
    "CancelPlan",
    "abort_record",
    "cancel_run",
    "plan_cancellation",
]


@dataclass(frozen=True)
class CancelPlan:
    """Which of a run's job ids gets which Batch call.

    Three buckets rather than one list because Batch draws the line itself:
    ``cancel_job`` only affects a job that has not started, and a ``RUNNING``
    trial needs ``terminate_job`` or it keeps billing until it finishes on its
    own. Submission order is preserved throughout so a printed plan reads in
    the same order as the run's own trial list.
    """

    cancel: tuple[str, ...] = ()
    terminate: tuple[str, ...] = ()
    already_settled: tuple[str, ...] = ()

    @property
    def signalled(self) -> tuple[str, ...]:
        """Every id this plan actually touches, in submission order."""
        return self.cancel + self.terminate


def plan_cancellation(
    job_ids: Sequence[str], states: Mapping[str, str]
) -> CancelPlan:
    """Route each job id to ``cancel_job``, ``terminate_job``, or nothing.

    Pure. ``job_ids`` is the authoritative list — this run's own — and
    ``states`` is whatever ``describe_jobs`` most recently said about them.

    **An id absent from ``states`` is cancelled, not skipped.** It is an id we
    own (it came out of this run's ``jobs.json``), ``describe_jobs`` drops ids
    it cannot resolve rather than erroring, and ``cancel_job`` against a job
    that has already settled is a documented no-op. So the two mistakes on
    offer are "issue a harmless call" and "leave a billing trial running
    because a describe was incomplete", and only one of them costs money.
    """
    cancel: list[str] = []
    terminate: list[str] = []
    settled: list[str] = []
    for job_id in job_ids:
        state = states.get(job_id)
        if state in _RUNNING_STATES:
            terminate.append(job_id)
        elif state in _SETTLED_STATES:
            settled.append(job_id)
        elif state is None or state in _QUEUED_STATES:
            cancel.append(job_id)
        else:
            # Batch declares no other states today. If it grows one, the safe
            # reading is "still alive, still billing" — the same direction the
            # unknown-id rule above takes, for the same reason.
            cancel.append(job_id)
    return CancelPlan(
        cancel=tuple(cancel),
        terminate=tuple(terminate),
        already_settled=tuple(settled),
    )


def cancel_run(
    executor: Any,
    run_id: str,
    *,
    reason: str,
    out: Callable[[str], None] = print,
) -> CancelPlan:
    """Stop every job of ``run_id`` that has not already settled.

    The id list comes from ``executor.load_jobs(run_id)`` and from nowhere
    else — see the module docstring for why that is the whole safety argument.

    Each call is issued and reported individually, and a failure is printed and
    stepped over rather than raised: one job that refuses to cancel must not
    leave the other twenty running. The returned plan is what was *attempted*,
    which is what the abort record needs to be honest about.
    """
    record = executor.load_jobs(run_id)
    job_ids = [job["id"] for job in record.get("jobs", [])]
    if not job_ids:
        out(f"cancel    : {run_id} has no jobs in its record — nothing to stop")
        return CancelPlan()

    states = executor.job_states(job_ids)
    plan = plan_cancellation(job_ids, states)
    batch = executor._client("batch")  # noqa: SLF001 - same package, one adapter

    out(
        f"cancel    : {run_id} — {len(plan.terminate)} running, "
        f"{len(plan.cancel)} queued, {len(plan.already_settled)} already "
        f"settled  ({reason})"
    )
    for job_id in plan.terminate:
        _signal(batch.terminate_job, job_id, reason, verb="terminate", out=out)
    for job_id in plan.cancel:
        _signal(batch.cancel_job, job_id, reason, verb="cancel", out=out)
    return plan


def _signal(
    call: Callable[..., Any],
    job_id: str,
    reason: str,
    *,
    verb: str,
    out: Callable[[str], None],
) -> None:
    """One Batch call, reported either way, never raising.

    ``except Exception`` rather than a narrower clause on purpose: the only
    exceptions in scope are botocore's, which are generated per-service and
    cannot be caught by name without importing boto3 — the dependency this
    package keeps optional. Whatever comes back, the answer is the same
    (print it, keep going), so narrowing would buy nothing but an unhandled
    error on the one variant we failed to predict.
    """
    try:
        call(jobId=job_id, reason=reason)
    except Exception as exc:  # noqa: BLE001 - see the docstring
        out(f"  !! {verb} {job_id} failed: {exc}")
        return
    out(f"  {verb}d {job_id}")


def abort_record(
    *,
    run_id: str,
    codes: Sequence[str],
    trial_label: str,
    report: str,
    plan: CancelPlan,
) -> dict[str, Any]:
    """The durable artifact an abort leaves behind, as JSON-ready data.

    This is the thing somebody argues with. An abort throws away the rest of a
    paid run on the strength of one automated verdict, so the record has to
    carry enough for a reader to conclude **the gate was wrong** without any
    access to the session that aborted: which run, which behavior codes, which
    trial tripped it, the gate's full report text (never a truncation — the
    truncated half is reliably the half that matters), and exactly which job
    ids were cancelled and terminated so the loss is enumerable rather than
    described.

    Pure and stdlib-only: the caller writes it wherever the run's artifacts
    live.
    """
    return {
        "run_id": run_id,
        "aborted_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "trial": trial_label,
        "codes": list(codes),
        "report": report,
        "cancelled": list(plan.cancel),
        "terminated": list(plan.terminate),
        "already_settled": list(plan.already_settled),
    }
