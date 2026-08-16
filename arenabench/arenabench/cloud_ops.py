# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Operator verbs over a submitted cloud run — ``status``, ``fetch``, ``cancel``.

Split out of :mod:`.cloud`, which holds the submit side of the substrate
contract and sits against its file-size ceiling; these verbs are the *read and
stop* side of the same contract, and each carries an honesty rule the submit
path does not:

* **``status`` reconciles its two sources instead of displaying their
  contradiction.** The DynamoDB rows are written by the trial container
  itself, so a trial killed from outside (operator ``TerminateJob``, SIGTERM)
  never writes its terminal row and reads ``running`` forever — while the
  Batch summary on the same screen says zero jobs are running. Five such rows
  across two runs sat stale indefinitely (#3217). A Batch-terminal job now
  renders as its Batch state with a marker that the trial never checked in;
  nothing is written back to DynamoDB, because the row's absence of a
  terminal write is itself evidence.
* **``fetch`` refuses to snapshot a live run without being told to.** A fetch
  is a point-in-time snapshot of S3, and assembling one taken mid-run makes a
  silently short match — h2h891's morning head-to-head was computed missing
  two Claude Code results that existed in S3 by the time anyone looked
  (#3218). Non-terminal jobs are named per job and require ``--partial``.
  Every fetch also writes a ``jobs.json`` sidecar carrying the per-job Batch
  state it saw, so :mod:`.assemble` can say *which kind* of missing a missing
  task is without ever making an AWS call of its own.
* **``cancel`` stops this run and nothing else in the account.**
  :func:`.cancel.cancel_run` had implemented the safe selector — the run's
  own ``jobs.json``, never a queue sweep or a name prefix — since the gate
  needed it, but no CLI verb reached it, so an operator stopping a run
  hand-rolled the exact dangerous prefix sweep the module warns about
  (#3257).
"""

from __future__ import annotations

import json
import sys
from collections.abc import Callable, Iterable, Mapping
from pathlib import Path
from typing import Any

from .cloud import (
    _QUEUED_STATES,
    _RUNNING_STATES,
    _SETTLED_STATES,
    CloudError,
    CloudExecutor,
    progress_from_states,
)

__all__ = [
    "JOBS_SIDECAR_NAME",
    "nonterminal_jobs",
    "reconciled_status",
    "register_verbs",
    "write_jobs_sidecar",
]

#: Where a fetch records the submission record plus the per-job Batch state it
#: observed, inside the fetch destination. The name deliberately matches the
#: S3 object it extends (``runs/<id>/jobs.json``): it *is* that record, with a
#: ``state`` field added per job.
JOBS_SIDECAR_NAME = "jobs.json"


# --------------------------------------------------------------------------
# pure decisions
# --------------------------------------------------------------------------


def reconciled_status(ddb_status: str, batch_state: str | None) -> str:
    """What to print for a trial's DynamoDB row, given its Batch state.

    The row is the trial's own self-report; Batch is the substrate's. When the
    row says ``running`` and Batch says the job is terminal, the trial was
    ended from outside without writing its terminal row — the marker names
    that, because "running forever" and "killed before it could say so" are
    opposite facts about the run (#3217). Every other combination prints the
    row as written: this is a read, and the reconciliation is rendering, not
    repair.
    """
    if ddb_status == "running" and batch_state in _SETTLED_STATES:
        if batch_state == "FAILED":
            return "failed (killed; no checkin)"
        return "succeeded (no terminal checkin)"
    return ddb_status


def nonterminal_jobs(
    jobs: Iterable[Mapping[str, Any]], states: Mapping[str, str]
) -> list[tuple[str, str]]:
    """``(label, state)`` for every job still queued or running.

    A job id ``describe_jobs`` no longer resolves is treated as terminal, not
    live: Batch ages jobs out after days, so an old run fetched months later
    must not read as "still running". The money-safe direction is the
    opposite of :func:`.cancel.plan_cancellation`'s, because the hazard here
    is a refused fetch, not a billing trial left alive.
    """
    live = _QUEUED_STATES | _RUNNING_STATES
    return [
        (str(job.get("label") or job.get("id") or "?"), states[str(job["id"])])
        for job in jobs
        if states.get(str(job.get("id"))) in live
    ]


# --------------------------------------------------------------------------
# the filesystem side
# --------------------------------------------------------------------------


def write_jobs_sidecar(
    dest: Path, record: Mapping[str, Any], states: Mapping[str, str]
) -> Path:
    """Write the submission record with observed Batch states into ``dest``.

    This is how job-state knowledge reaches :func:`.assemble.assemble_run`
    without assembly ever making an AWS call: the fetch — the step that
    already talks to the substrate — leaves what it saw beside what it
    downloaded (#3218).
    """
    enriched = dict(record)
    enriched["jobs"] = [
        {**job, "state": states.get(str(job.get("id")), "")}
        for job in record.get("jobs", [])
    ]
    dest.mkdir(parents=True, exist_ok=True)
    target = dest / JOBS_SIDECAR_NAME
    target.write_text(json.dumps(enriched, indent=2) + "\n", encoding="utf-8")
    return target


# --------------------------------------------------------------------------
# CLI verbs
# --------------------------------------------------------------------------


def _cmd_cloud_status(args: Any, executor: CloudExecutor | None = None) -> int:
    """``arenabench cloud status <run-id>`` — one snapshot, or follow."""
    executor = executor or CloudExecutor(region=args.region, bucket=args.bucket)
    record = executor.load_jobs(args.run_id)
    jobs = record["jobs"]
    print(f"run   : {record['run_id']}  ({len(jobs)} trials on {record['queue']})")
    if args.follow:
        states = executor.watch(jobs, poll_interval=args.poll)
    else:
        states = executor.job_states([job["id"] for job in jobs])
        print(progress_from_states(states.values()).to_line(0.0))
    for row in executor.run_rows(args.run_id):
        # The ddb key is the arenabench job UUID, which is also the Batch job
        # id — the join is an equality, not an inference.
        shown = reconciled_status(row["status"], states.get(row["job"]))
        detail = row["detail"]
        if shown.startswith("failed") and _results_uploaded(
            executor, args.run_id, row["job"]
        ):
            # The container exited non-zero *after* uploading its verdict —
            # a graded trial wearing a FAILED badge, not a trial that died
            # before doing any work (#3264).
            detail = (detail + "  " if detail else "") + "[results.json uploaded]"
        print(f"  ddb {row['job']}: {shown}  {detail}")
    final = progress_from_states(states.values())
    return 0 if final.failed == 0 else 1


def _results_uploaded(executor: CloudExecutor, run_id: str, job_id: str) -> bool:
    """Whether this job's ``results.json`` exists in the artifacts bucket."""
    s3 = executor._client("s3")  # same package, one adapter
    try:
        s3.head_object(
            Bucket=executor.bucket(), Key=f"runs/{run_id}/{job_id}/results.json"
        )
    except Exception:  # NoSuchKey arrives as a generated botocore class
        return False
    return True


def _cmd_cloud_fetch(args: Any, executor: CloudExecutor | None = None) -> int:
    """``arenabench cloud fetch <run-id>`` — pull results (and artifacts)."""
    from .cloud import _assemble_fetched

    executor = executor or CloudExecutor(region=args.region, bucket=args.bucket)
    record = executor.load_jobs(args.run_id)
    states = executor.job_states([job["id"] for job in record["jobs"]])
    live = nonterminal_jobs(record["jobs"], states)
    if live:
        print(
            f"{len(live)} job(s) of {args.run_id} are not terminal in Batch — "
            "a fetch now is a point-in-time snapshot and the assembled match "
            "will be silently short:",
            file=sys.stderr,
        )
        for label, state in live:
            print(f"  {label}: {state}", file=sys.stderr)
        if not args.partial:
            print(
                "refusing; re-run with --partial for an explicit snapshot, or "
                f"wait and follow with: arenabench cloud status {args.run_id} "
                "--follow",
                file=sys.stderr,
            )
            return 2
        print("continuing under --partial; re-run the fetch when the run "
              "settles — it is re-runnable and cheap.", file=sys.stderr)
    dest = Path(args.out or f"arenabench-cloud/{args.run_id}").expanduser()
    fetched = executor.fetch_results(
        args.run_id, record["jobs"], dest, artifacts=args.artifacts
    )
    write_jobs_sidecar(dest, record, states)
    print(f"results   : {fetched} file(s) -> {dest}")
    _assemble_fetched(dest)
    return 0


def _cmd_cloud_cancel(args: Any, executor: CloudExecutor | None = None) -> int:
    """``arenabench cloud cancel <run-id>`` — stop this run's remaining trials."""
    from .cancel import cancel_run

    executor = executor or CloudExecutor(region=args.region, bucket=args.bucket)
    try:
        plan = cancel_run(
            executor,
            args.run_id,
            reason=args.reason or f"arenabench cloud cancel {args.run_id}",
        )
    except CloudError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    print(
        f"plan      : {len(plan.terminate)} terminated, {len(plan.cancel)} "
        f"cancelled, {len(plan.already_settled)} already settled"
    )
    return 0


def register_verbs(verbs: Any, common_aws: Callable[[Any], None]) -> None:
    """Attach ``status``/``fetch``/``cancel`` to the ``cloud`` verb tree."""
    status = verbs.add_parser("status", help="progress of a submitted cloud run")
    status.add_argument("run_id")
    status.add_argument("--follow", action="store_true",
                        help="keep polling until every trial settles")
    status.add_argument("--poll", type=float, default=15.0)
    common_aws(status)
    status.set_defaults(func=_cmd_cloud_status)

    fetch = verbs.add_parser("fetch", help="download a cloud run's results")
    fetch.add_argument("run_id")
    fetch.add_argument("--artifacts", action="store_true",
                       help="also download the full artifact tree (can be large)")
    fetch.add_argument("--out", help="local directory for downloaded results")
    fetch.add_argument(
        "--partial", action="store_true",
        help="fetch even while jobs are still queued or running (the "
             "assembled match will be missing their trials)",
    )
    common_aws(fetch)
    fetch.set_defaults(func=_cmd_cloud_fetch)

    cancel = verbs.add_parser(
        "cancel", help="stop a run's remaining trials (this run's, nothing else)"
    )
    cancel.add_argument("run_id")
    cancel.add_argument("--reason", help="recorded on every Batch cancel/terminate")
    common_aws(cancel)
    cancel.set_defaults(func=_cmd_cloud_cancel)
