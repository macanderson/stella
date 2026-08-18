"""`stella init` is where the index is embedded, so init needs the credential.

The run path already hands `VOYAGE_API_KEY` over on the anonymous-FD wire, but
`_build_code_graph` used a plain agent exec with no credential at all. Init
therefore logged "semantic index: skipped — no embedding backend configured",
and the credential arriving later at run time had no vectors to rank: `search`
degraded to lexical for the whole trial while the trial metadata still
disclosed an embedding credential as configured.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest

from stella_harbor import StellaAgent, _secure_setup_exec_with_credential_fd


class _RecordingEnvironment:
    """Stands in for Harbor's Docker environment via the adapter's test hook."""

    def __init__(self) -> None:
        self.task_env_config = SimpleNamespace(workdir="/app")
        self.secure_calls: list[dict] = []
        self.agent_execs: list[str] = []

    async def _stella_secure_exec_with_stdin(
        self, *, command: list[str], env: dict[str, str], stdin: bytes
    ):
        self.secure_calls.append({"command": command, "env": env, "stdin": stdin})
        return SimpleNamespace(
            stdout="✓ semantic index: 1 files embedded", return_code=0
        )


class TestInitReceivesTheEmbeddingCredential:
    def test_the_credential_rides_stdin_and_never_the_exec_argv(self) -> None:
        secret = "voyage-init-secret"
        environment = _RecordingEnvironment()

        result = asyncio.run(
            _secure_setup_exec_with_credential_fd(
                environment,
                command=["/usr/local/bin/stella", "init"],
                env={},
                credentials={"VOYAGE_API_KEY": secret},
            )
        )
        assert result.return_code == 0

        (call,) = environment.secure_calls
        assert call["command"] == ["/usr/local/bin/stella", "init"]
        # The value goes on the pipe...
        assert call["stdin"] == f"{secret}\n".encode()
        # ...named positionally, never as an environment value.
        assert call["env"]["STELLA_CREDENTIAL_HANDOFF_TARGET"] == "VOYAGE_API_KEY"
        assert call["env"]["STELLA_CREDENTIAL_HANDOFF_FD"] == "0"
        assert secret not in "".join(call["env"].values())

    def test_a_setup_command_must_still_be_direct_stella_argv(self) -> None:
        """The run path's argv guard is not relaxed for setup."""
        with pytest.raises(RuntimeError, match="direct stella argv"):
            asyncio.run(
                _secure_setup_exec_with_credential_fd(
                    _RecordingEnvironment(),
                    command=["/bin/sh", "-c", "stella init"],
                    env={},
                    credentials={"VOYAGE_API_KEY": "x"},
                )
            )

    def test_a_newline_in_a_value_cannot_reframe_the_pipe(self) -> None:
        with pytest.raises(RuntimeError, match="line break"):
            asyncio.run(
                _secure_setup_exec_with_credential_fd(
                    _RecordingEnvironment(),
                    command=["/usr/local/bin/stella", "init"],
                    env={},
                    credentials={"VOYAGE_API_KEY": "one\ntwo"},
                )
            )

    def test_build_code_graph_routes_init_through_the_handoff(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The wiring, not just the helper: init must actually use it."""
        monkeypatch.setenv("VOYAGE_API_KEY", "voyage-wired-secret")
        environment = _RecordingEnvironment()
        agent = StellaAgent.__new__(StellaAgent)
        agent._configured_value = lambda name: __import__("os").environ.get(name)

        asyncio.run(StellaAgent._build_code_graph(agent, environment))

        assert environment.agent_execs == []  # never the plain, credential-less exec
        (call,) = environment.secure_calls
        assert call["command"] == ["/usr/local/bin/stella", "init"]
        assert call["stdin"] == b"voyage-wired-secret\n"

    def test_no_backend_configured_still_runs_init(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """No credential is the honest no-backend posture, never a failure."""
        monkeypatch.delenv("VOYAGE_API_KEY", raising=False)
        environment = _RecordingEnvironment()
        execed: list[str] = []

        async def exec_as_agent(_env, command: str, timeout_sec: int | None = None):
            execed.append(command)
            return SimpleNamespace(stdout="· semantic index: skipped", return_code=0)

        agent = StellaAgent.__new__(StellaAgent)
        agent._configured_value = lambda name: None
        agent.exec_as_agent = exec_as_agent

        asyncio.run(StellaAgent._build_code_graph(agent, environment))

        assert environment.secure_calls == []
        assert execed and execed[0].endswith("/usr/local/bin/stella init")
