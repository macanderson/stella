# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""A stopped match must not cost the next one its trials.

Harbor starts task containers through the Docker daemon, so they are not
children of the ``harbor run`` process group. Killing that group — which is
what ``cancel`` does — stopped the harness and left the containers running.

Observed 2026-08-08: three matches stopped mid-flight left ten orphaned
containers resident. The host's practical ceiling was about two task containers
per match, so the *next* match's Claude Code arm lost five of its six trials to
``RuntimeError: Docker compose command failed`` and the board read 1/6 — five
trials that never started, every one of them projected as an agent loss
(#2329).

Every witness here proves one of the three halves of the fix: reap on stop,
reap on start, and — for the container that survives anyway — keep the trial
out of the denominator.
"""

from __future__ import annotations

import signal
import subprocess
import time
from pathlib import Path
from typing import ClassVar

import pytest

from arenabench import reap, reauth_wiring
from arenabench.model import MatchSpec
from arenabench.registry import DEFAULT_REGISTRY
from arenabench.runner import ContestantRun, MatchRunner
from arenabench.telemetry import is_compose_failure


class _FakeDaemon:
    """A docker daemon that remembers what it was asked to remove."""

    def __init__(self, running: list[str]) -> None:
        self.running = list(running)
        self.removed: list[str] = []

    def install(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setattr(reap, "running_containers", lambda: list(self.running))
        real_remove = reap.remove_containers

        def remove(names: list[str]) -> list[str]:
            gone = real_remove(names)
            self.removed.extend(gone)
            for name in gone:
                self.running.remove(name)
            return gone

        monkeypatch.setattr(reap, "remove_containers", remove)
        monkeypatch.setattr(
            reap, "_docker", lambda *args, **kwargs: _Completed(list(args))
        )


class _Completed:
    def __init__(self, args: list[str]) -> None:
        self.args = args
        self.returncode = 0
        self.stdout = ""
        self.stderr = ""


class _Daemon:
    """A docker daemon that answers `list_containers` and remembers removals.

    The richer sibling of `_FakeDaemon`: that one patches
    `reap.running_containers`, which is enough for the name-derived half of a
    sweep and blind to everything reached through the compose project label.
    This one patches `reap.list_containers`, so a container can carry a
    project, be running or exited, and be named anything at all.
    """

    def __init__(self, containers: list[reap.Container]) -> None:
        self.containers = list(containers)
        self.removed: list[str] = []
        #: Every daemon interaction in order, so a test can assert that no
        #: container was removed before the processes were stopped.
        self.calls: list[str] = []

    def install(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setattr(reap, "list_containers", self._list)
        monkeypatch.setattr(reap, "remove_containers", self._remove)

    def _list(self) -> list[reap.Container]:
        self.calls.append("list")
        return list(self.containers)

    def _remove(self, names: list[str]) -> list[str]:
        present = {c.name for c in self.containers}
        gone = [name for name in names if name in present]
        if gone:
            self.calls.append(f"remove:{','.join(sorted(gone))}")
        self.removed.extend(gone)
        self.containers = [c for c in self.containers if c.name not in set(gone)]
        return gone

    def names(self) -> list[str]:
        return sorted(c.name for c in self.containers)


class _Process:
    """A `Popen` stand-in whose willingness to die is scripted.

    `dies_on` is the one signal this process honours: `SIGTERM` models a
    well-behaved harness, `SIGKILL` one that ignores the polite request, and
    `None` one blocked in the kernel that ignores even `SIGKILL`. Equality
    rather than "this signal or stronger", because signal numbers do not order
    by severity — `SIGKILL` is 9 and `SIGTERM` is 15 — and a comparison here
    made the ignores-SIGTERM case die on the first signal it got.
    """

    def __init__(self, pid: int = 4242, dies_on: int | None = signal.SIGTERM) -> None:
        self.pid = pid
        self.dies_on = dies_on
        self.signals: list[int] = []
        self.returncode: int | None = None

    def poll(self) -> int | None:
        return self.returncode

    def wait(self, timeout: float | None = None) -> int:
        if self.returncode is None:
            raise subprocess.TimeoutExpired(cmd="harbor", timeout=timeout or 0.0)
        return self.returncode

    def receive(self, signum: int) -> None:
        self.signals.append(signum)
        if signum == self.dies_on:
            self.returncode = -signum


@pytest.fixture
def signals(monkeypatch: pytest.MonkeyPatch) -> dict[int, _Process]:
    """Route `os.killpg` to the `_Process` registered under that pgid."""
    table: dict[int, _Process] = {}
    monkeypatch.setattr(reauth_wiring.os, "getpgid", lambda pid: pid)
    monkeypatch.setattr(
        reauth_wiring.os,
        "killpg",
        lambda pgid, signum: table[pgid].receive(signum),
    )
    return table


def _match_with_trials(tmp_path: Path, trials: list[str], status: str):
    """A match whose seat wrote ``trials`` on disk, in the given status."""
    spec = MatchSpec.from_json(
        {
            "id": "m" + status[:2],
            "dataset": "terminal-bench-2.1",
            "tasks": ["alpha"],
            "sut_ref": "",
            "contestants": [
                {
                    "name": "cc",
                    "agent": "claude-code",
                    "engine": {"api": "anthropic", "model": "claude-fable-5"},
                }
            ],
        }
    )
    runner = MatchRunner(DEFAULT_REGISTRY, tmp_path / f"ws-{status}")
    match = runner.create(spec)
    seat = spec.contestants[0]
    job_dir = match.jobs_root / f"{spec.id}-{seat.slug}"
    for trial in trials:
        (job_dir / trial).mkdir(parents=True)
    match.runs[seat.id] = ContestantRun(
        contestant=seat,
        job_name=job_dir.name,
        job_dir=job_dir,
        log_path=match.workspace / "cc.log",
    )
    match.status = status
    return runner, match


class TestAMatchOwnsNamableContainers:
    """Reaping is tractable because a match's trial ids name its containers."""

    def test_every_attempt_gets_its_own_container_name(self, tmp_path: Path) -> None:
        """Attempts must not be collapsed the way the scoreboard collapses them.

        `Match.trial_dirs` folds `alpha__1` and `alpha__2` onto one row so the
        grid stays one row per task. Reaping through that view would leave
        every attempt but one behind.
        """
        _runner, match = _match_with_trials(
            tmp_path, ["alpha__1", "alpha__2", "beta__1"], "cancelled"
        )
        assert reap.match_containers(match) == [
            "alpha__1-main-1",
            "alpha__2-main-1",
            "beta__1-main-1",
        ]
        assert len(match.trial_dirs(match.spec.contestants[0].id)) == 2

    def test_a_match_that_never_wrote_a_trial_owns_nothing(self, tmp_path: Path) -> None:
        _runner, match = _match_with_trials(tmp_path, [], "cancelled")
        assert reap.match_containers(match) == []


class TestReapOnStop:
    """The witness for the leak itself: cancelling must remove the containers."""

    def test_cancelling_removes_the_matchs_containers(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        runner, match = _match_with_trials(
            tmp_path, ["alpha__1", "beta__1"], "running"
        )
        daemon = _FakeDaemon(["alpha__1-main-1", "beta__1-main-1", "someone-else-1"])
        daemon.install(monkeypatch)

        runner.cancel(match)

        assert sorted(daemon.removed) == ["alpha__1-main-1", "beta__1-main-1"]
        assert daemon.running == ["someone-else-1"], "reaped a container it does not own"
        assert "reaped 2 container(s)" in match.note

    def test_a_host_that_cannot_be_asked_never_fails_the_cancel(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """No docker, no daemon, a wedged socket — cancel still cancels.

        Best-effort by contract: a host where nothing can be removed is not a
        host where the cancel should raise.
        """
        runner, match = _match_with_trials(tmp_path, ["alpha__1"], "running")

        def explode(*_args: object, **_kwargs: object) -> object:
            raise OSError("no docker on PATH")

        monkeypatch.setattr(reap.subprocess, "run", explode)
        runner.cancel(match)
        assert match.status == "cancelled"


class TestStopMeansStopped:
    """The operator's stop must leave nothing of the match running.

    Three defects, each of which let something survive a stop:

    * ``cancel`` sent ``SIGTERM`` and never looked back, so a Harbor mid
      ``docker compose up`` finished bringing that project up **after** the
      sweep had already listed the daemon;
    * the sweep only ever named ``<trial>-main-1``, so a task with a sidecar
      kept ``<trial>-db-1``;
    * and a compose project whose trial directory had not been written yet was
      not nameable at all, so nothing looked for it.
    """

    def test_a_process_that_ignores_sigterm_is_killed(
        self, signals: dict[int, _Process]
    ) -> None:
        process = _Process(pid=11, dies_on=signal.SIGKILL)
        signals[11] = process

        assert reauth_wiring.stop_process_group(process, grace=0.0, kill_grace=0.0)
        assert process.signals == [signal.SIGTERM, signal.SIGKILL]

    def test_a_process_that_dies_politely_is_never_killed(
        self, signals: dict[int, _Process]
    ) -> None:
        process = _Process(pid=12, dies_on=signal.SIGTERM)
        signals[12] = process

        assert reauth_wiring.stop_process_group(process, grace=0.0, kill_grace=0.0)
        assert process.signals == [signal.SIGTERM], "escalated for no reason"

    def test_a_process_surviving_sigkill_is_reported_not_assumed_dead(
        self, signals: dict[int, _Process]
    ) -> None:
        """The honest answer when the stop did not stop. Returning ``True``
        here would tell `cancel` it is safe to reap — while the thing that
        creates containers is still running."""
        process = _Process(pid=13, dies_on=None)
        signals[13] = process

        assert not reauth_wiring.stop_process_group(process, grace=0.0, kill_grace=0.0)

    def test_nothing_is_reaped_until_every_process_is_gone(
        self,
        tmp_path: Path,
        monkeypatch: pytest.MonkeyPatch,
        signals: dict[int, _Process],
    ) -> None:
        """The ordering witness, and the race the old `cancel` lost.

        Reaping while the harness is alive removes the containers that exist
        and leaves it free to create the next trial's — into a match nobody is
        watching. So the first daemon interaction of a cancel must come after
        the last signal, which is what this asserts by watching both orders at
        once.
        """
        runner, match = _match_with_trials(tmp_path, ["alpha__1"], "running")
        seat = match.spec.contestants[0]
        process = _Process(pid=21, dies_on=signal.SIGTERM)
        signals[21] = process
        match.runs[seat.id].process = process

        daemon = _Daemon([reap.Container("alpha__1-main-1", "alpha__1", running=True)])
        daemon.install(monkeypatch)
        monkeypatch.setattr(reauth_wiring, "STOP_GRACE_SECONDS", 0.0)

        runner.cancel(match)

        assert process.signals, "the seat was never signalled"
        assert daemon.calls, "the daemon was never asked"
        assert daemon.removed == ["alpha__1-main-1"]
        # The real assertion: `_Process.receive` appends to `process.signals`
        # only while alive, and `_Daemon` records every interaction — so a
        # daemon call recorded before the process exited is the race.
        assert process.returncode is not None

    def test_a_sidecar_container_is_reaped_with_its_trial(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """`container_for_trial` can only name the `main` service. A task that
        brings up a database beside it owns two containers, and naming one of
        them is the leak wearing a different service name."""
        _runner, match = _match_with_trials(tmp_path, ["alpha__1"], "cancelled")
        daemon = _Daemon(
            [
                reap.Container("alpha__1-main-1", "alpha__1", running=True),
                reap.Container("alpha__1-db-1", "alpha__1", running=True),
                reap.Container("someone-else-main-1", "someone-else", running=True),
            ]
        )
        daemon.install(monkeypatch)

        assert sorted(reap.reap_match(match)) == ["alpha__1-db-1", "alpha__1-main-1"]
        assert daemon.names() == ["someone-else-main-1"], "reaped another match's"

    def test_a_project_with_no_trial_directory_is_still_reaped(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """A compose project that came up in the seconds before the stop has
        no trial directory to be named from — but it carries the label, and the
        label is what the daemon can be asked about."""
        _runner, match = _match_with_trials(tmp_path, ["alpha__1"], "cancelled")
        daemon = _Daemon(
            [
                reap.Container("alpha__1-main-1", "alpha__1", running=True),
                # Same project, a service the name scheme does not predict.
                reap.Container("alpha__1_sidecar", "alpha__1", running=True),
            ]
        )
        daemon.install(monkeypatch)

        assert sorted(reap.reap_match(match)) == ["alpha__1-main-1", "alpha__1_sidecar"]
        assert daemon.names() == []

    def test_a_host_with_no_daemon_still_reaps_what_it_can_name(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The derivable half must not depend on the daemon answering: a
        `docker ps` that fails is a host where nothing is known, and falling
        back to the trial's own name is strictly better than reaping nothing."""
        _runner, match = _match_with_trials(tmp_path, ["alpha__1"], "cancelled")
        monkeypatch.setattr(reap, "list_containers", list)

        assert reap.match_containers(match) == ["alpha__1-main-1"]


class TestReapOnStart:
    """`kill -9` runs no exit hook, so the next launch sweeps what it can."""

    def test_a_finished_matchs_containers_are_swept_before_the_next_launch(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        _old_runner, dead = _match_with_trials(tmp_path, ["alpha__1"], "finished")
        daemon = _FakeDaemon(["alpha__1-main-1"])
        daemon.install(monkeypatch)

        assert reap.reap_finished([dead]) == ["alpha__1-main-1"]
        assert daemon.running == []

    def test_a_running_matchs_containers_are_never_swept(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The hazard the sweep must not become.

        Reaping a live match's container manufactures exactly the mis-scored
        loss this module exists to prevent — and an arena cannot see a match
        `arenabench run` started in another process (#2326), so "I don't
        recognise it" must mean "leave it".
        """
        _runner, live = _match_with_trials(tmp_path, ["alpha__1"], "running")
        daemon = _FakeDaemon(["alpha__1-main-1", "unknown-to-us__x-main-1"])
        daemon.install(monkeypatch)

        assert reap.reap_finished([live]) == []
        assert daemon.running == ["alpha__1-main-1", "unknown-to-us__x-main-1"]

    def test_a_running_container_refuses_the_sweep_whatever_the_status_says(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The interlock behind the status.

        A match's status is this process's *belief*, and for one restored from
        disk it is derived rather than observed — so "over" can be wrong. A
        running container cannot be: something is executing that trial right
        now, and sweeping it manufactures the mis-scored loss (#2326).
        """
        _runner, dead = _match_with_trials(tmp_path, ["alpha__1"], "finished")
        daemon = _Daemon([reap.Container("alpha__1-main-1", "alpha__1", running=True)])
        daemon.install(monkeypatch)

        assert reap.match_is_live(dead)
        assert reap.reap_finished([dead]) == []
        assert daemon.names() == ["alpha__1-main-1"]

    def test_an_exited_container_is_residue_and_is_swept(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The other side of the same interlock: existing is not running, and
        refusing to sweep an exited container would defeat the module."""
        _runner, dead = _match_with_trials(tmp_path, ["alpha__1"], "finished")
        daemon = _Daemon([reap.Container("alpha__1-main-1", "alpha__1", running=False)])
        daemon.install(monkeypatch)

        assert not reap.match_is_live(dead)
        assert reap.reap_finished([dead]) == ["alpha__1-main-1"]

    def test_starting_a_match_sweeps_the_ones_already_over(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """End to end through `MatchRunner.start`, which is where headroom is
        needed and therefore where the sweep has to happen."""
        runner, dead = _match_with_trials(tmp_path, ["alpha__1"], "finished")
        daemon = _FakeDaemon(["alpha__1-main-1"])
        daemon.install(monkeypatch)

        fresh = runner.create(
            MatchSpec.from_json(
                {
                    "id": "next",
                    "dataset": "terminal-bench-2.1",
                    "tasks": ["alpha"],
                    "sut_ref": "",
                    "contestants": [
                        {
                            "name": "cc",
                            "agent": "claude-code",
                            "engine": {"api": "anthropic", "model": "claude-fable-5"},
                        }
                    ],
                }
            )
        )
        runner.matches[dead.spec.id] = dead
        runner.start(fresh)
        deadline = time.monotonic() + 10.0
        while fresh.status == "running" and time.monotonic() < deadline:
            time.sleep(0.05)

        assert daemon.removed == ["alpha__1-main-1"]


class TestAComposeFailureIsNotAnAgentLoss:
    """A trial whose container never came up asked the agent nothing.

    Classified on the message, and only for this one shape: Harbor raises a
    bare ``RuntimeError``, and that name is far too broad to put in
    ``INFRASTRUCTURE_FAILURES`` — an honest agent-side ``RuntimeError`` would
    leave the denominator with it.
    """

    COMPOSE: ClassVar[dict[str, str]] = {
        "exception_type": "RuntimeError",
        "exception_message": (
            "Docker compose command failed for sqlite-with-gcov__gaw7dfm"
        ),
    }

    def test_the_compose_failure_is_recognised(self) -> None:
        assert is_compose_failure(self.COMPOSE)

    def test_another_runtime_error_is_still_the_agents(self) -> None:
        assert not is_compose_failure(
            {"exception_type": "RuntimeError", "exception_message": "the agent gave up"}
        )

    def test_a_message_alone_is_not_enough(self) -> None:
        """A named agent failure quoting a compose error stays an agent failure."""
        assert not is_compose_failure(
            {
                "exception_type": "NonZeroAgentExitCodeError",
                "exception_message": "Docker compose command failed for x",
            }
        )

    def test_a_composed_out_trial_stays_out_of_the_denominator(
        self, tmp_path: Path
    ) -> None:
        """The witness at the layer the scoreboard reads.

        Without it the trial scores `resolved=False` — a loss — for an agent
        that was never started.
        """
        import json

        from arenabench.telemetry import MetricsReader

        trial = tmp_path / "job" / "sqlite-with-gcov__gaw7dfm"
        (trial / "agent").mkdir(parents=True)
        (trial / "result.json").write_text(
            json.dumps({"exception_info": self.COMPOSE}), encoding="utf-8"
        )
        metrics = MetricsReader().read(trial, "sqlite-with-gcov")
        assert metrics.infrastructure is True
        assert metrics.resolved is None, "a trial that never started is not a loss"


class TestAMeasuredAttemptIsNeverHiddenBehindAVoid:
    """A scoreboard that hides a result the seat produced reads too low.

    ``Match.trial_dirs`` collapses ``<task>__<attempt>`` to one row per task by
    keeping whichever attempt sorted first — and an attempt id says nothing
    about whether that attempt measured anything. With ``--n-attempts > 1``, a
    seat whose first attempt died on a revoked credential and whose second
    *solved the task* published the void: ``resolved=None``, the pass invisible,
    the arm reading worse than it performed.

    This is the design problem #2545 names as the prerequisite for
    re-dispatching a rotated-out seat: a retry that lands beside a voided trial
    must be able to be seen, or the feature introduces a scoring regression.
    """

    def _attempts(self, tmp_path: Path, attempts: dict[str, dict]):
        import json

        _runner, match = _match_with_trials(tmp_path, [], "running")
        seat = match.spec.contestants[0]
        job_dir = match.runs[seat.id].job_dir
        for name, result in attempts.items():
            trial = job_dir / name
            (trial / "agent").mkdir(parents=True)
            (trial / "result.json").write_text(json.dumps(result), encoding="utf-8")
        return match, seat

    VOID: ClassVar[dict] = {"exception_info": {"exception_type": "AuthenticationError"}}
    SOLVED: ClassVar[dict] = {"verifier_result": {"rewards": {"reward": 1.0}}}
    LOST: ClassVar[dict] = {"verifier_result": {"rewards": {"reward": 0.0}}}

    def test_a_solved_retry_beats_a_voided_first_attempt(self, tmp_path: Path) -> None:
        """The witness: sorted order puts the void first, and it must not win."""
        match, seat = self._attempts(
            tmp_path, {"alpha__aaa": self.VOID, "alpha__bbb": self.SOLVED}
        )
        assert match.trial_dirs(seat.id)["alpha"].name == "alpha__bbb"
        metrics = match.metrics_for(seat.id)["alpha"]
        assert metrics.resolved is True
        assert metrics.reward == 1.0

    def test_a_measured_loss_also_beats_a_void(self, tmp_path: Path) -> None:
        """Measured is measured. The rank is about *whether* the agent was
        asked, never about what it answered."""
        match, seat = self._attempts(
            tmp_path, {"alpha__aaa": self.VOID, "alpha__bbb": self.LOST}
        )
        assert match.trial_dirs(seat.id)["alpha"].name == "alpha__bbb"
        assert match.metrics_for(seat.id)["alpha"].resolved is False

    def test_a_win_never_replaces_a_measured_loss(self, tmp_path: Path) -> None:
        """The property that keeps this honest.

        Promoting the best of several *measured* attempts would be re-running
        a task until it passes and reporting the pass. Two measured attempts
        rank equal, so sort order decides exactly as it always did.
        """
        match, seat = self._attempts(
            tmp_path, {"alpha__aaa": self.LOST, "alpha__bbb": self.SOLVED}
        )
        assert match.trial_dirs(seat.id)["alpha"].name == "alpha__aaa"
        assert match.metrics_for(seat.id)["alpha"].resolved is False

    def test_a_lone_void_is_still_reported_as_a_void(self, tmp_path: Path) -> None:
        """Nothing is invented: with no measured attempt the void stands, and
        stays out of the denominator."""
        match, seat = self._attempts(tmp_path, {"alpha__aaa": self.VOID})
        metrics = match.metrics_for(seat.id)["alpha"]
        assert metrics.resolved is None
        assert metrics.infrastructure is True

    def test_an_in_flight_attempt_outranks_a_void_but_not_a_result(
        self, tmp_path: Path
    ) -> None:
        match, seat = self._attempts(tmp_path, {"alpha__aaa": self.VOID})
        (match.runs[seat.id].job_dir / "alpha__bbb" / "agent").mkdir(parents=True)
        assert match.trial_dirs(seat.id)["alpha"].name == "alpha__bbb"

        match2, seat2 = self._attempts(tmp_path / "two", {"alpha__aaa": self.SOLVED})
        (match2.runs[seat2.id].job_dir / "alpha__bbb" / "agent").mkdir(parents=True)
        assert match2.trial_dirs(seat2.id)["alpha"].name == "alpha__aaa"
