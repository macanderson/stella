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
from dataclasses import replace
from types import SimpleNamespace

import pytest

from arenabench.cloud import (
    BURST_QUEUE,
    JOB_DEFINITION,
    MEASURE_QUEUE,
    CloudError,
    CloudExecutor,
    _cmd_cloud_fetch,
    _cmd_cloud_merge,
    _cmd_cloud_run,
    is_moving_ref,
    job_name,
    pin_slice_to_commit,
    plan_trials,
    progress_from_states,
    ref_safe,
    refused_seats,
    select_queue,
    slice_spec,
    ssm_env_name,
    sut_cache_is_current,
    sut_seats,
    tip_from_ls_remote,
    trial_environment,
)
from arenabench.config import match_from_toml
from arenabench.model import Contestant, Engine, MatchSpec

ACCOUNT = "123456789012"
BUCKET = f"arenabench-artifacts-{ACCOUNT}"
#: What ``infra/core.yaml``'s ``SutGitUrl`` defaults to, as the SUT build
#: project reports it back.
GIT_URL = "https://github.com/macanderson/stella.git"


# --------------------------------------------------------------------------
# recorded-call fakes — small on purpose; moto would be a heavyweight
# dependency for what is a handful of parameter-shape assertions
# --------------------------------------------------------------------------


class _NoSuchKeyError(Exception):
    pass


class FakeS3:
    def __init__(self, objects: dict[str, bytes] | None = None) -> None:
        self.objects: dict[str, bytes] = dict(objects or {})
        self.calls: list[tuple] = []
        # boto3 spells it NoSuchKey; the adapter catches it by that name.
        self.exceptions = SimpleNamespace(NoSuchKey=_NoSuchKeyError)

    # The keyword casing is the AWS SDK's, not PEP 8's: the adapter calls
    # with Bucket=/Key=/Body=, so the fakes must accept those spellings.
    def put_object(self, *, Bucket: str, Key: str, Body: bytes, **_: object) -> dict:  # noqa: N803
        self.calls.append(("put_object", Bucket, Key))
        self.objects[Key] = Body
        return {}

    def get_object(self, *, Bucket: str, Key: str) -> dict:  # noqa: N803
        self.calls.append(("get_object", Bucket, Key))
        if Key not in self.objects:
            raise _NoSuchKeyError(Key)
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
    def __init__(self, statuses: list[str] | None = None) -> None:
        self.statuses = list(statuses or ["SUCCEEDED"])
        self.started: list[dict] = []
        self.project_calls: list[list[str]] = []
        self.polls = 0

    def start_build(self, **kwargs: object) -> dict:
        self.started.append(dict(kwargs))
        return {"build": {"id": "arenabench-sut-build:1234"}}

    def batch_get_builds(self, *, ids: list[str]) -> dict:
        status = self.statuses[min(self.polls, len(self.statuses) - 1)]
        self.polls += 1
        return {"builds": [{"id": ids[0], "buildStatus": status}]}

    def batch_get_projects(self, *, names: list[str]) -> dict:
        """The project's own ``GIT_URL`` — the remote a freshness check must
        consult, since it is the one the buildspec clones."""
        self.project_calls.append(list(names))
        return {
            "projects": [
                {
                    "name": names[0],
                    "environment": {
                        "environmentVariables": [
                            {"name": "GIT_URL", "value": GIT_URL},
                            {"name": "GIT_REF", "value": "main"},
                        ]
                    },
                }
            ]
        }


class FakeLsRemote:
    """``git ls-remote`` as a recorded call over a fixed remote ref table."""

    def __init__(self, refs: dict[str, str] | None = None) -> None:
        self.refs = dict(refs or {})
        self.calls: list[tuple[str, list[str]]] = []

    def __call__(self, url: str, patterns) -> str:
        patterns = list(patterns)
        self.calls.append((url, patterns))
        return "".join(
            f"{self.refs[p]}\t{p}\n" for p in patterns if p in self.refs
        )


def _refuse_ls_remote(url: str, patterns) -> str:
    """The seam a test uses to prove the remote is never consulted."""
    raise AssertionError(f"git ls-remote must not run: {url} {list(patterns)}")


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


def _executor(ls_remote=None, **clients: object) -> CloudExecutor:
    defaults: dict[str, object] = {"sts": FakeSts()}
    defaults.update(clients)
    return CloudExecutor(
        clients=defaults,
        ls_remote=ls_remote if ls_remote is not None else _refuse_ls_remote,
    )


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


