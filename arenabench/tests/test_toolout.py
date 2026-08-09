# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The tool-event decoding every arena transcript surface shares.

Each test here is a witness for one defect that shipped: the four in
:class:`TestDecodeToolOutput` / :class:`TestTranscriptToolRows` were all live
in the reader at once, and each one alone is enough to make a transcript
unreadable or — worse — quietly wrong about whether a tool call succeeded.
"""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path

import pytest

from arenabench.toolout import (
    cap_middle,
    decode_tool_output,
    format_tool_input,
    strip_ansi,
    summarize,
)
from arenabench.transcript import TranscriptReader


def write_events(path: Path, events: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        for event in events:
            handle.write(json.dumps(event) + "\n")


def ok_result(call_id: str, content: str, **extra) -> dict:
    """A `tool_result` in the shape Stella actually emits."""
    return {
        "type": "tool_result",
        "call_id": call_id,
        "output": {"ok": {"content": content}},
        "duration_ms": 12,
        "speculated": False,
        **extra,
    }


def start(call_id: str, name: str, arguments: dict) -> dict:
    """A `tool_start` in the shape Stella actually emits — note `input`."""
    return {
        "type": "tool_start",
        "call": {"call_id": call_id, "name": name, "input": arguments},
    }


class TestDecodeToolOutput:
    def test_the_ok_arm_yields_its_content_with_real_line_breaks(self):
        """The defect this module was written for.

        `str()` on the tagged dict yields a Python *repr*: single quotes, and
        every newline rendered as the two characters `\\` `n`, so an entire
        `git reflog` arrived as one unreadable line.
        """
        payload = "--- reflog ---\nd7d3e4b HEAD@{0}: checkout\n\n[exit code: 0]"
        decoded = decode_tool_output({"ok": {"content": payload}})
        assert decoded.text == payload
        assert decoded.text.count("\n") == 3
        assert "\\n" not in decoded.text
        assert "{'ok'" not in decoded.text

    def test_the_error_arm_is_the_only_thing_that_says_a_call_failed(self):
        """`tool_result` carries no `error`/`is_error` field and never has.

        A reader asking for one gets `None` on every event, so every failed
        tool call renders as a success. The tag is the verdict.
        """
        decoded = decode_tool_output({"error": {"message": "no such file"}})
        assert decoded.ok is False
        assert decoded.text == "no such file"

    def test_an_empty_result_is_recognized_rather_than_re_encoded(self):
        """A tool that legitimately produced nothing sends `{"content": ""}`.

        Testing the arm's truthiness instead of its presence would classify
        that as an unknown shape and replace the empty string with JSON
        describing one.
        """
        decoded = decode_tool_output({"ok": {"content": ""}})
        assert (decoded.text, decoded.ok, decoded.recognized) == ("", True, True)

    def test_an_unknown_shape_degrades_to_json_never_to_a_repr(self):
        """The protocol may grow an arm this reader predates. Both outcomes
        are wrong to show a reader; only one keeps the quotes parseable and
        the newlines real."""
        decoded = decode_tool_output({"partial": {"chunk": "x"}})
        assert decoded.recognized is False
        assert json.loads(decoded.text) == {"partial": {"chunk": "x"}}

    @pytest.mark.parametrize("value", [None, "already text", 42])
    def test_non_dict_payloads_never_reach_repr(self, value):
        assert "{'" not in decode_tool_output(value).text


class TestStripAnsi:
    def test_a_colourised_build_line_renders_clean(self):
        line = "\x1b[0m\x1b[1m\x1b[32m    Finished\x1b[0m `dev` in 2.41s"
        assert strip_ansi(line) == "    Finished `dev` in 2.41s"

    def test_bare_brackets_without_esc_survive(self):
        """Over-eager stripping is a worse bug than the residue it removes."""
        line = "error[E0308]: mismatched types in [u8; 4]"
        assert strip_ansi(line) == line

    def test_osc_hyperlinks_and_titles_are_removed(self):
        assert strip_ansi("\x1b]0;window title\x07visible") == "visible"
        st = "\x1b]8;;https://example.com\x1b\\link text\x1b]8;;\x1b\\ after"
        assert strip_ansi(st) == "link text after"

    def test_truncated_and_two_byte_escapes_do_not_leak(self):
        assert strip_ansi("a\x1b(Bb") == "ab"
        # Capped output can cut a sequence mid-stream; swallow it rather than
        # leaking a partial `[1;3` into the page.
        assert strip_ansi("done\x1b[1;3") == "done"
        assert strip_ansi("done\x1b") == "done"


class TestCapMiddle:
    def test_both_ends_of_a_failing_payload_survive(self):
        """Head-truncation is the obvious implementation and the wrong one:
        the exit code and the error live in the tail."""
        text = "START" + ("x" * 5000) + "[exit code: 1]"
        capped = cap_middle(text, 200)
        assert capped.startswith("START")
        assert capped.endswith("[exit code: 1]")
        assert len(capped) <= 200

    def test_an_in_budget_payload_is_returned_whole(self):
        assert cap_middle("short", 200) == "short"

    def test_summarize_flattens_onto_one_line(self):
        assert "\n" not in summarize("a\nb\nc")


class TestFormatToolInput:
    def test_a_shell_call_shows_its_command(self):
        assert format_tool_input("bash", {"cmd": "git status"}) == "git status"

    def test_a_file_tool_shows_its_path(self):
        assert format_tool_input("edit_file", {"path": "src/lib.rs"}) == "src/lib.rs"

    def test_a_write_states_its_size(self):
        label = format_tool_input("write_file", {"path": "a.rs", "content": "x\ny\nz"})
        assert label == "a.rs  (3 lines)"

    def test_a_batch_edit_surfaces_its_paths(self):
        label = format_tool_input(
            "apply_edits", {"edits": [{"path": "a.rs"}, {"path": "b.rs"}]}
        )
        assert label == "a.rs, b.rs  (2 files)"

    def test_an_unrecognized_tool_falls_back_to_compact_json(self):
        label = format_tool_input("mcp__x__y", {"alpha": 1})
        assert label == '{"alpha":1}'


class TestTranscriptToolRows:
    """The reader's own rows, which is where each defect was visible."""

    def test_a_result_body_is_the_decoded_content(self, tmp_path: Path):
        path = tmp_path / "e.jsonl"
        payload = "--- branches ---\n* master d7d3e4b off the job market woo"
        write_events(path, [
            start("c1", "bash", {"cmd": "git branch -v"}),
            ok_result("c1", payload),
        ])
        result = TranscriptReader().read(path)[-1]
        assert result["body"] == payload
        assert "{'ok'" not in result["body"]

    def test_a_result_row_is_labelled_with_its_tools_name(self, tmp_path: Path):
        """`ToolResult` carries only `call_id`; the name is recovered from the
        `tool_start` that opened the call. Without it every result in every
        transcript read `result`, and a reader scrolling a fan-out could not
        tell which row answered which call."""
        path = tmp_path / "e.jsonl"
        write_events(path, [
            start("c1", "read_file", {"path": "a.rs"}),
            ok_result("c1", "contents"),
        ])
        assert TranscriptReader().read(path)[-1]["title"] == "read_file"

    def test_a_result_with_no_recorded_call_still_renders(self, tmp_path: Path):
        """A reader that joined mid-stream has no `tool_start` to correlate.
        The row falls back to a generic label rather than vanishing."""
        path = tmp_path / "e.jsonl"
        write_events(path, [ok_result("orphan", "x")])
        assert TranscriptReader().read(path)[-1]["title"] == "tool"

    def test_a_failed_call_is_marked_failed(self, tmp_path: Path):
        path = tmp_path / "e.jsonl"
        write_events(path, [
            start("c1", "read_file", {"path": "missing.rs"}),
            {
                "type": "tool_result",
                "call_id": "c1",
                "output": {"error": {"message": "no such file"}},
                "duration_ms": 3,
            },
        ])
        result = TranscriptReader().read(path)[-1]
        assert result["meta"]["error"] is True
        assert result["body"] == "no such file"

    def test_a_tool_call_shows_its_arguments(self, tmp_path: Path):
        """The arguments ride under `input`. The reader asked for `arguments`
        — a key the wire has never carried — so every tool row rendered with
        an empty body and a `bash` call did not show its command."""
        path = tmp_path / "e.jsonl"
        write_events(path, [start("c1", "bash", {"cmd": "git reflog"})])
        row = TranscriptReader().read(path)[0]
        assert row["body"] == "git reflog"
        assert json.loads(row["meta"]["raw"]) == {"cmd": "git reflog"}

    def test_ansi_escapes_never_reach_the_page(self, tmp_path: Path):
        path = tmp_path / "e.jsonl"
        write_events(path, [
            start("c1", "bash", {"cmd": "cargo build"}),
            ok_result("c1", "\x1b[32mok\x1b[0m\nline two"),
        ])
        body = TranscriptReader().read(path)[-1]["body"]
        assert body == "ok\nline two"
        assert "\x1b" not in body

    def test_speculation_and_line_count_reach_the_page(self, tmp_path: Path):
        path = tmp_path / "e.jsonl"
        write_events(path, [
            start("c1", "read_file", {"path": "a.rs"}),
            ok_result("c1", "a\nb\nc", speculated=True),
        ])
        meta = TranscriptReader().read(path)[-1]["meta"]
        assert meta["speculated"] is True
        assert meta["lines"] == 3


