"""Reaping the agent when Harbor's deadline cancels the run (#2131).

Harbor enforces the per-trial agent deadline by cancelling the coroutine that
awaits the exec client. Before this module's subject existed, that was the
whole of the timeout: the ``docker compose exec`` *client* went away and the
Stella process it was proxying for kept running in the container — 414 seconds
of it in match ``cc00894779ff``, billing model calls and mutating the very
filesystem the verifier had already started scoring, and appending to the
journal the adapter re-reads for the trial's usage (which is why every timeout
trial under-reported by 16–35%).

These tests use real local processes, in the shapes that matter:

* an in-container agent, modelled as a process in its own session that the
  host-side client cannot reach by signal — only an explicit reap can;
* a client with a child of its own, which a signal to the leader alone leaves
  orphaned;
* the reap program itself, run against a real process through ``/proc``.

The completion branch (#2122) is pinned here too, from the other side: a run
that ends on its own must issue no reap at all, because the daemons a finished
agent left behind are what the verifier is about to probe.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import shlex
import shutil
import signal
import subprocess
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

pytest.importorskip("harbor", reason="Harbor is required to import the adapter")

import stella_harbor  # noqa: E402
from stella_harbor import (  # noqa: E402
    _INSTALL_PATH,
    _secure_exec_with_credential_fd,
)
from stella_harbor.stream_release import (  # noqa: E402
    communicate_with_release,
    terminate_client,
)
from stella_harbor.timeout_reap import (  # noqa: E402
    REAP_LOG_PREFIX,
    container_reap_argv,
    container_reap_program,
    exec_with_agent_reap,
)

_CREDENTIAL = "timeout-reap-witness-secret"


def _pid_alive(pid: int) -> bool:
    """Whether ``pid`` still exists, without waiting on it."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:  # pragma: no cover - not reachable for own children
        return True
    return True


