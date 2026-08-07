# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The cloud executor's submit contract, pinned (#2099).

The AWS Batch substrate (``arenabench/infra/``) speaks one contract — the
runner entrypoint's env names, the provisioned queue and job-definition
names, the SSM credential export — and every test here proves the executor
speaks exactly that contract, against recorded-call fakes rather than AWS.
The decisions (queue selection, trial fan-out, env assembly, refusal on
missing credentials, progress derivation) are pure functions exercised
directly; the boto3 adapter is exercised through fakes that record every
call, so a drifted parameter name fails here instead of in a container that
billed real compute first.

These tests are the witness for #2099: none of them can pass on a tree
without ``arenabench/arenabench/cloud.py``.
"""

from __future__ import annotations

import argparse
import io
import json
import tomllib
from types import SimpleNamespace

import pytest

from arenabench.cloud import (
    BURST_QUEUE,
    JOB_DEFINITION,
    MEASURE_QUEUE,
    CloudError,
    CloudExecutor,
    _cmd_cloud_run,
    job_name,
    plan_trials,
    progress_from_states,
    ref_safe,
    refused_seats,
    select_queue,
    slice_spec,
    ssm_env_name,
    sut_seats,
    trial_environment,
)
from arenabench.config import match_from_toml
from arenabench.model import Contestant, Engine, MatchSpec

ACCOUNT = "123456789012"
BUCKET = f"arenabench-artifacts-{ACCOUNT}"


# --------------------------------------------------------------------------
# recorded-call fakes — small on purpose; moto would be a heavyweight
# dependency for what is a handful of parameter-shape assertions
# --------------------------------------------------------------------------


class _NoSuchKey(Exception):
    pass


class FakeS3:
    def __init__(self, objects: dict[str, bytes] | None = None) -> None:
        self.objects: dict[str, bytes] = dict(objects or {})
        self.calls: list[tuple] = []
        self.exceptions = SimpleNamespace(NoSuchKey=_NoSuchKey)

    def put_object(self, *, Bucket: str, Key: str, Body: bytes, **_: object) -> dict:
        self.calls.append(("put_object", Bucket, Key))
        self.objects[Key] = Body
        return {}

    def get_object(self, *, Bucket: str, Key: str) -> dict:
        self.calls.append(("get_object", Bucket, Key))
        if Key not in self.objects:
            raise _NoSuchKey(Key)
        return {"Body": io.BytesIO(self.objects[Key])}

    def list_objects_v2(self, **kwargs: object) -> dict:
        self.calls.append(("list_objects_v2", kwargs.get("Prefix")))
        prefix = str(kwargs.get("Prefix") or "")
        keys = sorted(k for k in self.objects if k.startswith(prefix))
        return {
            "Contents": [{"Key": k} for k in keys],
            "IsTruncated": False,
        }


class FakeBatch:
    """Records submissions; serves job states from a per-sweep script."""

    def __init__(self, state_script: list[dict[str, str]] | None = None) -> None:
        self.submitted: list[dict] = []
        self.describe_calls: list[list[str]] = []
        self.state_script = list(state_script or [])
        self._counter = 0

    def submit_job(self, **kwargs: object) -> dict:
        self.submitted.append(kwargs)
        self._counter += 1
        return {"jobId": f"job-{self._counter:03d}", "jobName": kwargs["jobName"]}

    def describe_jobs(self, *, jobs: list[str]) -> dict:
        self.describe_calls.append(list(jobs))
        states = self.state_script[0] if self.state_script else {}
        return {"jobs": [{"jobId": j, "status": states.get(j, "SUCCEEDED")} for j in jobs]}

    def advance(self) -> None:
        if len(self.state_script) > 1:
            self.state_script.pop(0)


class FakeSsm:
    def __init__(self, parameters: list[str]) -> None:
        self.parameters = parameters
        self.calls: list[dict] = []

    def get_parameters_by_path(self, **kwargs: object) -> dict:
        self.calls.append(dict(kwargs))
        # Two pages, to prove the paginator loop follows NextToken.
        first, rest = self.parameters[:1], self.parameters[1:]
        if "NextToken" not in kwargs:
            page = {"Parameters": [{"Name": n} for n in first]}
            if rest:
                page["NextToken"] = "page-2"
            return page
        return {"Parameters": [{"Name": n} for n in rest]}


class FakeCodeBuild:
    def __init__(self, statuses: list[str]) -> None:
        self.statuses = list(statuses)
        self.started: list[dict] = []
        self.polls = 0

    def start_build(self, **kwargs: object) -> dict:
        self.started.append(dict(kwargs))
        return {"build": {"id": "arenabench-sut-build:1234"}}

    def batch_get_builds(self, *, ids: list[str]) -> dict:
        status = self.statuses[min(self.polls, len(self.statuses) - 1)]
        self.polls += 1
        return {"builds": [{"id": ids[0], "buildStatus": status}]}


class FakeSts:
    def get_caller_identity(self) -> dict:
        return {"Account": ACCOUNT}


class FakeDynamo:
    def __init__(self, pages: list[dict]) -> None:
        self.pages = list(pages)
        self.calls: list[dict] = []

    def query(self, **kwargs: object) -> dict:
        self.calls.append(dict(kwargs))
        return self.pages[min(len(self.calls) - 1, len(self.pages) - 1)]


def _executor(**clients: object) -> CloudExecutor:
    defaults: dict[str, object] = {"sts": FakeSts()}
    defaults.update(clients)
    return CloudExecutor(clients=defaults)


# --------------------------------------------------------------------------
# spec helpers
# --------------------------------------------------------------------------


def _seat(seat_id: str, agent: str = "stella", api: str = "openrouter") -> Contestant:
    return Contestant(
        id=seat_id,
        name=seat_id,
        agent=agent,
        engine=Engine(api=api, model="z-ai/glm-5.2"),
        required_env=("OPENROUTER_API_KEY",),
    )


def _spec(
    tasks: tuple[str, ...] = ("task-a", "task-b"),
    seats: tuple[Contestant, ...] | None = None,
    attempts: int = 1,
) -> MatchSpec:
    return MatchSpec(
        id="m1",
        name="cloud match",
        dataset="terminal-bench-2.1",
        tasks=tasks,
        contestants=seats or (_seat("stella"), _seat("cc", agent="claude-code")),
        attempts=attempts,
    )


SUT = None  # filled per-test via resolve_sut or built directly


def _sut():
    from arenabench.cloud import SutBinary

    commit = "c" * 40
    return SutBinary(
        ref="main", commit=commit, uri=f"s3://{BUCKET}/binaries/main/{commit}/stella"
    )


# --------------------------------------------------------------------------
# pure decisions
# --------------------------------------------------------------------------


class TestSsmEnvName:
    """The refusal check is honest only if this transform matches the
    entrypoint's export (basename | upper | non-[A-Z0-9_] -> '_')."""

    def test_matches_the_entrypoint_export(self) -> None:
        assert ssm_env_name("/arenabench/anthropic_api_key") == "ANTHROPIC_API_KEY"

    def test_folds_hyphens_like_tr_c(self) -> None:
        assert ssm_env_name("/arenabench/zai-api-key") == "ZAI_API_KEY"

    def test_takes_the_basename_of_a_nested_parameter(self) -> None:
        assert ssm_env_name("/arenabench/team/openrouter_api_key") == (
            "OPENROUTER_API_KEY"
        )


