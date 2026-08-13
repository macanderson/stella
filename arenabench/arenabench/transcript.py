# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Turning a trial's append-only event stream into renderable transcript lines.

Split out of :mod:`arenabench.telemetry`, which had grown past the 1500-line
ratchet (#2397). The seam is not arbitrary: this half shares *no* module-level
name with the metrics half — no path constant, no reader, no reducer — because
the two answer different questions about the same files. :mod:`telemetry`
reduces a trial to the scoreboard's dimensions; this module replays one trial's
stream in order, for a human reading along.

:class:`TranscriptReader` keeps a byte offset per file and yields only what is
new, which is what the SSE endpoint streams. :class:`TranscriptState` is that
cursor. :func:`_proof_line` renders one ``proof`` step, and is the one place in
either module that deliberately does **not** truncate its body.
"""

from __future__ import annotations

import json
import time
import unicodedata
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .proof import flip_outcome
from .toolclass import class_label, classify
from .toolout import (
    cap_middle,
    decode_tool_output,
    format_tool_input,
    strip_ansi,
)

__all__ = [
    "TranscriptReader",
    "TranscriptState",
]

#: How much of one tool result the transcript keeps. Generous, because the
#: page folds a long body behind a disclosure rather than making the reader
#: leave — and because the cut is a *middle* elision (:func:`cap_middle`), so
#: raising it costs bytes on the wire and never costs the end of a payload.
TOOL_RESULT_BUDGET = 8000

#: The same for a tool call's full argument object, kept beside the one-line
#: label as ``meta.raw`` for the page's expanded view.
TOOL_INPUT_BUDGET = 4000

#: The file-*mutating* built-ins — the only calls whose result entry carries an
#: inline diff (reads must not). A port of ``is_file_mutation`` in
#: ``crates/stella-tui/src/model/summarize.rs``, which itself must stay in
#: lockstep with the ``FileChange`` emitter that owns the list.
_FILE_MUTATIONS = frozenset({"write_file", "edit_file", "apply_edits", "delete_file"})


def _tool_input_path(arguments: Any) -> str | None:
    """The workspace-relative path a file tool targets — the join key between
    a tool result and the ``file_change`` diff its call produced.

    A port of ``tool_input_path`` (beside ``is_file_mutation``, above): every
    built-in file tool takes its path under the ``path`` key, and the engine
    emits ``file_change`` for that same path. ``apply_edits`` carries its
    paths in a batch rather than at the top level; the first edit's path
    stands in, so a single-file batch still renders an inline diff under its
    result row.
    """
    if not isinstance(arguments, dict):
        return None
    path = arguments.get("path")
    if isinstance(path, str):
        return path
    edits = arguments.get("edits")
    if isinstance(edits, list) and edits and isinstance(edits[0], dict):
        first = edits[0].get("path")
        if isinstance(first, str):
            return first
    return None


def _proof_line(step: dict[str, Any]) -> tuple[str, str, dict[str, Any]]:
    """One ``proof`` step as a transcript line: title, body, metadata.

    The body is **never truncated**. Every other long field in this reader is
    clipped because a transcript is a reading surface and a 60 KB tool result
    helps nobody — but a proof reason is the pipeline's entire explanation of
    why it could not prove something, and the half that gets cut is reliably
    the half that says which model, which command, or which constraint. Those
    strings are bounded by construction: the longest is a model verdict's
    prose, and it is the thing a reader opened this page to read.
    """
    kind = str(step.get("kind") or "proof")
    meta: dict[str, Any] = {"step": kind}
    reason = str(step.get("reason") or "")

    if kind == "warrant":
        required = bool(step.get("required"))
        lines = step.get("diff_lines")
        meta.update(required=required, diff_lines=lines)
        title = "warrant: proof required" if required else "warrant: no proof required"
        detail = f"{lines} diff lines" if lines is not None else ""
        return title, reason or detail, meta

    if kind == "assurance":
        witness = bool(step.get("witness"))
        verifier = step.get("verifier")
        if verifier is None:
            verifier = step.get("judge")
        meta.update(witness=witness, verifier=verifier)
        return (
            "assurance",
            f"witness {'on' if witness else 'off'}"
            f" · verifier {'on' if verifier else 'off'}",
            meta,
        )

    if kind == "witness_authored":
        meta.update(
            path=step.get("path"),
            command=step.get("command"),
            fingerprint=step.get("fingerprint"),
        )
        return (
            "witness authored",
            f"{step.get('path') or ''}\n{step.get('command') or ''}",
            meta,
        )

    if kind == "oracle":
        passed = bool(step.get("passed"))
        tree = str(step.get("tree") or "")
        meta.update(passed=passed, tree=tree, command=step.get("command"))
        return (
            f"oracle: {tree} {'passed' if passed else 'failed'}",
            str(step.get("command") or ""),
            meta,
        )

    if kind == "verdict_degraded":
        meta.update(candidate=step.get("candidate"))
        return "verdict degraded", reason, meta

    # `witness_unavailable`, `verification_unavailable`, and any step kind
    # added to the protocol after this was written. An unknown kind renders
    # as itself with whatever it carries rather than vanishing — the same
    # posture `_entries_for` takes on unknown event types.
    if reason:
        return kind.replace("_", " "), reason, meta
    return kind.replace("_", " "), json.dumps(
        {k: v for k, v in step.items() if k != "kind"}
    ), meta


#: Columns a citation label is elided to in the rendered recall table. Labels
#: are heterogeneous by nature — ``fn review`` beside a whole recalled user
#: prompt — so the column is capped rather than sized to the widest, which one
#: episodic memory would otherwise stretch across the page on its own.
RECALL_LABEL_COL = 40

#: Columns a frame's location is elided to. Left-elided, never right: a recall
#: row is actionable because of its filename and line.
RECALL_LOCATION_COL = 44

#: The gutter between two columns of the recall table. Two, not one: a single
#: space between an elided label ending in ``…`` and a location starting in
#: ``…`` reads as one token rather than two cells.
RECALL_GAP = "  "


def _width(text: str) -> int:
    """Columns ``text`` occupies in a monospaced cell.

    Every width in the recall table is measured through here, because a
    terminal — and the monospaced ``<pre>`` this body lands in — lays out in
    *columns*, while ``len`` and ``str.ljust`` count *code points*. The two
    agree only while every character is one column wide, which is why sizing a
    table in ``len`` looks correct against an ASCII fixture and skews the whole
    grid the first time a recalled label carries CJK text or an emoji. Recalled
    labels are model- and user-authored prose, so that is an input.

    East Asian *wide* and *fullwidth* characters take two columns; a combining
    mark takes none, since it composes onto the character before it.
    """
    return sum(
        0
        if unicodedata.combining(ch)
        else 2
        if unicodedata.east_asian_width(ch) in ("W", "F")
        else 1
        for ch in text
    )


def _take(text: str, cap: int, *, right: bool = False) -> str:
    """The longest prefix (or suffix) of ``text`` fitting in ``cap`` columns."""
    kept: list[str] = []
    used = 0
    for ch in reversed(text) if right else text:
        w = _width(ch)
        if used + w > cap:
            break
        used += w
        kept.append(ch)
    return "".join(reversed(kept) if right else kept)


def _cell(text: str, col: int, *, right: bool = False) -> str:
    """One table cell: ``text`` in *exactly* ``col`` display columns.

    Every cell of the rendered table goes through here, which is what makes
    the alignment a property of the construction rather than a convention each
    call site has to honour: a cell physically cannot displace the cell to its
    right, so no future field can quietly break the grid.
    """
    text = _elide(text, col)
    pad = " " * max(col - _width(text), 0)
    return pad + text if right else text + pad


def _elide_left(text: str, cap: int) -> str:
    """``…/command_deck/hunk_gate.rs:32`` — keep the tail, drop the prefix.

    A frame's URI is a ``path:line`` whose *tail* identifies it; the head is a
    repo prefix every row on the page already shares. Cutting from the right —
    what a plain CSS truncation does — removes exactly the discriminating part.
    """
    if _width(text) <= cap:
        return text
    if cap <= 0:
        return ""
    tail = _take(text, cap - 1, right=True)
    cut = tail.find("/")
    # Snap the elision to a separator so it lands between path segments rather
    # than mid-directory-name, but only in the leading third — snapping near
    # the end trades a readable ``…re/src/driver.rs:88`` for a bare filename.
    # The third is measured in *columns*: ``find`` returns a code-point index,
    # and comparing that to a column budget refuses the snap on any path with
    # a wide character in front of the cut.
    return "…" + (tail[cut:] if 0 <= _width(tail[:cut]) <= cap // 3 else tail)


def _recall_line(event: dict[str, Any]) -> tuple[str, str, dict[str, Any]]:
    """One ``context_recall`` event as a transcript line: title, body, metadata.

    This reader had **no arm for the event at all** — it fell through to
    ``return []`` — so every arena transcript ever produced silently dropped the
    entire context-recall stage. That is the stage that decides what the model
    sees before it does anything, and the transcript is the artifact used to
    argue about whether recall helped a run.

    The body is the same table the Command Deck renders
    (``crates/stella-tui/src/render/entry.rs``), in plain text: one row per
    frame with its kind, citation, location and token cost. A recall is a small
    table and every surface that rendered it as comma-joined prose lost the
    boundary between records, the kind that separates an 800-token episodic
    memory from a 60-token graph symbol, and the per-frame cost that turns a
    total into a finding.

    ``meta`` carries the frames structured as well, so the React page can lay
    out its own table without re-parsing this one.
    """
    frames = event.get("frames")
    frames = frames if isinstance(frames, list) else []
    tokens = event.get("tokens") or 0
    latency = event.get("latency_ms") or 0
    ann = event.get("used_ann_index")

    head = f"recall · {len(frames)} frames · {tokens} tok"
    # `0` means *not measured* on the wire, never "instant", so it is omitted
    # rather than reported as a measurement nobody took.
    if latency:
        head += f" · {latency}ms"
    if ann is not None:
        head += " · ann" if ann else " · scan"

    rows: list[dict[str, Any]] = []
    for frame in frames:
        if not isinstance(frame, dict):
            continue
        # An empty kind is a stream recorded before the field existed. `frame`
        # says that honestly; a blank column reads as a rendering bug.
        rows.append(
            {
                "kind": str(frame.get("kind") or "frame"),
                "label": str(frame.get("citation_label") or ""),
                "uri": frame.get("uri"),
                "provider": str(frame.get("provider") or ""),
                "source": str(frame.get("source") or ""),
                "method": frame.get("method"),
                "id": frame.get("id"),
                # A missing digest is not nothing: per the context-reuse spec
                # such a frame is *not verifiable* and a host must re-query
                # rather than reuse it, so the page reports the absence.
                "digest": frame.get("content_digest"),
                "tokens": frame.get("token_cost") or 0,
            }
        )

    body = _recall_table(rows)

    usage = event.get("usage")
    usage = usage if isinstance(usage, dict) else {}
    legs = usage.get("providers")
    mix = event.get("provider_mix")
    meta: dict[str, Any] = {
        "frames": rows,
        "tokens": tokens,
        "latency_ms": latency,
        "ann": ann,
        # The frames that *won fusion and reached the prompt*, per leg.
        "providers": mix if isinstance(mix, list) else [],
        # What each leg served and what the host rejected — a strictly
        # different question, and the only place a provider that misdeclared
        # its cost is visible, since a rejected frame never reaches `frames`.
        "budget": {
            "requested": usage.get("budget_requested"),
            "consumed": usage.get("budget_consumed"),
            "providers": legs if isinstance(legs, list) else [],
        }
        if usage
        else None,
    }
    return head, body, meta


def _recall_table(rows: list[dict[str, Any]]) -> str:
    """The recall's frames as an aligned table: a heading, a rule, one row each.

    Laid out the way the Command Deck lays it out
    (``crates/stella-tui/src/render/entry/recall.rs``), because it is the same
    data answering the same questions. The deck has a pane to fit and flushes
    its cost column to the edge; this body has no width to track, so the grid
    is fixed and every column closes up against the next.

    Columns are fitted to the block rather than set to a constant — a page of
    ``symbol`` frames should not reserve the width of the longest kind the
    protocol can name — and capped, so one recalled user prompt cannot stretch
    the citation column across the page. The location column is dropped whole
    when no frame carries a URI, rather than left as a stripe of blanks.
    """
    if not rows:
        return ""

    def fit(values: list[str], heading: str) -> int:
        """The column's width: its widest cell, but never narrower than the
        heading it has to sit under — a heading elided into its own column
        would name a column nothing else in the table keeps."""
        return max(max((_width(v) for v in values), default=0), _width(heading))

    locations = [
        _elide_left(str(r["uri"]), RECALL_LOCATION_COL) for r in rows if r["uri"]
    ]
    labels = [_elide(str(r["label"]), RECALL_LABEL_COL) for r in rows]
    costs = [f"{r['tokens']} tok" for r in rows]

    kind_col = fit([str(r["kind"]) for r in rows], "kind")
    label_col = fit(labels, "citation")
    location_col = fit(locations, "location") if locations else 0
    cost_col = fit(costs, "cost")

    def line(kind: str, label: str, location: str, cost: str) -> str:
        cells = [_cell(kind, kind_col), _cell(label, label_col)]
        if location_col:
            cells.append(_cell(location, location_col))
        cells.append(_cell(cost, cost_col, right=True))
        return RECALL_GAP.join(cells)

    # The rule is drawn per column rather than as one hairline across the page,
    # so it *shows* the grid: each segment is exactly its column's width, which
    # makes a column that has drifted visible in the chrome itself.
    out = [
        line("kind", "citation", "location", "cost"),
        line(*("─" * n for n in (kind_col, label_col, location_col, cost_col))),
    ]
    out += [
        line(
            str(r["kind"]),
            _elide(str(r["label"]), RECALL_LABEL_COL),
            _elide_left(str(r["uri"]), RECALL_LOCATION_COL) if r["uri"] else "",
            f"{r['tokens']} tok",
        )
        for r in rows
    ]
    return "\n".join(out)


def _elide(text: str, cap: int) -> str:
    """Truncate to ``cap`` display columns with a trailing ellipsis.

    The ``…`` is one column, so the kept text is budgeted ``cap - 1``. A cut
    that would land mid-wide-character keeps the narrower text and lets
    :func:`_cell` pad the hole, rather than emitting a cell one column over.
    """
    if _width(text) <= cap:
        return text
    if cap <= 0:
        return ""
    return _take(text, cap - 1).rstrip() + "…"


@dataclass
class TranscriptState:
    """Cursor into one trial's transcript."""

    offset: int = 0
    seq: int = 0
    started: float | None = None
    #: The first line's own ``ts`` (epoch millis), when the stream carries one.
    #: Every later entry's ``t`` is measured from this, so a finished trial read
    #: in one pass reports the offsets the run actually had rather than the
    #: offsets of the *read* (#2111). ``None`` for a stream recorded before the
    #: field existed, which falls back to ``started``.
    origin_ms: float | None = None
    #: Open text/reasoning entries being accumulated from deltas, by kind.
    open_entries: dict[str, int] = field(default_factory=dict)
    #: ``call_id`` -> entry sequence, so a tool result can find its call.
    tool_index: dict[str, int] = field(default_factory=dict)
    #: ``call_id`` -> tool name. A ``tool_result`` carries only the id, so the
    #: name a result row is labelled with is recovered from the ``tool_start``
    #: that opened the call — the correlation the deck's fold has always done
    #: (``crates/stella-tui/src/model.rs``) and this reader never did, which is
    #: why every result in every arena transcript read "result".
    tool_names: dict[str, str] = field(default_factory=dict)
    #: ``call_id`` -> ``(path, change_seq when the call opened)`` for a
    #: *mutating* file tool (:data:`_FILE_MUTATIONS`). The path is the join
    #: key to the ``file_change`` the call will emit; the mark is what makes
    #: the join honest: only a mutation recorded *after* the call opened may
    #: ride its result, so a no-op edit — which emits no ``file_change`` —
    #: cannot inherit the path's previous diff. The deck keeps the same
    #: discipline with ``FileState::changes`` as its freshness tag
    #: (``crates/stella-tui/src/render/entry.rs``).
    tool_paths: dict[str, tuple[str, int]] = field(default_factory=dict)
    #: Mutations folded so far — the clock :attr:`tool_paths` marks are read
    #: against.
    change_seq: int = 0
    #: path -> the latest mutation recorded for it: ``{diff, added, removed,
    #: at}``. ``diff`` is ``None`` for a mutation that carried none — recorded
    #: anyway, so it *supersedes* an older diff rather than exposing it to the
    #: next result on the same path.
    file_diffs: dict[str, dict[str, Any]] = field(default_factory=dict)
    #: The last fragment-built text entry not yet superseded by its
    #: consolidated ``text`` event. Outlives ``open_entries`` on purpose:
    #: ``step_usage`` routinely closes the run *before* the consolidated
    #: event arrives, and the consolidation must still find its run.
    pending_text: int | None = None


class TranscriptReader:
    """Turns an append-only event stream into renderable transcript entries.

    Stateful and incremental by design: it remembers a byte offset per trial
    and returns only entries produced since the last call, which is what lets
    an SSE connection push deltas instead of re-sending a transcript that may
    already be thousands of entries long.

    Streaming deltas are *coalesced*. Stella emits ``text`` and ``reasoning``
    as many small fragments; forwarding each one as its own entry would make
    the browser do the reassembly and would flood the wire. Instead each run of
    fragments becomes one entry that grows, and the entry is re-sent with the
    same ``seq`` — so a client keyed by ``seq`` naturally replaces rather than
    appends.
    """

    def __init__(
        self,
        *,
        tool_result_budget: int = TOOL_RESULT_BUDGET,
        tool_input_budget: int = TOOL_INPUT_BUDGET,
    ) -> None:
        #: `cap_middle` treats a non-positive budget as "no cap" — so a caller
        #: reading a *finished* trial for the "load full transcript" button
        #: (`ArenaServer.full_transcript`) passes `0` here instead of the SSE
        #: endpoint's generous-but-still-finite defaults, and gets every byte
        #: the trial actually wrote rather than the wire-cost compromise a live
        #: stream has to make.
        self._tool_result_budget = tool_result_budget
        self._tool_input_budget = tool_input_budget
        self._states: dict[Path, TranscriptState] = {}
        #: Accumulated bodies for open streaming entries, keyed by
        #: ``(path, seq)``. Per-instance: a class-level dict would be shared by
        #: every reader in the process and would leak for the process lifetime.
        self._bodies: dict[tuple[str, int], str] = {}

    def reset(self, path: Path) -> None:
        self._states.pop(path, None)
        prefix = str(path)
        for key in [k for k in self._bodies if k[0] == prefix]:
            del self._bodies[key]

    def read(self, path: Path, *, limit: int = 2000) -> list[dict[str, Any]]:
        """Entries produced since the previous call for this path."""
        state = self._states.setdefault(path, TranscriptState())
        try:
            stat = path.stat()
        except OSError:
            return []
        if stat.st_size < state.offset:
            # Truncated or replaced — a re-run of the same trial. Start over
            # rather than reading from a stale offset into unrelated bytes.
            self._states[path] = state = TranscriptState()
        if stat.st_size == state.offset:
            return []

        entries: list[dict[str, Any]] = []
        try:
            with path.open("rb") as handle:
                handle.seek(state.offset)
                raw = handle.read()
        except OSError:
            return []

        # Keep an incomplete trailing line for the next read: the writer may be
        # mid-append, and half a JSON object is not an event.
        text = raw.decode("utf-8", errors="replace")
        consumed = len(raw)
        if not text.endswith("\n"):
            cut = text.rfind("\n")
            if cut == -1:
                return []
            consumed = len(text[: cut + 1].encode("utf-8"))
            text = text[: cut + 1]
        state.offset += consumed

        for line in text.splitlines():
            if not line.strip():
                continue
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if isinstance(event, dict):
                entries.extend(self._entries_for(event, state, str(path)))
            if len(entries) >= limit:
                break
        return entries

    def _next_seq(self, state: TranscriptState) -> int:
        state.seq += 1
        return state.seq

    @staticmethod
    def _elapsed_for(event: dict[str, Any], state: TranscriptState) -> float:
        """Seconds from the trial's first event to this one.

        Read from the line's own ``ts`` (epoch millis, stamped by Stella's sink
        — see ``stella_protocol::journal``). Measuring from ``time.time()``
        instead was the #2111 bug: a finished trial is read in a single pass, so
        every event in the file was parsed within the same few milliseconds and
        every row rendered ``0:00``. Anchoring to the *stream's* first stamp
        makes an archived transcript report the offsets the run actually had —
        and makes the paced replay in ``ui/components/arena/transcript-page.tsx``
        pace on them instead of flushing at its 16 ms floor.

        Two hedges the wire contract requires. A system clock is not monotonic,
        so an NTP step can put a later line before the origin; that is clamped
        to ``0.0`` rather than rendered as a negative offset. And a stream with
        no ``ts`` at all — anything recorded before the field existed — keeps
        the old read-time behaviour, which is wrong in the same way it always
        was but is the only thing such a file supports.
        """
        stamp = event.get("ts")
        if isinstance(stamp, (int, float)) and not isinstance(stamp, bool):
            if state.origin_ms is None:
                state.origin_ms = float(stamp)
            return round(max(0.0, (float(stamp) - state.origin_ms) / 1000.0), 2)
        if state.started is None:
            state.started = time.time()
        return round(time.time() - state.started, 2)

    def _entries_for(
        self, event: dict[str, Any], state: TranscriptState, path_key: str
    ) -> list[dict[str, Any]]:
        kind = str(event.get("type", ""))
        now = self._elapsed_for(event, state)

        def entry(
            seq: int, etype: str, title: str, body: str = "", **meta: Any
        ) -> dict[str, Any]:
            return {
                "seq": seq,
                "t": now,
                "kind": etype,
                "title": title,
                "body": body,
                "meta": meta,
            }

        # ---- streaming text / reasoning: coalesce into one growing entry ----
        # The field names are crossed on the wire and that is the trap:
        # `text_delta` events carry a *fragment* (in `text`), while the one
        # `text` event per message carries the *complete* body (in `delta`).
        if kind in ("text_delta", "reasoning"):
            bucket = "reasoning" if kind == "reasoning" else "text"
            fragment = str(event.get("delta") or event.get("text") or "")
            if not fragment:
                return []
            seq = state.open_entries.get(bucket)
            if seq is None:
                seq = self._next_seq(state)
                state.open_entries[bucket] = seq
                if bucket == "text":
                    state.pending_text = seq
            key = (path_key, seq)
            self._bodies[key] = self._bodies.get(key, "") + fragment
            return [
                entry(
                    seq,
                    bucket,
                    "reasoning" if bucket == "reasoning" else "response",
                    self._bodies[key],
                    streaming=True,
                )
            ]

        if kind == "text":
            # The consolidated message. It REPLACES the fragment run rather
            # than appending to it: appending rendered every response twice
            # (once assembled from fragments, once whole), and replacing also
            # self-heals any fragment this reader never saw. `pending_text`
            # rather than `open_entries` finds the run, because a `step_usage`
            # usually closed the run before this event arrived. The message is
            # complete, so both trackers reset here.
            full = str(event.get("delta") or event.get("text") or "")
            if not full:
                return []
            seq = state.open_entries.get("text") or state.pending_text
            if seq is None:
                seq = self._next_seq(state)
            state.open_entries.clear()
            state.pending_text = None
            self._bodies[(path_key, seq)] = full
            return [entry(seq, "text", "response", full)]

        # Any non-delta event closes the open text/reasoning runs, so the next
        # fragment starts a fresh entry rather than reopening a finished one.
        state.open_entries.clear()

        if kind == "tool_start":
            call = event.get("call") if isinstance(event.get("call"), dict) else {}
            call_id = str(call.get("call_id") or call.get("id") or "")
            name = str(call.get("name") or "tool")
            seq = self._next_seq(state)
            if call_id:
                state.tool_index[call_id] = seq
                state.tool_names[call_id] = name
            # The arguments ride under `input` — `ToolCall { call_id, name,
            # input }`. This read used to ask for `arguments`, a key the wire
            # has never carried, so every tool row in every arena transcript
            # rendered with an empty body: a `bash` call did not show its
            # command, an `edit_file` did not show its path.
            #
            # The row gets the one-line label the deck puts beside the name;
            # the whole object rides as `meta.raw` for the expanded view, so
            # nothing is lost by summarizing here.
            arguments = call.get("input", call.get("arguments"))
            # A mutating file tool's target, kept so the *result* entry can
            # carry the diff of the change this very call made — the same
            # correlate-through-state move `tool_names` makes for the name,
            # keyed the same way.
            if call_id and name in _FILE_MUTATIONS:
                path = _tool_input_path(arguments)
                if path is not None:
                    state.tool_paths[call_id] = (path, state.change_seq)
            # The call's CLASS (`crate::tool_class` in the deck, ported at
            # `arenabench.toolclass`) — a read, a write, a shell, a test, a
            # push, a hand-off. The page paints the name in this class's hue
            # rather than the arena's flat accent, the same categorical
            # colour the deck uses and for the same reason: the class is the
            # first question a reader asks of any row, and it should answer
            # from the margin before a single name is read.
            cls = classify(name)
            return [
                entry(
                    seq,
                    "tool",
                    name,
                    format_tool_input(name, arguments),
                    call_id=call_id,
                    state="running",
                    tool_class=cls,
                    tool_class_label=class_label(cls),
                    raw=cap_middle(
                        json.dumps(arguments, indent=2, ensure_ascii=False)
                        if isinstance(arguments, (dict, list))
                        else "",
                        self._tool_input_budget,
                    ),
                )
            ]

        if kind == "tool_result":
            call_id = str(event.get("call_id") or "")
            # `ToolOutput` is externally tagged, and the tag is the verdict.
            # Reading `event["error"]` — a key the wire does not carry — meant
            # every failed call rendered as a success, in the success colour,
            # with the failure message presented as ordinary output.
            decoded = decode_tool_output(event.get("output", event.get("result")))
            body = cap_middle(strip_ansi(decoded.text), self._tool_result_budget)
            name = state.tool_names.get(call_id, "tool")
            cls = classify(name)
            # The mutation's diff, correlated onto the result the same way the
            # name is: through state recorded at `tool_start`. The
            # `file_change` event precedes its `tool_result` on the wire
            # (`ToolRegistry::record_touch` emits mid-execution), so by the
            # time this result folds, the path's latest recorded mutation is
            # this very call's — and the `at > seen` mark guards the one case
            # where it is not: a call that emitted no `file_change` must not
            # inherit an older call's diff. Gated to successful calls, like
            # the deck (`crates/stella-tui/src/model.rs`): a failed call
            # produced no `FileChange`, and rendering the path's previous diff
            # under its ✗ would attribute a change the call never made.
            #
            # The diff is carried whole, never `cap_middle`d: a mid-hunk
            # elision garbles the render, and the emitter already bounds it
            # (`crates/stella-tools/src/file_touch.rs::changed_region_diff`
            # caps each side of the changed region).
            mutation: dict[str, Any] = {}
            marked = state.tool_paths.get(call_id)
            if decoded.ok and marked is not None:
                path, seen = marked
                change = state.file_diffs.get(path)
                if change is not None and change["at"] > seen and change["diff"]:
                    mutation = {
                        "diff": change["diff"],
                        "diff_path": path,
                        "diff_added": change["added"],
                        "diff_removed": change["removed"],
                    }
            return [
                entry(
                    self._next_seq(state),
                    "tool_result",
                    name,
                    body,
                    call_id=call_id,
                    error=not decoded.ok,
                    unrecognized=not decoded.recognized,
                    duration_ms=event.get("duration_ms"),
                    speculated=bool(event.get("speculated")),
                    lines=body.count("\n") + 1 if body else 0,
                    tool_class=cls,
                    tool_class_label=class_label(cls),
                    **mutation,
                )
            ]

        if kind == "step_usage":
            return [
                entry(
                    self._next_seq(state),
                    "usage",
                    f"step {event.get('step')} · {event.get('role') or 'model'}",
                    "",
                    model=event.get("model"),
                    role=event.get("role"),
                    tokens_in=event.get("input_tokens"),
                    tokens_out=event.get("output_tokens"),
                    cache_read=event.get("cached_input_tokens"),
                    cache_write=event.get("cache_write_tokens"),
                    cost_usd=event.get("cost_usd"),
                    duration_ms=event.get("duration_ms"),
                    tool_calls=event.get("tool_calls"),
                )
            ]

        if kind == "stage":
            return [
                entry(self._next_seq(state), "stage", str(event.get("name") or "stage"))
            ]

        if kind == "error":
            return [
                entry(
                    self._next_seq(state),
                    "error",
                    "error",
                    str(event.get("message") or ""),
                    retryable=event.get("retryable"),
                )
            ]

        if kind == "proof":
            step = event.get("step")
            if not isinstance(step, dict):
                return []
            title, body, meta = _proof_line(step)
            return [entry(self._next_seq(state), "proof", title, body, **meta)]

        if kind == "context_recall":
            title, body, meta = _recall_line(event)
            return [
                entry(self._next_seq(state), "context_recall", title, body, **meta)
            ]

        if kind in ("verdict", "judge_verdict"):
            passed = event.get("passed")
            # The body is `evidence.summary`. It was read from a top-level
            # `reasoning` key that the wire has never carried, so every
            # verdict in every transcript rendered with an empty body — and
            # the summary is where the pipeline says whether the pass was
            # actually proven (`UNVERIFIABLE`/`UNVERIFIED`/`UNPROVEN`).
            evidence = event.get("evidence")
            evidence = evidence if isinstance(evidence, dict) else {}
            ladder = evidence.get("ladder")
            ladder = ladder if isinstance(ladder, dict) else {}
            return [
                entry(
                    self._next_seq(state),
                    "verdict",
                    "verifier: pass" if passed else "verifier: fail",
                    str(evidence.get("summary") or event.get("reasoning") or ""),
                    passed=passed,
                    rung=ladder.get("rung"),
                    deterministic=evidence.get("deterministic"),
                    # The tri-state, through the one decoder (#2556): a bool
                    # here could not tell "the oracle ran and came back short"
                    # from "nothing was ever in a position to observe a flip",
                    # and a transcript that shows the second as the first is
                    # reporting a shortfall nobody measured.
                    flip=flip_outcome(ladder),
                    witness_intact=ladder.get("witness_intact"),
                    verifier_independent=ladder.get("verifier_independent"),
                    diff_coverage=ladder.get("diff_coverage"),
                    oracle_trace=ladder.get("oracle_trace"),
                )
            ]

        if kind == "complete":
            return [
                entry(
                    self._next_seq(state),
                    "complete",
                    "complete",
                    "",
                    model=event.get("model"),
                    cost_usd=event.get("cost_usd"),
                )
            ]

        if kind == "file_change":
            # Not (only) a row: the correlation state the *result* entry needs
            # to render this mutation's diff inline (see the `tool_result`
            # arm). `added`/`removed` are the emitter's own counts and are
            # carried rather than recounted from the diff — the diff is a
            # bounded, deliberately coarse rendering of the changed region,
            # and re-deriving the delta from it is the exact disagreement the
            # event's contract warns against
            # (`crates/stella-protocol/src/event.rs::FileChange`). A mutation
            # with no diff is recorded too, so it supersedes the path's
            # previous diff instead of leaving it to be misattributed. Reads
            # carry `0/0` and no diff, and record nothing.
            path = event.get("path")
            diff = event.get("diff")
            if isinstance(path, str) and path and str(event.get("kind") or "") != "read":
                state.change_seq += 1
                state.file_diffs[path] = {
                    "diff": diff if isinstance(diff, str) and diff else None,
                    "added": event.get("added") or 0,
                    "removed": event.get("removed") or 0,
                    "at": state.change_seq,
                }
            # ...and then the compact row below, unchanged.

        if kind in ("file_change", "commit", "task_update", "loop_detected"):
            return [
                entry(
                    self._next_seq(state),
                    kind,
                    kind.replace("_", " "),
                    json.dumps({k: v for k, v in event.items() if k != "type"})[:2000],
                )
            ]

        return []
