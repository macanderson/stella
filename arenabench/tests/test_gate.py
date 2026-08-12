# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The banned-behavior gate: its exit contract, its arming, and its hook.

Every test here is hermetic. The gate command is either a fake ``runner``
callable or a two-line ``python3 -c`` script written into ``tmp_path`` — never
a real checker, never a network call, never an AWS client. That is the point
of the seam being a subprocess: the thing on the other side of it can be
faked with a shell one-liner, so ArenaBench can prove its half of the contract
without knowing what an agent's defect ledger looks like.

The exit contract under test is the whole design: **0 is clear, 9 is banned,
anything else is the gate itself failing** — and the third case must never
abort a run. A checker that crashes and takes healthy runs down with it gets
switched off by the second operator it inconveniences, and a gate nobody arms
catches nothing.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

import pytest

from arenabench.gate import (
    BANNED_EXIT,
    GateNotArmed,
    GateOutcome,
    GateSpec,
    arm_gate,
    gate_argv,
    resolve_gate,
    run_gate,
)


def _spec(*command: str, allowed: frozenset[str] = frozenset()) -> GateSpec:
    return GateSpec(command=tuple(command) or ("gate",), allowed=allowed, enabled=True)


class _Runner:
    """A recorded-call stand-in for :func:`subprocess.run`."""

    def __init__(self, returncode: int = 0, stdout: str = "", stderr: str = "") -> None:
        self.completed = subprocess.CompletedProcess(
            args=[], returncode=returncode, stdout=stdout, stderr=stderr
        )
        self.calls: list[list[str]] = []

    def __call__(self, argv, **kwargs):  # noqa: ANN001 - the subprocess.run shape
        self.calls.append(list(argv))
        self.completed.args = list(argv)
        return self.completed


# --------------------------------------------------------------------------
# the exit contract
# --------------------------------------------------------------------------


class TestExitContract:
    def test_exit_nine_and_only_exit_nine_is_banned(self, tmp_path: Path) -> None:
        runner = _Runner(returncode=BANNED_EXIT, stdout='{"codes": ["B-001"]}')
        outcome = run_gate(_spec("check"), tmp_path, runner=runner)
        assert outcome.banned is True
        assert outcome.codes == ("B-001",)
        assert outcome.error == ""

    def test_exit_zero_is_clear(self, tmp_path: Path) -> None:
        outcome = run_gate(_spec("check"), tmp_path, runner=_Runner(0, stdout="{}"))
        assert outcome.banned is False
        assert outcome.codes == ()
        assert outcome.error == ""

    def test_a_broken_gate_reports_an_error_and_does_not_abort_the_run(
        self, tmp_path: Path
    ) -> None:
        """The rule that keeps the gate armed: a checker that crashed has said
        nothing about the trial, and a run killed by a broken checker is how a
        gate gets switched off for good."""
        outcome = run_gate(
            _spec("check"), tmp_path, runner=_Runner(1, stderr="Traceback ...")
        )
        assert outcome.banned is False
        assert outcome.error != ""
        assert "1" in outcome.error

    def test_a_timeout_is_an_error_not_a_ban(self, tmp_path: Path) -> None:
        def runner(argv, **kwargs):  # noqa: ANN001
            raise subprocess.TimeoutExpired(cmd=list(argv), timeout=0.5)

        outcome = run_gate(_spec("check"), tmp_path, runner=runner, timeout=0.5)
        assert outcome.banned is False
        assert "timed out" in outcome.error

    def test_a_gate_that_cannot_be_executed_is_an_error_not_a_ban(
        self, tmp_path: Path
    ) -> None:
        def runner(argv, **kwargs):  # noqa: ANN001
            raise FileNotFoundError(2, "No such file or directory", argv[0])

        outcome = run_gate(_spec("no-such-checker"), tmp_path, runner=runner)
        assert outcome.banned is False
        assert outcome.error != ""
        assert "no-such-checker" in outcome.error

    def test_unparseable_stdout_still_carries_the_verdict_and_the_report(
        self, tmp_path: Path
    ) -> None:
        """The exit code is the verdict; JSON is a courtesy. A gate whose
        output ArenaBench cannot parse must never be downgraded to clear."""
        outcome = run_gate(
            _spec("check"),
            tmp_path,
            runner=_Runner(BANNED_EXIT, stdout="B-014: witness authored twice\n"),
        )
        assert outcome.banned is True
        assert outcome.codes == ()
        assert "witness authored twice" in outcome.report

    def test_codes_are_read_from_several_tolerated_json_shapes(
        self, tmp_path: Path
    ) -> None:
        bare_list = run_gate(
            _spec("check"), tmp_path, runner=_Runner(BANNED_EXIT, stdout='["A", "B"]')
        )
        assert bare_list.codes == ("A", "B")
        objects = run_gate(
            _spec("check"),
            tmp_path,
            runner=_Runner(BANNED_EXIT, stdout='{"findings": [{"code": "C"}]}'),
        )
        assert objects.codes == ("C",)

    def test_a_disabled_spec_never_runs_the_command(self, tmp_path: Path) -> None:
        runner = _Runner(BANNED_EXIT)
        outcome = run_gate(
            GateSpec(command=("check",), allowed=frozenset(), enabled=False),
            tmp_path,
            runner=runner,
        )
        assert runner.calls == []
        assert outcome == GateOutcome()