class TestRefSafe:
    def test_mirrors_the_codebuild_slash_fold(self) -> None:
        assert ref_safe("feature/x") == "feature-x"
        assert ref_safe("main") == "main"


class TestSelectQueue:
    """Measured comparisons ride on-demand only; spot interruption is a
    confound, so the burst queue takes only explicitly non-measurement work."""

    def test_measured_goes_to_the_on_demand_queue(self) -> None:
        assert select_queue(False) == MEASURE_QUEUE == "arenabench-measure"

    def test_burst_is_the_explicit_opt_out(self) -> None:
        assert select_queue(True) == BURST_QUEUE == "arenabench-burst"


class TestPlanTrials:
    def test_the_12x12_shape_is_144_submissions(self) -> None:
        """The array-sizing half of the witness: 12 agents x 12 tasks fans
        into exactly 144 per-trial jobs, one container each."""
        seats = tuple(_seat(f"agent{i:02d}") for i in range(12))
        tasks = tuple(f"task{i:02d}" for i in range(12))
        plans = plan_trials(_spec(tasks=tasks, seats=seats))
        assert len(plans) == 144
        assert len({p.label for p in plans}) == 144
        assert [p.index for p in plans] == list(range(144))

    def test_attempts_multiply_the_grid(self) -> None:
        plans = plan_trials(_spec(attempts=2))
        assert len(plans) == 2 * 2 * 2
        assert {p.attempt for p in plans} == {1, 2}

    def test_no_named_tasks_fans_per_seat_only(self) -> None:
        """The task list of a whole-dataset match is only known to Harbor
        inside the container, so the fan-out is one job per seat."""
        plans = plan_trials(_spec(tasks=()))
        assert len(plans) == 2
        assert all(p.task is None for p in plans)


