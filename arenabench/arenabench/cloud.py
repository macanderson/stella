# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Submitting a match to the AWS Batch cloud substrate (``arenabench cloud``).

``arenabench/infra/core.yaml`` provisions scale-to-zero capacity — queues
``arenabench-measure`` (on-demand) and ``arenabench-burst`` (spot), job
definition ``arenabench-trial``, the runner image, an artifacts bucket and a
DynamoDB status table — and the runner entrypoint
(``arenabench/infra/runner/entrypoint.sh``) speaks a fixed contract:
``RUN_ID``, ``MATCH_S3_URI``, optionally ``SUT_S3_URI`` + ``SUT_COMMIT``,
command ``["run"]``. This module is the submit side of that contract, so a
cloud run is one CLI verb rather than a hand-written ``aws batch submit-job``.

**Decisions are pure functions; AWS calls are a thin adapter.** Queue
selection, trial fan-out, env assembly, credential refusal, and progress
derivation all operate on plain data and are tested without any AWS client
(``tests/test_cloud.py`` exercises the adapter against recorded-call fakes).
That is the same ports-not-concretions split the rest of the tree holds, and
it is what lets this file stay stdlib-only at import time: ``boto3`` is
imported lazily inside :func:`_boto3`, so everything else in ArenaBench keeps
working without it (``pip install 'arenabench[cloud]'`` adds it).

**One Batch job per trial.** The runner executes one whole match TOML per
container, so the fan-out happens here: a match naming N tasks and M seats
becomes ``attempts x N x M`` sliced match TOMLs uploaded under
``runs/<run-id>/trials/``, each submitted as its own job. A Batch array job
cannot vary ``MATCH_S3_URI`` per child without entrypoint changes, so
individual submissions are the shape the contract supports today. Batch packs
them onto instances up to the account's vCPU quota; **excess trials queue in
``RUNNABLE`` and that is progress, not a hang** — the watcher prints the
queued count and says so, because a 144-trial submission on a 64-vCPU quota
spends most of its life there by design.

**Measured comparisons ride on-demand only.** ``arenabench-burst`` is spot,
and a spot interruption mid-trial is a confound no scoreboard should carry;
:func:`select_queue` routes there only when the caller explicitly marks the
work as non-measurement (``--burst``).

