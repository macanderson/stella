# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The rotation supervisor, wired to a real ``MatchRunner`` (#2545).

``reauth.py`` shipped with 22 tests and no caller: ``runner.py`` mentioned
neither ``reauth`` nor ``Supervisor`` nor ``read_trials``, so a Claude Code seat
still died exactly as before and an 89-task sweep still ended measured on 20.
Those 22 tests could not have caught it — they exercise ``decide`` and
``Supervisor`` in isolation, which is *why* the module shipped unwired. Every
test here drives the runner instead, and each one fails on a tree where the
wiring is absent.

No network, no Harbor, no Docker, no containers: ``Popen`` is a fake, the
process group is a recorder, the keychain is a lambda, and the trials are
``result.json`` files written by hand.
"""

from __future__ import annotations

import json
import os
import subprocess
import threading
import time
from pathlib import Path
from typing import Any

import pytest

from arenabench import reap, reauth_wiring
from arenabench import runner as runner_mod
from arenabench.model import MatchSpec
from arenabench.reauth import SeatState
from arenabench.registry import DEFAULT_REGISTRY
from arenabench.runner import MatchRunner

TASKS = ("alpha", "beta", "gamma")
LAUNCH_TOKEN = "sk-ant-oat01-launch-value"
FRESH_TOKEN = "sk-ant-oat01-rotated-value"

VOID: dict[str, Any] = {"exception_info": {"exception_type": "AuthenticationError"}}
SOLVED: dict[str, Any] = {"verifier_result": {"rewards": {"reward": 1.0}}}
LOST: dict[str, Any] = {"verifier_result": {"rewards": {"reward": 0.0}}}
TIMED_OUT: dict[str, Any] = {"exception_info": {"exception_type": "AgentTimeoutError"}}


class FakeProcess:
    """A ``harbor run`` that never ran. Exits only when a test says so."""

    _next_pid = 900001

    def __init__(self, command: list[str]) -> None:
        self.command = command
        self.pid = FakeProcess._next_pid
        FakeProcess._next_pid += 1
        self.returncode: int | None = None
        self.terminated = 0

    def poll(self) -> int | None:
        return self.returncode

    def wait(self, timeout: float | None = None) -> int | None:
        return self.returncode

    def terminate(self) -> None:
        self.terminated += 1

    def exit(self, code: int = 0) -> None:
        self.returncode = code

    def tasks(self) -> tuple[str, ...]:
        """The ``--include-task-name`` set this process was launched with.

        Bare names: a ref'd dataset is filtered by ``<namespace>/<task>``, and
        the retry inherits that spelling from ``_launch`` like everything else.
        """
        return tuple(
            self.command[i + 1].rsplit("/", 1)[-1]
            for i, arg in enumerate(self.command)
            if arg == "--include-task-name"
        )

    def flag(self, name: str) -> str:
        return self.command[self.command.index(name) + 1]


class Rig:
    """One match, its fakes, and the levers a test pulls on them."""

    def __init__(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, **over: Any):
        self.processes: list[FakeProcess] = []
        self.signalled: list[int] = []
        self.reaped: list[str] = []
        self.keychain: str | None = LAUNCH_TOKEN

        monkeypatch.setattr(
            "arenabench.harbor.harbor_bin", lambda dataset_key=None: "/usr/bin/harbor"
        )
        monkeypatch.setattr(
            "arenabench.harbor.harbor_version", lambda binary=None: "0.20.0"
        )
        monkeypatch.setattr(
            "arenabench.harbor.supports_agent_import_path", lambda binary=None: False
        )
        monkeypatch.setattr(
            "arenabench.adapter.stella_seat_problem", lambda binary=None, **_kw: None
        )
        monkeypatch.setattr(subprocess, "Popen", self._popen)
        # Never signal a real process group, and record what would have been.
        monkeypatch.setattr(reauth_wiring.os, "getpgid", lambda pid: pid)
        monkeypatch.setattr(reauth_wiring.os, "killpg", self._killpg)
        monkeypatch.setattr(reap, "remove_containers", self._reap)
        # The local Claude Code login, read fresh on every tick exactly as the
        # real one is.
        monkeypatch.setattr(
            reauth_wiring, "local_claude_token", lambda: self.keychain
        )

        self.spec = MatchSpec.from_json(
            {
                "id": "m1",
                "dataset": "terminal-bench-2.1",
                "tasks": list(over.pop("tasks", TASKS)),
                "sut_ref": "",
                "contestants": over.pop("contestants", [self.seat()]),
                **over,
            }
        )
        self.runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / "ws")
        self.match = self.runner.create(self.spec)

    @staticmethod
    def seat(name: str = "cc", token: str | None = LAUNCH_TOKEN) -> dict[str, Any]:
        env = {"CLAUDE_CODE_OAUTH_TOKEN": token} if token else {}
        return {
            "id": name,
            "name": name,
            "agent": "claude-code",
            "engine": {"api": "anthropic", "model": "claude-fable-5"},
            "env": env,
        }

    # -- fakes ------------------------------------------------------------

    def _popen(self, command: list[str], **_kwargs: Any) -> FakeProcess:
        process = FakeProcess(list(command))
        self.processes.append(process)
        return process

    def _killpg(self, pid: int, _sig: int) -> None:
        """Record the signal, and let the group actually die, as SIGTERM does."""
        self.signalled.append(pid)
        for process in self.processes:
            if process.pid == pid:
                process.exit(-15)

    def _reap(self, names: list[str]) -> list[str]:
        self.reaped.extend(names)
        return list(names)

    # -- levers -----------------------------------------------------------

    def start(self, *, detached: bool = True) -> None:
        """Launch the match. ``detached`` skips the poll loop so a test can tick."""
        if detached:
            self._monkeypatch.setattr(
                MatchRunner, "_await_completion", lambda _self, _match: None
            )
        self.runner.start(self.match)

    def trial(self, task: str, result: dict[str, Any], attempt: str = "a") -> Path:
        """Write one finished trial into the seat's job directory."""
        job_dir = next(iter(self.match.runs.values())).job_dir
        trial = job_dir / f"{task}__{attempt}"
        (trial / "agent").mkdir(parents=True, exist_ok=True)
        (trial / "result.json").write_text(json.dumps(result), encoding="utf-8")
        return trial

    def tick(self) -> bool:
        return reauth_wiring.tick(self.match)

    @property
    def seat_state(self) -> SeatState:
        assert self.match.reauth is not None
        return self.match.reauth.seats[0].state