def _await_condition(predicate, *, timeout: float = 10.0) -> bool:
    """Poll ``predicate`` from synchronous test code until it holds."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.05)
    return predicate()


class _JournalWriter:
    """A stand-in for Stella running inside the container.

    In its own session on purpose: the container is a different namespace, so
    no signal aimed at the host-side exec client can reach the real thing
    either. Only an explicit reap can, which is exactly what is under test.
    """

    def __init__(self, journal: Path) -> None:
        self.journal = journal
        self.process = subprocess.Popen(  # noqa: S603 - fixture, fixed argv
            [
                "sh",
                "-c",
                f'i=0; while [ "$i" -lt 600 ]; do echo "event $i" '
                f">> {shlex.quote(str(journal))}; sleep 0.05; i=$((i + 1)); done",
            ],
            start_new_session=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

    @property
    def pid(self) -> int:
        return self.process.pid

    def running(self) -> bool:
        """Whether the stand-in is still alive — reaping it as we ask.

        ``os.kill(pid, 0)`` cannot answer this: a killed child of the test
        process is a zombie until something waits on it, and a zombie answers
        signal 0 exactly as a live process does.
        """
        return self.process.poll() is None

    def events(self) -> int:
        try:
            return len(self.journal.read_text().splitlines())
        except OSError:
            return 0

    def close(self) -> None:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(self.process.pid, signal.SIGKILL)
        with contextlib.suppress(subprocess.TimeoutExpired):
            self.process.wait(timeout=5)


def _fake_compose_driver(tmp_path: Path, journal_writer: _JournalWriter) -> Path:
    """A stand-in for ``docker compose`` that answers both invocations.

    The run invocation holds the exec stream open, as a live agent's does. The
    reap invocation is recognised by the marker the real reap program carries,
    and stops the in-container stand-in — which is precisely the authority the
    adapter must exercise and previously never did.
    """
    driver = tmp_path / "compose-driver"
    driver.write_text(
        "#!/bin/sh\n"
        'case "$*" in\n'
        f"    *{REAP_LOG_PREFIX}*)\n"
        f"        kill -TERM {journal_writer.pid} 2>/dev/null || true\n"
        f"        echo '{REAP_LOG_PREFIX} the stand-in agent was reaped'\n"
        "        exit 0\n"
        "        ;;\n"
        "esac\n"
        "exec sleep 60\n",
        encoding="utf-8",
    )
    driver.chmod(0o755)
    return driver


def _fake_environment(tmp_path: Path) -> SimpleNamespace:
    return SimpleNamespace(
        session_id="trial/timeout-reap",
        environment_dir=tmp_path,
        _docker_compose_paths=[],
        _env_vars=SimpleNamespace(
            to_env_dict=lambda *, include_os_env: {"PATH": os.environ.get("PATH", "")}
        ),
        _compose_task_env={},
        _persistent_env={},
        task_env_config=SimpleNamespace(workdir="/workspace"),
        _resolve_user=lambda _user: "agent",
    )


async def _spawn(script: str) -> asyncio.subprocess.Process:
    """A client shaped like the compose exec client: its own session."""
    return await asyncio.create_subprocess_exec(
        "sh",
        "-c",
        script,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
        start_new_session=True,
    )


class TestContainerAgentReap:
    def test_a_cancelled_run_reaps_the_container_agent(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The witness for #2131, end to end through the secure runner.

        Harbor's deadline arrives as a cancellation of the awaited exec. Before
        the fix the cancellation stopped at the client, the agent kept writing,
        and the journal the adapter re-reads for the trial's usage was still
        growing while the verifier ran. After it, the agent is reaped and the
        journal is final — which is what makes that re-read honest.
        """
        journal = tmp_path / "stella-events.jsonl"
        agent = _JournalWriter(journal)
        try:
            driver = _fake_compose_driver(tmp_path, agent)
            monkeypatch.setattr(
                stella_harbor, "_compose_base_argv", lambda _environment: [str(driver)]
            )
            monkeypatch.setattr(
                stella_harbor,
                "_compose_host_environment",
                lambda _environment: {"PATH": os.environ.get("PATH", "")},
            )

            async def scenario() -> None:
                task = asyncio.ensure_future(
                    _secure_exec_with_credential_fd(
                        _fake_environment(tmp_path),
                        command=[
                            _INSTALL_PATH,
                            "--model",
                            "openrouter/model",
                            "run",
                            "task",
                        ],
                        env={"STELLA_CREDENTIAL_HANDOFF_FD": "0"},
                        credentials=(_CREDENTIAL,),
                    )
                )
                # Let the run get going, then time it out the way Harbor does.
                while agent.events() < 3:
                    await asyncio.sleep(0.05)
                task.cancel()
                with contextlib.suppress(asyncio.CancelledError):
                    await task

            asyncio.run(scenario())

            assert _await_condition(lambda: not agent.running()), (
                "the agent outlived the cancelled await: it is still running "
                "alongside the verifier, still billing, still writing"
            )
            settled = agent.events()
            time.sleep(0.5)
            assert agent.events() == settled, (
                "the journal kept growing after the timeout, so the usage the "
                "adapter reconstructs from it is a moving target"
            )
        finally:
            agent.close()

    def test_a_completed_run_issues_no_reap(self, tmp_path: Path) -> None:
        """Composition with #2122: the completion branch signals nothing.

        A run that ended on its own leaves daemons the verifier is about to
        probe. Reaping there would break the trials the exec-stream release was
        built for, so the reap must be reachable only from the failure path.
        """
        marker = tmp_path / "reap-was-issued"
        driver = tmp_path / "compose-driver"
        driver.write_text(
            "#!/bin/sh\n"
            'case "$*" in\n'
            f"    *{REAP_LOG_PREFIX}*) : > {shlex.quote(str(marker))} ;;\n"
            "esac\n"
            "echo done\n",
            encoding="utf-8",
        )
        driver.chmod(0o755)

        result = asyncio.run(
            exec_with_agent_reap(
                [str(driver), "run"],
                [str(driver)],
                {"PATH": os.environ.get("PATH", "")},
                b"",
                None,
                _INSTALL_PATH,
            )
        )

        assert result.stdout == "done\n"
        assert result.return_code == 0
        assert not marker.exists(), "a finished run must not be reaped"


class TestClientTeardown:
    def test_a_cancelled_wait_reaps_the_clients_children(self) -> None:
        """A signal to the leader alone orphans everything it spawned.

        The compose client is one process among the several a teardown has to
        account for; terminating only the leader left its children running and
        reaped nothing. The client is spawned into its own session so the group
        signal is provably scoped to it and can never reach the harness.
        """
        holder: dict[str, int] = {}

        async def scenario() -> None:
            process = await _spawn("sleep 60 & echo $!; exec sleep 60")
            assert process.stdout is not None
            holder["child"] = int((await process.stdout.readline()).strip())
            holder["client"] = process.pid
            task = asyncio.ensure_future(communicate_with_release(process, b"", None))
            await asyncio.sleep(0.2)
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task

        try:
            asyncio.run(scenario())
            assert not _pid_alive(holder["client"]), "the client was not reaped"
            assert _await_condition(lambda: not _pid_alive(holder["child"])), (
                "the client's child outlived the teardown"
            )
        finally:
            for pid in holder.values():
                with contextlib.suppress(ProcessLookupError):
                    os.kill(pid, signal.SIGKILL)

    def test_a_client_that_leads_no_session_is_signalled_alone(self) -> None:
        """The safety interlock: never group-signal the harness's own group.

        A client spawned without its own session shares the group pytest runs
        in. The teardown must fall back to signalling that one process — a
        group signal there would take the whole benchmark run with it.
        """

        async def scenario() -> int:
            process = await asyncio.create_subprocess_exec(
                "sleep",
                "60",
                stdout=asyncio.subprocess.PIPE,
            )
            assert os.getpgid(process.pid) != process.pid
            await terminate_client(process, grace_seconds=2.0)
            return process.returncode

        assert asyncio.run(scenario()) is not None