**Unauthenticated seats are refused at submit time.** In the cloud, provider
credentials come from SSM parameters under ``/arenabench/`` (exported by the
entrypoint, upper-cased basename); a seat none of whose declared names will
exist in the trial environment would burn a container to score a ``0.0``
indistinguishable from a real loss — the same contract #1777/#1827 enforce on
the local path, applied before any job is submitted.
"""

from __future__ import annotations

import json
import time
import uuid
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any

from .model import MatchSpec, slugify

__all__ = [
    "BURST_QUEUE",
    "JOB_DEFINITION",
    "MEASURE_QUEUE",
    "CloudError",
    "CloudExecutor",
    "Progress",
    "SutBinary",
    "TrialPlan",
    "job_name",
    "plan_trials",
    "progress_from_states",
    "ref_safe",
    "refused_seats",
    "register_cli",
    "select_queue",
    "slice_spec",
    "ssm_env_name",
    "sut_seats",
    "trial_environment",
]

#: The provisioned resource names, mirrored from ``infra/core.yaml``. Constants
#: rather than settings: they are the stack's contract, and an operator able to
#: retarget them from a flag is an operator submitting to a stack this module
#: has never been tested against.
MEASURE_QUEUE = "arenabench-measure"
BURST_QUEUE = "arenabench-burst"
JOB_DEFINITION = "arenabench-trial"
SUT_BUILD_PROJECT = "arenabench-sut-build"
TABLE_NAME = "arenabench"
SSM_PREFIX = "/arenabench"

#: Per-trial container size, mirrored from the infra README's runbook. One
#: trial is one privileged docker-in-docker container running a harbor task
#: stack; 4 vCPU / 15 GiB is the shape the compute environment packs.
TRIAL_VCPUS = 4
TRIAL_MEMORY_MB = 15360

#: ``describe_jobs`` accepts at most this many ids per call.
_DESCRIBE_CHUNK = 100

#: Batch job states that mean "waiting for capacity". A quota-limited account
#: parks most of a large submission here; surfacing the count is what keeps a
#: 144-trial run from looking like a hang.
_QUEUED_STATES = frozenset({"SUBMITTED", "PENDING", "RUNNABLE"})
_RUNNING_STATES = frozenset({"STARTING", "RUNNING"})


class CloudError(RuntimeError):
    """A cloud submission could not proceed; the message says why."""


def _boto3() -> Any:
    """Import boto3 on first use, with an actionable error when absent.

    ArenaBench is stdlib-only by contract (see ``pyproject.toml``); the AWS
    SDK is the one optional extra, pulled in only by the ``cloud`` verbs.
    """
    try:
        import boto3
    except ImportError as exc:  # pragma: no cover — exercised only without boto3
        raise CloudError(
            "cloud execution needs boto3 — install it with: "
            "pip install 'arenabench[cloud]'"
        ) from exc
    return boto3


# --------------------------------------------------------------------------
# pure decisions
# --------------------------------------------------------------------------


def ref_safe(ref: str) -> str:
    """A git ref as it appears in the S3 ``binaries/`` prefix.

    Mirrors the CodeBuild spec's ``tr '/' '-'`` exactly — ``feature/x`` builds
    land under ``binaries/feature-x/``, so resolving the same ref must apply
    the same fold.
    """
    return ref.replace("/", "-")


def ssm_env_name(parameter_name: str) -> str:
    """The environment variable a ``/arenabench/*`` parameter becomes.

    Mirrors the entrypoint's export exactly: basename, upper-cased, with
    every character outside ``[A-Z0-9_]`` folded to ``_``
    (``/arenabench/anthropic_api_key`` → ``ANTHROPIC_API_KEY``). The refusal
    check below is only honest if this transform and the runner's agree.
    """
    base = parameter_name.rsplit("/", 1)[-1].upper()
    return "".join(ch if (ch.isascii() and (ch.isalnum() or ch == "_")) else "_"
                   for ch in base)


def select_queue(burst: bool) -> str:
    """Which job queue a submission goes to.

    Measured comparisons go to the on-demand queue, always: the burst queue is
    spot capacity, and a spot interruption mid-trial invalidates the trial in
    a way that scores as an agent loss. ``burst=True`` is the caller's
    explicit statement that this work is not a measurement.
    """
    return BURST_QUEUE if burst else MEASURE_QUEUE


@dataclass(frozen=True)
class TrialPlan:
    """One Batch job's slice of the match grid.

    ``task`` is ``None`` for a whole-dataset seat: a match that names no tasks
    cannot be fanned out per task from here (the task list is only enumerated
    by Harbor inside the container), so each seat runs its full dataset as one
    job instead.
    """

    index: int
    contestant_id: str
    contestant_slug: str
    task: str | None
    attempt: int

    @property
    def label(self) -> str:
        """Stable, filesystem- and jobname-safe identity for this trial."""
        parts = [f"{self.index:03d}", self.contestant_slug]
        if self.task is not None:
            parts.append(slugify(self.task))
        if self.attempt > 1:
            parts.append(f"a{self.attempt}")
        return "-".join(parts)

    def to_json(self) -> dict[str, Any]:
        return {
            "index": self.index,
            "contestant_id": self.contestant_id,
            "contestant_slug": self.contestant_slug,
            "task": self.task,
            "attempt": self.attempt,
            "label": self.label,
        }


def plan_trials(spec: MatchSpec) -> tuple[TrialPlan, ...]:
    """Fan a match out into per-trial Batch submissions.

    With tasks named, the grid is ``attempts x tasks x contestants`` — the
    12-agents-at-12-tasks shape is 144 plans, one job each, and Batch queues
    whatever the vCPU quota cannot place. With no tasks named the fan-out is
    per contestant only, each seat running its whole dataset inside one job
    (attempts and concurrency preserved in the slice).
    """
    plans: list[TrialPlan] = []
    index = 0
    if spec.tasks:
        for attempt in range(1, spec.attempts + 1):
            for task in spec.tasks:
                for contestant in spec.contestants:
                    plans.append(
                        TrialPlan(
                            index=index,
                            contestant_id=contestant.id,
                            contestant_slug=contestant.slug,
                            task=task,
                            attempt=attempt,
                        )
                    )
                    index += 1
    else:
        for contestant in spec.contestants:
            plans.append(
                TrialPlan(
                    index=index,
                    contestant_id=contestant.id,
                    contestant_slug=contestant.slug,
                    task=None,
                    attempt=1,
                )
            )
            index += 1
    return tuple(plans)


def pin_slice_to_commit(spec: MatchSpec, sut: SutBinary | None) -> MatchSpec:
    """Rewrite a symbolic SUT pin to the commit the executor resolved.

    The runner image carries a staged binary and no Stella source, by
    design — ArenaBench fetches its system under test, it does not vendor
    it. A slice that still says ``sut_ref = "main"`` therefore asks the
    container to resolve a branch it has no repository for, and
    ``create_match`` correctly refuses the seat rather than guess.

    Resolving once here and shipping the *commit* is the same discipline
    #2098 established within one machine, extended across the boundary to
    the trial: one observation of what the floating ref meant, shared by the
    binary in S3, the ``SUT_COMMIT`` the entrypoint stages, and the pin the
    match declares. Clearing the pin instead would also run, but it records
    the trial as unverified provenance when the commit is known exactly —
    trading a fact for a blank.

    Seats pinned to their own ref (per-seat pins, #2082) are left alone:
    only the executor's own resolution may be substituted, and it resolved
    the match-level ref.
    """
    if sut is None:
        return spec
    contestants = tuple(
        replace(c, sut_ref=sut.commit)
        if c.agent == "stella" and c.sut_ref in (None, spec.sut_ref)
        else c
        for c in spec.contestants
    )
    return replace(spec, sut_ref=sut.commit, contestants=contestants)


def slice_spec(spec: MatchSpec, plan: TrialPlan) -> MatchSpec:
    """The one-trial match a plan's Batch job actually runs.

    A per-task slice pins one task, one seat, one attempt at concurrency 1 —
    the container is the unit of isolation, so the slice must not fan out
    again inside it. A whole-dataset slice (``plan.task is None``) keeps the
    match's own attempts/concurrency, because that job *is* the seat's whole
    run. ``sut_ref`` is match-level today (#2082) and ``dump_match`` does not
    carry it; in the cloud the binary rides the ``SUT_S3_URI``/``SUT_COMMIT``
    env contract instead, staged by the entrypoint before the run starts.
    """
    contestant = next(c for c in spec.contestants if c.id == plan.contestant_id)
    if plan.task is None:
        return replace(spec, contestants=(contestant,))
    return replace(
        spec,
        tasks=(plan.task,),
        contestants=(contestant,),
        attempts=1,
        concurrency=1,
    )


@dataclass(frozen=True)
class SutBinary:
    """A prebuilt SUT artifact resolved from ``binaries/<ref>/latest.json``."""

    ref: str
    commit: str
    uri: str


def trial_environment(
    run_id: str, match_uri: str, sut: SutBinary | None
) -> list[dict[str, str]]:
    """The exact environment the runner entrypoint contracts for.

    Names are the entrypoint's, verbatim: ``RUN_ID`` and ``MATCH_S3_URI``
    always; ``SUT_S3_URI`` + ``SUT_COMMIT`` as a pair when a prebuilt binary
    is staged (the entrypoint refuses one without the other).
    """
    env = [
        {"name": "RUN_ID", "value": run_id},
        {"name": "MATCH_S3_URI", "value": match_uri},
    ]
    if sut is not None:
        env.append({"name": "SUT_S3_URI", "value": sut.uri})
        env.append({"name": "SUT_COMMIT", "value": sut.commit})
    return env


def job_name(run_id: str, plan: TrialPlan) -> str:
    """A Batch job name: alphanumeric start, ``[A-Za-z0-9_-]``, max 128."""
    raw = f"{run_id}-{plan.label}"
    cleaned = "".join(
        ch if (ch.isascii() and (ch.isalnum() or ch in "-_")) else "-" for ch in raw
    ).strip("-_")
    if not cleaned or not cleaned[0].isalnum():
        cleaned = "t" + cleaned
    return cleaned[:128]


def sut_seats(spec: MatchSpec) -> list[str]:
    """The seats that run the Stella binary under test.

    These are the seats a cloud run must stage a prebuilt SUT for: no Stella
    checkout or toolchain exists inside the runner, so an unpinned match with
    a Stella seat would launch a container guaranteed to have no binary.
    """
    return [c.name for c in spec.contestants if c.agent == "stella"]


def refused_seats(
    needed: Mapping[str, list[str]],
    seat_env: Mapping[str, Mapping[str, str]],
    available: Iterable[str],
) -> dict[str, list[str]]:
    """Seats whose credentials will not exist in the trial environment.

    ``needed`` maps contestant id → candidate variable names (alternatives,
    not a conjunction — same contract as
    :func:`~.credentials.missing_required_credentials`); ``available`` is the
    set of names the SSM export will provide. A seat is credentialled if any
    candidate is available from SSM or already held in its own env; anything
    else must be refused at submit time, because a cloud trial that launches
    unauthenticated burns a container to score a ``0.0`` indistinguishable
    from a real loss (#1827).
    """
    provided = set(available)
    out: dict[str, list[str]] = {}
    for contestant_id, candidates in needed.items():
        if not candidates:
            continue
        held = seat_env.get(contestant_id, {})
        if any(name in provided or held.get(name) for name in candidates):
            continue
        out[contestant_id] = list(candidates)
    return out


@dataclass(frozen=True)
class Progress:
    """Counts of a run's Batch jobs by coarse state."""

    queued: int
    running: int
    succeeded: int
    failed: int

    @property
    def total(self) -> int:
        return self.queued + self.running + self.succeeded + self.failed

    @property
    def done(self) -> bool:
        return self.queued == 0 and self.running == 0

    def to_line(self, elapsed: float) -> str:
        """One progress line; the queued count is always spelled out."""
        return (
            f"[{elapsed / 60:6.1f}m] queued {self.queued:>3}  "
            f"running {self.running:>3}  ok {self.succeeded:>3}  "
            f"failed {self.failed:>3}  ({self.total} trials)"
        )


def progress_from_states(states: Iterable[str]) -> Progress:
    """Fold Batch job states into the four counts an operator watches.

    ``SUBMITTED``/``PENDING``/``RUNNABLE`` are all "queued" — ``RUNNABLE`` is
    where a quota-limited account parks the overflow, and it must read as
    progress rather than as a hang.
    """
    queued = running = succeeded = failed = 0
    for state in states:
        if state in _QUEUED_STATES:
            queued += 1
        elif state in _RUNNING_STATES:
            running += 1
        elif state == "SUCCEEDED":
            succeeded += 1
        else:
            failed += 1
    return Progress(queued=queued, running=running, succeeded=succeeded, failed=failed)


# --------------------------------------------------------------------------
# the AWS adapter
# --------------------------------------------------------------------------


class CloudExecutor:
    """Thin adapter over the AWS clients the cloud verbs need.

    Every decision above happens before a method here is called; this class
    only moves bytes. ``clients`` injects recorded-call fakes in tests — when
    a service is missing from the mapping, a real boto3 client is built
    lazily, so importing and testing this module never requires boto3.
    """

    def __init__(
        self,
        *,
        region: str | None = None,
        bucket: str | None = None,
        clients: Mapping[str, Any] | None = None,
    ) -> None:
        self._region = region
        self._bucket = bucket
        self._clients: dict[str, Any] = dict(clients or {})

    def _client(self, service: str) -> Any:
        if service not in self._clients:
            kwargs = {"region_name": self._region} if self._region else {}
            self._clients[service] = _boto3().client(service, **kwargs)
        return self._clients[service]

    # -- identity ----------------------------------------------------------

    def bucket(self) -> str:
        """The artifacts bucket, derived from the account id unless pinned."""
        if self._bucket is None:
            account = self._client("sts").get_caller_identity()["Account"]
            self._bucket = f"arenabench-artifacts-{account}"
        return self._bucket

    # -- credentials -------------------------------------------------------

    def available_credentials(self) -> set[str]:
        """Environment names the runner's SSM export will provide.

        Names only, never values (``WithDecryption=False``): the submit-time
        question is "will this variable exist in the trial", and the secret
        itself has no business transiting the operator's machine.
        """
        ssm = self._client("ssm")
        names: set[str] = set()
        token: str | None = None
        while True:
            kwargs: dict[str, Any] = {"Path": SSM_PREFIX, "WithDecryption": False}
            if token:
                kwargs["NextToken"] = token
            page = ssm.get_parameters_by_path(**kwargs)
            names.update(
                ssm_env_name(p["Name"]) for p in page.get("Parameters", [])
            )
            token = page.get("NextToken")
            if not token:
                return names

    # -- the SUT -----------------------------------------------------------

    def resolve_sut(
        self,
        ref: str,
        *,
        build_missing: bool = True,
        poll_interval: float = 15.0,
        sleep: Callable[[float], None] = time.sleep,
        out: Callable[[str], None] = print,
    ) -> SutBinary:
        """The prebuilt binary for ``ref``, building it first when absent.

        Reads ``binaries/<ref>/latest.json`` (written by the
        ``arenabench-sut-build`` CodeBuild project alongside each artifact);
        on a miss, starts that project with a ``GIT_REF`` override and polls
        until the build settles. The manifest's ``commit`` is what the trial
        records as its SUT identity, so it comes from the build, never from a
        local git resolution that could disagree with what was compiled.
        """
        manifest = self._latest_manifest(ref)
        if manifest is None:
            if not build_missing:
                raise CloudError(f"no prebuilt SUT for ref {ref!r} and building is off")
            self._build_sut(ref, poll_interval=poll_interval, sleep=sleep, out=out)
            manifest = self._latest_manifest(ref)
            if manifest is None:
                raise CloudError(
                    f"SUT build for {ref!r} succeeded but wrote no "
                    f"binaries/{ref_safe(ref)}/latest.json"
                )
        commit = str(manifest.get("commit") or "")
        if not commit:
            raise CloudError(f"binaries/{ref_safe(ref)}/latest.json names no commit")
        uri = f"s3://{self.bucket()}/binaries/{ref_safe(ref)}/{commit}/stella"
        return SutBinary(ref=ref, commit=commit, uri=uri)

    def _latest_manifest(self, ref: str) -> dict[str, Any] | None:
        s3 = self._client("s3")
        key = f"binaries/{ref_safe(ref)}/latest.json"
        try:
            body = s3.get_object(Bucket=self.bucket(), Key=key)["Body"].read()
        except s3.exceptions.NoSuchKey:
            return None
        try:
            manifest = json.loads(body.decode("utf-8"))
        except (UnicodeDecodeError, ValueError) as exc:
            raise CloudError(f"{key} is not valid JSON: {exc}") from exc
        return manifest if isinstance(manifest, dict) else None

    def _build_sut(
        self,
        ref: str,
        *,
        poll_interval: float,
        sleep: Callable[[float], None],
        out: Callable[[str], None],
    ) -> None:
        codebuild = self._client("codebuild")
        started = codebuild.start_build(
            projectName=SUT_BUILD_PROJECT,
            environmentVariablesOverride=[
                {"name": "GIT_REF", "value": ref, "type": "PLAINTEXT"}
            ],
        )
        build_id = started["build"]["id"]
        out(f"sut: no artifact for {ref!r}; building via {SUT_BUILD_PROJECT} ({build_id})")
        while True:
            build = codebuild.batch_get_builds(ids=[build_id])["builds"][0]
            status = build["buildStatus"]
            if status == "SUCCEEDED":
                out(f"sut: build {build_id} succeeded")
                return
            if status != "IN_PROGRESS":
                raise CloudError(f"SUT build {build_id} for {ref!r} ended {status}")
            sleep(poll_interval)

    # -- submission --------------------------------------------------------

    def submit_run(
        self,
        spec: MatchSpec,
        plans: Iterable[TrialPlan],
        *,
        run_id: str,
        queue: str,
        sut: SutBinary | None,
        vcpus: int = TRIAL_VCPUS,
        memory_mb: int = TRIAL_MEMORY_MB,
    ) -> list[dict[str, Any]]:
        """Upload the match and its slices, then submit one job per plan.

        Returns the submitted job records (also persisted to
        ``runs/<run-id>/jobs.json`` so ``cloud status`` / ``cloud fetch`` — or
        another machine entirely — can find the run again without this
        process's memory).
        """
        from .config import dump_match

        s3 = self._client("s3")
        batch = self._client("batch")
        bucket = self.bucket()
        prefix = f"runs/{run_id}"

        # Ship the resolved commit, never the symbolic ref: the container has
        # no Stella checkout to resolve one against.
        spec = pin_slice_to_commit(spec, sut)

        s3.put_object(
            Bucket=bucket,
            Key=f"{prefix}/match.toml",
            Body=dump_match(spec).encode("utf-8"),
        )

        jobs: list[dict[str, Any]] = []
        for plan in plans:
            key = f"{prefix}/trials/{plan.label}/match.toml"
            s3.put_object(
                Bucket=bucket,
                Key=key,
                Body=dump_match(slice_spec(spec, plan)).encode("utf-8"),
            )
            submitted = batch.submit_job(
                jobName=job_name(run_id, plan),
                jobQueue=queue,
                jobDefinition=JOB_DEFINITION,
                containerOverrides={
                    "command": ["run"],
                    "resourceRequirements": [
                        {"type": "VCPU", "value": str(vcpus)},
                        {"type": "MEMORY", "value": str(memory_mb)},
                    ],
                    "environment": trial_environment(
                        run_id, f"s3://{bucket}/{key}", sut
                    ),
                },
            )
            jobs.append({"id": submitted["jobId"], **plan.to_json()})

        record = {
            "run_id": run_id,
            "queue": queue,
            "job_definition": JOB_DEFINITION,
            "sut": None if sut is None else {
                "ref": sut.ref, "commit": sut.commit, "uri": sut.uri,
            },
            "jobs": jobs,
        }
        s3.put_object(
            Bucket=bucket,
            Key=f"{prefix}/jobs.json",
            Body=json.dumps(record, indent=2).encode("utf-8"),
        )
        return jobs

    def load_jobs(self, run_id: str) -> dict[str, Any]:
        """The persisted submission record for a run id."""
        s3 = self._client("s3")
        key = f"runs/{run_id}/jobs.json"
        try:
            body = s3.get_object(Bucket=self.bucket(), Key=key)["Body"].read()
        except s3.exceptions.NoSuchKey:
            raise CloudError(
                f"no submission record at s3://{self.bucket()}/{key} — "
                "was this run submitted with `arenabench cloud run`?"
            ) from None
        return json.loads(body.decode("utf-8"))

    # -- progress ----------------------------------------------------------

    def job_states(self, job_ids: Iterable[str]) -> dict[str, str]:
        """Current Batch state per job id, in describe chunks of 100."""
        batch = self._client("batch")
        ids = list(job_ids)
        states: dict[str, str] = {}
        for start in range(0, len(ids), _DESCRIBE_CHUNK):
            chunk = ids[start : start + _DESCRIBE_CHUNK]
            for job in batch.describe_jobs(jobs=chunk)["jobs"]:
                states[job["jobId"]] = job["status"]
        return states

    def watch(
        self,
        jobs: list[dict[str, Any]],
        *,
        poll_interval: float = 15.0,
        sleep: Callable[[float], None] = time.sleep,
        clock: Callable[[], float] = time.monotonic,
        out: Callable[[str], None] = print,
    ) -> dict[str, str]:
        """Poll until every job settles, printing transitions and counts.

        No busy-waiting: one ``describe_jobs`` sweep per interval. The first
        time trials sit queued, one explanatory line says why — a
        quota-limited account is *supposed* to park the overflow in
        ``RUNNABLE`` and drain it as capacity frees.
        """
        labels = {job["id"]: job["label"] for job in jobs}
        started = clock()
        previous: dict[str, str] = {}
        quota_note_shown = False
        while True:
            states = self.job_states(labels.keys())
            for job_id, state in states.items():
                before = previous.get(job_id)
                if before is not None and before != state:
                    out(f"  {labels[job_id]}: {before} -> {state}")
            previous = states
            progress = progress_from_states(states.values())
            out(progress.to_line(clock() - started))
            if progress.queued and not quota_note_shown:
                out(
                    f"  note: {progress.queued} trial(s) queued — Batch holds "
                    "them in RUNNABLE while the account vCPU quota is spent "
                    "and drains the backlog as capacity frees. Queued is "
                    "progress, not a hang."
                )
                quota_note_shown = True
            if progress.done:
                return states
            sleep(poll_interval)

    def run_rows(self, run_id: str) -> list[dict[str, str]]:
        """The DynamoDB status rows the runner wrote for this run."""
        dynamodb = self._client("dynamodb")
        rows: list[dict[str, str]] = []
        kwargs: dict[str, Any] = {
            "TableName": TABLE_NAME,
            "KeyConditionExpression": "PK = :pk",
            "ExpressionAttributeValues": {":pk": {"S": f"RUN#{run_id}"}},
        }
        while True:
            page = dynamodb.query(**kwargs)
            for item in page.get("Items", []):
                rows.append(
                    {
                        "job": item.get("SK", {}).get("S", "").removeprefix("JOB#"),
                        "status": item.get("status", {}).get("S", ""),
                        "detail": item.get("detail", {}).get("S", ""),
                    }
                )
            last = page.get("LastEvaluatedKey")
            if not last:
                return rows
            kwargs["ExclusiveStartKey"] = last

    # -- results -----------------------------------------------------------

    def fetch_results(
        self,
        run_id: str,
        jobs: list[dict[str, Any]],
        dest: Path,
        *,
        artifacts: bool = False,
        out: Callable[[str], None] = print,
    ) -> int:
        """Download each trial's ``results.json`` (and, opted in, everything).

        ``results.json`` per job is the default because it is the scoreboard;
        the full artifact tree (events, recordings, workspaces) can be
        gigabytes, so ``artifacts=True`` is a choice, not a side effect.
        Returns the number of results files downloaded.
        """
        s3 = self._client("s3")
        bucket = self.bucket()
        fetched = 0
        for job in jobs:
            key = f"runs/{run_id}/{job['id']}/results.json"
            try:
                body = s3.get_object(Bucket=bucket, Key=key)["Body"].read()
            except s3.exceptions.NoSuchKey:
                out(f"  {job['label']}: no results.json (trial did not finish?)")
                continue
            target = dest / job["label"] / "results.json"
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(body)
            fetched += 1
        if artifacts:
            fetched += self._fetch_prefix(f"runs/{run_id}/", dest, out=out)
        return fetched

    def _fetch_prefix(
        self, prefix: str, dest: Path, *, out: Callable[[str], None]
    ) -> int:
        s3 = self._client("s3")
        bucket = self.bucket()
        count = 0
        kwargs: dict[str, Any] = {"Bucket": bucket, "Prefix": prefix}
        while True:
            page = s3.list_objects_v2(**kwargs)
            for entry in page.get("Contents", []):
                key = entry["Key"]
                relative = key.removeprefix(prefix)
                if not relative or relative.endswith("/"):
                    continue
                target = dest / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(
                    s3.get_object(Bucket=bucket, Key=key)["Body"].read()
                )
                count += 1
            if not page.get("IsTruncated"):
                out(f"  artifacts: {count} object(s) under s3://{bucket}/{prefix}")
                return count
            kwargs["ContinuationToken"] = page["NextContinuationToken"]


# --------------------------------------------------------------------------
# CLI verbs
# --------------------------------------------------------------------------


def _new_run_id() -> str:
    return time.strftime("r%Y%m%d-%H%M%S-") + uuid.uuid4().hex[:6]


def _cmd_cloud_run(args: Any, executor: CloudExecutor | None = None) -> int:
    """``arenabench cloud run match.toml --ref main`` — submit, watch, fetch."""
    import sys

    from .config import MatchTemplateError, load_match, required_env

    try:
        spec = load_match(Path(args.template).expanduser())
    except MatchTemplateError as exc:
        for problem in exc.problems:
            print(f"error: {problem}", file=sys.stderr)
        return 2

    executor = executor or CloudExecutor(region=args.region, bucket=args.bucket)

    # Refusal before any upload or submission: a seat whose declared
    # credentials will not exist in the trial environment must not launch.
    needed = required_env(spec)
    seat_env = {c.id: c.env for c in spec.contestants}
    refused = refused_seats(needed, seat_env, executor.available_credentials())
    if refused:
        names = {c.id: c.name for c in spec.contestants}
        print(
            "error: seat(s) have no credentials in SSM under "
            f"{SSM_PREFIX}/ — refusing to submit:",
            file=sys.stderr,
        )
        for contestant_id, candidates in refused.items():
            print(
                f"  {names.get(contestant_id, contestant_id)}: "
                f"any of {', '.join(candidates)}",
                file=sys.stderr,
            )
        print(
            f"  (aws ssm put-parameter --name '{SSM_PREFIX}/<key_name>' "
            "--type SecureString --value ... to add one)",
            file=sys.stderr,
        )
        return 2

    ref = args.ref if args.ref is not None else spec.sut_ref
    sut: SutBinary | None = None
    if ref:
        sut = executor.resolve_sut(ref)
    elif sut_seats(spec):
        print(
            "error: this match seats Stella "
            f"({', '.join(sut_seats(spec))}) but pins no SUT ref; the cloud "
            "runner has no local binary to fall back to. Pass --ref.",
            file=sys.stderr,
        )
        return 2

    run_id = args.run_id or _new_run_id()
    queue = select_queue(args.burst)
    plans = plan_trials(spec)

    print(f"run       : {run_id}")
    print(f"match     : {spec.name}")
    print(f"queue     : {queue}" + ("  (spot — not for measured comparisons)"
                                    if args.burst else ""))
    if sut is not None:
        print(f"sut       : {sut.commit[:12]} ({sut.ref})")
    grid = "per-seat whole-dataset" if not spec.tasks else (
        f"{spec.attempts} attempt(s) x {len(spec.tasks)} task(s) x "
        f"{len(spec.contestants)} seat(s)"
    )
    print(f"trials    : {len(plans)}  [{grid}]")

    jobs = executor.submit_run(
        spec, plans, run_id=run_id, queue=queue, sut=sut,
        vcpus=args.vcpus, memory_mb=args.memory_mb,
    )
    print(f"submitted : {len(jobs)} job(s) to {queue}")
    print(f"record    : s3://{executor.bucket()}/runs/{run_id}/jobs.json")

    if args.no_wait:
        print(f"not waiting; follow with: arenabench cloud status {run_id}")
        return 0

    states = executor.watch(jobs, poll_interval=args.poll)
    final = progress_from_states(states.values())
    print(f"\nfinished: {final.succeeded} succeeded, {final.failed} failed")
    for row in executor.run_rows(run_id):
        print(f"  ddb {row['job']}: {row['status']}  {row['detail']}")

    if not args.no_fetch:
        dest = Path(args.out or f"arenabench-cloud/{run_id}").expanduser()
        fetched = executor.fetch_results(
            run_id, jobs, dest, artifacts=args.artifacts
        )
        print(f"results   : {fetched} file(s) -> {dest}")
    return 0 if final.failed == 0 else 1


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
        print(f"  ddb {row['job']}: {row['status']}  {row['detail']}")
    final = progress_from_states(states.values())
    return 0 if final.failed == 0 else 1


def _cmd_cloud_fetch(args: Any, executor: CloudExecutor | None = None) -> int:
    """``arenabench cloud fetch <run-id>`` — pull results (and artifacts)."""
    executor = executor or CloudExecutor(region=args.region, bucket=args.bucket)
    record = executor.load_jobs(args.run_id)
    dest = Path(args.out or f"arenabench-cloud/{args.run_id}").expanduser()
    fetched = executor.fetch_results(
        args.run_id, record["jobs"], dest, artifacts=args.artifacts
    )
    print(f"results   : {fetched} file(s) -> {dest}")
    return 0


def _common_aws_arguments(parser: Any) -> None:
    parser.add_argument("--region", help="AWS region (default: the SDK's chain)")
    parser.add_argument(
        "--bucket",
        help="artifacts bucket (default: arenabench-artifacts-<account-id>)",
    )


def register_cli(subparsers: Any) -> None:
    """Attach the ``cloud`` verb tree to the main CLI's subparsers."""
    cloud = subparsers.add_parser(
        "cloud", help="run matches on the AWS Batch substrate (needs boto3)"
    )
    verbs = cloud.add_subparsers(dest="cloud_command", required=True)

    run = verbs.add_parser(
        "run", help="submit a match to Batch, stream progress, fetch results"
    )
    run.add_argument("template", help="path to an arenabench.toml")
    run.add_argument(
        "--ref",
        help="git ref of the Stella SUT to run (default: the match's sut_ref); "
             "built via CodeBuild first if no artifact exists",
    )
    run.add_argument("--run-id", help="run identifier (default: generated)")
    run.add_argument(
        "--burst",
        action="store_true",
        help="submit to the spot queue — for non-measurement work ONLY; spot "
             "interruption invalidates a measured comparison",
    )
    run.add_argument("--poll", type=float, default=15.0,
                     help="seconds between progress polls")
    run.add_argument("--vcpus", type=int, default=TRIAL_VCPUS,
                     help=f"vCPUs per trial container (default: {TRIAL_VCPUS})")
    run.add_argument(
        "--memory-mb", type=int, default=TRIAL_MEMORY_MB,
        help=f"memory per trial container in MiB (default: {TRIAL_MEMORY_MB})",
    )
    run.add_argument("--no-wait", action="store_true",
                     help="submit and exit; follow later with `cloud status`")
    run.add_argument("--no-fetch", action="store_true",
                     help="do not download results when the run finishes")
    run.add_argument("--artifacts", action="store_true",
                     help="also download the full artifact tree (can be large)")
    run.add_argument("--out", help="local directory for downloaded results")
    _common_aws_arguments(run)
    run.set_defaults(func=_cmd_cloud_run)

    status = verbs.add_parser("status", help="progress of a submitted cloud run")
    status.add_argument("run_id")
    status.add_argument("--follow", action="store_true",
                        help="keep polling until every trial settles")
    status.add_argument("--poll", type=float, default=15.0)
    _common_aws_arguments(status)
    status.set_defaults(func=_cmd_cloud_status)

    fetch = verbs.add_parser("fetch", help="download a cloud run's results")
    fetch.add_argument("run_id")
    fetch.add_argument("--artifacts", action="store_true",
                       help="also download the full artifact tree (can be large)")
    fetch.add_argument("--out", help="local directory for downloaded results")
    _common_aws_arguments(fetch)
    fetch.set_defaults(func=_cmd_cloud_fetch)
