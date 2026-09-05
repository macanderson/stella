"""Unit tests for :mod:`stella_harbor.code_graph`'s pure parsers.

``from_stdout`` and ``unavailable`` are the only place a trial's metadata
learns what ``stella init`` actually did in the container. Before #3669 the
semantic-index outcome was not parsed at all — a trial could not say whether
embeddings were built, skipped for want of a backend, or attempted and
incomplete, because the parser structurally could not emit any of the three.
Before #3670 a failed exec's exit code and stderr were only ever reachable by
re-parsing the exception's ``str()`` by eye.

``TestUnavailable`` covers ``_NONZERO_EXIT_PATTERN`` against a message the
test wrote. ``TestUnavailableAgainstHarbor`` covers it against the message
**Harbor** writes, which is the only one that ships (#4899): the parser is
pinned to Harbor's behaviour by a regex and to Harbor's version by nothing,
so a reordered line or a renamed ``stderr:`` prefix on an upgrade would drop
``exit_code`` and ``stderr`` from every trial's metadata with the whole suite
still green.

In its own file rather than ``test_adapter.py``, which is grandfathered at
its file-size ceiling and closed to growth.
"""

from __future__ import annotations

import asyncio
import logging
from types import SimpleNamespace

import pytest

from stella_harbor import code_graph

pytest.importorskip("harbor", reason="Harbor is required to produce its own message")

from harbor.agents.installed.base import (  # noqa: E402 - after importorskip by design
    NonZeroAgentExitCodeError,
)

from stella_harbor import StellaAgent  # noqa: E402 - after importorskip by design


class TestFromStdoutCodeGraphLine:
    """The pre-existing ``code graph:`` classification is unchanged by the
    semantic-index work landing beside it."""

    def test_reports_the_code_graph_summary_line(self) -> None:
        stdout = "some banner\n✓ code graph: 6 symbols, 3 imports across 1 file\nmore\n"
        result = code_graph.from_stdout(stdout)
        assert result["state"] == "reported"
        assert result["summary"] == "✓ code graph: 6 symbols, 3 imports across 1 file"

    def test_no_summary_line_when_init_says_nothing_about_it(self) -> None:
        result = code_graph.from_stdout("only unrelated output\n")
        assert result["state"] == "no_summary_line"
        assert "1 line" in result["detail"]

    def test_no_summary_line_on_empty_stdout(self) -> None:
        result = code_graph.from_stdout(None)
        assert result["state"] == "no_summary_line"


class TestFromStdoutSemanticIndex:
    """The three (at minimum) distinguishable semantic-index outcomes."""

    def test_skipped_no_backend(self) -> None:
        stdout = (
            "· semantic index: skipped — no embedding backend configured (set "
            "VOYAGE_API_KEY, OPENAI_API_KEY, or STELLA_EMBED_URL + STELLA_EMBED_MODEL "
            "to let `stella search` rank by meaning)\n"
            "✓ code graph: 0 symbols, 0 imports across 0 files\n"
        )
        result = code_graph.from_stdout(stdout)
        assert result["semantic"]["state"] == "skipped_no_backend"

    def test_built_with_backend_and_file_count(self) -> None:
        stdout = (
            "✓ code graph: 6 symbols, 3 imports across 1 file\n"
            "✓ semantic index: 1 files embedded by voyage-code-3\n"
            "✓ chunk index: 1 file(s) embedded by voyage-code-3\n"
        )
        result = code_graph.from_stdout(stdout)
        semantic = result["semantic"]
        assert semantic["state"] == "built"
        assert semantic["files_embedded"] == "1"
        assert semantic["model"] == "voyage-code-3"
        # The code-graph line is still captured unchanged alongside it.
        assert result["state"] == "reported"
        assert result["summary"] == "✓ code graph: 6 symbols, 3 imports across 1 file"

    def test_progress_ticks_do_not_shadow_the_terminal_line(self) -> None:
        stdout = (
            "◈ embedding files for semantic search…\n"
            "· semantic index: 3 files embedded…\n"
            "· semantic index: 7 files embedded…\n"
            "✓ semantic index: 12 files embedded by voyage-code-3\n"
        )
        result = code_graph.from_stdout(stdout)
        assert result["semantic"]["state"] == "built"
        assert result["semantic"]["files_embedded"] == "12"

    def test_attempted_failed_when_not_built(self) -> None:
        stdout = "! semantic index: not built — incomplete backend configuration\n"
        result = code_graph.from_stdout(stdout)
        assert result["semantic"]["state"] == "attempted_failed"

    def test_attempted_failed_when_partially_embedded(self) -> None:
        stdout = (
            "! semantic index: 4 files embedded by voyage-code-3 — 2 left unembedded, "
            "2 of them because their content could not be read\n"
        )
        result = code_graph.from_stdout(stdout)
        assert result["semantic"]["state"] == "attempted_failed"

    def test_no_summary_line_when_init_says_nothing_about_semantic_index(self) -> None:
        result = code_graph.from_stdout(
            "✓ code graph: 0 symbols, 0 imports across 0 files\n"
        )
        assert result["semantic"]["state"] == "no_summary_line"

    def test_the_states_are_pairwise_distinguishable(self) -> None:
        outcomes = [
            code_graph.from_stdout("· semantic index: skipped — no backend\n")[
                "semantic"
            ],
            code_graph.from_stdout(
                "✓ semantic index: 1 files embedded by voyage-code-3\n"
            )["semantic"],
            code_graph.from_stdout("! semantic index: not built — incomplete\n")[
                "semantic"
            ],
            code_graph.from_stdout("✓ code graph: 0 symbols\n")["semantic"],
        ]
        rendered = [repr(sorted(outcome.items())) for outcome in outcomes]
        assert len(set(rendered)) == len(outcomes), (
            f"two semantic-index outcomes are indistinguishable: {rendered}"
        )


