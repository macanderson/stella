#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Render a live agent event stream to a terminal, for the camera.

This is the *content* of the MP4: it runs inside the xterm that ffmpeg is
filming. It tails the agent's append-only JSONL and paints a transcript that
reads well at video framerates — pinned header and footer, a scrolling body,
and colour that distinguishes reasoning from output from tool calls at a
glance.

**Painting model: a diffed absolute repaint, not a scroll region.** The obvious
implementation is a DECSTBM scroll region plus a bare newline per line, and it
is what this file used to do — but xterm under Xvfb did not scroll it, so the
body silently never appeared while the absolutely-positioned header did. Rather
than depend on that behaviour, the screen is now modelled explicitly: a ring
buffer of body lines, repainted by absolute cursor address, writing only the
rows whose content actually changed since the last frame.

That costs nothing at these sizes and buys two things a scroll region did not.
It is deterministic across terminals, and because unchanged rows are never
rewritten, the capture cannot catch a row mid-repaint — which is the tearing
that made a naive full redraw look like flicker in the finished video.

stdlib only, and read-only with respect to everything it displays.
"""

from __future__ import annotations

import contextlib
import json
import os
import shutil
import sys
import textwrap
import time
from typing import Any

# --- Stella's tool events, decoded ---------------------------------------
# An ACKNOWLEDGED COPY of `arenabench.toolout.decode_tool_output` and
# `strip_ansi`. This file is the entire Python payload of the recorder image —
# its Docker build context is `arenabench/recorder/`, which cannot reach the
# package — so importing the canonical module is not available here and a copy
# is the honest alternative to a fifth hand-rolled decoder.
#
# The copy is not left to trust: `tests/test_toolout.py::
# TestRecorderCopyStaysHonest` imports this file by path and holds both
# functions to the canonical ones input for input. Change one, change the
# other, or that test goes red. Keep the signatures identical (a plain tuple
# stands in for the canonical NamedTuple) so the comparison stays direct.


def decode_tool_output(output: Any) -> tuple[str, bool, bool]:
    """``ToolOutput`` as ``(text, ok, recognized)`` — see the canonical copy.

    The enum is externally tagged and the tag is the verdict: a `tool_result`
    carries no `error` field, so `output["error"]` is the only thing that says
    a call failed. Passing the dict to `str()` — which this file used to do —
    puts a Python repr on screen with every newline escaped.
    """
    if isinstance(output, dict):
        ok_arm = output.get("ok")
        if isinstance(ok_arm, dict) and "content" in ok_arm:
            return _as_text(ok_arm["content"]), True, True
        error_arm = output.get("error")
        if isinstance(error_arm, dict) and "message" in error_arm:
            return _as_text(error_arm["message"]), False, True
    if output is None:
        return "", True, False
    if isinstance(output, str):
        return output, True, False
    return _as_text(output), True, False


def _as_text(value: Any) -> str:
    if isinstance(value, str):
        return value
    return json.dumps(value, ensure_ascii=False, sort_keys=True)


_CSI_FINAL = range(0x40, 0x7F)
_NF_INTERMEDIATE = range(0x20, 0x30)


def strip_ansi(text: str) -> str:
    """Remove ANSI escape sequences, leaving the visible text.

    The recorder paints its *own* colour, so a tool's escapes are not merely
    ugly here — they reset the renderer's foreground mid-row and bleed the
    tool's palette across the rest of the frame.
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
            break
        intro = text[i]
        i += 1
        if intro == "[":
            while i < n and ord(text[i]) not in _CSI_FINAL:
                i += 1
            i += 1
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
            i += 1
    return "".join(out)


# --- palette -------------------------------------------------------------
# 24-bit colour; xterm in the recorder image supports it. Chosen for contrast
# against #07080B at video bitrates, where thin low-contrast text turns to mud.
RESET = "\x1b[0m"
DIM = "\x1b[2m"
BOLD = "\x1b[1m"


def rgb(r: int, g: int, b: int) -> str:
    return f"\x1b[38;2;{r};{g};{b}m"


AMBER = rgb(255, 176, 0)
CYAN = rgb(55, 213, 242)
VIOLET = rgb(199, 125, 255)
MINT = rgb(92, 230, 138)
ROSE = rgb(255, 107, 157)
SLATE = rgb(126, 138, 160)
BONE = rgb(214, 219, 229)
CITRON = rgb(255, 224, 102)

