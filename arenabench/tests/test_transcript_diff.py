# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""A mutating tool call's diff rides its ``tool_result`` entry (#3110).

The Command Deck renders a mutation's diff inline under the result row —
``crates/stella-tui/src/render/entry.rs``, the ``ToolResult`` arm — by
correlating the ``file_change`` the call emitted back to the call through
state its fold recorded at ``tool_start``. Until this existed,
:class:`~arenabench.transcript.TranscriptReader` made no such join: a
``file_change`` was its own compact-JSON row, never associated with the
``tool_result`` that produced it, so the arena transcript could not show what
an ``edit_file`` actually changed.

The wire facts these tests lean on (``crates/stella-protocol/src/event.rs``):
a ``file_change`` carries ``{path, kind, added, removed, diff}``; the counts
are the emitter's own measurement and the authority over the diff, which is a
bounded rendering of the changed region; reads carry ``0/0`` and no diff; and
the event precedes its ``tool_result`` in the stream, because
``ToolRegistry::record_touch`` emits it during the tool's execution.
"""

from __future__ import annotations

import json
from pathlib import Path

from arenabench.transcript import TOOL_RESULT_BUDGET, TranscriptReader

DIFF = "@@ -1,2 +1,3 @@\n-old\n+new\n+more\n context\n"


def write_events(path: Path, events: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        for event in events:
            handle.write(json.dumps(event) + "\n")


def call(name: str, call_id: str = "c1", **arguments) -> dict:
    return {
        "type": "tool_start",
        "call": {"call_id": call_id, "name": name, "input": arguments},
    }


def change(
    path: str = "src/lib.rs",
    diff: str | None = DIFF,
    added: int = 2,
    removed: int = 1,
    kind: str = "modified",
) -> dict:
    return {
        "type": "file_change",
        "path": path,
        "kind": kind,
        "added": added,
        "removed": removed,
        "diff": diff,
    }


def result(call_id: str = "c1", ok: bool = True, text: str = "done") -> dict:
    output = {"ok": {"content": text}} if ok else {"error": {"message": text}}
    return {"type": "tool_result", "call_id": call_id, "output": output, "duration_ms": 12}


def results_of(entries: list[dict]) -> list[dict]:
    return [e for e in entries if e["kind"] == "tool_result"]


class TestAMutationsDiffRidesItsResult:
    def test_an_edit_files_result_carries_its_diff(self, tmp_path: Path):
        """The witness. Before the join existed, a `tool_result` entry's meta
        never held a `diff` at all — the deck-parity gap of #3110."""
        path = tmp_path / "e.jsonl"
        write_events(
            path,
            [
                call("edit_file", path="src/lib.rs"),
                change(),
                result(text="Applied edit to src/lib.rs"),
            ],
        )
        (entry,) = results_of(TranscriptReader().read(path))
        assert entry["meta"]["diff"] == DIFF
        assert entry["meta"]["diff_path"] == "src/lib.rs"

    def test_the_counts_are_the_emitters_not_a_recount(self, tmp_path: Path):
        """`added`/`removed` come from the event's own fields — the measured
        delta — never from counting `+`/`-` lines in the bounded diff, which
        is the re-derivation the wire contract forbids."""
        path = tmp_path / "e.jsonl"
        write_events(
            path,
            [
                call("write_file", path="a.txt"),
                # A coarse diff showing fewer lines than the change really had.
                change(path="a.txt", diff="@@ -1,1 +1,1 @@\n+tail\n", added=40, removed=0),
                result(),
            ],
        )
        (entry,) = results_of(TranscriptReader().read(path))
        assert entry["meta"]["diff_added"] == 40
        assert entry["meta"]["diff_removed"] == 0

    def test_a_read_only_calls_result_carries_none(self, tmp_path: Path):
        """The other half of the witness: a read's `file_change` carries no
        diff and must not put one on the result row."""
        path = tmp_path / "e.jsonl"
        write_events(
            path,
            [
                call("read_file", path="src/lib.rs"),
                change(diff=None, added=0, removed=0, kind="read"),
                result(text="1: fn main() {}"),
            ],
        )
        (entry,) = results_of(TranscriptReader().read(path))
        assert "diff" not in entry["meta"]

    def test_a_failed_mutations_result_carries_none(self, tmp_path: Path):
        """A failed call produced no `file_change` (the registry only records
        touches on success). Rendering the path's previous diff under its ✗
        would attribute a change the call never made — the deck's own gate."""
        path = tmp_path / "e.jsonl"
        write_events(
            path,
            [
                call("edit_file", path="src/lib.rs"),
                change(),
                result(text="Applied edit to src/lib.rs"),
                call("edit_file", call_id="c2", path="src/lib.rs"),
                result(call_id="c2", ok=False, text="old_string not found"),
            ],
        )
        first, second = results_of(TranscriptReader().read(path))
        assert first["meta"]["diff"] == DIFF
        assert "diff" not in second["meta"]

    def test_apply_edits_correlates_through_its_batch(self, tmp_path: Path):
        """`apply_edits` names its paths inside `edits`, not at the top level;
        the first edit's path is the join key, as in the deck's
        `tool_input_path`."""
        path = tmp_path / "e.jsonl"
        write_events(
            path,
            [
                call("apply_edits", edits=[{"path": "b.rs", "old": "x", "new": "y"}]),
                change(path="b.rs"),
                result(),
            ],
        )
        (entry,) = results_of(TranscriptReader().read(path))
        assert entry["meta"]["diff"] == DIFF
        assert entry["meta"]["diff_path"] == "b.rs"

    def test_a_non_file_tool_stays_bare(self, tmp_path: Path):
        """`bash` mutates through redirects and heredocs and emits no
        `file_change` at all; nothing must be invented for it — and a stray
        recorded diff for some other path must not leak onto it."""
        path = tmp_path / "e.jsonl"
        write_events(
            path,
            [
                call("edit_file", path="src/lib.rs"),
                change(),
                result(),
                call("bash", call_id="c2", cmd="make test"),
                result(call_id="c2", text="ok"),
            ],
        )
        _, bash = results_of(TranscriptReader().read(path))
        assert "diff" not in bash["meta"]


