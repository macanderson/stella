# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""The context-recall stage, as an arena transcript renders it.

Until this existed, :class:`~arenabench.transcript.TranscriptReader` had no arm
for ``context_recall`` at all: the event fell through every branch of
``_entries_for`` to its trailing ``return []``, so **every arena transcript ever
produced silently dropped the entire stage**. That is not a presentation
problem. Context recall decides what the model sees before it does anything,
and a trial transcript is the artifact used to argue about whether recall
helped or hurt a run — the evidence was never in it.

The shapes below are the wire's, not a convenience form: ``frames`` is a list of
``ContextFrameRef`` (``citation_label``, ``kind``, ``uri``, ``token_cost``,
``content_digest``, …), the budget report is ``usage``, and ``provider_mix``
counts only the frames that won fusion. Reading a key the wire does not carry is
the exact failure mode this module's siblings were written to stop.
"""

from __future__ import annotations

import json
import unicodedata
from pathlib import Path

from arenabench.transcript import TranscriptReader


def _width(text: str) -> int:
    """Display columns ``text`` occupies — an *independent* measurement.

    Deliberately not imported from :mod:`arenabench.transcript`. The subject of
    these assertions is that module's own column arithmetic, and a test that
    borrows the arithmetic it is checking agrees with the implementation by
    construction, including where the implementation is wrong. Kept to the two
    rules that matter for a recall label: East Asian wide/fullwidth characters
    take two columns, a combining mark takes none.
    """
    return sum(
        0
        if unicodedata.combining(ch)
        else 2
        if unicodedata.east_asian_width(ch) in ("W", "F")
        else 1
        for ch in text
    )


def write_events(path: Path, events: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        for event in events:
            handle.write(json.dumps(event) + "\n")


def frame(label: str, kind: str, provider: str, tokens: int, **extra) -> dict:
    """One `ContextFrameRef`, in the shape Stella actually emits."""
    return {
        "citation_label": label,
        "kind": kind,
        "provider": provider,
        "source": "stella-graph" if provider == "code-graph" else "stella-context",
        "token_cost": tokens,
        **extra,
    }


def recall_event() -> dict:
    """The recall that prompted this work: four symbols and one episode."""
    return {
        "type": "context_recall",
        "frames": [
            frame(
                "fn line",
                "symbol",
                "code-graph",
                82,
                uri="arenabench/recorder/render.py:294",
                method="symbol-name",
                content_digest="sha256:9f2c1ab",
            ),
            frame(
                "fn review",
                "symbol",
                "code-graph",
                104,
                uri="crates/stella-cli/src/command_deck/hunk_gate.rs:32",
                method="symbol-name",
                content_digest="sha256:44a1bcd",
            ),
            frame(
                "create a new minor release 0.8.0 for stella and build a release "
                "notes page to publish",
                "episode",
                "workspace-memory",
                812,
                method="embedding",
                id="nod_01HQZ",
            ),
        ],
        "provider_mix": [
            {"provider": "code-graph", "frames": 2},
            {"provider": "workspace-memory", "frames": 1},
        ],
        "tokens": 998,
        "usage": {
            "budget_requested": 4000,
            "budget_consumed": 998,
            "as_of": "2026-08-09T00:00:00Z",
            "providers": [
                {
                    "provider_id": "code-graph",
                    "frames_served": 2,
                    "frames_rejected": 0,
                    "token_cost": 186,
                },
                {
                    "provider_id": "workspace-memory",
                    "frames_served": 1,
                    "frames_rejected": 2,
                    "token_cost": 812,
                },
            ],
        },
        "latency_ms": 34,
        "used_ann_index": True,
    }


class TestRecallReachesTheTranscript:
    def test_a_recall_produces_an_entry_at_all(self, tmp_path: Path):
        """The witness. `_entries_for` had no `context_recall` branch, so this
        returned an empty list and the stage was invisible in every transcript
        the arena has ever rendered."""
        path = tmp_path / "e.jsonl"
        write_events(path, [recall_event()])
        entries = TranscriptReader().read(path)
        assert len(entries) == 1, entries
        assert entries[0]["kind"] == "context_recall"

    def test_the_title_carries_the_facts_a_reader_scans_for(self, tmp_path: Path):
        """How much came back, what it cost, whether recall was the reason the
        turn felt slow, and whether the ANN index fired."""
        path = tmp_path / "e.jsonl"
        write_events(path, [recall_event()])
        title = TranscriptReader().read(path)[0]["title"]
        assert "3 frames" in title
        assert "998 tok" in title
        assert "34ms" in title
        assert "ann" in title

    def test_the_body_is_a_table_with_one_row_per_frame(self, tmp_path: Path):
        """A recall is a small table. Rendered as comma-joined labels it loses
        the boundary between records, the kind that separates an 812-token
        episodic memory from an 82-token graph symbol, and the per-frame cost
        that turns a total into a finding.

        A heading and a rule precede the rows, so the body reads as the table
        it is rather than as a run of aligned-looking text."""
        path = tmp_path / "e.jsonl"
        write_events(path, [recall_event()])
        body = TranscriptReader().read(path)[0]["body"]
        heading, rule, *rows = body.splitlines()
        assert heading.split() == ["kind", "citation", "location", "cost"], body
        assert set(rule) <= {"─", " "}, body
        assert len(rows) == 3, body
        assert all(row.endswith("tok") for row in rows), body
        assert "symbol" in rows[0]
        assert "episode" in rows[2]
        assert "812 tok" in rows[2]

    def test_every_row_is_the_same_width_and_the_rule_proves_it(
        self, tmp_path: Path
    ):
        """Alignment is the whole reason this is a table, and it is asserted in
        *display columns* — the unit a monospaced cell lays out in — not in code
        points, which agree with the truth only while every character is one
        column wide."""
        path = tmp_path / "e.jsonl"
        write_events(path, [recall_event()])
        body = TranscriptReader().read(path)[0]["body"]
        widths = {_width(line) for line in body.splitlines()}
        assert len(widths) == 1, f"ragged table {widths}:\n{body}"

        # Each rule segment starts where its heading cell starts, so the chrome
        # cannot drift away from the columns it claims to describe. Index
        # arithmetic is safe here: every character in these two rows is one
        # column wide, `─` included.
        heading, rule = body.splitlines()[:2]
        starts = [
            i
            for i, ch in enumerate(rule)
            if ch == "─" and (i == 0 or rule[i - 1] == " ")
        ]
        assert len(starts) == 4, f"one rule segment per column:\n{body}"
        for start, name in zip(starts, ["kind", "citation", "location"], strict=False):
            assert heading[start:].startswith(name), f"{name} at {start}:\n{body}"
        # `cost` is right-aligned over its digits, so it ends with its segment
        # rather than starting with it.
        assert heading.endswith("cost"), body
        assert rule.endswith("─"), body

    def test_a_wide_citation_never_shifts_the_columns(self, tmp_path: Path):
        """The failure an ASCII fixture cannot provoke.

        The label elision, the location's left-elision and the column fit were
        all counted in ``len`` while ``str.ljust`` realises them in display
        columns. The two agree until a recalled label carries CJK text — a
        recalled Chinese commit message, an emoji in an episodic memory's title
        — and then a 24-character label passes a 40-*column* budget, renders 48
        columns wide, and pushes the location and the cost off the grid on that
        row alone. Recalled labels are model- and user-authored prose, so this
        is an input, not a hypothetical."""
        path = tmp_path / "e.jsonl"
        event = recall_event()
        event["frames"][0]["citation_label"] = "在上下文中回忆一个符号并计算其令牌成本"
        write_events(path, [event])
        body = TranscriptReader().read(path)[0]["body"]
        widths = {_width(line) for line in body.splitlines()}
        assert len(widths) == 1, f"a wide label skewed the grid {widths}:\n{body}"
        # The location is what a skewed label displaces first, and the filename
        # is what a reader came to the row for.
        assert "hunk_gate.rs:32" in body, body

    def test_a_block_with_no_locations_drops_the_column(self, tmp_path: Path):
        """A column no frame can fill is dropped whole rather than left as a
        stripe of blanks — the same judgement the deck makes when a pane is too
        narrow to hold a location that would still say something."""
        path = tmp_path / "e.jsonl"
        event = recall_event()
        for f in event["frames"]:
            f.pop("uri", None)
        write_events(path, [event])
        body = TranscriptReader().read(path)[0]["body"]
        heading, _rule, *rows = body.splitlines()
        # Three columns, not four with one left blank — which is what a fixed
        # 44-column location cell produced: forty-four spaces on every row of
        # every recall whose frames carry no URI.
        assert heading.split() == ["kind", "citation", "cost"], body
        assert len(rows) == 3, body
        widths = {_width(line) for line in body.splitlines()}
        assert len(widths) == 1, f"ragged table {widths}:\n{body}"

    def test_a_location_keeps_its_filename_and_line(self, tmp_path: Path):
        """Elided from the left. The tail is what identifies the frame; the
        head is a repo prefix every row on the page already shares."""
        path = tmp_path / "e.jsonl"
        write_events(path, [recall_event()])
        body = TranscriptReader().read(path)[0]["body"]
        assert "hunk_gate.rs:32" in body

    def test_meta_carries_the_frames_structured_for_the_page(self, tmp_path: Path):
        """The React transcript page lays out its own table, so it gets the
        frames as data rather than having to re-parse the rendered body."""
        path = tmp_path / "e.jsonl"
        write_events(path, [recall_event()])
        meta = TranscriptReader().read(path)[0]["meta"]
        assert [f["kind"] for f in meta["frames"]] == ["symbol", "symbol", "episode"]
        assert [f["tokens"] for f in meta["frames"]] == [82, 104, 812]
        assert meta["ann"] is True
        assert meta["latency_ms"] == 34

    def test_the_budget_report_survives_including_rejections(self, tmp_path: Path):
        """`frames_rejected` is the number the frame list cannot show — a
        rejected frame never reaches it — so it is the only evidence that a
        provider misdeclared its cost."""
        path = tmp_path / "e.jsonl"
        write_events(path, [recall_event()])
        budget = TranscriptReader().read(path)[0]["meta"]["budget"]
        assert budget["requested"] == 4000
        assert budget["consumed"] == 998
        rejected = {p["provider_id"]: p["frames_rejected"] for p in budget["providers"]}
        assert rejected == {"code-graph": 0, "workspace-memory": 2}

    def test_an_absent_digest_is_carried_as_absent(self, tmp_path: Path):
        """Per the context-reuse spec a frame with no digest is *not
        verifiable* and a host must re-query rather than reuse it, so the
        absence is meaningful and is carried rather than papered over."""
        path = tmp_path / "e.jsonl"
        write_events(path, [recall_event()])
        frames = TranscriptReader().read(path)[0]["meta"]["frames"]
        assert frames[0]["digest"] == "sha256:9f2c1ab"
        assert frames[2]["digest"] is None


class TestRecallDegradesRatherThanVanishing:
    def test_a_stream_with_no_latency_or_index_omits_them(self, tmp_path: Path):
        """`latency_ms: 0` means *not measured*, never "instant", and a recall
        path that does not report the ANN flag has said nothing rather than
        `false`. Neither is invented."""
        path = tmp_path / "e.jsonl"
        event = recall_event()
        del event["latency_ms"]
        del event["used_ann_index"]
        write_events(path, [event])
        title = TranscriptReader().read(path)[0]["title"]
        assert "ms" not in title
        assert "ann" not in title and "scan" not in title

    def test_a_frame_recorded_before_kind_existed_still_names_itself(
        self, tmp_path: Path
    ):
        """An empty kind column reads as a rendering bug; `frame` reads as the
        missing field it is."""
        path = tmp_path / "e.jsonl"
        write_events(
            path,
            [
                {
                    "type": "context_recall",
                    "frames": [{"citation_label": "old", "token_cost": 10}],
                    "tokens": 10,
                }
            ],
        )
        entry = TranscriptReader().read(path)[0]
        assert entry["meta"]["frames"][0]["kind"] == "frame"
        assert "frame" in entry["body"]

    def test_a_recall_with_no_usage_report_has_no_budget(self, tmp_path: Path):
        """A recall path with no CGP host behind it produces no report, and an
        absent report is `None` rather than zeroes that would read as a
        measured budget of nothing."""
        path = tmp_path / "e.jsonl"
        event = recall_event()
        del event["usage"]
        write_events(path, [event])
        assert TranscriptReader().read(path)[0]["meta"]["budget"] is None