class TestPinSliceToCommit:
    """A slice must name a commit, never a branch.

    The runner image stages a binary and carries no Stella checkout, so a
    slice still saying ``sut_ref = "main"`` asks the container to resolve a
    ref it cannot, and ``create_match`` refuses the seat. Every trial of
    match ctl-0808-0014 died exactly there, after credentials and staging
    had already succeeded.
    """

    def test_the_match_and_its_stella_seats_carry_the_resolved_commit(self) -> None:
        pinned = pin_slice_to_commit(_spec(), _sut())
        assert pinned.sut_ref == "c" * 40
        stella = next(c for c in pinned.contestants if c.agent == "stella")
        assert stella.sut_ref == "c" * 40

    def test_a_non_stella_seat_is_left_alone(self) -> None:
        """A SUT pin means nothing to Claude Code, and `config` rejects one."""
        pinned = pin_slice_to_commit(_spec(), _sut())
        cc = next(c for c in pinned.contestants if c.agent == "claude-code")
        assert cc.sut_ref is None

    def test_a_seat_pinned_to_its_own_ref_keeps_it(self) -> None:
        """Per-seat pins (#2082) are the point of two Stella builds racing:
        substituting the match-level resolution would collapse them into
        one, silently measuring the same commit twice."""
        own = replace(_seat("stella-b"), sut_ref="deadbeef")
        spec = _spec(seats=(_seat("stella"), own))
        pinned = pin_slice_to_commit(spec, _sut())
        assert next(c for c in pinned.contestants if c.id == "stella-b").sut_ref == (
            "deadbeef"
        )

    def test_an_unpinned_match_is_untouched(self) -> None:
        """No staged binary means nothing was resolved to substitute."""
        spec = _spec()
        assert pin_slice_to_commit(spec, None) is spec

    def test_the_commit_survives_the_toml_round_trip(self) -> None:
        """`dump_match` writes the slice the container parses back; a pin
        that does not round-trip is the same failure wearing a new hat."""
        from arenabench.config import dump_match, match_from_toml

        pinned = pin_slice_to_commit(_spec(), _sut())
        reparsed = match_from_toml(tomllib.loads(dump_match(pinned)))
        assert reparsed.sut_ref == "c" * 40
        stella = next(c for c in reparsed.contestants if c.agent == "stella")
        assert stella.sut_ref == "c" * 40


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


class TestRefMutability:
    """A ref's mutability is a property of its *shape*, and the freshness
    decision is a pure function of three strings (#2388)."""

    def test_only_a_full_sha_is_immutable(self) -> None:
        assert not is_moving_ref("a" * 40)
        assert is_moving_ref("main")
        assert is_moving_ref("feature/x")
        assert is_moving_ref("v2.1"), "a tag can be force-moved; treat it as moving"
        assert is_moving_ref("a" * 12), "an abbreviated sha is not the pinned shape"

    def test_a_moving_ref_reuses_the_cache_only_at_the_remote_tip(self) -> None:
        tip, stale = "a" * 40, "b" * 40
        assert sut_cache_is_current("main", tip, tip)
        assert not sut_cache_is_current("main", stale, tip)
        assert not sut_cache_is_current("main", "", tip)
        assert not sut_cache_is_current("main", stale, ""), (
            "an unresolved tip must never read as agreement"
        )

    def test_a_full_sha_needs_no_tip_at_all(self) -> None:
        sha = "c" * 40
        assert sut_cache_is_current(sha, sha, "")
        assert not sut_cache_is_current(sha, "", "")


