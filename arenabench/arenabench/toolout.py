# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Stella's tool events, decoded the way a human reads them.

Every surface that shows a transcript — this package's SSE reader, the React
transcript page it feeds, the MP4 recorder, Stella's own Command Deck — is
looking at the same two events, and each one that decoded them by hand got it
wrong in a different way. This module is the arena side's single owner of that
decoding, so a fix lands once.

Three facts about the wire that a hand-rolled reader reliably misses:

**``ToolOutput`` is externally tagged.** A result arrives as
``{"ok": {"content": …}}`` or ``{"error": {"message": …}}`` — never as a bare
string. Passing that dict to ``str()`` yields a Python *repr*
(``{'ok': {'content': '--- branches ---\\n* master …'}}``): single quotes, and
every newline in the payload rendered as the two characters ``\\`` ``n`` on one
enormous line. That is what every arena transcript showed until
:func:`decode_tool_output` existed.

**The tag is the verdict.** ``tool_result`` carries no ``error`` or
``is_error`` field, and never has. A reader that asks for one gets ``None`` for
every event, so *every failed tool call renders as a success* — in the success
colour, with the failure message presented as output. Only
``output["error"]`` says a call failed.

**The name is on the call, not the result.** ``ToolResult`` is
``{call_id, output, duration_ms, speculated}``. Recovering "which tool was
this" means correlating back to the ``tool_start`` that shares its ``call_id``
— which is exactly what :class:`~arenabench.transcript.TranscriptReader` now
does, and what the deck has always done
(``crates/stella-tui/src/model.rs``'s ``ToolResult`` fold).

Stdlib only, and pure: every function here maps values to values, reads no
file and holds no state. That is what lets ``recorder/render.py`` — which
ships into a Docker image with no access to this package — carry an
acknowledged copy of :func:`decode_tool_output` and :func:`strip_ansi` that a
test holds to this one, input for input.
"""

from __future__ import annotations

import json
from typing import Any, NamedTuple

__all__ = [
    "DecodedOutput",
    "cap_middle",
    "decode_tool_output",
    "format_tool_input",
    "strip_ansi",
    "summarize",
]


class DecodedOutput(NamedTuple):
    """One ``ToolOutput`` as the transcript needs it."""

    #: The payload the model itself received — ``content`` or ``message``,
    #: with its real line breaks intact.
    text: str
    #: ``False`` only for the ``error`` arm. Read from the tag, never sniffed
    #: out of the prose.
    ok: bool
    #: Whether the value matched either arm of the enum. ``False`` means the
    #: protocol grew a shape this reader predates; ``text`` then holds compact
    #: JSON, which is at least readable and parseable, rather than a repr.
    recognized: bool


def decode_tool_output(output: Any) -> DecodedOutput:
    """Decode the externally-tagged ``ToolOutput`` a ``tool_result`` carries.

    The ``ok``/``error`` key is the success verdict, so it is read here and
    nowhere else. Note the membership test is on the *key*, not on the value's
    truthiness: a tool that legitimately produced no output at all sends
    ``{"ok": {"content": ""}}``, and treating that as unrecognized would
    replace an empty result with a JSON blob describing one.

    An unrecognized shape degrades to compact JSON rather than to ``repr``.
    Both are wrong to show a reader, but only one of them keeps the newlines
    escaped and the quotes un-parseable.
    """
    if isinstance(output, dict):
        ok_arm = output.get("ok")
        if isinstance(ok_arm, dict) and "content" in ok_arm:
            return DecodedOutput(_as_text(ok_arm["content"]), True, True)
        error_arm = output.get("error")
        if isinstance(error_arm, dict) and "message" in error_arm:
            return DecodedOutput(_as_text(error_arm["message"]), False, True)
    if output is None:
        return DecodedOutput("", True, False)
    if isinstance(output, str):
        # A stream that predates the typed enum, or a synthetic event written
        # by a test. Already the text; there is nothing to unwrap.
        return DecodedOutput(output, True, False)
    return DecodedOutput(_as_text(output), True, False)


def _as_text(value: Any) -> str:
    """A decoded arm's payload as a string, without ever reaching ``repr``.

    ``content`` is a string on every stream this reader has seen. It is not
    *guaranteed* to be one by anything the arena can check, and the failure
    mode of assuming so is the exact bug this module exists to remove — so a
    non-string is serialized, not repr'd.
    """
    if isinstance(value, str):
        return value
    return json.dumps(value, ensure_ascii=False, sort_keys=True)


# --- ANSI ----------------------------------------------------------------
# A port of `crates/stella-tui/src/ansi.rs::strip_ansi`, state for state, so
# the arena and the deck agree about what a colourised `cargo build` looks
# like once it is text. Tool output reaches a transcript verbatim, and any
# child process that detects a TTY colourises it; the escapes are invisible in
# a terminal that understands them and visible garbage everywhere else — an
# HTML `<pre>`, a React node, an MP4 frame.

_CSI_FINAL = range(0x40, 0x7F)
_NF_INTERMEDIATE = range(0x20, 0x30)


def strip_ansi(text: str) -> str:
    """Remove ANSI escape sequences, leaving the visible text.

    Handles the three shapes that occur in practice: CSI (``ESC [`` through a
    final byte in ``@..~``, which covers every SGR colour code), OSC (``ESC ]``
    through BEL or ST, as emitted for hyperlinks and window titles), and the
    residual two-byte ``ESC x`` escapes. A bare ``[`` not preceded by ESC is
    legitimate content — ``error[E0308]`` — and always survives; over-eager
    stripping would be a worse bug than the residue it removes.
    """
    if "\x1b" not in text:
        return text
    out: list[str] = []
    i, n = 0, len(text)
    while i < n:
        ch = text[i]
        if ch != "\x1b":
            out.append(ch)
            i += 1
            continue
        i += 1
        if i >= n:
            break  # a trailing lone ESC introduces nothing
        intro = text[i]
        i += 1
        if intro == "[":
            while i < n and ord(text[i]) not in _CSI_FINAL:
                i += 1
            i += 1  # the final byte, or one past the end of a truncated run
        elif intro == "]":
            while i < n:
                if text[i] == "\x07":
                    i += 1
                    break
                if text[i] == "\x1b" and i + 1 < n and text[i + 1] == "\\":
                    i += 2
                    break
                i += 1
        elif ord(intro) in _NF_INTERMEDIATE:
            while i < n and ord(text[i]) in _NF_INTERMEDIATE:
                i += 1
            i += 1  # the single final byte
    return "".join(out)


# --- capping -------------------------------------------------------------

#: The elision marker :func:`cap_middle` splices in, on its own lines so a
#: capped payload still reads as a payload. Matches `stella-tui`'s.
ELISION = "\n[… truncated …]\n"


def cap_middle(text: str, budget: int, marker: str = ELISION) -> str:
    """Cap ``text`` to ``budget`` characters, elided from the **middle**.

    Head-truncation is the obvious implementation and the wrong one: the two
    ends of a tool result are the two parts a reader wants. The head says what
    the command was and how it started; the tail carries the error, the exit
    code, and the summary line. A tail-truncated 8 KB clip of a failing build
    is the one slice that answers nothing.
    """
    if budget <= 0 or len(text) <= budget:
        return text
    keep = max(0, budget - len(marker))
    head = keep // 2
    tail = keep - head
    return text[:head] + marker + (text[len(text) - tail :] if tail else "")


def summarize(text: str, budget: int = 200) -> str:
    """One flat line standing in for a whole payload — a row label, not a body.

    Caps *before* flattening. The replacement is one character in, one out, so
    the two orders agree on the result while this one avoids copying a
    multi-megabyte payload to throw all but 200 characters of it away.
    """
    return cap_middle(text, budget, "...").replace("\n", " ").replace("\r", " ")


# --- tool arguments ------------------------------------------------------

#: Tools whose *first* argument is worth the row on its own, in probe order.
#: A port of `stella-tui`'s `format_tool_input`, and deliberately the same
#: order: the deck and the arena must label the same call the same way, or a
#: reader comparing a recording against a live run sees two different runs.
_PRIMARY_FIELDS: tuple[tuple[tuple[str, ...], int], ...] = (
    (("path", "file_path"), 0),
    (("cmd", "command"), 120),
    (("query", "pattern", "symbol"), 80),
    (("question", "prompt"), 80),
)


def format_tool_input(name: str, arguments: Any) -> str:
    """The one-line argument label beside a tool's name.

    A tool call's whole object is rarely what a reader wants in a scrolling
    transcript — the path, the command, or the query is. The full object stays
    available (the reader keeps it as ``meta.raw``); this is the row.
    """
    if not isinstance(arguments, dict):
        return summarize("" if arguments is None else _as_text(arguments))

    # `apply_edits` (a name appearing only in the archived traces this
    # renders) carries its paths inside a batch
    # rather than at the top level; surfacing them keeps its row reading like
    # the other file tools' instead of falling through to the raw-JSON tail.
    if name == "apply_edits" and isinstance(arguments.get("edits"), list):
        paths: list[str] = []
        for edit in arguments["edits"]:
            if isinstance(edit, dict):
                path = edit.get("path")
                if isinstance(path, str) and path not in paths:
                    paths.append(path)
        if paths:
            labels = [_truncate_field(p, 48) for p in paths]
            if len(paths) == 1:
                return labels[0]
            return f"{', '.join(labels)}  ({len(paths)} files)"

    for keys, width in _PRIMARY_FIELDS:
        for key in keys:
            value = arguments.get(key)
            if not isinstance(value, str):
                continue
            if width == 0:
                # A path stands alone, except that a write states its size —
                # the one number that says whether this was a touch or a
                # rewrite, and the one the diff below cannot show yet.
                if name == "write_file" and isinstance(arguments.get("content"), str):
                    return f"{value}  ({len(arguments['content'].splitlines())} lines)"
                return value
            return _truncate_field(value, width)

    return summarize(json.dumps(arguments, ensure_ascii=False, separators=(",", ":")))


def _truncate_field(text: str, limit: int) -> str:
    """Clip one field to ``limit`` characters, flattened onto a single line."""
    if len(text) <= limit:
        return text.replace("\n", " ").replace("\r", " ")
    return text[: max(0, limit - 1)].replace("\n", " ").replace("\r", " ") + "…"
