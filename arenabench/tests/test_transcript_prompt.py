# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The trial's prompt row — the ``YOU`` half of the conversation (#4039).

The transcript-spec addendum §1 renders a turn as Prompt → Prose → Steps →
Answer, with the prompt "never folded away by default (it's the anchor for
everything below)". The arena transcript rendered **no prompt row at all**: a
reader opening a trial saw stages, tool calls and responses with nothing
saying what was asked.

The wire has carried the instruction all along. The engine registers the task
text as a ``block_registered`` event of kind ``user_goal`` whose ``content``
is the exact prompt the model was handed, and
:class:`~arenabench.transcript.TranscriptReader` dropped every
``block_registered`` on the floor. These assertions pin the recovery:

* the instruction becomes one ``prompt`` entry on the reserved seq 0, so the
  page's seq-ordered render puts it first even though the block event lands
  *after* the trial's first stage rule on the wire;
* it is emitted once, at ``t`` 0.0, and never re-emitted on a later read;
* the other block kinds stay unrendered — the receipt plane is a
  reconstruction record, not reading material;
* a content-free stream (the block registered without its preimage) emits no
  prompt row rather than an empty one.
"""

from __future__ import annotations

import json
from pathlib import Path

from arenabench.transcript import TranscriptReader


def write_events(path: Path, events: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        for event in events:
            handle.write(json.dumps(event) + "\n")


def goal_block(content: str | None = "Fix the bug in parser.py.") -> dict:
    """A ``user_goal`` ``block_registered``, in the shape Stella emits
    (``stella_protocol::event::AgentEvent::BlockRegistered``)."""
    event = {
        "ts": 1786450724282,
        "type": "block_registered",
        "block_id": "blk_d3ce1b6ca63b572bc4b013e6",
        "kind": "user_goal",
        "origin": {"turn_instance": 0, "step": 0},
        "token_cost": 12,
        "content_digest": "sha256:25c3bcdb",
    }
    if content is not None:
        event["content"] = content
    return event


def test_the_user_goal_block_becomes_the_prompt_row(tmp_path: Path) -> None:
    """The stream's real order — stage first, goal block after — still yields
    a prompt entry that sorts to the head of the transcript."""
    path = tmp_path / "stella-events.jsonl"
    write_events(
        path,
        [
            {"ts": 1786450720000, "type": "stage", "name": "context_recall"},
            goal_block(),
            {"ts": 1786450724290, "type": "stage", "name": "execute"},
        ],
    )
    entries = TranscriptReader().read(path)
    prompts = [e for e in entries if e["kind"] == "prompt"]
    assert len(prompts) == 1
    prompt = prompts[0]
    assert prompt["seq"] == 0, "the reserved seq is what puts the anchor first"
    assert prompt["t"] == 0.0
    assert prompt["body"] == "Fix the bug in parser.py."
    assert min(e["seq"] for e in entries if e["kind"] != "prompt") >= 1
    ordered = sorted(entries, key=lambda e: e["seq"])
    assert ordered[0]["kind"] == "prompt", (
        "seq order must open on the prompt even though the wire led with a stage"
    )


def test_the_prompt_is_emitted_once(tmp_path: Path) -> None:
    """A second read, and even a re-registered goal, must not print the
    question twice."""
    path = tmp_path / "stella-events.jsonl"
    reader = TranscriptReader()
    write_events(path, [goal_block()])
    first = reader.read(path)
    assert [e["kind"] for e in first] == ["prompt"]
    write_events(
        path,
        [goal_block(), {"ts": 1786450724300, "type": "stage", "name": "execute"}],
    )
    second = reader.read(path)
    assert [e["kind"] for e in second] == ["stage"]


def test_other_block_kinds_stay_unrendered(tmp_path: Path) -> None:
    path = tmp_path / "stella-events.jsonl"
    events = []
    for kind in ("system_prefix", "assistant_text", "tool_call", "tool_result"):
        block = goal_block("not the prompt")
        block["kind"] = kind
        events.append(block)
    write_events(path, events)
    assert TranscriptReader().read(path) == []


def test_a_content_free_goal_block_emits_no_prompt(tmp_path: Path) -> None:
    """The content-free projection registers the block without its preimage;
    an empty prompt row would read as a rendering bug, not an absence."""
    path = tmp_path / "stella-events.jsonl"
    write_events(path, [goal_block(content=None)])
    assert TranscriptReader().read(path) == []


def test_the_goal_block_does_not_break_text_coalescing(tmp_path: Path) -> None:
    """The prompt arm sits on the non-delta path, which closes open
    text/reasoning runs — exactly as every ``block_registered`` already did.
    What must hold: the consolidated ``text`` event still supersedes its
    fragment run afterwards."""
    path = tmp_path / "stella-events.jsonl"
    write_events(
        path,
        [
            goal_block(),
            {"ts": 1786450724300, "type": "text_delta", "text": "half an ans"},
            {"ts": 1786450724400, "type": "text", "delta": "half an answer, whole"},
        ],
    )
    entries = TranscriptReader().read(path)
    texts = [e for e in entries if e["kind"] == "text"]
    assert texts, f"no consolidated text entry in {entries!r}"
    assert texts[-1]["body"] == "half an answer, whole"