class TestTipFromLsRemote:
    def test_a_branch_wins_over_a_like_named_tag(self) -> None:
        """`git checkout <ref>` in the buildspec's fresh clone prefers the
        branch, so the freshness check must resolve the same way."""
        output = (
            f"{'a' * 40}\trefs/heads/release\n"
            f"{'b' * 40}\trefs/tags/release\n"
        )
        assert tip_from_ls_remote(output, "release") == "a" * 40

    def test_an_annotated_tag_resolves_to_the_peeled_commit(self) -> None:
        """`ls-remote` reports the *tag object* for the bare pattern; the
        build's `git rev-parse HEAD` reports the commit it peels to. Comparing
        against the tag object would rebuild on every annotated tag forever."""
        output = (
            f"{'7' * 40}\trefs/tags/v2.1\n"
            f"{'9' * 40}\trefs/tags/v2.1^{{}}\n"
        )
        assert tip_from_ls_remote(output, "v2.1") == "9" * 40

    def test_a_lightweight_tag_falls_back_to_the_unpeeled_line(self) -> None:
        assert tip_from_ls_remote(f"{'4' * 40}\trefs/tags/light\n", "light") == "4" * 40

    def test_no_match_is_the_empty_string_not_a_guess(self) -> None:
        """`ls-remote` exits 0 with no output when nothing matches."""
        assert tip_from_ls_remote("", "main") == ""
        assert tip_from_ls_remote(f"{'a' * 40}\trefs/heads/other\n", "main") == ""


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
        ls_remote = FakeLsRemote({"refs/heads/feature/x": commit})
        sut = _executor(
            s3=s3, codebuild=FakeCodeBuild(), ls_remote=ls_remote
        ).resolve_sut("feature/x")
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

        sut = _executor(
            s3=s3,
            codebuild=codebuild,
            ls_remote=FakeLsRemote({"refs/heads/main": commit}),
        ).resolve_sut("main", sleep=sleep, out=lambda _line: None)
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
            _executor(
                s3=FakeS3(),
                codebuild=codebuild,
                ls_remote=FakeLsRemote({"refs/heads/main": "e" * 40}),
            ).resolve_sut("main", sleep=lambda _s: None, out=lambda _line: None)


