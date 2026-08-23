"""Unit tests for :mod:`stella_harbor.code_graph`'s pure parsers.

``from_stdout`` and ``unavailable`` are the only place a trial's metadata
learns what ``stella init`` actually did in the container. Before #3669 the
semantic-index outcome was not parsed at all — a trial could not say whether
embeddings were built, skipped for want of a backend, or attempted and
incomplete, because the parser structurally could not emit any of the three.
Before #3670 a failed exec's exit code and stderr were only ever reachable by
re-parsing the exception's ``str()`` by eye.

In its own file rather than ``test_adapter.py``, which is grandfathered at
its file-size ceiling and closed to growth.
"""

from __future__ import annotations

from stella_harbor import code_graph


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
