# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Reading a third-party agent's own harness while it runs.

Stella publishes ``agent/stella-events.jsonl`` per event, so every number on
the scoreboard is current for a Stella arm and stale for nobody. Its opponents
publish ATIF — ``agent/trajectory.json`` — which is written **once, at the
end**. The consequence was that a Claude Code arm read zero steps, zero tools,
zero tokens and zero cost for the entire length of a match, and only became
real at teardown. :func:`arenabench.telemetry._liveness_age` made it look alive
throughout, because file mtimes moved; nothing measured it.

The fix needed no new instrumentation. Harbor runs Claude Code with
``--verbose --output-format=stream-json`` and tees stdout to
``/logs/agent/claude-code.txt``, which lands in the trial directory and is
appended **as the agent works**. That file has always carried more than ATIF
ever exposed, and only one field of it was ever read (Harbor's own parser digs
out ``total_cost_usd`` after the run). This module reads the rest.

# Why not hooks

Claude Code can be instrumented properly, with ``PreToolUse``/``PostToolUse``
hooks written into ``$CLAUDE_CONFIG_DIR/settings.json`` — which Harbor points
at ``/logs/agent/sessions``, so the seam is available. That was the obvious
design and it is the wrong one for the *measured* arm: a hook is a process
spawn on the agent's critical path, twice per tool call, in the arm whose
numbers are the bar everything else is compared against. Perturbing the
reference to observe it buys a worse measurement than the one already sitting
on disk.

So the stream is the instrument, and it costs the arm nothing. Hooks remain the
right answer for the two behaviours the stream genuinely cannot show —
compaction and permission prompts — and belong behind an explicit opt-in that
is recorded in provenance, because an instrumented arm is a different apparatus
from an uninstrumented one and the two must never average together.

# What the stream says

``system`` / ``subtype: init``
    The harness describing itself at boot: product version, model, the tool
    roster it will choose from, permission mode, credential source, MCP
    servers, skills, subagents. This is the half of a head-to-head that is
    normally argued about from documentation rather than from evidence.
``assistant``
    One model turn: ``message.usage`` (tokens and cache, both directions) and
    ``message.content`` split into ``thinking`` / ``text`` / ``tool_use``.
``user``
    Tool outcomes, as ``tool_result`` blocks plus a ``tool_use_result``
    payload. Joined to the ``tool_use`` that requested them by id, which is
    what turns two timestamps into a tool's wall clock.
``system`` / ``subtype: thinking_tokens``
    A cumulative reasoning-token estimate, emitted continuously — 38,389 lines
    in one 7.8 MB trial, roughly 99% of the file by count. Only the last one
    matters, so the drain recognises them without parsing them.
``result``
    The final word: cost, turn count, stop reason, permission denials, and
    whether the run ended in an API error.

# Cursors, not re-reads

The file is append-only and reaches tens of megabytes, so a live dashboard
cannot re-parse it per poll. :class:`StreamReader` keeps a byte offset and a
running :class:`HarnessTotals` per path and parses only what arrived since last
time. A torn final line — guaranteed, since the file is being written while it
is read — is left unconsumed rather than skipped, so the event it belongs to is
counted exactly once when the rest of it lands.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

__all__ = [
    "STREAM_NAME",
    "HarnessProfile",
    "HarnessTotals",
    "StreamReader",
    "reduce_stream",
]

#: Where Harbor tees the Claude Code CLI's stream-json stdout, relative to a
#: trial directory. Named here, beside the only reader of it, for the same
#: reason :func:`arenabench.telemetry.seat_manifest_path` is.
STREAM_NAME = "agent/claude-code.txt"

#: Substring that identifies the continuous reasoning-token estimate without
#: parsing the line. These are ~99% of the stream by count and each one
#: supersedes the last, so the drain keeps only the most recent and parses it
#: alone. Matched on the raw text because `json.loads` on 38k lines per poll is
#: the entire cost of reading the file.
_THINKING_MARK = '"thinking_tokens"'

#: Content-block types that mean the model asked for a tool.
_TOOL_USE = "tool_use"