class TestArgv:
    def test_the_trial_directory_and_every_waiver_reach_the_command(
        self, tmp_path: Path
    ) -> None:
        argv = gate_argv(
            _spec("triage", "--json", allowed=frozenset({"B-002", "B-001"})), tmp_path
        )
        assert argv == (
            "triage",
            "--json",
            str(tmp_path),
            "--allow",
            "B-001",
            "--allow",
            "B-002",
        )


class TestRealSubprocess:
    """One end-to-end pass through a real interpreter, because the fake
    ``runner`` cannot prove the keyword arguments ``subprocess.run`` is
    actually called with."""

    def test_a_real_gate_script_is_invoked_and_its_exit_code_believed(
        self, tmp_path: Path
    ) -> None:
        script = tmp_path / "gate.py"
        script.write_text(
            "import json, sys\n"
            'print(json.dumps({"codes": ["B-007"], "argv": sys.argv[1:]}))\n'
            "sys.exit(9)\n",
            encoding="utf-8",
        )
        outcome = run_gate(
            GateSpec(
                command=(sys.executable, str(script)),
                allowed=frozenset({"B-009"}),
                enabled=True,
            ),
            tmp_path,
        )
        assert outcome.banned is True
        assert outcome.codes == ("B-007",)
        assert "--allow" in outcome.report


# --------------------------------------------------------------------------
# resolution and arming
# --------------------------------------------------------------------------


class TestResolveGate:
    def test_the_flag_beats_the_environment(self) -> None:
        spec = resolve_gate(
            command="triage --json",
            env={"ARENABENCH_GATE": "other"},
            disabled=False,
        )
        assert spec is not None
        assert spec.command == ("triage", "--json")

    def test_the_environment_arms_it_when_no_flag_is_given(self) -> None:
        spec = resolve_gate(
            command=None, env={"ARENABENCH_GATE": "check --json"}, disabled=False
        )
        assert spec is not None
        assert spec.command == ("check", "--json")

    def test_disabled_wins_over_both(self) -> None:
        assert (
            resolve_gate(
                command="triage", env={"ARENABENCH_GATE": "other"}, disabled=True
            )
            is None
        )

    def test_neither_resolves_to_nothing(self) -> None:
        assert resolve_gate(command=None, env={}, disabled=False) is None

    def test_a_blank_environment_value_is_not_a_gate(self) -> None:
        assert resolve_gate(command=None, env={"ARENABENCH_GATE": "  "}, disabled=False) is None

    def test_waivers_are_carried_onto_the_spec(self) -> None:
        spec = resolve_gate(
            command="triage", env={}, disabled=False, allowed=("B-002", "B-002", "B-001")
        )
        assert spec is not None
        assert spec.allowed == frozenset({"B-001", "B-002"})


class TestArmGate:
    def test_an_unarmed_run_is_refused_rather_than_started_unguarded(self) -> None:
        with pytest.raises(GateNotArmed) as caught:
            arm_gate(command=None, disabled=False, allowed=(), env={})
        message = str(caught.value)
        assert "--gate" in message
        assert "ARENABENCH_GATE" in message
        assert "--no-gate" in message

    def test_an_explicit_waiver_is_allowed_and_says_so_in_the_header(self) -> None:
        spec, line = arm_gate(command=None, disabled=True, allowed=(), env={})
        assert spec is None
        assert line == "gate      : DISABLED (--no-gate)"

    def test_an_armed_gate_names_itself_in_the_header(self) -> None:
        spec, line = arm_gate(
            command="triage --json", disabled=False, allowed=("B-001",), env={}
        )
        assert spec is not None
        assert "triage --json" in line
        assert "B-001" in line


# --------------------------------------------------------------------------
# the watch hook and the CLI refusal
# --------------------------------------------------------------------------