class TestUnavailable:
    """The step raised — legibility of what the exception actually carried."""

    def test_kind_names_the_exception_class(self) -> None:
        result = code_graph.unavailable(
            RuntimeError("Command timed out after 300 seconds")
        )
        assert result["state"] == "unavailable"
        assert result["kind"] == "RuntimeError"
        assert result["detail"] == "Command timed out after 300 seconds"
        assert "exit_code" not in result
        assert "stderr" not in result

    def test_extracts_exit_code_and_stderr_from_a_nonzero_exit_message(self) -> None:
        message = (
            "Command failed (exit 1): /usr/local/bin/stella init\n"
            "stdout: some stdout\n"
            "stderr: tree-sitter: unsupported grammar for *.mojo"
        )

        class _NonZeroAgentExitCodeError(RuntimeError):
            pass

        result = code_graph.unavailable(_NonZeroAgentExitCodeError(message))
        assert result["kind"] == "_NonZeroAgentExitCodeError"
        assert result["exit_code"] == "1"
        assert result["stderr"] == "tree-sitter: unsupported grammar for *.mojo"
        assert result["detail"] == message


_STDERR = "tree-sitter: unsupported grammar for *.mojo"
_STDOUT = "indexing /app"


class _FailingEnvironment:
    """Harbor's ``BaseEnvironment.exec`` contract, answering non-zero.

    Only ``exec`` — the adapter's own ``_stella_secure_exec_with_stdin`` hook
    is absent, so a test that reached the credential branch by mistake would
    raise rather than pass on the wrong path.
    """

    def __init__(self, *, return_code: int, stderr: str) -> None:
        self.task_env_config = SimpleNamespace(workdir="/app")
        self.commands: list[str] = []
        self._return_code = return_code
        self._stderr = stderr

    async def exec(
        self,
        *,
        command: str,
        user: str | int | None = None,
        env: dict[str, str] | None = None,
        cwd: str | None = None,
        timeout_sec: int | None = None,
    ) -> SimpleNamespace:
        self.commands.append(command)
        return SimpleNamespace(
            return_code=self._return_code, stdout=_STDOUT, stderr=self._stderr
        )


def _agent_over_harbors_exec() -> StellaAgent:
    """An agent whose ``exec_as_agent`` is Harbor's real, unpatched ``_exec``.

    Constructed through ``__new__`` like ``test_exit_cause``'s agents, because
    ``BaseInstalledAgent.__init__`` wants a logs dir and a resolved descriptor
    set that none of this needs. Nothing here overrides ``_exec``, which is
    the point: the exception under test has to be the one Harbor raises.
    """
    agent = StellaAgent.__new__(StellaAgent)
    agent._extra_env = {}
    agent.logger = logging.getLogger("stella_harbor.tests.code_graph")
    agent._configured_value = lambda name, default=None: None
    return agent


