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