class _HookBatch:
    """Just enough Batch to drive ``watch`` through a scripted state sequence."""

    def __init__(self, script: list[dict[str, str]]) -> None:
        self.script = list(script)

    def describe_jobs(self, *, jobs: list[str]) -> dict:
        states = self.script[0]
        return {
            "jobs": [
                {"jobId": j, "status": states.get(j, "RUNNABLE")} for j in jobs
            ]
        }

    def advance(self) -> None:
        if len(self.script) > 1:
            self.script.pop(0)


def _watch_executor(batch: _HookBatch):
    from arenabench.cloud import CloudExecutor

    return CloudExecutor(clients={"batch": batch})


class TestWatchHook:
    JOBS = [
        {"id": "job-001", "label": "000-stella-task-a"},
        {"id": "job-002", "label": "001-cc-task-a"},
        {"id": "job-003", "label": "002-stella-task-b"},
    ]

    def _script(self) -> list[dict[str, str]]:
        return [
            {"job-001": "RUNNABLE", "job-002": "RUNNABLE", "job-003": "RUNNABLE"},
            {"job-001": "SUCCEEDED", "job-002": "RUNNING", "job-003": "RUNNABLE"},
            {"job-001": "SUCCEEDED", "job-002": "FAILED", "job-003": "RUNNING"},
            {"job-001": "SUCCEEDED", "job-002": "FAILED", "job-003": "SUCCEEDED"},
        ]

    def _watch(self, on_settled, script=None):
        batch = _HookBatch(script or self._script())
        seen: list[float] = []

        def sleep(seconds: float) -> None:
            seen.append(seconds)
            batch.advance()

        ticks = iter(range(0, 10_000, 10))
        states = _watch_executor(batch).watch(
            self.JOBS,
            poll_interval=1.0,
            sleep=sleep,
            clock=lambda: float(next(ticks)),
            out=lambda _line: None,
            on_settled=on_settled,
        )
        return states, seen

    def test_each_settled_job_is_reported_exactly_once(self) -> None:
        """The hook is per settlement, not per poll: a job that stays
        SUCCEEDED for three sweeps must not pay for three gate runs."""
        calls: list[tuple[str, str]] = []

        def hook(job_id: str, state: str) -> bool:
            calls.append((job_id, state))
            return False

        self._watch(hook)
        assert calls == [
            ("job-001", "SUCCEEDED"),
            ("job-002", "FAILED"),
            ("job-003", "SUCCEEDED"),
        ]

    def test_a_true_hook_returns_the_watch_immediately(self) -> None:
        calls: list[str] = []

        def hook(job_id: str, _state: str) -> bool:
            calls.append(job_id)
            return True

        states, slept = self._watch(hook)
        assert calls == ["job-001"], "the sweep stops at the first abort"
        assert states["job-003"] == "RUNNABLE", (
            "watch returns mid-flight, so the un-settled trials are still "
            "un-settled — which is exactly what the caller cancels"
        )
        assert slept == [1.0], "no further polling after the abort"

    def test_no_hook_is_the_unchanged_behaviour(self) -> None:
        states, _ = self._watch(None)
        assert states["job-003"] == "SUCCEEDED"


class _FetchingExecutor:
    """``_fetch_prefix``/``load_jobs``/``job_states``/``_client`` — the seams
    the settled-gate callback drives, and nothing else."""

    def __init__(self, record: dict, states: dict[str, str]) -> None:
        self.record = record
        self.states = dict(states)
        self.prefixes: list[str] = []
        self.cancelled: list[str] = []
        self.terminated: list[str] = []

    # -- fetch --
    def _fetch_prefix(self, prefix: str, dest: Path, *, out) -> int:
        self.prefixes.append(prefix)
        dest.mkdir(parents=True, exist_ok=True)
        (dest / "results.json").write_text("{}", encoding="utf-8")
        return 1

    # -- cancel --
    def load_jobs(self, run_id: str) -> dict:
        return self.record

    def job_states(self, job_ids) -> dict[str, str]:
        return {j: self.states[j] for j in job_ids if j in self.states}

    def _client(self, service: str):
        assert service == "batch"
        return self

    def cancel_job(self, *, jobId: str, reason: str) -> dict:  # noqa: N803
        self.cancelled.append(jobId)
        return {}

    def terminate_job(self, *, jobId: str, reason: str) -> dict:  # noqa: N803
        self.terminated.append(jobId)
        return {}


def _gate_script(tmp_path: Path, body: str) -> GateSpec:
    script = tmp_path / "gate_script.py"
    script.write_text(body, encoding="utf-8")
    return GateSpec(
        command=(sys.executable, str(script)), allowed=frozenset(), enabled=True
    )


