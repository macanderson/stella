# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The AWS Batch adapter :mod:`.cloud`'s submit-time decisions drive.

Split out of ``cloud.py`` (#3255's file-size fallout — the refusal check it
added crossed the 1500-line ratchet) rather than folded into it: every
decision above this module — queue selection, trial fan-out, credential and
task-name refusal — is a pure function over plain data, exercised without any
AWS client. :class:`CloudExecutor` is the one place that actually calls
boto3, and keeping it a separate file makes that boundary a module boundary,
not just a section comment.

``clients`` injects recorded-call fakes in tests — when a service is missing
from the mapping, a real boto3 client is built lazily via :func:`.cloud._boto3`,
so importing and testing this module never requires boto3. ``ls_remote`` is
the same seam for the one non-AWS call (:func:`.cloud.git_ls_remote`), so the
freshness tests neither shell out nor reach the network.

Re-exported from :mod:`.cloud` (``from .cloud import CloudExecutor`` still
works) so callers and tests need not know the class moved.
"""

from __future__ import annotations

import json
import time
from collections.abc import Callable, Iterable, Mapping
from pathlib import Path
from typing import Any

from .cloud import (
    _DESCRIBE_CHUNK,
    _SETTLED_STATES,
    GIT_URL_VAR,
    JOB_DEFINITION,
    SSM_PREFIX,
    SUT_BUILD_PROJECT,
    TABLE_NAME,
    TRIAL_MEMORY_MB,
    TRIAL_VCPUS,
    CloudError,
    SutBinary,
    TrialPlan,
    _boto3,
    _ref_patterns,
    git_ls_remote,
    is_moving_ref,
    job_name,
    pin_slice_to_commit,
    progress_from_states,
    ref_safe,
    slice_spec,
    ssm_env_name,
    sut_cache_is_current,
    tip_from_ls_remote,
    trial_environment,
)
from .model import MatchSpec
from .sut import is_safe_ref

__all__ = ["CloudExecutor"]


class CloudExecutor:
    """Thin adapter over the AWS clients the cloud verbs need.

    Every decision above happens before a method here is called; this class
    only moves bytes. ``clients`` injects recorded-call fakes in tests — when
    a service is missing from the mapping, a real boto3 client is built
    lazily, so importing and testing this module never requires boto3.
    ``ls_remote`` is the same seam for the one non-AWS call
    (:func:`git_ls_remote`), so the freshness tests neither shell out nor
    reach the network.
    """

    def __init__(
        self,
        *,
        region: str | None = None,
        bucket: str | None = None,
        clients: Mapping[str, Any] | None = None,
        ls_remote: Callable[[str, Iterable[str]], str] = git_ls_remote,
    ) -> None:
        self._region = region
        self._bucket = bucket
        self._clients: dict[str, Any] = dict(clients or {})
        self._ls_remote = ls_remote
        self._git_url: str | None = None

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
        """The prebuilt binary for ``ref``, rebuilt unless it is current.

        Reads ``binaries/<ref>/latest.json`` (written by the
        ``arenabench-sut-build`` CodeBuild project alongside each artifact).
        Whether that artifact may be *reused* is :func:`sut_cache_is_current`'s
        call: a full SHA's artifact is always current, while a moving ref's is
        current only when it was built from the commit the remote reports for
        that name now. Otherwise the project is started with a ``GIT_REF``
        override and polled until the build settles.

        The returned ``commit`` always comes from the manifest — i.e. from
        what CodeBuild actually compiled — never from the ``ls-remote`` tip,
        so the identity a trial records cannot drift from the bytes it runs
        even if the branch moves mid-build (#2388).
        """
        ref = ref.strip()
        manifest = self._latest_manifest(ref)
        cached = str((manifest or {}).get("commit") or "")
        if manifest is not None and not cached:
            raise CloudError(f"binaries/{ref_safe(ref)}/latest.json names no commit")

        moving = is_moving_ref(ref)
        tip = self._remote_tip(ref) if moving else ref
        if not moving and cached and cached != ref:
            # Only reachable if something other than the buildspec wrote the
            # manifest: `git checkout <sha>` can only ever rev-parse to <sha>.
            # Rebuilding would write the same disagreement again, so say so.
            raise CloudError(
                f"binaries/{ref_safe(ref)}/latest.json names commit {cached} "
                f"but the ref is {ref}; refusing to run a binary whose "
                "identity is in doubt"
            )

        if not sut_cache_is_current(ref, cached, tip):
            if not build_missing:
                raise CloudError(
                    f"no current prebuilt SUT for ref {ref!r} and building is off"
                    + (f" (cached {cached[:12]}, {ref} is at {tip[:12]})"
                       if cached else "")
                )
            reason = (
                f"{ref} is at {tip[:12]}, cached artifact is {cached[:12]}"
                if cached
                else f"no artifact for {ref!r}"
            )
            self._build_sut(
                ref, reason=reason, poll_interval=poll_interval, sleep=sleep, out=out
            )
            manifest = self._latest_manifest(ref)
            cached = str((manifest or {}).get("commit") or "")
            if manifest is None:
                raise CloudError(
                    f"SUT build for {ref!r} succeeded but wrote no "
                    f"binaries/{ref_safe(ref)}/latest.json"
                )
            if not cached:
                raise CloudError(
                    f"binaries/{ref_safe(ref)}/latest.json names no commit"
                )

        uri = f"s3://{self.bucket()}/binaries/{ref_safe(ref)}/{cached}/stella"
        return SutBinary(ref=ref, commit=cached, uri=uri)

    def _sut_git_url(self) -> str:
        """The remote the SUT build clones, read off the build project itself.

        Not a constant in this module: ``SutGitUrl`` is a stack parameter, and
        a freshness check that consulted a different remote than the build
        clones would answer a question nobody asked.
        """
        if self._git_url is None:
            codebuild = self._client("codebuild")
            projects = codebuild.batch_get_projects(names=[SUT_BUILD_PROJECT]).get(
                "projects", []
            )
            if not projects:
                raise CloudError(
                    f"CodeBuild project {SUT_BUILD_PROJECT!r} not found — is the "
                    "arenabench stack deployed in this account/region?"
                )
            env_vars = projects[0].get("environment", {}).get("environmentVariables", [])
            url = next(
                (
                    str(var.get("value") or "")
                    for var in env_vars
                    if var.get("name") == GIT_URL_VAR
                ),
                "",
            )
            if not url or url.startswith("-"):
                raise CloudError(
                    f"CodeBuild project {SUT_BUILD_PROJECT!r} declares no usable "
                    f"{GIT_URL_VAR} ({url!r}); cannot check whether a moving ref "
                    "is current"
                )
            self._git_url = url
        return self._git_url

    def _remote_tip(self, ref: str) -> str:
        """The commit ``ref`` names on the SUT remote right now.

        Raising rather than returning ``""`` on a miss is the point: an
        unresolvable ref must stop the submission, because the alternative is
        running whatever the cache happens to hold and calling it ``ref``.
        """
        if not is_safe_ref(ref):
            raise CloudError(f"refusing to resolve unsafe ref {ref!r}")
        url = self._sut_git_url()
        tip = tip_from_ls_remote(self._ls_remote(url, _ref_patterns(ref)), ref)
        if not tip:
            raise CloudError(
                f"{url} has no ref {ref!r} — nothing to build or compare "
                "against. Pass a branch, tag, or full commit SHA that exists "
                "on that remote."
            )
        return tip

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
        reason: str,
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
        out(f"sut: {reason}; building via {SUT_BUILD_PROJECT} ({build_id})")
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
        on_settled: Callable[[str, str], bool] | None = None,
    ) -> dict[str, str]:
        """Poll until every job settles, printing transitions and counts.

        No busy-waiting: one ``describe_jobs`` sweep per interval. The first
        time trials sit queued, one explanatory line says why — a
        quota-limited account is *supposed* to park the overflow in
        ``RUNNABLE`` and drain it as capacity frees.

        ``on_settled`` is called once per job the moment this loop first sees
        it reach ``SUCCEEDED``/``FAILED`` — once per *settlement*, never once
        per poll — and returning ``True`` from it returns this method
        immediately, leaving the rest of the run in flight for the caller to
        cancel. It hangs off the submitter's loop rather than off the runner
        container because one Batch job is one trial: a container that
        inspects itself can save nothing, and the cost worth saving is the
        other N-1 trials, which only the process holding ``jobs.json`` can
        stop. :func:`.gate_watch.settled_gate` is the intended argument.
        """
        labels = {job["id"]: job["label"] for job in jobs}
        started = clock()
        previous: dict[str, str] = {}
        reported: set[str] = set()
        quota_note_shown = False
        while True:
            states = self.job_states(labels.keys())
            for job_id, state in states.items():
                before = previous.get(job_id)
                if before is not None and before != state:
                    out(f"  {labels[job_id]}: {before} -> {state}")
            previous = states
            if on_settled is not None:
                for job_id, state in states.items():
                    if state in _SETTLED_STATES and job_id not in reported:
                        reported.add(job_id)
                        if on_settled(job_id, state):
                            return states
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
