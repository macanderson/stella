"""What a trial spent waiting: the `sleep` seconds a command committed to.

This mirrors `blocking_sleep_seconds` in `crates/stella-core/src/shell_text.rs`,
which is the predicate the shipped `bash` rungs ask. A trace read with a
different predicate than the binary used would report a share the binary never
acted on.

The mirror is close, not exact. Rust uses a quote-aware `shell_words`. Here
the split is on whitespace, once the operators are cut out. So a `sleep` in a
quoted string is a sleep here and data there. No trace this has run against
has that shape. If one turns up, fix the parser rather than explain the
number.

Both sides share the segment rule, and it is strict. A segment counts only
when it is exactly `sleep N`. So `…; sleep 1; done` reads as one second.
`do sleep 20;` reads as no sleep, because the loop keyword is in the segment.
The predicate under-reads a poll loop. That is why a poll loop is never
declined.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

#: The `sleep` seconds past which `bash` declines a call before it spawns.
#: This is `SLEEP_REFUSAL_THRESHOLD_SECS` in
#: `crates/stella-tools/src/bash/wait.rs`, which is that tool's
#: `DEFAULT_TIMEOUT_SECS`. It lives here so an old trace and a new one are
#: scored against one bound.
REFUSAL_BOUND_SECS = 120

#: The seconds past which the call still runs and the result names the wait —
#: `SLEEP_ADVISORY_THRESHOLD_SECS` in the same file.
ADVISORY_BOUND_SECS = 30

_OPERATORS = frozenset({";", "&&", "||", "|", "&", "\n"})
_UNITS = {"s": 1.0, "m": 60.0, "h": 3600.0, "d": 86400.0}

#: Longest first, so `&&` is one token and not two. The separator is captured
#: and kept. `shell_words` does the same: an operator is its own word, even
#: glued to one.
_SPLIT = re.compile(r"(&&|\|\||;|\||&|\n)")


def _words(command: str) -> list[str]:
    out: list[str] = []
    for piece in _SPLIT.split(command):
        if piece in _OPERATORS:
            out.append(piece)
        else:
            out.extend(piece.split())
    return out


def _arg_seconds(arg: str) -> float | None:
    digits, per_unit = arg, 1.0
    if arg and arg[-1] in _UNITS:
        digits, per_unit = arg[:-1], _UNITS[arg[-1]]
    try:
        return float(digits) * per_unit
    except ValueError:
        return None


def blocking_sleep_seconds(command: str) -> int | None:
    """Seconds this command waits in a foreground `sleep`, or `None` for none.

    A segment the shell backgrounds is skipped, as the Rust walk skips it.
    `pip install … & sleep 490` waits 490 seconds. The install is not part of
    the wait.
    """
    words = _words(command)
    total = 0.0
    saw_sleep = False
    start = 0
    for end in range(len(words) + 1):
        separator = words[end] if end < len(words) and words[end] in _OPERATORS else None
        if end < len(words) and separator is None:
            continue
        segment = words[start:end]
        start = end + 1
        if separator == "&":
            continue
        if len(segment) == 2 and segment[0] == "sleep":
            secs = _arg_seconds(segment[1])
            if secs is not None:
                total += secs
                saw_sleep = True
    return round(total) if saw_sleep else None


@dataclass(frozen=True)
class Wait:
    """One `bash` call that committed to a wait, with what it actually cost."""

    call_id: str
    command: str
    declared_sleep_secs: int
    elapsed_secs: float | None

    @property
    def declined_by_the_shipped_rung(self) -> bool:
        return self.declared_sleep_secs >= REFUSAL_BOUND_SECS


def waits_in(trial) -> list[Wait]:  # noqa: ANN001 — `run_trace.Trial`, imported by its user
    """Every `bash` call in `trial` whose text commits to a foreground sleep.

    `elapsed_secs` joins `tool_start` to `tool_result` by `call_id`. It is the
    wall clock the call took, not the seconds it asked for. The two agree on a
    blind wait and part on anything that exits early, so both are carried.
    """
    # `start_index` is a line number, not an index into `events`. So the start
    # stamp comes from the `tool_start` event itself.
    starts = {
        str(e.get("call", {}).get("call_id") or ""): e.get("ts")
        for e in trial.of_type("tool_start")
    }
    found: list[Wait] = []
    for call in trial.tool_calls:
        command = call.input.get("command")
        if call.name != "bash" or not isinstance(command, str):
            continue
        secs = blocking_sleep_seconds(command)
        if secs is None:
            continue
        elapsed = None
        started = starts.get(call.call_id)
        ended = call.result.get("ts") if call.result else None
        if isinstance(started, int | float) and isinstance(ended, int | float):
            elapsed = (ended - started) / 1000.0
        found.append(
            Wait(
                call_id=call.call_id,
                command=command,
                declared_sleep_secs=secs,
                elapsed_secs=elapsed,
            )
        )
    return found