class TestSettledGate:
    RECORD = {
        "run_id": "r-test",
        "jobs": [
            {"id": "job-001", "label": "000-stella-task-a"},
            {"id": "job-002", "label": "001-cc-task-a"},
        ],
    }

    def test_a_banned_trial_cancels_the_run_and_leaves_the_dispute_record(
        self, tmp_path: Path, capsys
    ) -> None:
        from arenabench.gate_watch import settled_gate

        executor = _FetchingExecutor(
            self.RECORD, {"job-001": "SUCCEEDED", "job-002": "RUNNING"}
        )
        spec = _gate_script(
            tmp_path,
            "import json, sys\n"
            'print(json.dumps({"codes": ["B-001"]}))\n'
            "sys.exit(9)\n",
        )
        dest = tmp_path / "out"
        hook = settled_gate(
            executor, run_id="r-test", spec=spec, jobs=self.RECORD["jobs"], dest=dest
        )

        assert hook("job-001", "SUCCEEDED") is True
        assert executor.prefixes == ["runs/r-test/job-001/"], (
            "only the settled trial's artifacts are pulled — the rest of the "
            "run has not finished and paying to download it would be waste"
        )
        assert executor.terminated == ["job-002"]
        assert hook.aborted is not None
        record = json.loads((dest / "abort.json").read_text(encoding="utf-8"))
        assert record["codes"] == ["B-001"]
        assert record["trial"] == "000-stella-task-a"
        assert "banned" in capsys.readouterr().out.lower()

    def test_a_clear_trial_changes_nothing(self, tmp_path: Path) -> None:
        from arenabench.gate_watch import settled_gate

        executor = _FetchingExecutor(
            self.RECORD, {"job-001": "SUCCEEDED", "job-002": "RUNNING"}
        )
        spec = _gate_script(tmp_path, "import sys\nsys.exit(0)\n")
        hook = settled_gate(
            executor,
            run_id="r-test",
            spec=spec,
            jobs=self.RECORD["jobs"],
            dest=tmp_path / "out",
        )
        assert hook("job-001", "SUCCEEDED") is False
        assert executor.cancelled == []
        assert executor.terminated == []
        assert hook.aborted is None

    def test_a_broken_gate_is_loud_and_the_run_survives(
        self, tmp_path: Path, capsys
    ) -> None:
        """The rule that keeps the gate armed, at the composition level."""
        from arenabench.gate_watch import settled_gate

        executor = _FetchingExecutor(
            self.RECORD, {"job-001": "SUCCEEDED", "job-002": "RUNNING"}
        )
        spec = _gate_script(tmp_path, "import sys\nsys.exit(3)\n")
        hook = settled_gate(
            executor,
            run_id="r-test",
            spec=spec,
            jobs=self.RECORD["jobs"],
            dest=tmp_path / "out",
        )
        assert hook("job-001", "SUCCEEDED") is False
        assert executor.terminated == []
        assert "gate failed" in capsys.readouterr().out.lower()

    def test_an_unknown_job_id_is_ignored_rather_than_guessed_at(
        self, tmp_path: Path
    ) -> None:
        from arenabench.gate_watch import settled_gate

        executor = _FetchingExecutor(self.RECORD, {})
        spec = _gate_script(tmp_path, "import sys\nsys.exit(9)\n")
        hook = settled_gate(
            executor,
            run_id="r-test",
            spec=spec,
            jobs=self.RECORD["jobs"],
            dest=tmp_path / "out",
        )
        assert hook("job-999", "SUCCEEDED") is False
        assert executor.prefixes == []


TEMPLATE = """\
[match]
name = "gate smoke"
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


def _run_args(template: Path, **overrides) -> argparse.Namespace:
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
        gate=None,
        no_gate=False,
        gate_allow=None,
    )
    defaults.update(overrides)
    return argparse.Namespace(**defaults)


class TestCloudRunRefusesToStartUnguarded:
    def test_no_gate_and_no_explicit_waiver_is_exit_two(
        self, tmp_path: Path, capsys, monkeypatch
    ) -> None:
        """A bench run costs real money and the whole point of the gate is to
        stop paying for a defect we already fixed. Starting unguarded by
        accident is the failure this refusal exists to prevent."""
        monkeypatch.delenv("ARENABENCH_GATE", raising=False)
        from arenabench.cloud import _cmd_cloud_run

        template = tmp_path / "match.toml"
        template.write_text(TEMPLATE, encoding="utf-8")

        rc = _cmd_cloud_run(_run_args(template), executor=object())

        assert rc == 2
        err = capsys.readouterr().err
        assert "--gate" in err
        assert "ARENABENCH_GATE" in err
        assert "--no-gate" in err