class TestResolveSutFreshness:
    """#2388: `--ref main` must measure what `main` points at *now*.

    Before this, `resolve_sut` started a build only when
    `binaries/<ref>/latest.json` was absent — so a moving ref reused whichever
    artifact happened to be there, forever, and recorded a real commit that
    was simply not the one asked for.
    """

    @staticmethod
    def _cached(ref_safe_name: str, commit: str) -> FakeS3:
        return FakeS3(
            {
                f"binaries/{ref_safe_name}/latest.json": json.dumps(
                    {"git_ref": ref_safe_name, "commit": commit}
                ).encode()
            }
        )

    def test_a_moving_ref_behind_the_remote_tip_rebuilds(self) -> None:
        stale, tip = "9" * 40, "4" * 40
        s3 = self._cached("main", stale)
        codebuild = FakeCodeBuild(["SUCCEEDED"])
        ls_remote = FakeLsRemote({"refs/heads/main": tip})

        def start_build(**kwargs: object) -> dict:
            # The build lands the *current* tip's artifact.
            s3.objects["binaries/main/latest.json"] = json.dumps(
                {"git_ref": "main", "commit": tip}
            ).encode()
            return FakeCodeBuild.start_build(codebuild, **kwargs)

        codebuild.start_build = start_build  # type: ignore[method-assign]
        lines: list[str] = []
        sut = _executor(s3=s3, codebuild=codebuild, ls_remote=ls_remote).resolve_sut(
            "main", sleep=lambda _s: None, out=lines.append
        )

        assert sut.commit == tip, "a stale cache must not be reused"
        assert codebuild.started, "a moving ref behind its remote must rebuild"
        assert codebuild.started[0]["environmentVariablesOverride"] == [
            {"name": "GIT_REF", "value": "main", "type": "PLAINTEXT"}
        ]
        text = "\n".join(lines)
        assert tip[:12] in text and stale[:12] in text, (
            "the rebuild must say which commit it is leaving behind"
        )

    def test_a_moving_ref_at_the_remote_tip_reuses_the_cache(self) -> None:
        tip = "4" * 40
        s3 = self._cached("main", tip)
        codebuild = FakeCodeBuild(["SUCCEEDED"])
        sut = _executor(
            s3=s3, codebuild=codebuild, ls_remote=FakeLsRemote({"refs/heads/main": tip})
        ).resolve_sut("main", sleep=lambda _s: None, out=lambda _line: None)
        assert sut.commit == tip
        assert codebuild.started == [], "an up-to-date artifact must not rebuild"

    def test_a_full_sha_never_consults_the_remote(self) -> None:
        """The other half of the contract: a pinned run is offline and free.
        `_refuse_ls_remote` is the default seam, so reaching the remote here
        raises rather than quietly passing."""
        sha = "c" * 40
        s3 = self._cached(sha, sha)
        codebuild = FakeCodeBuild(["SUCCEEDED"])
        sut = _executor(s3=s3, codebuild=codebuild).resolve_sut(
            sha, sleep=lambda _s: None, out=lambda _line: None
        )
        assert sut.commit == sha
        assert codebuild.started == []
        assert codebuild.project_calls == [], "no GIT_URL lookup for a pinned ref"

    def test_the_remote_is_the_build_projects_own_git_url(self) -> None:
        """Not a constant in `cloud.py`: `SutGitUrl` is a stack parameter, and
        checking a different remote than the buildspec clones answers a
        question nobody asked."""
        tip = "4" * 40
        ls_remote = FakeLsRemote({"refs/heads/main": tip})
        codebuild = FakeCodeBuild(["SUCCEEDED"])
        _executor(
            s3=self._cached("main", tip), codebuild=codebuild, ls_remote=ls_remote
        ).resolve_sut("main", sleep=lambda _s: None, out=lambda _line: None)
        (url, patterns) = ls_remote.calls[0]
        assert url == GIT_URL
        assert codebuild.project_calls == [["arenabench-sut-build"]]
        assert patterns == [
            "refs/heads/main",
            "refs/tags/main^{}",
            "refs/tags/main",
        ], "branch first, then the peeled tag — the buildspec's own precedence"

    def test_the_git_url_is_read_once_per_executor(self) -> None:
        tip = "4" * 40
        codebuild = FakeCodeBuild(["SUCCEEDED"])
        executor = _executor(
            s3=self._cached("main", tip),
            codebuild=codebuild,
            ls_remote=FakeLsRemote({"refs/heads/main": tip}),
        )
        for _ in range(3):
            executor.resolve_sut("main", sleep=lambda _s: None, out=lambda _l: None)
        assert len(codebuild.project_calls) == 1

    def test_a_ref_absent_from_the_remote_stops_the_submission(self) -> None:
        """`ls-remote` exits 0 with no output for a ref that does not exist,
        so the miss has to be raised here or it reads as "unchanged"."""
        with pytest.raises(CloudError, match="no ref 'nope'"):
            _executor(
                s3=self._cached("nope", "9" * 40),
                codebuild=FakeCodeBuild(["SUCCEEDED"]),
                ls_remote=FakeLsRemote({"refs/heads/main": "4" * 40}),
            ).resolve_sut("nope", sleep=lambda _s: None, out=lambda _l: None)

    def test_an_unreachable_remote_is_an_error_not_a_stale_reuse(self) -> None:
        """Failing closed is the point: the alternative is publishing a number
        measured against a tree nobody chose."""

        def offline(url: str, patterns) -> str:
            raise CloudError("git ls-remote https://... failed: no route to host")

        with pytest.raises(CloudError, match="no route to host"):
            _executor(
                s3=self._cached("main", "9" * 40),
                codebuild=FakeCodeBuild(["SUCCEEDED"]),
                ls_remote=offline,
            ).resolve_sut("main", sleep=lambda _s: None, out=lambda _l: None)

    def test_a_manifest_disagreeing_with_a_pinned_sha_is_refused(self) -> None:
        """`git checkout <sha>` can only rev-parse to <sha>, so a manifest
        under `binaries/<sha>/` naming anything else means the artifact's
        identity is unknown — which is exactly what must never be measured."""
        sha, other = "c" * 40, "d" * 40
        with pytest.raises(CloudError, match="identity is in doubt"):
            _executor(
                s3=self._cached(sha, other), codebuild=FakeCodeBuild(["SUCCEEDED"])
            ).resolve_sut(sha, sleep=lambda _s: None, out=lambda _l: None)

    def test_build_missing_off_refuses_a_stale_cache_too(self) -> None:
        """`build_missing=False` means "do not build", never "run whatever is
        cached": the stale artifact is still the wrong answer."""
        stale, tip = "9" * 40, "4" * 40
        with pytest.raises(CloudError, match="no current prebuilt SUT"):
            _executor(
                s3=self._cached("main", stale),
                codebuild=FakeCodeBuild(["SUCCEEDED"]),
                ls_remote=FakeLsRemote({"refs/heads/main": tip}),
            ).resolve_sut("main", build_missing=False)


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
        _s3, batch, _jobs = self._submit(_spec())
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


def _single_task_result(task: str, contestant_id: str, reward: float) -> bytes:
    body = {
        "match": {
            "id": f"{contestant_id}-{task}",
            "name": "smoke",
            "dataset": "terminal-bench-2.1",
            "tasks": [task],
            "contestants": [{"id": contestant_id, "name": contestant_id}],
        },
        "dataset": {"key": "terminal-bench-2.1"},
        "rows": [
            {
                "task": task,
                "cells": {
                    contestant_id: {
                        "task": task,
                        "status": "done",
                        "reward": reward,
                        "resolved": reward >= 1.0,
                        "infrastructure": False,
                        "tokens_in": 10,
                        "tokens_out": 10,
                        "clock_time": 1.0,
                        "total_cost": 0.01,
                        "priced_cost": 0.01,
                        "cache_read": 0,
                        "cache_write": 0,
                    }
                },
            }
        ],
    }
    return json.dumps(body).encode()