@dataclass
class HarnessProfile:
    """What an agent's harness disclosed about itself at boot.

    Every field is what the agent *said*, never what ArenaBench configured —
    which is the point. A seat's declared posture and the harness's own account
    of its wiring are two statements that can disagree, and only one of them
    was on the wire. ``permission_mode`` and ``api_key_source`` have each been
    the explanation for a whole arm's result: a Claude Code seat booting with
    ``apiKeySource: none`` fails every trial with an authentication error that
    scores as a loss (:func:`arenabench.agents.dead_seat_reason`), and this is
    where that would have been visible on trial one instead of at the post-mortem.
    """

    version: str = ""
    model: str = ""
    permission_mode: str = ""
    api_key_source: str = ""
    output_style: str = ""
    cwd: str = ""
    #: The tool roster the harness offered the model. A head-to-head between
    #: two agents with different tool surfaces is a different comparison from
    #: one between two agents with the same surface, and until this was read
    #: nothing recorded which of the two a match had been.
    tools: tuple[str, ...] = ()
    mcp_servers: tuple[str, ...] = ()
    skills: tuple[str, ...] = ()
    subagents: tuple[str, ...] = ()
    slash_commands: int = 0
    session_id: str = ""

    def to_json(self) -> dict[str, Any]:
        return {
            "version": self.version,
            "model": self.model,
            "permission_mode": self.permission_mode,
            "api_key_source": self.api_key_source,
            "output_style": self.output_style,
            "cwd": self.cwd,
            "tools": list(self.tools),
            "mcp_servers": list(self.mcp_servers),
            "skills": list(self.skills),
            "subagents": list(self.subagents),
            "slash_commands": self.slash_commands,
            "session_id": self.session_id,
        }


@dataclass
class HarnessTotals:
    """One trial's stream, folded to the dimensions a scoreboard compares.

    Deliberately the same vocabulary as
    :class:`arenabench.telemetry.TrialMetrics` uses for a Stella arm, so the
    two arms are described in one language and a reader is never quietly
    comparing "steps" against something else. Where the two agents genuinely
    differ the field says so rather than being forced into a shared name:
    ``turns`` is Claude Code's own count and is not Stella's ``steps``.
    """

    steps: int = 0
    tools: int = 0
    tokens_in: int = 0
    tokens_out: int = 0
    cache_read: int = 0
    cache_write: int = 0
    #: What the harness said it spent, from the final ``result`` event. Kept
    #: for the record and never compared across seats — see
    #: :mod:`arenabench.pricing` on why two vendors' price tables subtracted
    #: from each other is not a cost difference.
    total_cost: float = 0.0
    #: The harness's own cumulative reasoning-token estimate. Not summed into
    #: ``tokens_out``: it is an *estimate* the CLI publishes for its own UI,
    #: and adding an estimate to a measurement makes the measurement an
    #: estimate too.
    thinking_tokens: int = 0
    #: Model ids seen on the wire, first-seen order.
    models: tuple[str, ...] = ()
    #: How many times each tool was called. The behavioural half of the
    #: comparison: two agents that solve the same task with 35 `Bash` calls and
    #: with 7 `Read` plus one `Edit` did not do the same thing, and a solve
    #: rate cannot tell them apart.
    tool_calls: dict[str, int] = field(default_factory=dict)
    #: Summed wall clock per tool, milliseconds, from the gap between the
    #: ``tool_use`` block and the ``tool_result`` that answered it. Absent for
    #: a call whose result never arrived — a trial killed mid-tool.
    tool_ms: dict[str, int] = field(default_factory=dict)
    #: Tool calls that came back flagged as errors. An agent that spends half
    #: its turns recovering from its own failed commands is a different agent
    #: from one that does not, at identical solve rates.
    tool_errors: int = 0
    #: Turns the harness itself counted, from the ``result`` event.
    turns: int = 0
    #: ``True`` once the ``result`` event has landed — the agent's own
    #: statement that it finished, as distinct from Harbor tearing the trial
    #: down around it.
    complete: bool = False
    #: The harness's final self-report. ``api_error`` non-empty is the shape
    #: that scored three trials as Claude Code losses when the truth was a 429
    #: (#1480) — recorded here so it is visible during the match rather than
    #: reconstructed from it.
    stop_reason: str = ""
    terminal_reason: str = ""
    api_error: str = ""
    is_error: bool = False
    permission_denials: int = 0
    #: Milliseconds the harness reports it spent inside provider calls, versus
    #: the trial's whole wall clock. The difference is the harness's own
    #: overhead, which is exactly what "compare the harnesses" means.
    api_ms: int = 0
    duration_ms: int = 0

    @property
    def tool_wall_ms(self) -> int:
        """Total measured time inside tools.

        Deliberately **not** subtracted from :attr:`duration_ms` to derive a
        "harness overhead". Harbor runs Claude Code with
        ``FORCE_AUTO_BACKGROUND_TASKS=1``, so tools and provider calls overlap:
        on a measured trial the three figures were 380.7 s in tools, 180.2 s in
        the API and 447.3 s of wall clock, and the subtraction is negative.
        Concurrency is a real property of the harness worth comparing; a
        derived number that goes negative is not.
        """
        return sum(self.tool_ms.values())

    @property
    def prompt_tokens(self) -> int:
        """Input tokens in ATIF's sense: fresh input plus both cache legs.

        The wire keeps the three apart and so does this class, because they are
        three different prices. ATIF does not — Harbor's trajectory writer sums
        them into ``total_prompt_tokens`` — and every Claude Code cost and
        cache figure ever recorded by ArenaBench was read through that
        convention. So the fold lives here, named, and the scoreboard keeps
        using the one convention it has always used rather than silently
        changing what "tokens in" means for one arm mid-archive.
        """
        return self.tokens_in + self.cache_read + self.cache_write

    def to_json(self) -> dict[str, Any]:
        return {
            "steps": self.steps,
            "tools": self.tools,
            "tokens_in": self.tokens_in,
            "prompt_tokens": self.prompt_tokens,
            "tokens_out": self.tokens_out,
            "cache_read": self.cache_read,
            "cache_write": self.cache_write,
            "total_cost": round(self.total_cost, 6),
            "thinking_tokens": self.thinking_tokens,
            "models": list(self.models),
            "tool_calls": dict(sorted(self.tool_calls.items())),
            "tool_ms": dict(sorted(self.tool_ms.items())),
            "tool_wall_ms": self.tool_wall_ms,
            "tool_errors": self.tool_errors,
            "turns": self.turns,
            "complete": self.complete,
            "stop_reason": self.stop_reason,
            "terminal_reason": self.terminal_reason,
            "api_error": self.api_error,
            "is_error": self.is_error,
            "permission_denials": self.permission_denials,
            "api_ms": self.api_ms,
            "duration_ms": self.duration_ms,
        }


