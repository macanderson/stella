# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Reading each arm's own harness while it runs.

Every contestant publishes an append-only account of what it is doing, and
each one spells it differently. Stella writes ``agent/stella-events.jsonl``
per event; Claude Code is teed to ``agent/claude-code.txt`` by Harbor, which
runs it with ``--verbose --output-format=stream-json``. Their opponents'
common denominator is ATIF — ``agent/trajectory.json`` — which is written
**once, at the end**, so before this module a Claude Code arm read zero steps,
zero tools, zero tokens and zero cost for the entire length of a match and
only became real at teardown. :func:`arenabench.telemetry._liveness_age` made
it look alive throughout, because file mtimes moved; nothing measured it.

The fix needed no new instrumentation: both files were already on disk, and
only one field of one of them was ever read (Harbor's parser digs
``total_cost_usd`` out after the run).

# One vocabulary, one fold per arm

:class:`HarnessTotals` is the shared vocabulary, and it is deliberately the
same one :class:`arenabench.telemetry.TrialMetrics` uses, so a reader is never
quietly comparing "steps" against something else. What differs per arm is only
*how the bytes spell it*, so that — and nothing else — is what a
:class:`HarnessDialect` carries: the artifact to read, the fold that reduces
it, the prose saying what this arm's ``steps`` actually counts, and the fields
its stream never publishes at all. Adding a third arm is a dialect row plus a
fold; no caller of this module learns its name (#2519).

The declared gaps are the point. Stella publishes no boot disclosure and no
reasoning estimate, and a zero in those columns would be a measurement that
was never taken — the shape this repository treats as worse than a crash. A
dialect names them, and every surface prints them as unpublished.

Where the two agents genuinely differ, the field says so rather than being
forced into a shared name: ``turns`` is Claude Code's own count and is not
Stella's ``steps``, and Stella's ``steps`` counts one model call per pipeline
role (worker, verifier, triage) rather than one assistant turn in a single
loop. :attr:`HarnessDialect.steps_are` is that sentence, printed beside the
number by every surface that shows it.

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

# What the Claude Code stream says

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

# What the Stella stream says

``tool_start`` / ``tool_result``
    One tool call and the answer to it, correlated by ``call.call_id``. The
    result carries the engine's own measured ``duration_ms``; a call it never
    arrives for contributes its count and no duration.
``step_usage``
    One model call: tokens in all four directions, cost, model and role.
``complete``
    The agent's own statement that it finished.

# Zeroed usage is not zero usage (#2520)

A Claude Code seat routed through ``ANTHROPIC_BASE_URL`` to a non-Anthropic
gateway streams ``{"input_tokens": 0, "output_tokens": 0}`` on **every**
assistant message, while the same trial on the Anthropic route carries real
figures on every one. Its tokens are not zero — they are unpublished on that
file, and they are sitting in the CLI's own session transcript under
``agent/sessions/projects/…`` the whole time (Harbor points
``CLAUDE_CONFIG_DIR`` at ``/logs/agent/sessions``).

So a dialect may name a **sidecar**: a second append-only artifact consulted
only for what the primary stream failed to carry. It is a fallback, never an
addend — the two sources describe the same messages, so adding them would
double every token. :meth:`_Fold._resolve_usage` is the single place that
decides, and it records which source won as
:attr:`HarnessTotals.usage_source`. Its third value is the one that matters:
``unobserved`` means nothing has published usage yet, which is a different
fact from "this trial spent nothing" and must never be printed as ``0``.

# Cursors, not re-reads

The files are append-only and reach tens of megabytes, so a live dashboard
cannot re-parse them per poll. :class:`StreamReader` keeps a byte offset per
file and a running fold per arm, and parses only what arrived since last time.
A torn final line — guaranteed, since the file is being written while it is
read — is left unconsumed rather than skipped, so the event it belongs to is
counted exactly once when the rest of it lands.
"""

from __future__ import annotations

import json
from abc import ABC, abstractmethod
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, ClassVar

from .toolout import decode_tool_output

__all__ = [
    "CLAUDE_CODE",
    "DIALECTS",
    "STELLA",
    "STELLA_EVENTS_NAME",
    "STREAM_NAME",
    "TRANSCRIPT_GLOB",
    "USAGE_STREAM",
    "USAGE_TRANSCRIPT",
    "USAGE_UNOBSERVED",
    "HarnessDialect",
    "HarnessProfile",
    "HarnessReading",
    "HarnessTotals",
    "SeatBehaviour",
    "StreamReader",
    "reduce_stream",
    "summarize",
]

#: Where Harbor tees the Claude Code CLI's stream-json stdout, relative to a
#: trial directory. Named here, beside the only reader of it, for the same
#: reason :func:`arenabench.telemetry.seat_manifest_path` is.
STREAM_NAME = "agent/claude-code.txt"

#: Stella's own event stream, relative to a trial directory. Defined here
#: rather than in :mod:`arenabench.telemetry` — which re-exports it as
#: ``EVENTS_NAME`` — so the scoreboard's reducer and this one can never
#: disagree about which file they are describing.
STELLA_EVENTS_NAME = "agent/stella-events.jsonl"

#: Claude Code's own session transcript inside a trial. Harbor points
#: ``CLAUDE_CONFIG_DIR`` at ``/logs/agent/sessions``, and the CLI writes
#: ``projects/<mangled cwd>/<session id>.jsonl`` beneath it, appending as it
#: works. Scoped to ``projects/`` rather than the whole ``sessions/`` tree so
#: the arm's other config artifacts can never be mistaken for a transcript;
#: a layout this misses reads as :data:`USAGE_UNOBSERVED`, never as zero.
TRANSCRIPT_GLOB = "agent/sessions/projects/**/*.jsonl"

#: Substring that identifies the continuous reasoning-token estimate without
#: parsing the line. These are ~99% of the stream by count and each one
#: supersedes the last, so the drain keeps only the most recent and parses it
#: alone. Matched on the raw text because `json.loads` on 38k lines per poll is
#: the entire cost of reading the file.
_THINKING_MARK = '"thinking_tokens"'

#: The same idea for Stella's stream, which is overwhelmingly streamed text
#: and reasoning fragments. A miss costs one wasted `json.loads` and nothing
#: else — the fold ignores both types — so matching raw bytes here can never
#: change an answer, only a runtime.
_STELLA_DELTA_MARKS = (b'"type":"reasoning"', b'"type":"text_delta"')

#: Content-block types that mean the model asked for a tool.
_TOOL_USE = "tool_use"

#: How a trial's four token counters were obtained. The distinction the
#: gateway-route defect (#2520) turns on: a column nobody has published yet
#: must read as unknown, never as a measured zero — the same discipline the
#: engine's flip oracle draws between `unobserved` and `not_achieved`.
USAGE_UNOBSERVED = "unobserved"
#: The arm's primary stream carried nonzero usage.
USAGE_STREAM = "stream"
#: The primary stream published only zeros and a sidecar carried the figures.
USAGE_TRANSCRIPT = "transcript"

#: The four :class:`HarnessTotals` counters a fold credits, in the order every
#: dialect's ``usage_fields`` spells them.
_USAGE_SLOTS = ("tokens_in", "tokens_out", "cache_read", "cache_write")

_ZERO_USAGE = (0, 0, 0, 0)


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

    Empty for an arm whose stream carries no boot event at all — Stella's
    does not, and inferring one of these from later traffic would report the
    first role's model as "the model the harness booted with". A dialect says
    so with :attr:`HarnessDialect.boot_disclosure`; the SUT commit in
    ``provenance.sut_seats`` is what identifies such an arm.
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
    #: nothing recorded which of the two a match had been. Deliberately *not*
    #: back-filled from the tools an arm was seen to call: "offered" and "used"
    #: are different facts and the difference is the measurement.
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

    A field an arm's stream never carries stays at its default here and is
    named in :attr:`HarnessDialect.unpublished`, so a surface can print it as
    unpublished instead of as a measured zero.
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
    #: Summed wall clock per tool, milliseconds. Absent for a call whose result
    #: never arrived — a trial killed mid-tool.
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
    #: Which source the four token counters above came from —
    #: :data:`USAGE_STREAM`, :data:`USAGE_TRANSCRIPT`, or
    #: :data:`USAGE_UNOBSERVED` when no source has published any yet. Carried
    #: beside the numbers because on a gateway route the primary stream's
    #: zeros are a publishing gap rather than a measurement (#2520), and a
    #: reader has to be able to tell the two apart.
    usage_source: str = USAGE_UNOBSERVED

    @property
    def tool_wall_ms(self) -> int:
        """Total measured time inside tools.

        Deliberately **not** subtracted from :attr:`duration_ms` to derive a
        "harness overhead". Harbor runs Claude Code with
        ``FORCE_AUTO_BACKGROUND_TASKS=1``, so tools and provider calls overlap:
        on a measured trial the three figures were 380.7 s in tools, 180.2 s in
        the API and 447.3 s of wall clock, and the subtraction is negative.
        Stella overlaps them too, by speculating read-only calls while the
        model is still streaming. Concurrency is a real property of a harness
        worth comparing; a derived number that goes negative is not.
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
            "usage_source": self.usage_source,
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


def _as_int(value: Any) -> int:
    """A wire number as an int, or ``0`` — never an exception.

    Every number here is runtime data from a file another process is writing,
    so a malformed one must degrade rather than take the drain down with it.
    ``bool`` is excluded explicitly: it is an ``int`` in Python, and a stray
    ``true`` crediting one token is exactly the plausible wrong number this
    module exists to avoid.
    """
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return 0
    return int(value)


def _usage_values(usage: dict[str, Any], fields: tuple[str, ...]) -> tuple[int, ...]:
    """One usage object as the four counters, in :data:`_USAGE_SLOTS` order."""
    return tuple(_as_int(usage.get(name)) for name in fields)


class _Fold(ABC):
    """One arm's running reduction of one trial's stream.

    Owns *what the bytes mean*; :class:`StreamReader` owns *which bytes are
    new*. That split is what lets a second arm arrive as a fold plus a
    :class:`HarnessDialect` row instead of another branch inside the reader,
    and it keeps the cursor discipline — one offset per file, a torn tail held
    rather than dropped — identical for every arm, because there is one copy
    of it.
    """

    #: The wire spelling of the four usage counters, in :data:`_USAGE_SLOTS`
    #: order. The two arms disagree on all four names.
    usage_fields: ClassVar[tuple[str, ...]] = ()

    def __init__(self) -> None:
        self.profile = HarnessProfile()
        self.totals = HarnessTotals()
        #: Usage credited from the primary stream and from a sidecar, kept
        #: apart so neither can ever be added to the other: they describe the
        #: same messages, so a sum would double every token.
        #: :meth:`_resolve_usage` is the one place that chooses between them.
        self._primary_usage = [0, 0, 0, 0]
        self._sidecar_usage = [0, 0, 0, 0]

    @abstractmethod
    def consume(self, lines: list[bytes]) -> None:
        """Fold the complete lines that just arrived on the primary stream."""

    def consume_sidecar(self, lines: list[bytes]) -> None:
        """Fold the complete lines that just arrived on a fallback source.

        Unreachable for an arm whose stream publishes everything it knows: the
        reader opens a fallback only for a dialect that names a
        ``sidecar_glob`` *and* a fold that says it still needs one. A fold that
        gets here has been handed bytes it never declared it could read, which
        is a dialect table out of step with its folds — so it says so rather
        than discarding them silently.
        """
        raise NotImplementedError(f"{type(self).__name__} declared no sidecar source")

    def needs_sidecar(self) -> bool:
        """Whether the fallback source is worth opening at all right now.

        ``False`` once the primary stream has published real usage, so an arm
        on a route that works pays nothing for the workaround an arm on a
        broken route needs.
        """
        return False

    def _resolve_usage(self) -> None:
        """Publish the four counters from whichever source actually has them.

        The whole of the primary-versus-sidecar decision, in one place, run
        after every drain. "All four zero" is never published as a
        measurement: no real model call reports zero of everything, so the
        honest reading of it is :data:`USAGE_UNOBSERVED`.
        """
        if any(self._primary_usage):
            chosen, source = self._primary_usage, USAGE_STREAM
        elif any(self._sidecar_usage):
            chosen, source = self._sidecar_usage, USAGE_TRANSCRIPT
        else:
            chosen, source = list(_ZERO_USAGE), USAGE_UNOBSERVED
        for slot, value in zip(_USAGE_SLOTS, chosen, strict=True):
            setattr(self.totals, slot, value)
        self.totals.usage_source = source


# --------------------------------------------------------------------------
# Claude Code
# --------------------------------------------------------------------------


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


class _ClaudeCodeFold(_Fold):
    """The stream-json stdout tee, plus the session transcript behind it."""

    usage_fields: ClassVar[tuple[str, ...]] = (
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    )

    def __init__(self) -> None:
        super().__init__()
        #: ``tool_use_id`` → ``(tool name, requested-at ms)`` for calls whose
        #: result has not arrived yet. A call still open when the trial ends
        #: contributes its count but no duration — the honest reading of "we
        #: never saw it finish".
        self._open_calls: dict[str, tuple[str, float | None]] = {}
        self._seen_models: list[str] = []
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
        self._credited: dict[str, tuple[int, ...]] = {}
        #: The same ledger for the session transcript, which repeats a message
        #: per content block exactly as the stdout stream does. Separate,
        #: because the two files carry the same message ids and one ledger
        #: would let each source suppress the other's crediting.
        self._transcript_credited: dict[str, tuple[int, ...]] = {}
        #: ``tool_use_id``s already counted, for the same reason.
        self._counted_calls: set[str] = set()

    def needs_sidecar(self) -> bool:
        return self.totals.usage_source != USAGE_STREAM

    def consume(self, lines: list[bytes]) -> None:
        thinking: bytes | None = None
        for raw in lines:
            if not raw or not raw.lstrip().startswith(b"{"):
                continue
            if _THINKING_MARK.encode() in raw:
                # Cumulative and self-superseding: only the last one in this
                # drain can matter, so 38,000 JSON parses become one.
                thinking = raw
                continue
            event = _parse(raw)
            if event is not None:
                self._apply(event)
        if thinking is not None:
            event = _parse(thinking)
            if event is not None:
                estimated = event.get("estimated_tokens")
                if isinstance(estimated, (int, float)):
                    self.totals.thinking_tokens = int(estimated)
        self._resolve_usage()

    def consume_sidecar(self, lines: list[bytes]) -> None:
        """Credit usage from the CLI's own session transcript.

        Deliberately **only** usage. Steps, tools and their wall clock are
        already correct from the stdout stream on every route — it is the
        counters that a gateway zeroes — and folding the transcript's copy of
        the same messages into them would double both.
        """
        for raw in lines:
            if not raw or not raw.lstrip().startswith(b"{"):
                continue
            event = _parse(raw)
            if event is None or event.get("type") != "assistant":
                continue
            message = event.get("message")
            if not isinstance(message, dict):
                continue
            usage = message.get("usage")
            if isinstance(usage, dict):
                self._credit(
                    usage,
                    str(message.get("id") or ""),
                    self._transcript_credited,
                    self._sidecar_usage,
                )
        self._resolve_usage()

    def _credit(
        self,
        usage: dict[str, Any],
        message_id: str,
        ledger: dict[str, tuple[int, ...]],
        into: list[int],
    ) -> None:
        """Credit one message's usage as a delta against what it was charged.

        The four counters stay apart, because they are four prices. ATIF folds
        three of them into one ``total_prompt_tokens``, which is a lossy
        summary the wire does not oblige us to adopt; the fold is available as
        :attr:`HarnessTotals.prompt_tokens` for the callers that need ATIF's
        convention, and nothing here throws the split away.
        """
        current = _usage_values(usage, self.usage_fields)
        before = ledger.get(message_id, _ZERO_USAGE)
        for index, (now, was) in enumerate(zip(current, before, strict=True)):
            # `max(0, …)` because a revision must never *subtract*. A provider
            # that reports a smaller figure on a later chunk is correcting
            # itself, and un-crediting tokens the trial really spent would
            # understate exactly the arm we are measuring.
            into[index] += max(0, now - was)
        if message_id:
            ledger[message_id] = current

    def _apply(self, event: dict[str, Any]) -> None:
        """Fold one parsed stream event into the running totals."""
        totals = self.totals
        kind = event.get("type")

        if kind == "system":
            if event.get("subtype") == "init":
                _profile_from_init(event, self.profile)
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
            # One *message*, not one event. The stream re-emits a message once
            # per content block, so counting events reports 87 steps for a
            # trial that took 29 turns. A message with no id cannot be
            # deduplicated and is counted as its own step, which is the
            # conservative reading.
            if not message_id or message_id not in self._credited:
                totals.steps += 1
            model = str(message.get("model") or "")
            if model and model not in self._seen_models:
                self._seen_models.append(model)
                totals.models = tuple(self._seen_models)

            usage = message.get("usage")
            if isinstance(usage, dict):
                self._credit(usage, message_id, self._credited, self._primary_usage)

            for block in blocks:
                if not isinstance(block, dict) or block.get("type") != _TOOL_USE:
                    continue
                call_id = str(block.get("id") or "")
                if call_id and call_id in self._counted_calls:
                    continue
                name = str(block.get("name") or "")
                totals.tools += 1
                totals.tool_calls[name] = totals.tool_calls.get(name, 0) + 1
                if call_id:
                    self._counted_calls.add(call_id)
                    self._open_calls[call_id] = (name, stamp)
            return

        if kind == "user":
            for block in blocks:
                if not isinstance(block, dict) or block.get("type") != "tool_result":
                    continue
                call_id = str(block.get("tool_use_id") or "")
                opened = self._open_calls.pop(call_id, None)
                if block.get("is_error"):
                    totals.tool_errors += 1
                if opened is None:
                    continue
                name, started = opened
                if started is not None and stamp is not None:
                    # Two timestamps, because this CLI publishes no duration.
                    # The gap is the harness's own view of the call's wall
                    # clock, which is the figure being compared.
                    elapsed = max(0, int(stamp - started))
                    totals.tool_ms[name] = totals.tool_ms.get(name, 0) + elapsed


# --------------------------------------------------------------------------
# Stella
# --------------------------------------------------------------------------


class _StellaFold(_Fold):
    """Stella's own event stream, folded into the shared vocabulary.

    Every number here is already on the wire (#2519); nothing is inferred.
    What Stella does *not* publish — a boot disclosure, a reasoning-token
    estimate, a harness-side wall clock — is declared on :data:`STELLA`
    instead of being defaulted to zero.
    """

    usage_fields: ClassVar[tuple[str, ...]] = (
        "input_tokens",
        "output_tokens",
        "cached_input_tokens",
        "cache_write_tokens",
    )

    def __init__(self) -> None:
        super().__init__()
        #: ``call_id`` → ``(tool name, started-at ms)``. The result event
        #: carries only the id, so the name a duration is filed under comes
        #: from the ``tool_start`` that opened it — the same join the deck and
        #: the transcript reader do.
        self._open_calls: dict[str, tuple[str, float | None]] = {}
        self._seen_models: list[str] = []

    def consume(self, lines: list[bytes]) -> None:
        for raw in lines:
            if not raw or not raw.lstrip().startswith(b"{"):
                continue
            if any(mark in raw for mark in _STELLA_DELTA_MARKS):
                continue
            event = _parse(raw)
            if event is not None:
                self._apply(event)
        self._resolve_usage()

    def _apply(self, event: dict[str, Any]) -> None:
        totals = self.totals
        kind = event.get("type")
        stamp = event.get("ts")
        stamp = float(stamp) if isinstance(stamp, (int, float)) else None

        if kind == "tool_start":
            call = event.get("call")
            call = call if isinstance(call, dict) else {}
            name = str(call.get("name") or "")
            call_id = str(call.get("call_id") or call.get("id") or "")
            totals.tools += 1
            totals.tool_calls[name] = totals.tool_calls.get(name, 0) + 1
            # No dedupe by id, unlike the Claude Code fold: Stella emits one
            # `tool_start` per call, so suppressing a repeat would drop a real
            # second call rather than a re-emission of the first.
            if call_id:
                self._open_calls[call_id] = (name, stamp)
            return

        if kind == "tool_result":
            call_id = str(event.get("call_id") or "")
            opened = self._open_calls.pop(call_id, None)
            # The tag is the verdict: `ToolOutput` is externally tagged and
            # carries no `is_error` field, so the one decoder that knows that
            # (:mod:`arenabench.toolout`) reads it here too.
            if not decode_tool_output(event.get("output")).ok:
                totals.tool_errors += 1
            if opened is None:
                return
            name, started = opened
            # The engine measured this itself, so its figure wins over a gap
            # between two journal timestamps — which would also charge the
            # tool for the time its result spent being written. The join is
            # still what supplies the tool's *name*, and the timestamps are
            # the fallback for a stream recorded without a duration.
            measured = _as_int(event.get("duration_ms"))
            if not measured and started is not None and stamp is not None:
                measured = max(0, int(stamp - started))
            if measured:
                totals.tool_ms[name] = totals.tool_ms.get(name, 0) + measured
            return

        if kind == "speculation_discarded":
            # A read-only call that really ran, whose result the model never
            # saw. It already counted at `tool_start`, so this closes the join
            # without inventing a duration and without calling it an error:
            # the tool did not fail, its answer was thrown away.
            self._open_calls.pop(str(event.get("call_id") or ""), None)
            return

        if kind == "step_usage":
            totals.steps += 1
            for index, value in enumerate(_usage_values(event, self.usage_fields)):
                self._primary_usage[index] += value
            cost = event.get("cost_usd")
            if isinstance(cost, (int, float)) and not isinstance(cost, bool):
                totals.total_cost += float(cost)
            model = str(event.get("model") or "")
            if model and model not in self._seen_models:
                self._seen_models.append(model)
                totals.models = tuple(self._seen_models)
            return

        if kind == "complete":
            # `Complete` carries the turn's own `cost_usd`, and is deliberately
            # not read for it: a run can complete more than once, so the last
            # one is a floor rather than a total. The per-step sum above is the
            # only monotonic reading, and it is the one the scoreboard already
            # uses (`telemetry._reduce_events`).
            totals.complete = True


# --------------------------------------------------------------------------
# The dialect table
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class HarnessDialect:
    """How one arm publishes itself, and what its numbers do and do not mean.

    A row here plus a fold is the whole cost of a third arm. Everything a
    surface needs in order to describe an arm honestly is declared rather than
    inferred: what it writes, what its ``steps`` counts, and which columns its
    stream never carries.
    """

    #: Stable id for the arm this dialect reads. Printed, so it is the name a
    #: reader uses when two blocks disagree.
    arm: str
    #: The artifact whose existence means this arm ran in a trial directory.
    stream_name: str
    #: What one ``steps`` is, for this arm, in one sentence. Printed beside
    #: the number, because Claude Code's assistant turns and Stella's
    #: per-role model calls are different units and a table that puts them in
    #: one column is the sort of plausible number this repository treats as
    #: worse than a crash.
    steps_are: str
    #: Whether the arm's stream opens with a self-description. ``False`` means
    #: :class:`HarnessProfile` stays empty for it, on purpose.
    boot_disclosure: bool
    #: :class:`HarnessTotals` fields this arm's stream never carries. They keep
    #: their defaults, and a surface prints them as unpublished rather than as
    #: zero. Held to real field names by ``tests/test_harness.py``, so a rename
    #: cannot strand an entry here.
    unpublished: frozenset[str]
    #: Glob, relative to a trial directory, for a **fallback** source consulted
    #: only for what the primary stream failed to carry (#2520). Empty for an
    #: arm whose stream publishes everything it knows.
    sidecar_glob: str
    #: Builds this dialect's fold. Private type on purpose: the fold is an
    #: implementation of the vocabulary, not part of it.
    new_fold: Callable[[], _Fold]


CLAUDE_CODE = HarnessDialect(
    arm="claude-code",
    stream_name=STREAM_NAME,
    steps_are="one assistant message per model turn, in a single loop",
    boot_disclosure=True,
    unpublished=frozenset(),
    sidecar_glob=TRANSCRIPT_GLOB,
    new_fold=_ClaudeCodeFold,
)

STELLA = HarnessDialect(
    arm="stella",
    stream_name=STELLA_EVENTS_NAME,
    steps_are=(
        "one model call per pipeline role — worker, verifier, triage — so it is "
        "not a turn count and does not compare to one"
    ),
    boot_disclosure=False,
    unpublished=frozenset(
        {
            # No cumulative reasoning estimate on the wire: Stella streams
            # `reasoning` deltas, and counting their characters would publish
            # an estimate of an estimate.
            "thinking_tokens",
            # `turn_instance` rides on `step_manifest`, which the arena stream
            # need not carry; a turn count derived from anything else here
            # would be a guess.
            "turns",
            # The engine's own self-report at the end of a run has no analogue
            # to the CLI `result` event's verdict fields.
            "stop_reason",
            "terminal_reason",
            "api_error",
            "is_error",
            "permission_denials",
            # Harbor times the trial; Stella's stream reports per-step
            # durations, not a harness-side wall clock, and summing steps
            # would report provider time as though it were the whole run.
            "api_ms",
            "duration_ms",
        }
    ),
    sidecar_glob="",
    new_fold=_StellaFold,
)

#: Every arm this module can read, in the order a report prints them.
DIALECTS: tuple[HarnessDialect, ...] = (CLAUDE_CODE, STELLA)


@dataclass(frozen=True)
class HarnessReading:
    """One arm's stream in one trial, folded — with the dialect that read it.

    The dialect travels with the numbers because half of reading them
    honestly is knowing which vocabulary they are in.
    """

    dialect: HarnessDialect
    profile: HarnessProfile
    totals: HarnessTotals


@dataclass(frozen=True)
class SeatBehaviour:
    """One seat's trials, folded into the one block a report prints for it."""

    dialect: HarnessDialect
    #: The seat's boot disclosure. The first trial's, because a seat that
    #: re-booted with a different roster mid-match is a provenance question
    #: (``provenance.mixed_version_seats``) rather than something to average.
    profile: HarnessProfile
    trials: int
    #: :meth:`HarnessTotals.to_json`'s payload summed across the trials —
    #: counters added, per-tool maps merged, model ids unioned in first-seen
    #: order. Fields this seat's dialect lists as ``unpublished`` are present
    #: and at their defaults; a reader must consult the dialect before
    #: printing one, or it prints a zero nobody measured.
    totals: dict[str, Any]
    #: Every distinct :attr:`HarnessTotals.usage_source` across the trials, in
    #: :data:`_USAGE_SOURCE_ORDER`. More than one means the seat's trials were
    #: not all measured the same way, which is a fact about the seat.
    usage_sources: tuple[str, ...]


#: Deterministic ordering for a seat's mixed usage sources.
_USAGE_SOURCE_ORDER = (USAGE_STREAM, USAGE_TRANSCRIPT, USAGE_UNOBSERVED)


def summarize(readings: Iterable[HarnessReading]) -> SeatBehaviour | None:
    """Fold one seat's trials into a single block. ``None`` for no trials.

    Every reading must share a dialect — they are one seat's — and the fold is
    deliberately arithmetic only: nothing here reconciles two arms, because two
    arms' numbers are in two vocabularies and the reconciliation is the reader's
    to make with :attr:`HarnessDialect.steps_are` in front of them.
    """
    readings = list(readings)
    if not readings:
        return None
    totals: dict[str, Any] = {}
    models: list[str] = []
    sources: set[str] = set()
    for reading in readings:
        sources.add(reading.totals.usage_source)
        for key, value in reading.totals.to_json().items():
            if key == "models":
                models.extend(name for name in value if name not in models)
            elif isinstance(value, bool):
                # `complete` is one trial's statement about itself; summing
                # booleans would report "3 completes" as a number.
                continue
            elif isinstance(value, (int, float)):
                totals[key] = totals.get(key, 0) + value
            elif isinstance(value, dict):
                bucket = totals.setdefault(key, {})
                for item, count in value.items():
                    bucket[item] = bucket.get(item, 0) + count
    totals["models"] = models
    return SeatBehaviour(
        dialect=readings[0].dialect,
        profile=readings[0].profile,
        trials=len(readings),
        totals=totals,
        usage_sources=tuple(s for s in _USAGE_SOURCE_ORDER if s in sources),
    )


# --------------------------------------------------------------------------
# The incremental reader
# --------------------------------------------------------------------------


def _parse(raw: bytes) -> dict[str, Any] | None:
    """One JSON line as an object, or ``None`` for anything else."""
    try:
        event = json.loads(raw)
    except ValueError:
        return None
    return event if isinstance(event, dict) else None


class _Cursor:
    """Where one file's last drain stopped."""

    __slots__ = ("offset", "pending")

    def __init__(self) -> None:
        self.offset = 0
        #: The trailing bytes of the last read that did not end in a newline.
        #: Held rather than dropped: the file is being appended to, so a torn
        #: line is the normal case and discarding it would lose whole events.
        self.pending = b""


class _Session:
    """One arm's fold, plus a cursor per file feeding it."""

    __slots__ = ("cursors", "fold")

    def __init__(self, fold: _Fold) -> None:
        self.fold = fold
        self.cursors: dict[Path, _Cursor] = {}


class StreamReader:
    """Incremental reader for the harness streams under one or many trials.

    Holds a byte offset per file and a running fold per arm, so a live
    dashboard pays only for what an agent appended since the last poll. That
    matters at the observed sizes: one archived Claude Code stream is 15 MB and
    99% of its lines are superseded reasoning-token estimates, so a re-parse per
    poll would cost more than everything else the arena does put together.

    Shrinking or replaced files reset rather than corrupt: a session whose file
    fell below its cursor is read again from zero, because the only ways that
    happens are a rerun into the same directory or a truncated archive, and both
    want a fresh reading rather than a continuation of a file that no longer
    exists.
    """

    def __init__(self) -> None:
        self._sessions: dict[tuple[str, Path], _Session] = {}

    def read(
        self, path: Path, dialect: HarnessDialect = CLAUDE_CODE
    ) -> tuple[HarnessProfile, HarnessTotals] | None:
        """Drain one known stream file and return the running profile and totals.

        ``None`` when the file does not exist — which is every arm that speaks
        another dialect, and a trial that has not started. Distinct from a file
        that exists and is empty, which reads as a real zero.

        Reads that one file and no sidecar: the caller named a path, not a
        trial. :meth:`read_arm` is the trial-directory form, and the one that
        can reach a fallback source.
        """
        session = self._session((dialect.arm, path), dialect, path)
        if session is None:
            return None
        self._feed(session, path)
        return session.fold.profile, session.fold.totals

    def read_arm(self, trial_dir: Path, dialect: HarnessDialect) -> HarnessReading | None:
        """Drain everything one arm published in one trial directory.

        ``None`` when this arm's stream is not there at all, which is how an
        arm that did not run in this trial reads — never as a zeroed reading.
        A sidecar is opened only when the fold says it still needs one, so an
        arm whose primary stream publishes its usage never pays for the
        fallback.

        The reading carries the reader's own running objects, so a caller
        holding one across two reads sees the numbers move. Snapshot with
        ``to_json`` for anything that must not.
        """
        primary = trial_dir / dialect.stream_name
        session = self._session((dialect.arm, primary), dialect, primary)
        if session is None:
            return None
        self._feed(session, primary)
        if dialect.sidecar_glob and session.fold.needs_sidecar():
            for path in sorted(trial_dir.glob(dialect.sidecar_glob)):
                self._feed(session, path, sidecar=True)
        return HarnessReading(dialect, session.fold.profile, session.fold.totals)

    def read_trial(self, trial_dir: Path) -> tuple[HarnessReading, ...]:
        """Every arm that published anything in one trial directory.

        Normally one — a trial belongs to a seat — but the reader does not
        assume it, because "which arm is this" is a fact about the files rather
        than about the directory's name.
        """
        readings = (self.read_arm(trial_dir, dialect) for dialect in DIALECTS)
        return tuple(reading for reading in readings if reading is not None)

    def forget(self, path: Path) -> None:
        """Drop a stream's cursor, so the next read starts over."""
        for key in [key for key in self._sessions if key[1] == path]:
            del self._sessions[key]

    def _session(
        self, key: tuple[str, Path], dialect: HarnessDialect, primary: Path
    ) -> _Session | None:
        """This arm's session for this trial, fresh if its files were replaced."""
        try:
            primary.stat()
        except OSError:
            return None
        session = self._sessions.get(key)
        if session is None or _replaced(session):
            session = _Session(dialect.new_fold())
            self._sessions[key] = session
        return session

    def _feed(self, session: _Session, path: Path, *, sidecar: bool = False) -> None:
        """Drain one file's new bytes into the session's fold."""
        cursor = session.cursors.get(path)
        if cursor is None:
            cursor = _Cursor()
            session.cursors[path] = cursor
        lines = _new_lines(path, cursor)
        if not lines:
            return
        if sidecar:
            session.fold.consume_sidecar(lines)
        else:
            session.fold.consume(lines)


def _replaced(session: _Session) -> bool:
    """Whether any file behind this session shrank below its cursor.

    One rerun into the same directory invalidates the whole reading, not just
    the file that was rewritten — the totals are a sum over every source, so
    continuing one and restarting another would blend two runs.
    """
    for path, cursor in session.cursors.items():
        try:
            size = path.stat().st_size
        except OSError:
            continue
        if size < cursor.offset:
            return True
    return False


def _new_lines(path: Path, cursor: _Cursor) -> list[bytes]:
    """The complete lines that arrived since this cursor last looked."""
    try:
        size = path.stat().st_size
    except OSError:
        return []
    if size <= cursor.offset:
        return []
    try:
        with path.open("rb") as handle:
            handle.seek(cursor.offset)
            chunk = handle.read(size - cursor.offset)
    except OSError:
        return []
    cursor.offset += len(chunk)
    buffer = cursor.pending + chunk
    lines = buffer.split(b"\n")
    # The last element is whatever followed the final newline: either an empty
    # string (the chunk ended cleanly) or a partial line still being written.
    # Either way it is not ready, and it is carried rather than parsed — the
    # cursor has already moved past those bytes, so dropping them would lose
    # the event permanently.
    cursor.pending = lines.pop()
    return lines


def reduce_stream(
    path: Path, dialect: HarnessDialect = CLAUDE_CODE
) -> tuple[HarnessProfile, HarnessTotals] | None:
    """One-shot read of a complete stream, for an archived trial.

    A thin wrapper over :class:`StreamReader` for callers with no reason to
    keep a cursor — a test, or a report on one finished file. A live caller
    wants the reader itself, and a caller with a trial directory rather than a
    path wants :meth:`StreamReader.read_arm`.
    """
    return StreamReader().read(path, dialect)