class TestSliceSpec:
    def test_a_task_slice_is_one_cell_of_the_grid(self) -> None:
        spec = _spec(attempts=3)
        plan = plan_trials(spec)[0]
        sliced = slice_spec(spec, plan)
        assert sliced.tasks == (plan.task,)
        assert [c.id for c in sliced.contestants] == [plan.contestant_id]
        assert sliced.attempts == 1
        assert sliced.concurrency == 1
        assert sliced.dataset == spec.dataset

    def test_a_whole_dataset_slice_keeps_the_matchs_own_shape(self) -> None:
        spec = _spec(tasks=(), attempts=3)
        sliced = slice_spec(spec, plan_trials(spec)[0])
        assert sliced.tasks == ()
        assert sliced.attempts == 3
        assert len(sliced.contestants) == 1


class TestTrialEnvironment:
    """The env names are the entrypoint's contract, verbatim."""

    def test_the_exact_names_the_entrypoint_speaks(self) -> None:
        env = trial_environment("r1", f"s3://{BUCKET}/runs/r1/t/match.toml", _sut())
        assert [e["name"] for e in env] == [
            "RUN_ID",
            "MATCH_S3_URI",
            "SUT_S3_URI",
            "SUT_COMMIT",
        ]
        values = {e["name"]: e["value"] for e in env}
        assert values["RUN_ID"] == "r1"
        assert values["SUT_COMMIT"] == "c" * 40
        assert values["SUT_S3_URI"].endswith("/stella")

    def test_no_sut_means_no_sut_pair(self) -> None:
        """SUT_S3_URI requires SUT_COMMIT in the entrypoint; they travel as a
        pair or not at all."""
        env = trial_environment("r1", "s3://b/k", None)
        assert [e["name"] for e in env] == ["RUN_ID", "MATCH_S3_URI"]


class TestJobName:
    def test_batch_charset_and_length(self) -> None:
        plan = plan_trials(_spec())[0]
        name = job_name("r.2026/08.07_x", plan)
        assert name[0].isalnum()
        assert len(name) <= 128
        assert all(c.isalnum() or c in "-_" for c in name)


class TestRefusedSeats:
    """A seat with no credentials in the trial environment must be refused at
    submit time (#1827) — never launched to score a silent 0.0."""

    def test_a_seat_with_nothing_available_is_refused(self) -> None:
        refused = refused_seats(
            {"stella": ["OPENROUTER_API_KEY", "ZAI_API_KEY"]}, {}, set()
        )
        assert refused == {"stella": ["OPENROUTER_API_KEY", "ZAI_API_KEY"]}

    def test_any_one_candidate_in_ssm_credentials_the_seat(self) -> None:
        """The candidate names are alternatives, not a conjunction."""
        refused = refused_seats(
            {"stella": ["OPENROUTER_API_KEY", "ZAI_API_KEY"]},
            {},
            {"ZAI_API_KEY"},
        )
        assert refused == {}

    def test_a_seat_holding_its_own_value_is_credentialled(self) -> None:
        refused = refused_seats(
            {"stella": ["OPENROUTER_API_KEY"]},
            {"stella": {"OPENROUTER_API_KEY": "sk-or"}},
            set(),
        )
        assert refused == {}

    def test_a_seat_declaring_nothing_is_never_refused(self) -> None:
        assert refused_seats({"free": []}, {}, set()) == {}


class TestProgress:
    """A quota-limited account parks the overflow in RUNNABLE; that must read
    as progress, not as a hang."""

    def test_runnable_counts_as_queued(self) -> None:
        progress = progress_from_states(
            ["SUBMITTED", "PENDING", "RUNNABLE", "STARTING", "RUNNING",
             "SUCCEEDED", "FAILED"]
        )
        assert (progress.queued, progress.running) == (3, 2)
        assert (progress.succeeded, progress.failed) == (1, 1)
        assert not progress.done

    def test_the_line_spells_out_the_queued_count(self) -> None:
        progress = progress_from_states(["RUNNABLE"] * 120 + ["RUNNING"] * 24)
        line = progress.to_line(190.0)
        assert "queued 120" in line
        assert "running  24" in line
        assert "(144 trials)" in line

    def test_done_only_when_nothing_is_pending(self) -> None:
        assert progress_from_states(["SUCCEEDED", "FAILED"]).done
        assert not progress_from_states(["SUCCEEDED", "RUNNABLE"]).done