class TestFetchMergesIntoOneTrial:
    """The offline half of the importer bug: a fetch that used to leave N
    disconnected single-task files now also writes one trial.json spanning
    every task both arms ran — proven by driving the real CLI verb, not just
    the pure merge function underneath it.
    """

    def test_cloud_fetch_writes_a_merged_trial_alongside_the_flat_files(
        self, tmp_path
    ) -> None:
        s3 = FakeS3(
            {
                "runs/r1/job-stella/results.json": _single_task_result(
                    "task-a", "stella", 1.0
                ),
                "runs/r1/job-cc/results.json": _single_task_result(
                    "task-a", "claude-code", 0.0
                ),
            }
        )
        dynamo = FakeDynamo([{"Items": []}])
        jobs = [
            {"id": "job-stella", "label": "000-stella-task-a"},
            {"id": "job-cc", "label": "001-cc-task-a"},
        ]
        executor = _executor(s3=s3, dynamodb=dynamo)
        executor.load_jobs = lambda run_id: {"jobs": jobs}  # type: ignore[method-assign]

        args = argparse.Namespace(
            run_id="r1", artifacts=False, out=str(tmp_path), region=None, bucket=None
        )
        rc = _cmd_cloud_fetch(args, executor=executor)

        assert rc == 0
        trial = json.loads((tmp_path / "trial.json").read_text())
        assert trial["match"]["tasks"] == ["task-a"]
        assert {c["id"] for c in trial["match"]["contestants"]} == {
            "stella",
            "claude-code",
        }
        # The bug this fixes: previously only the two flat per-job files
        # existed, with no view spanning both arms of task-a.
        assert (tmp_path / "000-stella-task-a" / "results.json").exists()
        assert (tmp_path / "001-cc-task-a" / "results.json").exists()


class TestCloudMerge:
    """`arenabench cloud merge` — pairing arms that were fetched separately."""

    def test_merges_two_already_fetched_destinations(self, tmp_path) -> None:
        arm_a = tmp_path / "run-a"
        arm_b = tmp_path / "run-b"
        out = tmp_path / "merged"
        (arm_a / "000-stella-task-a").mkdir(parents=True)
        (arm_a / "000-stella-task-a" / "results.json").write_bytes(
            _single_task_result("task-a", "stella", 1.0)
        )
        (arm_b / "000-cc-task-a").mkdir(parents=True)
        (arm_b / "000-cc-task-a" / "results.json").write_bytes(
            _single_task_result("task-a", "claude-code", 0.0)
        )

        args = argparse.Namespace(dest=[str(arm_a), str(arm_b)], out=str(out))
        rc = _cmd_cloud_merge(args)

        assert rc == 0
        trial = json.loads((out / "trial.json").read_text())
        assert trial["match"]["tasks"] == ["task-a"]
        assert {c["id"] for c in trial["match"]["contestants"]} == {
            "stella",
            "claude-code",
        }

    def test_no_results_anywhere_is_reported_not_a_silent_no_op(
        self, tmp_path, capsys
    ) -> None:
        args = argparse.Namespace(dest=[str(tmp_path)], out=str(tmp_path / "out"))
        rc = _cmd_cloud_merge(args)
        assert rc == 1
        assert "no results.json found" in capsys.readouterr().out


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
        # These tests are about submission, not about the banned-behavior
        # gate, so they take the explicit waiver — `_cmd_cloud_run` refuses to
        # start a run that is neither gated nor deliberately ungated, and that
        # refusal has its own witness in tests/test_gate.py.
        gate=None,
        no_gate=True,
        gate_allow=None,
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
            s3=s3,
            batch=batch,
            ssm=FakeSsm(["/arenabench/openrouter_api_key"]),
            codebuild=FakeCodeBuild(),
            ls_remote=FakeLsRemote({"refs/heads/main": commit}),
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
        out = capsys.readouterr().out
        assert "cloud status r-test" in out
        assert f"sut       : main -> {commit}" in out, (
            "submit-time output must name ref -> commit, so a reader can see "
            "which commit 'main' meant on this run (#2388)"
        )