STYLES = {
    "reasoning": (VIOLET, "think"),
    "text": (BONE, "say"),
    "tool": (CYAN, "tool"),
    "tool_result": (SLATE, " ->"),
    "stage": (AMBER, "stage"),
    "error": (ROSE, "error"),
    "verdict": (MINT, "verifier"),
    "complete": (MINT, "done"),
}

GUTTER = 14  # width of the "  0:07 think " prefix


def visible_len(text: str) -> int:
    """Length of ``text`` ignoring ANSI escape sequences."""
    out, index = 0, 0
    while index < len(text):
        if text[index] == "\x1b":
            end = text.find("m", index)
            index = len(text) if end == -1 else end + 1
            continue
        out += 1
        index += 1
    return out


class Screen:
    """A pinned header and footer with a scrolling body, painted by diff."""

    HEADER_ROWS = 2
    FOOTER_ROWS = 2

    def __init__(self) -> None:
        size = shutil.get_terminal_size(fallback=(160, 48))
        self.cols = max(60, size.columns)
        self.rows = max(16, size.lines)
        # Never write into the final column: that triggers the terminal's
        # auto-wrap and shifts everything below by a row.
        self.width = self.cols - 1
        self.body_top = self.HEADER_ROWS + 1
        self.body_bottom = self.rows - self.FOOTER_ROWS
        self.body_height = max(1, self.body_bottom - self.body_top + 1)
        self.out = sys.stdout
        with contextlib.suppress(AttributeError, ValueError):  # pragma: no cover
            self.out.reconfigure(encoding="utf-8", errors="replace")
        #: What is currently on each row, so a repaint can skip unchanged ones.
        self._painted: dict[int, str] = {}

    def begin(self) -> None:
        self.write("\x1b[?25l\x1b[2J")  # hide cursor, clear
        self.flush()

    def end(self) -> None:
        self.write(f"\x1b[?25h\x1b[{self.rows};1H\n")
        self.flush()

    def write(self, text: str) -> None:
        with contextlib.suppress(BrokenPipeError, ValueError):  # pragma: no cover
            self.out.write(text)

    def flush(self) -> None:
        with contextlib.suppress(BrokenPipeError, ValueError):  # pragma: no cover
            self.out.flush()

    def row(self, index: int, text: str) -> None:
        """Paint one absolute row, but only if its content changed."""
        if self._painted.get(index) == text:
            return
        self._painted[index] = text
        self.write(f"\x1b[{index};1H\x1b[2K{text}")

    def paint_body(self, lines: list[str]) -> None:
        """Paint the body ring buffer: fill from the top, then scroll.

        Top-anchored rather than bottom-anchored, which differs from a real
        terminal on purpose. This output is a video: a short trial that
        bottom-anchors spends its whole runtime as a few lines pinned above
        the footer with a screen of black above them. Filling downward and
        only scrolling once full reads far better and is identical for any
        trial long enough to fill the screen.
        """
        window = lines[-self.body_height :]
        for offset in range(self.body_height):
            content = window[offset] if offset < len(window) else ""
            self.row(self.body_top + offset, content)


def human(number: float) -> str:
    for limit, suffix in ((1e9, "B"), (1e6, "M"), (1e3, "k")):
        if abs(number) >= limit:
            return f"{number / limit:.1f}{suffix}"
    return f"{number:.0f}"


def clock(seconds: float) -> str:
    minutes, secs = divmod(int(max(0, seconds)), 60)
    hours, minutes = divmod(minutes, 60)
    return f"{hours:d}:{minutes:02d}:{secs:02d}" if hours else f"{minutes:d}:{secs:02d}"