class TestRecorderCopyStaysHonest:
    """`recorder/render.py` ships into a Docker image whose build context is
    `arenabench/recorder/` alone, so it cannot import this package and carries
    an acknowledged copy of the decoder. A copy nobody checks is how the two
    drift; this is the check.
    """

    @staticmethod
    def _render_module():
        path = Path(__file__).resolve().parents[1] / "recorder" / "render.py"
        spec = importlib.util.spec_from_file_location("_arena_render", path)
        assert spec and spec.loader
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    @pytest.mark.parametrize(
        "output",
        [
            {"ok": {"content": "a\nb"}},
            {"ok": {"content": ""}},
            {"error": {"message": "boom"}},
            {"partial": {"chunk": "x"}},
            None,
            "bare string",
        ],
    )
    def test_the_copy_decodes_identically(self, output):
        mirror = self._render_module()
        assert mirror.decode_tool_output(output) == tuple(decode_tool_output(output))

    @pytest.mark.parametrize(
        "text",
        [
            "\x1b[0m\x1b[1m\x1b[32m    Finished\x1b[0m `dev`",
            "error[E0308]: mismatched types in [u8; 4]",
            "\x1b]0;title\x07visible",
            "\x1b]8;;https://e.com\x1b\\link\x1b]8;;\x1b\\ after",
            "a\x1b(Bb",
            "done\x1b[1;3",
            "done\x1b",
            "no escapes here",
        ],
    )
    def test_the_copy_strips_identically(self, text):
        mirror = self._render_module()
        assert mirror.strip_ansi(text) == strip_ansi(text)