class TestUnavailableAgainstHarbor:
    """The parser against the string Harbor writes, not the string we wrote.

    ``TestUnavailable`` above builds the message by hand and raises a locally
    defined subclass, so it proves the regex matches the test's own literal
    and says nothing about Harbor. These drive Harbor's
    ``BaseInstalledAgent._exec`` against a fake environment and feed the
    ``NonZeroAgentExitCodeError`` *it* raises to :func:`code_graph.unavailable`
    — so a Harbor upgrade that reorders the message's lines, renames the
    ``stderr:`` prefix, or drops the ``(exit N)`` clause fails here instead of
    silently emptying every trial's metadata (#4899).
    """

    def test_harbors_own_exception_yields_the_exit_code_and_stderr(self) -> None:
        environment = _FailingEnvironment(return_code=3, stderr=_STDERR)
        agent = _agent_over_harbors_exec()

        with pytest.raises(NonZeroAgentExitCodeError) as raised:
            asyncio.run(
                StellaAgent.exec_as_agent(
                    agent, environment, command="/usr/local/bin/stella init"
                )
            )

        result = code_graph.unavailable(raised.value)
        assert result["kind"] == "NonZeroAgentExitCodeError"
        assert result["exit_code"] == "3"
        # Exact equality, not a substring: if Harbor moves `stdout:` below
        # `stderr:`, the greedy trailing group swallows it and this fails.
        assert result["stderr"] == _STDERR

    def test_build_code_graph_records_what_harbor_raised(self) -> None:
        """The whole chain, through the production call site.

        Harbor's ``_exec`` raises, ``_build_code_graph``'s ``except`` arm
        catches, and the parsed fields land in the summary a trial discloses.
        """
        environment = _FailingEnvironment(return_code=101, stderr=_STDERR)
        agent = _agent_over_harbors_exec()

        asyncio.run(StellaAgent._build_code_graph(agent, environment))

        summary = agent._code_graph_summary
        assert summary["state"] == "unavailable"
        assert summary["kind"] == "NonZeroAgentExitCodeError"
        assert summary["exit_code"] == "101"
        assert summary["stderr"] == _STDERR
        assert environment.commands, "init never reached the environment"

    def test_harbors_truncation_survives_the_parse(self) -> None:
        """``code_graph``'s comment claims Harbor truncates and we do not.

        ``_truncate_output`` cuts at 1000 characters and appends a marker.
        The parser must hand that string back whole, so the recovered
        ``stderr`` is exactly what Harbor decided to disclose — a second
        truncation of ours would show up here as a shorter string.
        """
        long_stderr = "x" * 1500
        environment = _FailingEnvironment(return_code=1, stderr=long_stderr)
        agent = _agent_over_harbors_exec()

        with pytest.raises(NonZeroAgentExitCodeError) as raised:
            asyncio.run(
                StellaAgent.exec_as_agent(
                    agent, environment, command="/usr/local/bin/stella init"
                )
            )

        recovered = code_graph.unavailable(raised.value)["stderr"]
        assert recovered == "x" * 1000 + " ... [truncated]"
        assert recovered != long_stderr


class TestFromResult:
    """The exec that returned instead of raising.

    Harbor raises on a non-zero exit, so the plain agent exec is covered by
    ``TestUnavailableAgainstHarbor`` above. The credential branch drives the
    Compose client itself and *returns* the command's exit code, so the code
    has to be read off the result. Read only stdout and a failed init lands in
    ``no_summary_line``, the state a clean init that printed no graph line
    gets.
    """

    def test_a_clean_exit_is_classified_from_stdout_as_before(self) -> None:
        result = SimpleNamespace(
            return_code=0,
            stdout="✓ code graph: 6 symbols, 3 imports across 1 file\n",
            stderr=None,
        )
        assert code_graph.from_result(result)["state"] == "reported"

    def test_a_missing_return_code_is_read_as_a_clean_exit(self) -> None:
        result = SimpleNamespace(stdout="✓ code graph: 0 symbols\n")
        assert code_graph.from_result(result)["state"] == "reported"

    def test_a_nonzero_exit_carries_the_code_and_both_streams(self) -> None:
        result = SimpleNamespace(return_code=1, stdout=_STDOUT, stderr=_STDERR)
        summary = code_graph.from_result(result)
        assert summary["state"] == "nonzero_exit"
        assert summary["exit_code"] == "1"
        assert summary["stdout"] == _STDOUT
        assert summary["stderr"] == _STDERR

    def test_a_nonzero_exit_is_not_a_quiet_one(self) -> None:
        """The property the defect turned on, stated directly."""
        failed = code_graph.from_result(
            SimpleNamespace(return_code=1, stdout=_STDOUT, stderr=_STDERR)
        )
        quiet = code_graph.from_result(
            SimpleNamespace(return_code=0, stdout=_STDOUT, stderr=None)
        )
        assert failed != quiet
        assert failed["state"] != quiet["state"]

    def test_long_output_keeps_its_tail(self) -> None:
        """The reason is on the last lines, so the last lines are what is kept."""
        stdout = "noise\n" * 400 + "the reason it failed"
        summary = code_graph.from_result(
            SimpleNamespace(return_code=2, stdout=stdout, stderr=None)
        )
        assert summary["stdout"].endswith("the reason it failed")
        assert summary["stdout"].startswith("[earlier output cut] ")
        assert len(summary["stdout"]) < len(stdout)
        assert summary["stderr"] == ""