class TestSutSeats:
    def test_names_the_stella_seats(self) -> None:
        assert sut_seats(_spec()) == ["stella"]
        assert sut_seats(_spec(seats=(_seat("cc", agent="claude-code"),))) == []


# --------------------------------------------------------------------------
# the adapter, against recorded calls
# --------------------------------------------------------------------------


class TestAvailableCredentials:
    def test_names_only_and_paginated(self) -> None:
        ssm = FakeSsm(["/arenabench/anthropic_api_key", "/arenabench/zai-api-key"])
        names = _executor(ssm=ssm).available_credentials()
        assert names == {"ANTHROPIC_API_KEY", "ZAI_API_KEY"}
        assert len(ssm.calls) == 2, "NextToken page was not followed"
        for call in ssm.calls:
            assert call["Path"] == "/arenabench"
            assert call["WithDecryption"] is False, "values must never transit"


class TestResolveSut:
    def test_reads_the_per_ref_latest_manifest(self) -> None:
        commit = "a" * 40
        s3 = FakeS3(
            {
                "binaries/feature-x/latest.json": json.dumps(
                    {"git_ref": "feature/x", "commit": commit}
                ).encode()
            }
        )
        sut = _executor(s3=s3).resolve_sut("feature/x")
        assert sut.commit == commit
        assert sut.uri == f"s3://{BUCKET}/binaries/feature-x/{commit}/stella"

    def test_a_missing_ref_triggers_a_codebuild_build_first(self) -> None:
        """The build is started with the GIT_REF override the project
        contracts for, polled to completion, then the manifest is re-read."""
        commit = "b" * 40
        s3 = FakeS3()
        codebuild = FakeCodeBuild(["IN_PROGRESS", "SUCCEEDED"])
        slept: list[float] = []

        def sleep(seconds: float) -> None:
            slept.append(seconds)
            # The build "finishes": its artifact appears in S3.
            s3.objects["binaries/main/latest.json"] = json.dumps(
                {"git_ref": "main", "commit": commit}
            ).encode()

        sut = _executor(s3=s3, codebuild=codebuild).resolve_sut(
            "main", sleep=sleep, out=lambda _line: None
        )
        assert sut.commit == commit
        (started,) = codebuild.started
        assert started["projectName"] == "arenabench-sut-build"
        assert started["environmentVariablesOverride"] == [
            {"name": "GIT_REF", "value": "main", "type": "PLAINTEXT"}
        ]
        assert slept, "polling must sleep between sweeps, not busy-wait"

    def test_a_failed_build_is_an_error_not_a_submission(self) -> None:
        codebuild = FakeCodeBuild(["FAILED"])
        with pytest.raises(CloudError, match="FAILED"):
            _executor(s3=FakeS3(), codebuild=codebuild).resolve_sut(
                "main", sleep=lambda _s: None, out=lambda _line: None
            )