class Renderer:
    def __init__(self, path: str, title: str, contestant: str) -> None:
        self.path = path
        self.title = title
        self.contestant = contestant
        self.screen = Screen()
        self.started = time.time()
        self.offset = 0
        self.totals = {
            "steps": 0,
            "tools": 0,
            "tin": 0,
            "tout": 0,
            "cread": 0,
            "cwrite": 0,
            "cost": 0.0,
        }
        self.model = ""
        self.state = "waiting"
        #: ``call_id`` -> tool name, so a result row can name the call it
        #: answers. A `tool_result` carries only the id.
        self.tool_names: dict[str, str] = {}
        #: Rendered body lines. Bounded: a long trial can emit tens of
        #: thousands, and only the last screenful can ever be visible.
        self.body: list[str] = []
        #: Which kind of streamed text the last body line belongs to, so
        #: consecutive fragments extend it instead of starting a new row.
        self.open_kind: str | None = None
        self.open_plain = ""
        self.open_rows = 0
        self.open_gutter = ""

    # -- body construction ------------------------------------------------

    def push(self, text: str) -> None:
        self.body.append(text)
        if len(self.body) > self.screen.body_height * 4:
            del self.body[: self.screen.body_height]

    def gutter(self, kind: str, label: str | None = None) -> str:
        color, tag = STYLES.get(kind, (SLATE, kind[:5]))
        tag = label if label is not None else tag
        stamp = clock(time.time() - self.started)
        return f"{DIM}{SLATE}{stamp:>7}{RESET} {color}{tag:<5}{RESET} "

    def line(self, kind: str, text: str) -> None:
        """Append a complete, wrapped entry."""
        self.open_kind = None
        self.open_rows = 0
        color, _ = STYLES.get(kind, (SLATE, ""))
        body_color = BONE if kind == "text" else color
        width = max(20, self.screen.width - GUTTER)
        chunks = textwrap.wrap(text, width=width) or [""]
        for index, chunk in enumerate(chunks):
            head = self.gutter(kind) if index == 0 else " " * GUTTER
            self.push(f"{head}{body_color}{chunk}{RESET}")

    def stream(self, kind: str, fragment: str) -> None:
        """Extend the current streamed run, wrapping onto new body lines.

        Rebuilding the tail line each time is what produces the word-by-word
        growth in the recording: the same row is repainted with one more word,
        which reads as a mind working rather than as a log file.
        """
        color = VIOLET if kind == "reasoning" else BONE
        width = max(20, self.screen.width - GUTTER)
        if self.open_kind != kind:
            self.open_kind = kind
            self.open_plain = ""
            self.open_rows = 0
            self.open_gutter = self.gutter(kind, "think" if kind == "reasoning" else "say")
        self.open_plain += fragment.replace("\n", " ")

        # Re-wrap the whole run and swap its rows in place. Rewrapping rather
        # than appending is what keeps a word from being split across the wrap
        # point as the text grows past it.
        if self.open_rows:
            del self.body[-self.open_rows :]
        wrapped = textwrap.wrap(self.open_plain, width=width) or [""]
        for index, chunk in enumerate(wrapped):
            head = self.open_gutter if index == 0 else " " * GUTTER
            self.push(f"{head}{color}{chunk}{RESET}")
        self.open_rows = len(wrapped)

    # -- painting ---------------------------------------------------------

    def paint_header(self) -> None:
        left = f"{AMBER}{BOLD}ARENABENCH{RESET}  {BONE}{self.contestant}{RESET}"
        right = f"{SLATE}{self.title}{RESET}"
        pad = max(1, self.screen.width - visible_len(left) - visible_len(right))
        self.screen.row(1, f"{left}{' ' * pad}{right}")
        self.screen.row(2, f"{DIM}{SLATE}{'─' * self.screen.width}{RESET}")

    def paint_footer(self) -> None:
        self.screen.row(self.screen.rows - 1, f"{DIM}{SLATE}{'─' * self.screen.width}{RESET}")
        t = self.totals
        dot = {"waiting": SLATE, "running": AMBER, "done": MINT}.get(self.state, SLATE)
        bar = (
            f"{dot}●{RESET} {BONE}{clock(time.time() - self.started)}{RESET}  "
            f"{SLATE}step{RESET} {BONE}{t['steps']}{RESET}  "
            f"{SLATE}tool{RESET} {BONE}{t['tools']}{RESET}  "
            f"{SLATE}in{RESET} {BONE}{human(t['tin'])}{RESET}  "
            f"{SLATE}out{RESET} {BONE}{human(t['tout'])}{RESET}  "
            f"{SLATE}cache{RESET} {CYAN}{human(t['cread'])}{RESET}"
            f"{SLATE}/{RESET}{CITRON}{human(t['cwrite'])}{RESET}  "
            f"{SLATE}self-rep{RESET} {AMBER}${t['cost']:.4f}{RESET}"
        )
        model = f"{SLATE}{self.model}{RESET}" if self.model else ""
        pad = max(1, self.screen.width - visible_len(bar) - visible_len(model))
        self.screen.row(self.screen.rows, f"{bar}{' ' * pad}{model}")

    def paint(self) -> None:
        self.paint_header()
        self.screen.paint_body(self.body)
        self.paint_footer()
        self.screen.flush()

    # -- ingest -----------------------------------------------------------

    def consume(self, event: dict) -> None:
        kind = str(event.get("type", ""))
        if self.state == "waiting":
            self.state = "running"

        if kind in ("text", "reasoning"):
            # `text` spells its body `text` since #1886, `delta` before;
            # `reasoning` still spells it `delta`. Read both.
            fragment = str(event.get("delta") or event.get("text") or "")
            if fragment:
                self.stream("reasoning" if kind == "reasoning" else "text", fragment)
            return
        if kind == "text_delta":
            return  # a preview of `text`; never show both

        self.open_kind = None
        self.open_rows = 0

        if kind == "stage":
            name = str(event.get("name", ""))
            self.line("stage", f"── {name} ".ljust(46, "─"))
        elif kind == "tool_start":
            call = event.get("call") or {}
            call_id = str(call.get("call_id") or call.get("id") or "")
            name = str(call.get("name") or "tool")
            # The arguments ride under `input` — `ToolCall { call_id, name,
            # input }`. Asking for `arguments` (a key the wire never carried)
            # rendered every tool row in every recording as a bare name with
            # no command, path or query beside it.
            args = call.get("input", call.get("arguments"))
            summary = (
                json.dumps(args, ensure_ascii=False)
                if isinstance(args, (dict, list))
                else str(args or "")
            )[:220]
            if call_id:
                self.tool_names[call_id] = name
            self.totals["tools"] += 1
            self.line("tool", f"{name}  {summary}")
        elif kind == "tool_result":
            text, ok, _ = decode_tool_output(
                event.get("output", event.get("result"))
            )
            body = strip_ansi(text).strip()
            # The name is on the call, not the result: a video row reading
            # `-> ok` names nothing a viewer can follow, and at video pace
            # scrolling back to find the matching call is not an option.
            name = self.tool_names.get(str(event.get("call_id") or ""), "tool")
            first = body.splitlines()[0][:220] if body else "(no output)"
            extra = f"  (+{body.count(chr(10))} lines)" if "\n" in body else ""
            self.line("error" if not ok else "tool_result", f"{name}  {first}{extra}")
        elif kind == "step_usage":
            self.totals["steps"] += 1
            self.totals["tin"] += int(event.get("input_tokens") or 0)
            self.totals["tout"] += int(event.get("output_tokens") or 0)
            self.totals["cread"] += int(event.get("cached_input_tokens") or 0)
            self.totals["cwrite"] += int(event.get("cache_write_tokens") or 0)
            self.totals["cost"] += float(event.get("cost_usd") or 0.0)
            self.model = str(event.get("model") or self.model)
        elif kind == "error":
            self.line("error", str(event.get("message") or ""))
        elif kind == "verdict":
            self.line("verdict", "PASS" if event.get("passed") else "FAIL")
        elif kind == "complete":
            self.state = "done"
            self.line("complete", f"complete · ${float(event.get('cost_usd') or 0):.4f}")

    def tail(self) -> None:
        try:
            size = os.path.getsize(self.path)
        except OSError:
            return
        if size <= self.offset:
            return
        try:
            with open(self.path, "rb") as handle:
                handle.seek(self.offset)
                raw = handle.read()
        except OSError:
            return
        text = raw.decode("utf-8", errors="replace")
        if not text.endswith("\n"):
            cut = text.rfind("\n")
            if cut == -1:
                return
            text = text[: cut + 1]
        self.offset += len(text.encode("utf-8"))
        for line in text.splitlines():
            if not line.strip():
                continue
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if isinstance(event, dict):
                self.consume(event)

    def run(self) -> None:
        self.screen.begin()
        self.line("stage", f"waiting for {os.path.basename(self.path)} …")
        self.paint()
        try:
            while True:
                self.tail()
                self.paint()
                time.sleep(0.1)
        except KeyboardInterrupt:
            pass
        finally:
            self.screen.end()


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("ARENA_EVENTS", "")
    if not path:
        print("usage: render.py <events.jsonl>", file=sys.stderr)
        return 2
    title = os.environ.get("ARENA_TASK", os.path.basename(os.path.dirname(path)))
    contestant = os.environ.get("ARENA_CONTESTANT", "contestant")
    Renderer(path, title, contestant).run()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
