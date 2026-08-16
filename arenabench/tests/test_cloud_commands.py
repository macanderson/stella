# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The ``_cmd_cloud_*`` CLI path — what a run refuses before it spends.

Split out of ``test_cloud.py`` when that file crossed the tree's 1500-line
ceiling. It keeps the submit contract and the pure decisions; this one keeps
the command surface: what ``cloud run`` validates before anything is
uploaded or submitted (the task-id preflight), and what ``cloud
fetch``/``cloud status``/``cloud cancel`` do once jobs exist. The fakes both
halves build on live in ``cloud_fakes.py``.
"""

from __future__ import annotations

import argparse
import json
from types import SimpleNamespace

import pytest
from cloud_fakes import (
    TEMPLATE,
    FakeBatch,
    FakeCodeBuild,
    FakeDynamo,
    FakeLsRemote,
    FakeS3,
    FakeSsm,
    _executor,
    _run_args,
)

from arenabench.cloud import _cmd_cloud_run
from arenabench.cloud_ops import _cmd_cloud_cancel, _cmd_cloud_fetch


class TestCloudRunCommand:
    @pytest.fixture(autouse=True)
    def _no_local_dataset(self, monkeypatch):
        """Task-id validation (#3208) reads the machine's dataset registry.

        Pinned to an empty registry so these tests behave identically on a
        machine with terminal-bench materialised and on CI, where it is not:
        an unfetched dataset is a declared validation gap that submits, and
        that is the neutral posture every non-#3208 test here wants.
        """
        import arenabench.registry as registry_mod

        monkeypatch.setattr(
            registry_mod, "DEFAULT_REGISTRY", registry_mod.Registry()
        )

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

        rc = _cmd_cloud_run(
            _run_args(template),
            executor=executor,
            task_lister=lambda _dataset: ["task-a"],
        )

        assert rc == 2
        assert batch.submitted == [], "a refused match must submit nothing"
        assert s3.calls == [], "a refused match must upload nothing"
        err = capsys.readouterr().err
        assert "OPENROUTER_API_KEY" in err
        assert "refusing to submit" in err

    def test_a_task_the_dataset_does_not_contain_is_refused_before_any_submit(
        self, tmp_path, capsys
    ) -> None:
        """The refusal half of #3255's witness: gate89high1, fulleightynine1
        and h2h891 each submitted a spec whose first sorted task was the
        literal count "89" (a task-listing capture's trailer), and each
        burned its index-000 container on it. With the dataset enumerable
        locally, the submission is refused before any upload or job."""
        template = tmp_path / "match.toml"
        template.write_text(
            TEMPLATE.replace('tasks = ["task-a"]', 'tasks = ["89", "task-a"]'),
            encoding="utf-8",
        )
        s3, batch = FakeS3(), FakeBatch()
        executor = _executor(s3=s3, batch=batch, ssm=FakeSsm([]))

        rc = _cmd_cloud_run(
            _run_args(template),
            executor=executor,
            task_lister=lambda _dataset: ["task-a", "task-b"],
        )

        assert rc == 2
        assert batch.submitted == [], "a refused match must submit nothing"
        assert s3.calls == [], "a refused match must upload nothing"
        err = capsys.readouterr().err
        assert "'89'" in err
        assert "does not contain" in err
        assert "task-a" not in err, "only the bogus names are refused"

    def test_an_unmaterialised_dataset_submits_but_says_it_was_not_checked(
        self, tmp_path, capsys, monkeypatch
    ) -> None:
        """The declared gap: with no local task tree to check against, the
        submission proceeds — refusing would make local materialisation a
        submit prerequisite — but the skipped validation is said out loud."""
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

        rc = _cmd_cloud_run(
            _run_args(template),
            executor=executor,
            task_lister=lambda _dataset: [],
        )

        assert rc == 0
        assert len(batch.submitted) == 1
        out = capsys.readouterr().out
        assert "task names not validated" in out

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

        rc = _cmd_cloud_run(
            _run_args(template),
            executor=executor,
            task_lister=lambda _dataset: ["task-a"],
        )

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


class TestSubmitTaskValidation:
    """#3208: a task id the dataset does not contain must not become a job.

    A phantom "89" pasted over `write-compressor` burned two Batch containers
    and silently shrank the "full 89" panel to 88 tasks.
    """

    def _registry_with(self, monkeypatch, names: list[str]) -> None:
        import arenabench.registry as registry_mod

        fake = SimpleNamespace(
            tasks=lambda key: [SimpleNamespace(name=n) for n in names]
        )
        monkeypatch.setattr(registry_mod, "DEFAULT_REGISTRY", fake)

    def test_an_unknown_task_id_refuses_before_any_upload_or_job(
        self, tmp_path, capsys, monkeypatch
    ) -> None:
        self._registry_with(monkeypatch, ["task-b", "write-compressor"])
        template = tmp_path / "match.toml"
        template.write_text(TEMPLATE, encoding="utf-8")
        s3, batch = FakeS3(), FakeBatch()
        executor = _executor(s3=s3, batch=batch, ssm=FakeSsm(["openrouter_api_key"]))

        rc = _cmd_cloud_run(_run_args(template), executor=executor)

        assert rc == 2
        assert batch.submitted == [], "a refused match must submit nothing"
        assert s3.calls == [], "a refused match must upload nothing"
        err = capsys.readouterr().err
        assert "task-a" in err, "the refusal must name the offending id"
        assert "refusing to submit" in err

    def test_a_known_task_list_submits_and_an_unfetched_dataset_says_so(
        self, tmp_path, capsys, monkeypatch
    ) -> None:
        """An empty registry is a validation gap, not a refusal — and the gap
        is said out loud rather than silently waved through."""
        self._registry_with(monkeypatch, [])
        template = tmp_path / "match.toml"
        template.write_text(TEMPLATE, encoding="utf-8")
        executor = _executor(
            s3=FakeS3(), batch=FakeBatch(), ssm=FakeSsm(["openrouter_api_key"])
        )

        # The run stops later, at the missing --ref — which is the point:
        # validation let it through and said why it could not check.
        rc = _cmd_cloud_run(_run_args(template, ref=""), executor=executor)

        captured = capsys.readouterr()
        assert "not validated" in captured.out
        assert rc == 2 and "Pass --ref" in captured.err


class TestCloudFetchGate:
    """#3218: a fetch of a live run is a silently short match without --partial."""

    def _executor_with_states(self, states: dict[str, str]):
        s3 = FakeS3({})
        executor = _executor(
            s3=s3, batch=FakeBatch([states]), dynamodb=FakeDynamo([{"Items": []}])
        )
        executor.load_jobs = lambda run_id: {  # type: ignore[method-assign]
            "run_id": run_id,
            "queue": "arenabench-measure",
            "jobs": [
                {"id": "job-1", "label": "000-stella-task-a", "task": "task-a"},
                {"id": "job-2", "label": "001-stella-task-b", "task": "task-b"},
            ],
        }
        return executor

    def test_a_live_run_is_refused_without_partial(self, tmp_path, capsys) -> None:
        executor = self._executor_with_states(
            {"job-1": "SUCCEEDED", "job-2": "RUNNING"}
        )
        args = argparse.Namespace(
            run_id="r1", artifacts=False, out=str(tmp_path), region=None,
            bucket=None, partial=False,
        )
        rc = _cmd_cloud_fetch(args, executor=executor)
        assert rc == 2
        err = capsys.readouterr().err
        assert "001-stella-task-b" in err and "RUNNING" in err
        assert "--partial" in err

    def test_partial_fetches_and_records_the_states_it_saw(self, tmp_path) -> None:
        executor = self._executor_with_states(
            {"job-1": "SUCCEEDED", "job-2": "RUNNING"}
        )
        args = argparse.Namespace(
            run_id="r1", artifacts=False, out=str(tmp_path), region=None,
            bucket=None, partial=True,
        )
        rc = _cmd_cloud_fetch(args, executor=executor)
        assert rc == 0
        sidecar = json.loads((tmp_path / "jobs.json").read_text())
        by_id = {j["id"]: j["state"] for j in sidecar["jobs"]}
        assert by_id == {"job-1": "SUCCEEDED", "job-2": "RUNNING"}

    def test_a_settled_run_fetches_without_ceremony(self, tmp_path) -> None:
        executor = self._executor_with_states(
            {"job-1": "SUCCEEDED", "job-2": "FAILED"}
        )
        args = argparse.Namespace(
            run_id="r1", artifacts=False, out=str(tmp_path), region=None,
            bucket=None, partial=False,
        )
        assert _cmd_cloud_fetch(args, executor=executor) == 0


class TestStatusReconciliation:
    """#3217: a ddb row an externally-killed trial never wrote must not read
    `running` when Batch says the job is terminal."""

    def test_the_killed_trial_shape_renders_terminal(self) -> None:
        from arenabench.cloud_ops import reconciled_status

        assert (
            reconciled_status("running", "FAILED") == "failed (killed; no checkin)"
        )
        assert (
            reconciled_status("running", "SUCCEEDED")
            == "succeeded (no terminal checkin)"
        )

    def test_everything_else_prints_the_row_as_written(self) -> None:
        from arenabench.cloud_ops import reconciled_status

        assert reconciled_status("running", "RUNNING") == "running"
        assert reconciled_status("running", None) == "running"
        assert reconciled_status("finished", "FAILED") == "finished"


class TestCloudCancelCommand:
    """#3257: `cloud cancel` reaches cancel_run — the safe, jobs.json-scoped
    stop — instead of leaving operators to hand-roll a name-prefix sweep."""

    def _args(self, run_id: str = "r1", reason: str | None = None):
        return argparse.Namespace(
            run_id=run_id, reason=reason, region=None, bucket=None
        )

    def test_a_run_with_no_live_jobs_is_a_clean_no_op(self, capsys) -> None:
        batch = FakeBatch([{"job-1": "SUCCEEDED", "job-2": "FAILED"}])
        executor = _executor(batch=batch)
        executor.load_jobs = lambda run_id: {  # type: ignore[method-assign]
            "jobs": [{"id": "job-1"}, {"id": "job-2"}]
        }
        rc = _cmd_cloud_cancel(self._args(), executor=executor)
        assert rc == 0
        out = capsys.readouterr().out
        assert "2 already settled" in out
        assert "0 terminated" in out and "0 cancelled" in out

    def test_live_jobs_are_signalled_and_the_split_is_printed(self, capsys) -> None:
        class _CancellingBatch(FakeBatch):
            def __init__(self, script):
                super().__init__(script)
                self.cancelled: list[str] = []
                self.terminated: list[str] = []

            def describe_jobs(self, *, jobs):
                # The real API drops ids it cannot resolve rather than
                # erroring; the base fake fabricates SUCCEEDED instead, which
                # is exactly the shape this test must not inherit.
                states = self.state_script[0] if self.state_script else {}
                return {
                    "jobs": [
                        {"jobId": j, "status": states[j]}
                        for j in jobs
                        if j in states
                    ]
                }

            def cancel_job(self, *, jobId, reason):  # noqa: N803 - Batch's spelling
                self.cancelled.append(jobId)

            def terminate_job(self, *, jobId, reason):  # noqa: N803 - Batch's spelling
                self.terminated.append(jobId)

        batch = _CancellingBatch([{"job-1": "RUNNING", "job-2": "RUNNABLE"}])
        executor = _executor(batch=batch)
        executor.load_jobs = lambda run_id: {  # type: ignore[method-assign]
            # job-3 is absent from describe_jobs: still cancelled, never
            # skipped — an id we own that a describe cannot resolve may still
            # be billing (the branch cancel.plan_cancellation takes on
            # purpose).
            "jobs": [{"id": "job-1"}, {"id": "job-2"}, {"id": "job-3"}]
        }
        rc = _cmd_cloud_cancel(self._args(reason="smoke"), executor=executor)
        assert rc == 0
        assert batch.terminated == ["job-1"]
        assert set(batch.cancelled) == {"job-2", "job-3"}
        out = capsys.readouterr().out
        assert "1 terminated" in out and "2 cancelled" in out