def _iso_ms(stamp: Any) -> float | None:
    """An ISO-8601 timestamp as epoch milliseconds, or ``None``.

    Tolerant of the trailing ``Z`` the CLI writes, which
    :meth:`datetime.fromisoformat` refused before Python 3.11 and which is not
    worth a dependency to handle.
    """
    if not isinstance(stamp, str) or not stamp:
        return None
    from datetime import datetime

    try:
        parsed = datetime.fromisoformat(stamp.replace("Z", "+00:00"))
    except ValueError:
        return None
    return parsed.timestamp() * 1000.0


#: The four usage counters, in the order they are credited.
_USAGE_FIELDS = (
    ("input_tokens", "tokens_in"),
    ("output_tokens", "tokens_out"),
    ("cache_read_input_tokens", "cache_read"),
    ("cache_creation_input_tokens", "cache_write"),
)


class _StreamState:
    """Everything a drain must remember between two reads of one file."""

    __slots__ = (
        "counted_calls",
        "credited",
        "offset",
        "open_calls",
        "pending",
        "profile",
        "seen_models",
        "totals",
    )

    def __init__(self) -> None:
        self.offset = 0
        #: The trailing bytes of the last read that did not end in a newline.
        #: Held rather than dropped: the file is being appended to, so a torn
        #: line is the normal case and discarding it would lose whole events.
        self.pending = b""
        self.profile = HarnessProfile()
        self.totals = HarnessTotals()
        #: ``tool_use_id`` → ``(tool name, requested-at ms)`` for calls whose
        #: result has not arrived yet. A call still open when the trial ends
        #: contributes its count but no duration — the honest reading of "we
        #: never saw it finish".
        self.open_calls: dict[str, tuple[str, float | None]] = {}
        self.seen_models: list[str] = []
        #: Usage already credited per assistant message id. **The stream emits
        #: one event per content block of the same message**, each carrying
        #: that message's full usage — 48 events for 20 messages in a measured
        #: trial — so summing every event over-counts tokens by 1.76x and
        #: over-counts steps by 2.4x. Both are the shape of bug this repository
        #: treats as worse than a crash: a plausible number nobody questions.
        #:
        #: Credited as a *delta* against what this id was last charged rather
        #: than by keeping the last value, because the reader is incremental:
        #: "last value wins" needs the whole file, and this needs only the
        #: bytes that just arrived.
        self.credited: dict[str, tuple[int, int, int, int]] = {}
        #: ``tool_use_id``s already counted, for the same reason.
        self.counted_calls: set[str] = set()