class TestDisclose:
    """A failed step reaches the run log as well as the trial's metadata."""

    def test_a_nonzero_exit_is_printed_with_what_init_said(
        self, capsys: pytest.CaptureFixture[str]
    ) -> None:
        summary = code_graph.from_result(
            SimpleNamespace(return_code=1, stdout=_STDOUT, stderr=_STDERR)
        )
        assert code_graph.disclose(summary) is summary
        printed = capsys.readouterr().err
        assert "code graph unavailable" in printed
        assert "exited 1" in printed
        assert _STDERR in printed

    def test_a_healthy_step_prints_nothing(
        self, capsys: pytest.CaptureFixture[str]
    ) -> None:
        code_graph.disclose(code_graph.from_stdout("✓ code graph: 0 symbols\n"))
        assert capsys.readouterr().err == ""

    def test_a_caught_exception_is_printed_once(
        self, capsys: pytest.CaptureFixture[str]
    ) -> None:
        """Harbor's message already quotes both streams; it is not repeated."""
        message = (
            "Command failed (exit 1): /usr/local/bin/stella init\n"
            f"stdout: {_STDOUT}\n"
            f"stderr: {_STDERR}"
        )
        code_graph.disclose(code_graph.unavailable(RuntimeError(message)))
        assert capsys.readouterr().err.count(_STDERR) == 1


class _NonZeroReturningEnvironment:
    """The credential branch's environment: init exits 1 and nothing raises.

    ``_stella_secure_exec_with_stdin`` is the adapter's own test hook, taken
    by :mod:`stella_harbor.setup_exec` before it builds a Compose argv. What
    it answers is the shape ``communicate_with_release`` builds from a real
    client: an ``ExecResult`` whose ``return_code`` is the command's.
    """

    def __init__(self) -> None:
        self.task_env_config = SimpleNamespace(workdir="/app")
        self.commands: list[list[str]] = []

    async def _stella_secure_exec_with_stdin(
        self, *, command: list[str], env: dict[str, str], stdin: bytes
    ) -> SimpleNamespace:
        self.commands.append(command)
        return SimpleNamespace(return_code=1, stdout=_STDOUT, stderr=_STDERR)


class TestBuildCodeGraphOnTheReturningBranch:
    """The witness, through the production call site.

    Strip ``from_result`` and ``disclose`` back out and this summary is
    ``{"state": "no_summary_line", "detail": "`stella init` exited without a
    'code graph:' line (1 line(s) of stdout)"}`` — no exit code, no stderr,
    nothing printed. Neither field is reachable from stdout, so the assertions
    below cannot pass without the change.
    """

    def test_the_exit_code_and_the_message_reach_the_metadata(
        self, capsys: pytest.CaptureFixture[str]
    ) -> None:
        environment = _NonZeroReturningEnvironment()
        agent = StellaAgent.__new__(StellaAgent)
        agent._configured_value = (
            lambda name, default=None: "voyage-secret"
            if name == "VOYAGE_API_KEY"
            else None
        )

        asyncio.run(StellaAgent._build_code_graph(agent, environment))

        assert environment.commands, "init never reached the environment"
        summary = agent._code_graph_summary
        assert summary["state"] == "nonzero_exit"
        assert summary["exit_code"] == "1"
        assert summary["stderr"] == _STDERR
        assert summary["stdout"] == _STDOUT
        assert _STDERR in capsys.readouterr().err


class TestUnavailableLiftsStdout:
    """Harbor's message carries stdout too, and it was being discarded.

    A second pattern rather than a third group on the first one: a Harbor
    release that renames or reorders one section then costs that field alone,
    where one pattern over all three would match nothing and take the exit
    code down with it.
    """

    def test_stdout_is_lifted_into_its_own_field(self) -> None:
        message = (
            "Command failed (exit 1): /usr/local/bin/stella init\n"
            f"stdout: {_STDOUT}\n"
            f"stderr: {_STDERR}"
        )
        result = code_graph.unavailable(RuntimeError(message))
        assert result["stdout"] == _STDOUT
        assert result["stderr"] == _STDERR

    def test_a_message_with_no_sections_yields_no_stdout_field(self) -> None:
        result = code_graph.unavailable(
            RuntimeError("Command timed out after 300 seconds")
        )
        assert "stdout" not in result