class TestTheJoinIsHonest:
    def test_a_diffless_mutation_does_not_inherit_the_previous_diff(
        self, tmp_path: Path
    ):
        """The freshness mark. A second edit of the same path whose
        `file_change` carried no diff supersedes the recorded one — its result
        must not resurrect the *first* call's diff, which is exactly the
        misattribution the deck's `FileState::changes` seq guards against."""
        path = tmp_path / "e.jsonl"
        write_events(
            path,
            [
                call("edit_file", path="src/lib.rs"),
                change(),
                result(),
                call("edit_file", call_id="c2", path="src/lib.rs"),
                change(diff=None, added=1, removed=1),
                result(call_id="c2"),
            ],
        )
        first, second = results_of(TranscriptReader().read(path))
        assert first["meta"]["diff"] == DIFF
        assert "diff" not in second["meta"]

    def test_a_call_that_emitted_nothing_does_not_reach_back(self, tmp_path: Path):
        """A mutation call whose execution recorded no `file_change` at all (a
        no-op write) sees only mutations recorded *after* it opened — the
        path's older diff is out of reach by construction."""
        path = tmp_path / "e.jsonl"
        write_events(
            path,
            [
                call("write_file", path="a.txt"),
                change(path="a.txt", diff="@@ -0,0 +1,1 @@\n+hi\n", added=1, removed=0),
                result(),
                call("write_file", call_id="c2", path="a.txt"),
                result(call_id="c2"),
            ],
        )
        first, second = results_of(TranscriptReader().read(path))
        assert first["meta"]["diff_added"] == 1
        assert "diff" not in second["meta"]

    def test_interleaved_paths_do_not_cross(self, tmp_path: Path):
        """Two mutations of different files each find their own diff, keyed by
        the path recorded at their `tool_start`."""
        path = tmp_path / "e.jsonl"
        a_diff = "@@ -1,1 +1,1 @@\n-a\n+A\n"
        b_diff = "@@ -1,1 +1,1 @@\n-b\n+B\n"
        write_events(
            path,
            [
                call("edit_file", call_id="ca", path="a.rs"),
                change(path="a.rs", diff=a_diff, added=1, removed=1),
                result(call_id="ca"),
                call("edit_file", call_id="cb", path="b.rs"),
                change(path="b.rs", diff=b_diff, added=1, removed=1),
                result(call_id="cb"),
            ],
        )
        first, second = results_of(TranscriptReader().read(path))
        assert first["meta"]["diff"] == a_diff and first["meta"]["diff_path"] == "a.rs"
        assert second["meta"]["diff"] == b_diff and second["meta"]["diff_path"] == "b.rs"

    def test_the_diff_is_never_capped(self, tmp_path: Path):
        """The result *body* is middle-elided for the SSE channel's sake; the
        diff must not be — a mid-hunk elision garbles the render, and the
        emitter already bounds the diff at the source
        (`file_touch.rs::changed_region_diff` caps each side)."""
        path = tmp_path / "e.jsonl"
        big = "@@ -1,1 +1,999 @@\n" + "".join(f"+line {i}\n" for i in range(999))
        assert len(big) > TOOL_RESULT_BUDGET
        write_events(
            path,
            [
                call("write_file", path="big.txt"),
                change(path="big.txt", diff=big, added=999, removed=0),
                result(text="Wrote 999 lines to big.txt"),
            ],
        )
        (entry,) = results_of(TranscriptReader().read(path))
        assert entry["meta"]["diff"] == big