def _profile_from_init(event: dict[str, Any], profile: HarnessProfile) -> None:
    """Fill a profile from a ``system``/``init`` event, in place."""

    def names(value: Any) -> tuple[str, ...]:
        """A roster field as plain names.

        The CLI spells these three ways across versions — a list of strings, a
        list of objects with a ``name``, or a mapping keyed by name — so all
        three are accepted rather than pinning the reader to whichever shape
        the current release happens to use.
        """
        if isinstance(value, dict):
            return tuple(str(key) for key in value)
        if isinstance(value, list):
            out: list[str] = []
            for item in value:
                if isinstance(item, str):
                    out.append(item)
                elif isinstance(item, dict):
                    name = item.get("name") or item.get("id")
                    if name:
                        out.append(str(name))
            return tuple(out)
        return ()

    profile.version = str(event.get("claude_code_version") or "") or profile.version
    profile.model = str(event.get("model") or "") or profile.model
    profile.permission_mode = str(event.get("permissionMode") or "")
    profile.api_key_source = str(event.get("apiKeySource") or "")
    profile.output_style = str(event.get("output_style") or "")
    profile.cwd = str(event.get("cwd") or "")
    profile.session_id = str(event.get("session_id") or "")
    profile.tools = names(event.get("tools"))
    profile.mcp_servers = names(event.get("mcp_servers"))
    profile.skills = names(event.get("skills"))
    profile.subagents = names(event.get("agents"))
    commands = event.get("slash_commands")
    profile.slash_commands = len(commands) if isinstance(commands, (list, dict)) else 0


def _apply(event: dict[str, Any], state: _StreamState) -> None:
    """Fold one parsed stream event into the running state."""
    totals = state.totals
    kind = event.get("type")

    if kind == "system":
        if event.get("subtype") == "init":
            _profile_from_init(event, state.profile)
        return

    if kind == "result":
        totals.complete = True
        cost = event.get("total_cost_usd")
        if isinstance(cost, (int, float)):
            totals.total_cost = float(cost)
        for name, attr in (
            ("num_turns", "turns"),
            ("duration_api_ms", "api_ms"),
            ("duration_ms", "duration_ms"),
        ):
            value = event.get(name)
            if isinstance(value, (int, float)):
                setattr(totals, attr, int(value))
        totals.stop_reason = str(event.get("stop_reason") or "")
        totals.terminal_reason = str(event.get("terminal_reason") or "")
        totals.api_error = str(event.get("api_error_status") or "")
        totals.is_error = bool(event.get("is_error"))
        denials = event.get("permission_denials")
        if isinstance(denials, list):
            totals.permission_denials = len(denials)
        elif isinstance(denials, (int, float)):
            totals.permission_denials = int(denials)
        return

    message = event.get("message")
    if not isinstance(message, dict):
        return
    stamp = _iso_ms(event.get("timestamp"))
    content = message.get("content")
    blocks = content if isinstance(content, list) else []

    if kind == "assistant":
        message_id = str(message.get("id") or "")
        # One *message*, not one event. The stream re-emits a message once per
        # content block, so counting events reports 87 steps for a trial that
        # took 29 turns. A message with no id cannot be deduplicated and is
        # counted as its own step, which is the conservative reading.
        if not message_id or message_id not in state.credited:
            totals.steps += 1
        model = str(message.get("model") or "")
        if model and model not in state.seen_models:
            state.seen_models.append(model)
            totals.models = tuple(state.seen_models)

        usage = message.get("usage")
        if isinstance(usage, dict):
            # The four counters stay apart, because they are four prices. ATIF
            # folds three of them into one `total_prompt_tokens`, which is a
            # lossy summary the wire does not oblige us to adopt; the fold is
            # available as `HarnessTotals.prompt_tokens` for the callers that
            # need ATIF's convention, and nothing here throws the split away.
            current = tuple(
                int(usage.get(wire) or 0) for wire, _ in _USAGE_FIELDS
            )
            before = state.credited.get(message_id, (0, 0, 0, 0))
            for (_, attr), now, was in zip(_USAGE_FIELDS, current, before, strict=True):
                # `max(0, …)` because a revision must never *subtract*. A
                # provider that reports a smaller figure on a later chunk is
                # correcting itself, and un-crediting tokens the trial really
                # spent would understate exactly the arm we are measuring.
                setattr(totals, attr, getattr(totals, attr) + max(0, now - was))
            if message_id:
                state.credited[message_id] = current  # type: ignore[assignment]

        for block in blocks:
            if not isinstance(block, dict) or block.get("type") != _TOOL_USE:
                continue
            call_id = str(block.get("id") or "")
            if call_id and call_id in state.counted_calls:
                continue
            name = str(block.get("name") or "")
            totals.tools += 1
            totals.tool_calls[name] = totals.tool_calls.get(name, 0) + 1
            if call_id:
                state.counted_calls.add(call_id)
                state.open_calls[call_id] = (name, stamp)
        return

    if kind == "user":
        for block in blocks:
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                continue
            call_id = str(block.get("tool_use_id") or "")
            opened = state.open_calls.pop(call_id, None)
            if block.get("is_error"):
                totals.tool_errors += 1
            if opened is None:
                continue
            name, started = opened
            if started is not None and stamp is not None:
                elapsed = max(0, int(stamp - started))
                totals.tool_ms[name] = totals.tool_ms.get(name, 0) + elapsed