class TestReapProgram:
    def test_the_reap_argv_runs_as_root_in_the_task_service(self) -> None:
        argv = container_reap_argv(["docker", "compose", "-p", "trial"], _INSTALL_PATH)
        assert argv[:4] == ["docker", "compose", "-p", "trial"]
        assert argv[4:9] == ["exec", "-T", "-u", "0", "main"]
        assert argv[9:11] == ["sh", "-c"]
        assert _INSTALL_PATH in argv[11]

    def test_the_reap_program_is_valid_posix_shell(self, tmp_path: Path) -> None:
        program = tmp_path / "reap.sh"
        program.write_text(container_reap_program(_INSTALL_PATH), encoding="utf-8")
        assert subprocess.run(  # noqa: S603 - fixed argv
            ["sh", "-n", str(program)], capture_output=True
        ).returncode == 0

    @pytest.mark.skipif(
        not Path("/proc/self/exe").exists(),
        reason="the reap identifies the agent through /proc (Linux containers)",
    )
    def test_the_reap_program_kills_the_agent_process_group(
        self, tmp_path: Path
    ) -> None:
        """Run the real program against a real process group.

        The agent is identified by ``/proc/<pid>/exe``, so the stand-in has to
        be a copied binary at its own path rather than an interpreter running a
        script — that is the identification the program actually performs.
        """
        agent_binary = tmp_path / "stella"
        shutil.copy(shutil.which("sleep") or "/bin/sleep", agent_binary)
        agent_binary.chmod(0o755)
        agent = subprocess.Popen(  # noqa: S603 - fixture, fixed argv
            [str(agent_binary), "60"], start_new_session=True
        )
        try:
            completed = subprocess.run(  # noqa: S603 - fixed argv
                ["sh", "-c", container_reap_program(str(agent_binary))],
                capture_output=True,
                text=True,
                timeout=30,
            )
            assert completed.returncode == 0, completed.stderr
            assert str(agent.pid) in completed.stdout
            assert "survivors: none" in completed.stdout
            assert agent.wait(timeout=5) != 0
        finally:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(agent.pid, signal.SIGKILL)
            with contextlib.suppress(subprocess.TimeoutExpired):
                agent.wait(timeout=5)

    def test_the_reap_program_is_quiet_when_no_agent_is_running(
        self, tmp_path: Path
    ) -> None:
        """An agent that already exited is the ordinary case on a slow client.

        The program must report that and exit clean rather than signalling a
        pid it guessed, and never with the shell's ``set -u`` tripping over an
        empty match.
        """
        completed = subprocess.run(  # noqa: S603 - fixed argv
            ["sh", "-c", container_reap_program(str(tmp_path / "absent-stella"))],
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert completed.returncode == 0, completed.stderr
        assert f"{REAP_LOG_PREFIX} no agent process is running" in completed.stdout


def test_the_reap_never_displaces_the_timeout(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A reap that cannot run must not turn a timeout into something else.

    Harbor scores the trial by the exception it receives. A Docker daemon that
    has itself wedged is exactly when the reap fails, and exactly when the
    trial most needs to still read as a timeout.
    """
    driver = tmp_path / "compose-driver"
    driver.write_text("#!/bin/sh\nexec sleep 60\n", encoding="utf-8")
    driver.chmod(0o755)

    async def _refuse(*_args: object, **_kwargs: object) -> None:
        raise OSError("docker is unavailable")

    async def scenario() -> None:
        task = asyncio.ensure_future(
            exec_with_agent_reap(
                [str(driver), "run"],
                [str(driver)],
                {"PATH": os.environ.get("PATH", "")},
                b"",
                None,
                _INSTALL_PATH,
            )
        )
        await asyncio.sleep(0.2)
        monkeypatch.setattr(asyncio, "create_subprocess_exec", _refuse)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

    asyncio.run(scenario())