class TestSubmitContract:
    """The centerpiece witness: what actually goes over the wire per trial."""

    def _submit(self, spec: MatchSpec, queue: str = MEASURE_QUEUE):
        s3, batch = FakeS3(), FakeBatch()
        executor = _executor(s3=s3, batch=batch)
        jobs = executor.submit_run(
            spec, plan_trials(spec), run_id="r1", queue=queue, sut=_sut()
        )
        return s3, batch, jobs

    def test_one_job_per_trial_with_the_entrypoint_env_contract(self) -> None:
        s3, batch, jobs = self._submit(_spec())
        assert len(batch.submitted) == 4  # 2 tasks x 2 seats
        match_uris = set()
        for submitted in batch.submitted:
            assert submitted["jobQueue"] == "arenabench-measure"
            assert submitted["jobDefinition"] == JOB_DEFINITION == "arenabench-trial"
            overrides = submitted["containerOverrides"]
            assert overrides["command"] == ["run"]
            env = {e["name"]: e["value"] for e in overrides["environment"]}
            assert set(env) == {"RUN_ID", "MATCH_S3_URI", "SUT_S3_URI", "SUT_COMMIT"}
            assert env["RUN_ID"] == "r1"
            assert env["MATCH_S3_URI"].startswith(f"s3://{BUCKET}/runs/r1/trials/")
            match_uris.add(env["MATCH_S3_URI"])
            resources = {
                r["type"]: r["value"] for r in overrides["resourceRequirements"]
            }
            assert resources == {"VCPU": "4", "MEMORY": "15360"}
        assert len(match_uris) == 4, "every trial must get its own slice"

    def test_uploads_the_match_the_slices_and_the_submission_record(self) -> None:
        s3, _batch, jobs = self._submit(_spec())
        assert "runs/r1/match.toml" in s3.objects
        record = json.loads(s3.objects["runs/r1/jobs.json"])
        assert record["queue"] == "arenabench-measure"
        assert record["sut"]["commit"] == "c" * 40
        assert [j["id"] for j in record["jobs"]] == [j["id"] for j in jobs]

    def test_each_slice_parses_back_to_one_cell(self) -> None:
        spec = _spec()
        s3, batch, _jobs = self._submit(spec)
        env = {
            e["name"]: e["value"]
            for e in batch.submitted[0]["containerOverrides"]["environment"]
        }
        key = env["MATCH_S3_URI"].removeprefix(f"s3://{BUCKET}/")
        sliced = match_from_toml(tomllib.loads(s3.objects[key].decode()))
        assert len(sliced.tasks) == 1
        assert len(sliced.contestants) == 1
        assert sliced.attempts == 1
        assert sliced.concurrency == 1

    def test_burst_submissions_go_to_the_spot_queue(self) -> None:
        _s3, batch, _jobs = self._submit(_spec(), queue=select_queue(True))
        assert all(s["jobQueue"] == "arenabench-burst" for s in batch.submitted)


class TestJobStates:
    def test_describes_in_chunks_of_100(self) -> None:
        """A 144-trial run cannot fit one describe_jobs call; the sweep must
        chunk, and every id must be covered exactly once."""
        batch = FakeBatch([{}])
        ids = [f"job-{i:03d}" for i in range(144)]
        states = _executor(batch=batch).job_states(ids)
        assert len(states) == 144
        assert len(batch.describe_calls) == 2
        assert all(len(chunk) <= 100 for chunk in batch.describe_calls)


class TestWatch:
    def test_streams_transitions_and_surfaces_the_queued_backlog(self) -> None:
        jobs = [
            {"id": "job-001", "label": "000-stella-task-a"},
            {"id": "job-002", "label": "001-cc-task-a"},
            {"id": "job-003", "label": "002-stella-task-b"},
        ]
        script = [
            {"job-001": "RUNNABLE", "job-002": "RUNNABLE", "job-003": "RUNNABLE"},
            {"job-001": "RUNNING", "job-002": "RUNNING", "job-003": "RUNNABLE"},
            {"job-001": "SUCCEEDED", "job-002": "SUCCEEDED", "job-003": "SUCCEEDED"},
        ]
        batch = FakeBatch(script)
        lines: list[str] = []
        slept: list[float] = []

        def sleep(seconds: float) -> None:
            slept.append(seconds)
            batch.advance()

        ticks = iter(range(0, 1000, 10))
        states = _executor(batch=batch).watch(
            jobs,
            poll_interval=7.5,
            sleep=sleep,
            clock=lambda: float(next(ticks)),
            out=lines.append,
        )
        assert set(states.values()) == {"SUCCEEDED"}
        assert slept == [7.5, 7.5], "one sleep per sweep, none after settling"
        text = "\n".join(lines)
        assert "queued   3" in text
        assert "Queued is progress, not a hang" in text
        assert text.count("not a hang") == 1, "the quota note prints once"
        assert "000-stella-task-a: RUNNABLE -> RUNNING" in text
        assert "002-stella-task-b: RUNNABLE -> SUCCEEDED" in text


class TestRunRows:
    def test_reads_the_runner_status_items_across_pages(self) -> None:
        pages = [
            {
                "Items": [
                    {
                        "PK": {"S": "RUN#r1"},
                        "SK": {"S": "JOB#job-001"},
                        "status": {"S": "done"},
                        "detail": {"S": "exit 0"},
                    }
                ],
                "LastEvaluatedKey": {"PK": {"S": "RUN#r1"}},
            },
            {
                "Items": [
                    {
                        "PK": {"S": "RUN#r1"},
                        "SK": {"S": "JOB#job-002"},
                        "status": {"S": "failed"},
                        "detail": {"S": "exit 1"},
                    }
                ]
            },
        ]
        dynamo = FakeDynamo(pages)
        rows = _executor(dynamodb=dynamo).run_rows("r1")
        assert rows == [
            {"job": "job-001", "status": "done", "detail": "exit 0"},
            {"job": "job-002", "status": "failed", "detail": "exit 1"},
        ]
        first = dynamo.calls[0]
        assert first["TableName"] == "arenabench"
        assert first["ExpressionAttributeValues"] == {":pk": {"S": "RUN#r1"}}