class StreamReader:
    """Incremental reader for one or many Claude Code stream files.

    Holds a byte offset and running totals per path, so a live dashboard pays
    only for what the agent appended since the last poll. That matters at the
    observed sizes: one archived trial's stream is 15 MB and 99% of its lines
    are superseded reasoning-token estimates, so a re-parse per poll would cost
    more than everything else the arena does put together.

    Shrinking or replaced files reset rather than corrupt: a path whose size
    fell below the cursor is read again from zero, because the only ways that
    happens are a rerun into the same directory or a truncated archive, and
    both want a fresh reading rather than a continuation of a file that no
    longer exists.
    """

    def __init__(self) -> None:
        self._states: dict[Path, _StreamState] = {}

    def read(self, path: Path) -> tuple[HarnessProfile, HarnessTotals] | None:
        """Drain any new bytes and return the running profile and totals.

        ``None`` when the file does not exist — which is every non-Claude-Code
        arm, and a Claude Code trial that has not started. Distinct from a file
        that exists and is empty, which reads as a real zero.
        """
        try:
            size = path.stat().st_size
        except OSError:
            return None
        state = self._states.get(path)
        if state is None or size < state.offset:
            state = _StreamState()
            self._states[path] = state
        if size > state.offset:
            self._drain(path, state, size)
        return state.profile, state.totals

    def forget(self, path: Path) -> None:
        """Drop a path's cursor, so the next read starts over."""
        self._states.pop(path, None)

    @staticmethod
    def _drain(path: Path, state: _StreamState, size: int) -> None:
        try:
            with path.open("rb") as handle:
                handle.seek(state.offset)
                chunk = handle.read(size - state.offset)
        except OSError:
            return
        state.offset += len(chunk)
        buffer = state.pending + chunk
        lines = buffer.split(b"\n")
        # The last element is whatever followed the final newline: either an
        # empty string (the chunk ended cleanly) or a partial line still being
        # written. Either way it is not ready, and it is carried rather than
        # parsed — the cursor has already moved past those bytes, so dropping
        # them would lose the event permanently.
        state.pending = lines.pop()

        thinking: bytes | None = None
        for raw in lines:
            if not raw or not raw.lstrip().startswith(b"{"):
                continue
            if _THINKING_MARK.encode() in raw:
                # Cumulative and self-superseding: only the last one in this
                # drain can matter, so 38,000 JSON parses become one.
                thinking = raw
                continue
            try:
                event = json.loads(raw)
            except ValueError:
                continue
            if isinstance(event, dict):
                _apply(event, state)
        if thinking is not None:
            try:
                event = json.loads(thinking)
            except ValueError:
                return
            estimated = event.get("estimated_tokens")
            if isinstance(estimated, (int, float)):
                state.totals.thinking_tokens = int(estimated)


def reduce_stream(path: Path) -> tuple[HarnessProfile, HarnessTotals] | None:
    """One-shot read of a complete stream, for an archived trial.

    A thin wrapper over :class:`StreamReader` for callers with no reason to
    keep a cursor — ``arenabench harness`` reporting on a finished match, or a
    test. A live caller wants the reader itself.
    """
    return StreamReader().read(path)
