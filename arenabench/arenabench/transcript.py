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
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

__all__ = [
    "TranscriptReader",
    "TranscriptState",
]


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

    def __init__(self) -> None:
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
            call_id = str(call.get("id") or call.get("call_id") or "")
            seq = self._next_seq(state)
            if call_id:
                state.tool_index[call_id] = seq
            arguments = call.get("arguments")
            body = (
                json.dumps(arguments, indent=2)[:4000]
                if isinstance(arguments, (dict, list))
                else str(arguments or "")[:4000]
            )
            return [
                entry(
                    seq,
                    "tool",
                    str(call.get("name") or "tool"),
                    body,
                    call_id=call_id,
                    state="running",
                )
            ]

        if kind == "tool_result":
            call_id = str(event.get("call_id") or "")
            output = str(event.get("output") or event.get("result") or "")
            is_error = bool(event.get("error") or event.get("is_error"))
            return [
                entry(
                    self._next_seq(state),
                    "tool_result",
                    "error" if is_error else "result",
                    output[:8000],
                    call_id=call_id,
                    error=is_error,
                    duration_ms=event.get("duration_ms"),
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
                    flip_achieved=ladder.get("flip_achieved"),
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
