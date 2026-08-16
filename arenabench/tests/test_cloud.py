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

The ``_cmd_cloud_*`` CLI path lives in ``test_cloud_commands.py`` and the
fakes both halves share live in ``cloud_fakes.py``.
"""

from __future__ import annotations

import argparse
import json
import tomllib
from dataclasses import replace

import pytest
from cloud_fakes import (
    BUCKET,
    GIT_URL,
    FakeBatch,
    FakeCodeBuild,
    FakeDynamo,
    FakeLsRemote,
    FakeS3,
    FakeSsm,
    _executor,
    _seat,
    _spec,
    _sut,
)

from arenabench.cloud import (
    BURST_QUEUE,
    JOB_DEFINITION,
    MEASURE_QUEUE,
    CloudError,
    _cmd_cloud_merge,
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
from arenabench.cloud_ops import _cmd_cloud_fetch
from arenabench.config import match_from_toml
from arenabench.model import MatchSpec

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
        executor = _executor(s3=s3, dynamodb=dynamo, batch=FakeBatch())
        executor.load_jobs = lambda run_id: {"jobs": jobs}  # type: ignore[method-assign]

        args = argparse.Namespace(
            run_id="r1", artifacts=False, out=str(tmp_path), region=None,
            bucket=None, partial=False,
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

    def test_an_assembled_run_is_not_also_merged(self, tmp_path, monkeypatch) -> None:
        """The ordering half: the merge is the fallback, never a second write.

        A default fetch downloads only each job's ``results.json``, so
        ``match.toml`` is absent and assembly declines — which is why the
        merge above still has to run. When the spec *is* on disk the assembled
        match is the restoration of the submitted one and strictly better, so
        the merge must not also fire and leave a rival ``trial.json`` beside
        it claiming to describe the same run.
        """
        import arenabench.assemble as assemble_mod

        assembled = assemble_mod.Assembly(
            match_id="r1",
            match_dir=tmp_path / "assembled" / "r1",
            tasks=("task-a",),
            linked={"stella": 1, "claude-code": 1},
        )
        monkeypatch.setattr(
            assemble_mod, "assemble_run", lambda run_dir, **kw: assembled
        )

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
        executor = _executor(
            s3=s3, dynamodb=FakeDynamo([{"Items": []}]), batch=FakeBatch()
        )
        executor.load_jobs = lambda run_id: {  # type: ignore[method-assign]
            "jobs": [
                {"id": "job-stella", "label": "000-stella-task-a"},
                {"id": "job-cc", "label": "001-cc-task-a"},
            ]
        }

        args = argparse.Namespace(
            run_id="r1", artifacts=True, out=str(tmp_path), region=None,
            bucket=None, partial=False,
        )
        rc = _cmd_cloud_fetch(args, executor=executor)

        assert rc == 0
        assert not (tmp_path / "trial.json").exists(), (
            "an assembled run must not also be merged — two files describing "
            "one run is the ambiguity assembly exists to remove"
        )


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