@pytest.fixture
def rig(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    def build(**over: Any) -> Rig:
        made = Rig(tmp_path, monkeypatch, **over)
        made._monkeypatch = monkeypatch
        return made

    return build


def _dead_credential(made: Rig) -> None:
    """The failure this whole module exists for: the token was revoked."""
    made.trial("alpha", VOID)
    made.processes[0].exit(1)
    made.keychain = None  # the local login has not rotated yet


# --------------------------------------------------------------------------
# the supervisor is actually wired
# --------------------------------------------------------------------------


class TestTheSupervisorIsReached:
    def test_a_subscription_seat_is_supervised_at_all(self, rig) -> None:
        """The bare witness for #2545: before the wiring this is ``None``."""
        made = rig()
        made.start()
        assert made.match.reauth is not None
        assert [seat.contestant for seat in made.match.reauth.seats] == ["cc"]

    def test_a_seat_with_no_subscription_token_is_not_supervised(self, rig) -> None:
        made = rig(contestants=[Rig.seat("openrouter-seat", token=None)])
        made.start()
        assert made.match.reauth is None

    def test_a_whole_dataset_sweep_says_why_it_is_unsupervised(self, rig) -> None:
        """A refusal an operator can read, never a silent absence.

        The supervisor's re-dispatch set is the sweep's tasks minus the measured
        ones, and a job with no ``--include-task-name`` filter has a task set
        nothing here can see.
        """
        made = rig(tasks=[])
        made.start()
        assert made.match.reauth is None
        (run,) = made.match.runs.values()
        assert any("explicit task list" in w for w in run.warnings)

    def test_several_attempts_per_task_says_why_it_is_unsupervised(self, rig) -> None:
        made = rig(attempts=2)
        made.start()
        assert made.match.reauth is None
        (run,) = made.match.runs.values()
        assert any("attempts per task" in w for w in run.warnings)


# --------------------------------------------------------------------------
# a paused seat is not a finished one
# --------------------------------------------------------------------------


class TestAPausedSeatIsNotFinished:
    """The exit-condition witness.

    ``_await_completion`` broke as soon as no seat process was alive. A paused
    seat has no live process and is *not* finished, so on the unwired code the
    match would be declared over at the exact moment the supervisor stopped the
    seat it is about to re-dispatch.
    """

    def test_the_tick_reports_work_still_owed(self, rig) -> None:
        made = rig()
        made.start()
        _dead_credential(made)
        assert made.tick() is True, "a paused seat still owes the match its tasks"
        assert made.seat_state is SeatState.PAUSED

    def test_the_seat_is_stopped_and_its_own_containers_reaped(self, rig) -> None:
        """Killing the group leaves the task containers up — they are the Docker
        daemon's children — and a retry inheriting them reproduces #2329."""
        made = rig(contestants=[Rig.seat("cc"), Rig.seat("rival", token=None)])
        made.start()
        made.trial("alpha", VOID)
        rival_dir = made.match.runs["rival"].job_dir
        (rival_dir / "alpha__r").mkdir(parents=True)
        made.keychain = None
        assert made.tick() is True
        assert made.signalled == [made.match.runs["cc"].process.pid]
        assert made.reaped == ["alpha__a-main-1"]
        assert not any("alpha__r" in name for name in made.reaped), (
            "reaped a container belonging to the arm that is still running"
        )

    def test_the_match_stays_running_while_a_seat_waits(self, rig) -> None:
        """End to end through the real poll loop: it must not settle the match.

        The loop is driven in a thread with its sleep shortened; the assertion
        is that after many iterations the match is still ``running`` and the
        supervisor is still asking for a credential.
        """
        made = rig()
        made.start(detached=False)
        _dead_credential(made)
        settled = threading.Event()

        def drive() -> None:
            made.runner._await_completion(made.match)
            settled.set()

        # Shorten the loop's own pacing without recursing into the patch: the
        # runner's `time` is this module's `time`.
        real_sleep = time.sleep
        made._monkeypatch.setattr(runner_mod.time, "sleep", lambda _s: real_sleep(0.01))
        thread = threading.Thread(target=drive, daemon=True)
        thread.start()
        try:
            assert not settled.wait(timeout=0.5), (
                "the match settled while a seat was paused — an arm measured on "
                "1 of 3 tasks reported as a finished contest"
            )
            assert made.match.status == "running"
            assert made.seat_state is SeatState.PAUSED
        finally:
            # Let the loop finish: hand over a token, then finish the retry.
            made.runner.provide_token(made.match, FRESH_TOKEN)
            deadline = time.monotonic() + 5.0
            while len(made.processes) < 2 and time.monotonic() < deadline:
                time.sleep(0.01)
            for task in TASKS:
                made.trial(task, SOLVED, attempt="retry")
            made.processes[-1].exit(0)
            assert settled.wait(timeout=5.0), "the loop never ended"
        assert made.match.status == "finished"


# --------------------------------------------------------------------------
# what a retry may and may not re-run
# --------------------------------------------------------------------------


class TestOnlyAVoidComesBack:
    """The honesty property, at the layer that dispatches.

    Keyed on the abort taxonomy — ``telemetry.VOID_CREDENTIALS`` — and never on
    "did not pass". Re-running a task until it passes and reporting the pass is
    the dishonesty this feature must not enable, and it is one wrong predicate
    away at all times.
    """

    def _rotate(self, made: Rig) -> FakeProcess:
        """Kill the credential, then let the keychain rotate and heal.

        Two ticks, and never one: a supervisor that killed and launched in the
        same step would put two Harbor jobs on one directory, so the stop and
        the dispatch are separated by a confirmation that the old process is
        gone.
        """
        made.keychain = FRESH_TOKEN
        assert made.tick() is True  # stops the seat
        assert made.processes[0].poll() is not None, "the seat was not stopped"
        assert len(made.processes) == 1, "launched while the old seat was alive"
        made.tick()  # re-dispatches on the fresh token
        assert len(made.processes) == 2, "no retry round was launched"
        return made.processes[1]

    def test_a_measured_loss_is_never_re_dispatched(self, rig) -> None:
        """The decisive one. ``alpha`` lost; it stays lost."""
        made = rig()
        made.start()
        made.trial("alpha", LOST)
        made.trial("beta", VOID)
        retry = self._rotate(made)
        assert "alpha" not in retry.tasks()
        assert retry.tasks() == ("beta", "gamma")

    def test_a_timeout_is_never_re_dispatched(self, rig) -> None:
        """A timeout is something the agent did with its whole budget."""
        made = rig()
        made.start()
        made.trial("alpha", TIMED_OUT)
        made.trial("beta", VOID)
        assert "alpha" not in self._rotate(made).tasks()

    def test_a_solved_task_is_never_re_dispatched(self, rig) -> None:
        made = rig()
        made.start()
        made.trial("alpha", SOLVED)
        made.trial("beta", VOID)
        assert "alpha" not in self._rotate(made).tasks()

    def test_the_never_run_tasks_come_back_with_the_voided_one(self, rig) -> None:
        """A rotation strands both, and leaving the never-run out is how a
        re-run converges on a partial sweep that looks complete."""
        made = rig()
        made.start()
        made.trial("alpha", VOID)
        assert self._rotate(made).tasks() == TASKS

    def test_the_retry_writes_into_the_same_job_directory(self, rig) -> None:
        """The design decision #2545 asked to be made explicitly.

        Same job dir, so the retry's trial lands beside the void it replaces and
        `Match.trial_dirs`'s measured-over-void rank publishes it — rather than
        a second directory every reader would have to learn to union.
        """
        made = rig()
        made.start()
        made.trial("alpha", VOID)
        first, retry = made.processes[0], self._rotate(made)
        assert retry.flag("--job-name") == first.flag("--job-name")
        assert retry.flag("--jobs-dir") == first.flag("--jobs-dir")
        rounds = made.match.rounds_for("cc")
        assert [run.job_dir for run in rounds] == [rounds[0].job_dir] * 2

    def test_the_void_that_caused_the_retry_does_not_then_kill_it(self, rig) -> None:
        """The trap this wiring walked into, and the reason for ``superseded``.

        A voided trial never leaves the disk. Read as evidence about the *live*
        credential it says "revoked" forever, and one tick later the keychain
        offers nothing newer than the token just launched with — so the seat
        pauses and the supervisor kills the retry seconds after starting it.
        A measurement destroyed by the machinery meant to save it.
        """
        made = rig()
        made.start()
        made.trial("alpha", VOID)
        retry = self._rotate(made)
        for _ in range(3):
            assert made.tick() is False
        assert retry.poll() is None, "the retry was killed by the void it re-runs"
        assert made.seat_state is SeatState.RUNNING
        assert len(made.processes) == 2, "a second retry round on one job dir"

    def test_a_retry_that_dies_on_the_new_token_pauses_again(self, rig) -> None:
        """Superseding must not blind the seat to a fresh credential fault."""
        made = rig()
        made.start()
        made.trial("alpha", VOID)
        self._rotate(made)
        made.trial("alpha", VOID, attempt="retry")
        assert made.tick() is True
        assert made.seat_state is SeatState.PAUSED

    def test_a_retry_reusing_a_trial_id_still_pauses_when_it_dies(self, rig) -> None:
        """The under-pausing direction, which fails silently.

        A re-dispatch writes into the same job directory, so whether a retry's
        trial gets a fresh ``<task>__<attempt>`` name is Harbor's business, not
        this repository's. If a generation keyed on that name were wrong, a
        dead *new* credential would read as old news: the seat would never
        pause, and would spend its whole budget re-dispatching against a token
        that can never work — each round looking like progress in the ledger.

        So the same identifier is reused here deliberately. What decides is
        when the verdict was written.
        """
        made = rig()
        made.start()
        made.trial("alpha", VOID)  # attempt "a"
        self._rotate(made)
        # The retry lands on the very same trial id and dies the same way.
        trial = made.trial("alpha", VOID)
        stamp = time.time() + 5.0
        os.utime(trial / "result.json", (stamp, stamp))

        assert made.tick() is True, "a dead new credential read as old news"
        assert made.seat_state is SeatState.PAUSED

    def test_the_retry_does_not_truncate_the_dead_round_s_log(self, rig) -> None:
        made = rig()
        made.start()
        made.trial("alpha", VOID)
        self._rotate(made)
        first, retry = made.match.rounds_for("cc")
        assert first.log_path != retry.log_path
        assert retry.log_path.name == "cc.round1.log"


# --------------------------------------------------------------------------
# the record
# --------------------------------------------------------------------------


class TestTheRetryIsVisibleInTheRecord:
    """A retry folded in silently would be a different dishonesty.

    A reader of the match must be able to tell a first-attempt pass from a
    re-dispatched one, and see why the re-dispatch happened.
    """

    def _healed(self, made: Rig) -> dict[str, Any]:
        made.trial("alpha", VOID)
        made.processes[0].exit(1)
        made.keychain = FRESH_TOKEN
        made.tick()
        made.tick()
        return made.match.snapshot()

    def test_the_round_is_on_the_match_snapshot(self, rig) -> None:
        made = rig()
        made.start()
        entry = self._healed(made)["contestants"][0]
        assert [r["round"] for r in entry["rounds"]] == [0, 1]
        retry = entry["rounds"][1]
        assert retry["tasks"] == list(TASKS)
        assert "credential rotated mid-match" in retry["reason"]

    def test_the_rotation_state_is_on_the_match_snapshot(self, rig) -> None:
        made = rig()
        made.start()
        entry = self._healed(made)["contestants"][0]
        assert entry["reauth"]["rounds_used"] == 1
        # Back at work on the fresh token, with the whole ladder on the record.
        assert entry["reauth"]["state"] == SeatState.RUNNING.value
        assert not entry["reauth"]["shortfall"]
        assert any("re-dispatching" in line for line in entry["reauth"]["log"])

    def test_an_unsupervised_seat_reports_no_rotation_state(self, rig) -> None:
        """Absent, not "healthy": nothing is watching that seat, and a badge
        that cannot tell those apart is how an unwired supervisor went unseen."""
        made = rig(contestants=[Rig.seat("openrouter-seat", token=None)])
        made.start()
        assert made.match.snapshot()["contestants"][0]["reauth"] is None

    def test_a_match_that_never_rotated_still_reports_its_one_round(self, rig) -> None:
        made = rig()
        made.start()
        entry = made.match.snapshot()["contestants"][0]
        assert [r["round"] for r in entry["rounds"]] == [0]
        assert entry["rounds"][0]["reason"] == ""

    def test_no_credential_reaches_the_record(self, rig) -> None:
        """Neither the token that died nor the one that replaced it."""
        made = rig()
        made.start()
        blob = json.dumps(self._healed(made))
        assert LAUNCH_TOKEN not in blob
        assert FRESH_TOKEN not in blob


# --------------------------------------------------------------------------
# who may be stopped, and on whose credential
# --------------------------------------------------------------------------


class TestOnlyTheSeatThatLostItsCredential:
    def test_the_opposing_arm_is_never_stopped(self, rig) -> None:
        """Pausing both arms to fix one is itself a confound: they would no
        longer have faced the same machine load."""
        made = rig(contestants=[Rig.seat("cc"), Rig.seat("rival", token=None)])
        made.start()
        rival = made.match.runs["rival"].process
        made.trial("alpha", VOID)
        made.keychain = None
        made.tick()
        assert made.signalled == [made.match.runs["cc"].process.pid]
        assert rival.poll() is None and rival.terminated == 0

    def test_a_seat_on_a_foreign_credential_never_heals_from_the_local_login(
        self, rig
    ) -> None:
        """A pasted token that dies must not be replaced by the operator's own.

        Healing there would run the second half of the arm on a different
        subscription than the first — a silently swapped variable in a
        controlled comparison, not a recovery. It pauses for an operator
        instead.
        """
        made = rig(contestants=[Rig.seat("cc", token="pasted-from-elsewhere")])
        made.keychain = "the-operators-own-login"
        made.start()
        made.trial("alpha", VOID)
        made.processes[0].exit(1)
        made.tick()
        assert made.seat_state is SeatState.PAUSED
        assert len(made.processes) == 1, "healed onto a credential it does not own"

    def test_an_operator_token_resumes_a_paused_seat(self, rig) -> None:
        made = rig()
        made.start()
        _dead_credential(made)
        made.tick()
        assert made.runner.provide_token(made.match, FRESH_TOKEN) == ["cc"]
        made.tick()
        assert len(made.processes) == 2
        assert made.processes[1].tasks() == TASKS

    def test_providing_a_token_when_nothing_waits_claims_no_resume(self, rig) -> None:
        made = rig()
        made.start()
        assert made.runner.provide_token(made.match, FRESH_TOKEN) == []


# --------------------------------------------------------------------------
# giving up honestly
# --------------------------------------------------------------------------


class TestGivingUpIsSaidOutLoud:
    def test_a_pause_that_outlives_its_grace_ends_with_a_named_shortfall(
        self, rig
    ) -> None:
        """An unbounded pause is a hang, and a hang publishes neither arm.

        The bound has to end the match, and it has to end it saying the tasks
        were never measured rather than letting them read as anything else.
        """
        made = rig()
        made.start()
        _dead_credential(made)
        assert made.match.reauth is not None
        clock = iter([0.0, 0.0, 10_000.0, 10_000.0])
        made.match.reauth.clock = lambda: next(clock)
        assert made.tick() is True
        assert made.tick() is False, "a run nobody answered must not wait forever"
        seat = made.match.reauth.seats[0]
        assert seat.shortfall
        assert "unmeasured" in seat.released
        assert "shortfall" in made.match.note

    def test_a_re_dispatch_that_cannot_launch_releases_the_seat(self, rig) -> None:
        """A retry that cannot start must not spin the supervision loop."""
        made = rig()
        made.start()
        made.trial("alpha", VOID)
        made.processes[0].exit(1)
        made.keychain = FRESH_TOKEN

        def explode(*_args: Any, **_kwargs: Any) -> None:
            raise RuntimeError("harbor vanished mid-match")

        made._monkeypatch.setattr(MatchRunner, "_launch", explode)
        assert made.tick() is False
        seat = made.match.reauth.seats[0]
        assert seat.shortfall and "harbor vanished" in seat.released
        # The failed round is a record, not a silence.
        assert made.match.rounds_for("cc")[-1].error == "harbor vanished mid-match"

    def test_a_seat_that_dies_of_something_else_still_ends_the_match(self, rig) -> None:
        """Supervision must not hold a match open for a failure it cannot fix.

        A Harbor that crashed, a dataset lookup that failed — the seat is dead
        with tasks unmeasured and no credential fault on the record, which is
        not this module's business and must settle exactly as it did before.
        """
        made = rig()
        made.start()
        made.trial("alpha", LOST)
        made.processes[0].exit(1)
        assert made.tick() is False
        assert made.seat_state is SeatState.RUNNING


# --------------------------------------------------------------------------
# the measurement the whole thing is for
# --------------------------------------------------------------------------


def test_a_rotated_out_sweep_ends_fully_measured(rig) -> None:
    """The definition of done, end to end.

    A seat loses its credential after one task, the keychain rotates, the
    remaining tasks are re-dispatched into the same job directory, and the
    scoreboard reads three measured tasks — not one measured and two voided.
    """
    made = rig()
    made.start()
    made.trial("alpha", SOLVED)
    made.trial("beta", VOID)
    made.processes[0].exit(1)
    made.keychain = FRESH_TOKEN
    made.tick()
    made.tick()
    assert made.processes[1].tasks() == ("beta", "gamma")

    made.trial("beta", LOST, attempt="retry")
    made.trial("gamma", SOLVED, attempt="retry")
    made.processes[1].exit(0)
    assert made.tick() is False

    metrics = made.match.metrics_for("cc")
    assert {task: metrics[task].resolved for task in TASKS} == {
        "alpha": True,
        "beta": False,  # a real answer from the re-run, not the void
        "gamma": True,
    }
    totals = made.match.snapshot()["contestants"][0]["totals"]
    assert totals["judged"] == 3, "an arm measured on fewer tasks than it ran"