class TestFetchResults:
    def test_downloads_each_trials_results_json(self, tmp_path) -> None:
        s3 = FakeS3({"runs/r1/job-001/results.json": b'{"ok": true}'})
        jobs = [
            {"id": "job-001", "label": "000-stella-task-a"},
            {"id": "job-002", "label": "001-cc-task-a"},
        ]
        notes: list[str] = []
        fetched = _executor(s3=s3).fetch_results(
            "r1", jobs, tmp_path, out=notes.append
        )
        assert fetched == 1
        assert (tmp_path / "000-stella-task-a" / "results.json").read_bytes() == (
            b'{"ok": true}'
        )
        assert any("001-cc-task-a" in note for note in notes), (
            "a missing results.json must be said out loud, not skipped silently"
        )


# --------------------------------------------------------------------------
# the CLI path
# --------------------------------------------------------------------------

TEMPLATE = """\
[match]
name = "cloud smoke"
dataset = "terminal-bench-2.1"
tasks = ["task-a"]

[[contestant]]
id = "stella"
agent = "stella"

  [contestant.engine]
  api = "openrouter"
  model = "z-ai/glm-5.2"

  [contestant.env]
  required = ["OPENROUTER_API_KEY"]
"""


def _run_args(template, **overrides) -> argparse.Namespace:
    defaults = dict(
        template=str(template),
        ref=None,
        run_id="r-test",
        burst=False,
        poll=0.0,
        vcpus=4,
        memory_mb=15360,
        no_wait=True,
        no_fetch=True,
        artifacts=False,
        out=None,
        region=None,
        bucket=None,
    )
    defaults.update(overrides)
    return argparse.Namespace(**defaults)


class TestCloudRunCommand:
    def test_a_seat_without_ssm_credentials_is_refused_before_any_submit(
        self, tmp_path, capsys
    ) -> None:
        """The refusal half of the witness: nothing is uploaded and no job is
        submitted when a seat's declared credentials will not exist in the
        trial environment (#1827)."""
        template = tmp_path / "match.toml"
        template.write_text(TEMPLATE, encoding="utf-8")
        s3, batch = FakeS3(), FakeBatch()
        executor = _executor(s3=s3, batch=batch, ssm=FakeSsm([]))

        rc = _cmd_cloud_run(_run_args(template), executor=executor)

        assert rc == 2
        assert batch.submitted == [], "a refused match must submit nothing"
        assert s3.calls == [], "a refused match must upload nothing"
        err = capsys.readouterr().err
        assert "OPENROUTER_API_KEY" in err
        assert "refusing to submit" in err

    def test_a_credentialled_match_submits_with_the_matchs_own_ref(
        self, tmp_path, capsys, monkeypatch
    ) -> None:
        """End to end through the CLI glue: SSM credentials present, the SUT
        resolved from the match's default ref (main), one job per trial."""
        monkeypatch.delenv("ARENABENCH_STELLA_REPO", raising=False)
        commit = "d" * 40
        template = tmp_path / "match.toml"
        template.write_text(TEMPLATE, encoding="utf-8")
        s3 = FakeS3(
            {
                "binaries/main/latest.json": json.dumps(
                    {"git_ref": "main", "commit": commit}
                ).encode()
            }
        )
        batch = FakeBatch()
        executor = _executor(
            s3=s3, batch=batch, ssm=FakeSsm(["/arenabench/openrouter_api_key"])
        )

        rc = _cmd_cloud_run(_run_args(template), executor=executor)

        assert rc == 0
        (submitted,) = batch.submitted
        env = {
            e["name"]: e["value"]
            for e in submitted["containerOverrides"]["environment"]
        }
        assert env["SUT_COMMIT"] == commit
        assert env["RUN_ID"] == "r-test"
        assert "runs/r-test/jobs.json" in s3.objects
        assert "cloud status r-test" in capsys.readouterr().out
