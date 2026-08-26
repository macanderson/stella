"""Grade an ingested trial's steps, turns and execution.

Usage:
    python3 grade.py --db bench.db [--run TAG] [--trial TRIAL_ID]
        [--grader-version v1-deterministic]

The second pass over a trial `ingest.py` has already loaded, implementing
sections 4, 5, 7 and phase 1 of section 9 of
doc:step-grading-and-productive-ratio. It reads `trials`/`events` and writes
only `step_grades`/`turn_grades`/`execution_grades`; it never touches
`trials.reward` or `trials.passed`, and it runs against artifacts already on
disk rather than a live agent ("replay, never perturb").

Determinism is the whole contract. Re-grading a trial six months from now must
produce the same rows, so every timestamp here is milliseconds from the trial's
own first stamped event and every price comes from the run date recorded on the
trial, never from the clock this process reads. `graded_at` is the one
wall-clock column, and it records when grading ran rather than anything about
the trial.

## What is implemented, and what is not

Of the five rules in section 5, three are implemented as written, one against a
substitute derivation, and one is not implementable from this store at all:

* **Rule 1, loop/retry** — implemented as written.
* **Rule 2, verifier delta** — NOT implemented, and not implementable here. It
  needs a `verifier_tests` snapshot from before and after a window, and that
  table has no window key, no sequence and no timestamp: its columns are
  `(id, trial_id, name, status, duration_ms, message)`, one final CTRF report
  per trial. There is nothing to difference. Tracked in #5089, which also
  carries the harness change it depends on.
* **Rule 3, diff convergence** — implemented against a *substitute* for "the
  trial's final accepted patch", which is not an artifact this store holds. A
  line a window adds counts as convergent when no later `file_change` on the
  same path removes it, and divergent when one does; the reverse for a line it
  removes. That answers the same question — did this edit survive? — from the
  event stream rather than from a patch file.
* **Rule 4, tool exit** — implemented. A nonzero shell exit is
  `ToolOutput::Error` (witnessed by `nonzero_exit_is_error` in
  `crates/stella-tools/src/bash.rs`), so the arm of a `tool_result` is the exit
  signal and no prose is parsed to find it.
* **Rule 5, no-op** — implemented, with one substitution stated in `_is_no_op`.

Section 6's agent-assisted fallback is phase 2 and is absent: a window no rule
settles is written `neutral` / `deterministic` / `unclassified_floor`, so
"ruled neutral" and "no rule fired" stay tellable apart in the data instead of
being folded together by a judge that does not exist yet. That fallback and
section 8's transcript join report are #5091.
"""

from __future__ import annotations

import argparse
import json
import sys

from ingest import (
    COST_NORM_NO_MODEL,
    COST_NORM_NO_TOKENS,
    COST_NORM_PRICED,
    COST_NORM_UNPRICED_MODEL,
    connect,
    normalized_cost,
    now,
    parse_timestamp,
)

# Phase 1 of section 9: the rule chain with no judge behind it. The version is
# the re-run key — an improved ruleset picks a new string and writes a new row
# set beside this one rather than over it.
GRADER_VERSION = "v1-deterministic"

# Where the truncated-payload hole in `_payload` is being fixed. A constant so
# the docstring's claim and the runtime report name the same issue.
TRUNCATION_GAP = "#5088"

PRODUCTIVE = "productive"
UNPRODUCTIVE = "unproductive"
NEUTRAL = "neutral"

DETERMINISTIC = "deterministic"

# The floor reason (section 9, phase 1). A window carrying this is one every
# rule declined, which is a different fact from a window ruled
# `no_measurable_effect` — the first is the residue phase 2's judge is for, the
# second is already settled.
UNCLASSIFIED_FLOOR = "unclassified_floor"
NO_MEASURABLE_EFFECT = "no_measurable_effect"

# Every deterministic rule is exact by construction, so its confidence is not a
# self-report and not a tuning knob.
DETERMINISTIC_CONFIDENCE = 1.0

# The shell tool: the one tool that can change the workspace without emitting a
# `file_change`, which is why `_is_no_op` names it rather than copying a
# read-only list out of `crates/stella-tools/src/catalog.rs`. A tool table
# duplicated into a second language is the drift `scripts/check-role-names.sh`
# exists to catch.
SHELL_TOOL = "bash"

# Command shapes rule 4 treats as a check: a build, a test run, or a lint.
# Matched as a plain substring of the command text, so `cargo test -p x` and
# `uv run pytest -q` both land. Short on purpose — a command this does not
# recognise falls through to a later rule instead of being graded on a guess.
CHECK_COMMANDS = (
    "cargo test",
    "cargo build",
    "cargo clippy",
    "cargo check",
    "pytest",
    "go test",
    "npm test",
    "make check",
    "make test",
    "make gate",
    "make lint",
)

# The boundary `verdict_alignment` calls a ratio high or low. A convention, not
# a measurement: nothing has established that 0.5 separates a healthy trial
# from a thrashing one. The column is a sanity flag for a human's eye and is
# never fed back into pass/fail (section 2, non-goal 2), and the number is
# written down here so a reader of a `discordant` row can see what produced it.
VERDICT_ALIGNMENT_RATIO_FLOOR = 0.5

CONSISTENT = "consistent"
DISCORDANT = "discordant"

# Turn outcomes, mirroring how a turn ended on the wire.
OUTCOME_COMPLETE = "complete"
OUTCOME_ERROR = "error"
OUTCOME_LOOP_ABORTED = "loop_aborted"
OUTCOME_IN_PROGRESS = "in_progress"

# Column positions in the `turn_grades` tuples `aggregate_turns` builds and
# `aggregate_execution` sums. Named because a bare `row[10]` in an aggregation
# is the shape that silently starts summing the wrong column.
_TURN_TS_START = 3
_TURN_TS_END = 4
_TURN_MODEL_MS = 6
_TURN_COST = 7
_TURN_COST_STATUS = 8
_TURN_PRODUCTIVE = 10
_TURN_UNPRODUCTIVE = 11
_TURN_NEUTRAL = 12


class Window:
    """One graded step-window: a `step_usage` event and everything since the
    previous one.

    Holds its raw events rather than a summary, so every rule reads the same
    slice and a rule added later needs no second pass over the store.
    """

    __slots__ = (
        "turn_instance",
        "step",
        "call_index",
        "seq_start",
        "seq_end",
        "usage",
        "events",
    )

    def __init__(self, *, turn_instance, step, call_index, seq_start, seq_end, usage, events):
        self.turn_instance = turn_instance
        self.step = step
        self.call_index = call_index
        self.seq_start = seq_start
        self.seq_end = seq_end
        self.usage = usage
        self.events = events

    def of_type(self, *types):
        """Every payload in this window whose event type is one of `types`."""
        return [payload for _seq, kind, payload in self.events if kind in types]


def _payload(raw):
    """Parse one `events.payload_json` cell, or return None.

    `ingest.py` stores `json.dumps(event)[:20000]`, so an event whose JSON
    exceeded that cap sits on disk as a truncated prefix that does not parse.
    That hole is loudest exactly where this grader wants to look — a
    `file_change` carrying a large diff, a `tool_result` carrying a long build
    log — so unreadable payloads are counted and reported rather than read as
    absent facts. See `TRUNCATION_GAP`.
    """
    try:
        return json.loads(raw)
    except (ValueError, TypeError):
        return None


def _int(value, default=None):
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _ts(payload):
    return _int(payload.get("ts"))


def load_events(conn, trial_id):
    """Every ingested event for one trial, oldest first, with its payload
    parsed. Returns `(events, unreadable_count)`.

    An unreadable payload keeps its row with an empty dict: the event's
    position and type are still facts, and dropping a `step_usage` would take
    every later window's identity key with it.
    """
    events = []
    unreadable = 0
    for seq, kind, raw in conn.execute(
        "SELECT seq, type, payload_json FROM events WHERE trial_id = ? ORDER BY seq",
        (trial_id,),
    ):
        payload = _payload(raw)
        if payload is None:
            unreadable += 1
            payload = {}
        events.append((seq, kind, payload))
    return events, unreadable


def origin_ms(events):
    """The trial's own clock origin: the `ts` of its first stamped event.

    Every window's timestamps are milliseconds from this, which is what makes a
    grade computed today and one computed after a schema migration assign the
    same `ts_start_ms` to the same step. A stream recorded before `ts` existed
    on the wire has no origin, and its timing columns stay NULL rather than
    being backfilled with a guess.
    """
    for _seq, _kind, payload in events:
        ts = _ts(payload)
        if ts is not None:
            return ts
    return None


def split_windows(events):
    """Split a trial's events into step-windows and turn outcomes.

    A window runs from the event after the previous `step_usage` through the
    `step_usage` that closes it, which is the span section 4 defines. Events
    trailing the last `step_usage` belong to no model call and form no window;
    they are still read for the turn outcome.

    Returns `(windows, outcomes)`, where `outcomes` maps a turn key to how that
    turn ended. A turn with no terminator in the stream is absent from the map
    and reads as `in_progress`, which is what a graded prefix is.
    """
    windows = []
    outcomes = {}
    pending = []
    # Fallback turn key for a stream whose `step_usage` carries no
    # `turn_instance` (pre-#4793). A positional count of the turns seen so far,
    # never claimed to be the engine's own instance id — when the stream
    # carries the real one, the real one wins.
    derived_turn = 0
    current_turn = 0
    # How many windows have already claimed each `(turn, step)`, so a
    # `step_usage` with no `call_seq` gets a position rather than colliding with
    # its siblings on the identity index. Positional order, which does not claim
    # to say which of them was the worker call.
    seen_in_step = {}

    for seq, kind, payload in events:
        pending.append((seq, kind, payload))
        instance = _int(payload.get("turn_instance"))
        if instance is not None:
            current_turn = instance

        if kind == "loop_detected" and payload.get("aborted"):
            outcomes[current_turn] = OUTCOME_LOOP_ABORTED
        elif kind == "error":
            outcomes.setdefault(current_turn, OUTCOME_ERROR)
            derived_turn += 1
        elif kind == "turn_complete":
            outcomes.setdefault(current_turn, OUTCOME_COMPLETE)
            derived_turn += 1

        if kind != "step_usage":
            continue

        turn = instance if instance is not None else derived_turn
        step = _int(payload.get("step"), 0)
        call_index = _int(payload.get("call_seq"))
        if call_index is None:
            call_index = seen_in_step.get((turn, step), 0)
        seen_in_step[(turn, step)] = max(seen_in_step.get((turn, step), 0), call_index) + 1

        windows.append(
            Window(
                turn_instance=turn,
                step=step,
                call_index=call_index,
                seq_start=pending[0][0],
                seq_end=seq,
                usage=payload,
                events=list(pending),
            )
        )
        pending = []
    return windows, outcomes


def price_window(usage, on):
    """`(cost_usd, cost_norm_status)` for one model call.

    The price table `ingest.py` applies to a whole trial, applied to one call's
    own tokens, so a step's cost and `trials.cost_usd_norm` are the same kind of
    number rather than two accountings. `cached_input_tokens` is a subset of
    `input_tokens` on the wire (`stella_protocol::completion::CompletionUsage`),
    which is exactly the shape `normalized_cost_usd` expects.

    Never returns a number without `COST_NORM_PRICED`, and never returns
    `COST_NORM_PRICED` without a number — the rule `ingest.py` states for the
    trial-level column, held at this grain too.
    """
    model = usage.get("model")
    if not isinstance(model, str) or not model.strip():
        return None, COST_NORM_NO_MODEL
    n_input = _int(usage.get("input_tokens"))
    n_cache = _int(usage.get("cached_input_tokens"))
    n_output = _int(usage.get("output_tokens"))
    if n_input is None or n_cache is None or n_output is None:
        return None, COST_NORM_NO_TOKENS
    cost = normalized_cost.normalized_cost_usd(
        model,
        n_input_tokens=n_input,
        n_cache_tokens=n_cache,
        n_output_tokens=n_output,
        on=on,
    )
    if cost is None:
        return None, COST_NORM_UNPRICED_MODEL
    return cost, COST_NORM_PRICED


def _diff_lines(diff):
    """The added and removed lines of a unified diff, as two lists.

    Hunk headers and the `+++`/`---` file headers are skipped; everything else
    is taken verbatim, because a line's identity is what rule 3 matches on.
    """
    added, removed = [], []
    if not isinstance(diff, str):
        return added, removed
    for line in diff.splitlines():
        if line.startswith(("+++", "---", "@@")):
            continue
        if line.startswith("+"):
            added.append(line[1:])
        elif line.startswith("-"):
            removed.append(line[1:])
    return added, removed


def _later_changes_by_path(windows, index):
    """Every `file_change` payload after window `index`, grouped by path."""
    later = {}
    for window in windows[index + 1 :]:
        for change in window.of_type("file_change"):
            path = change.get("path")
            if isinstance(path, str):
                later.setdefault(path, []).append(change)
    return later


def rule_loop_and_retry(window):
    """Rule 1 — the engine's own verdict, and the highest priority because of
    it: nothing this grader derives outranks the engine saying it was stuck."""
    for loop in window.of_type("loop_detected"):
        kind = loop.get("kind")
        kind = kind if isinstance(kind, str) and kind else "unknown"
        return UNPRODUCTIVE, f"loop_detected:{kind}"
    if window.of_type("retries_exhausted"):
        return UNPRODUCTIVE, "retries_exhausted"
    for error in window.of_type("error"):
        if error.get("retryable") is False:
            return UNPRODUCTIVE, "fatal_error"
    return None


def rule_diff_convergence(window, later_by_path):
    """Rule 3, against the substitute for "the final accepted patch" the module
    docstring states: a line survives when no later `file_change` on the same
    path undoes it.

    A revert therefore grades *productive*, and the window that wrote the
    reverted line is the one charged for it. That is the rule pointing at the
    trial's final state rather than at local churn: the final state does not
    carry the line, so removing it converged on the final state.
    """
    convergent = divergent = 0
    touched = False
    for change in window.of_type("file_change"):
        path = change.get("path")
        added, removed = _diff_lines(change.get("diff"))
        if not added and not removed:
            continue
        touched = True
        later_added, later_removed = [], []
        for follow_up in later_by_path.get(path, ()):
            a, r = _diff_lines(follow_up.get("diff"))
            later_added += a
            later_removed += r
        for line in added:
            if line in later_removed:
                divergent += 1
            else:
                convergent += 1
        for line in removed:
            if line in later_added:
                divergent += 1
            else:
                convergent += 1
    if not touched:
        return None
    tally = f"+{convergent}/-{divergent}"
    if convergent > divergent:
        return PRODUCTIVE, f"diff_convergent:{tally}"
    if divergent > convergent:
        return UNPRODUCTIVE, f"diff_divergent:{tally}"
    return NEUTRAL, f"diff_neutral:{tally}"


def _failure_signature(output):
    """`(failed, signature)` for one `tool_result` output.

    Reads the `ToolOutput` arm rather than sniffing the content string: the
    shell tool returns the `error` arm on a nonzero exit, so the arm *is* the
    exit signal. The signature is the failure's first line, which is what makes
    "the same failure again" a comparison rather than an impression.
    """
    error = output.get("error")
    if isinstance(error, dict):
        message = error.get("message")
        if isinstance(message, str) and message.strip():
            return True, message.strip().splitlines()[0]
        return True, ""
    if "ok" in output:
        return False, ""
    return None, ""


def _check_invocations(window, results_by_call):
    """The build/test/lint calls in this window, oldest first, as
    `(command, failed, signature)`."""
    found = []
    for start in window.of_type("tool_start"):
        call = start.get("call")
        if not isinstance(call, dict) or call.get("name") != SHELL_TOOL:
            continue
        args = call.get("input")
        command = args.get("command") if isinstance(args, dict) else None
        if not isinstance(command, str):
            continue
        if not any(shape in command for shape in CHECK_COMMANDS):
            continue
        output = results_by_call.get(call.get("call_id"))
        if not isinstance(output, dict):
            continue
        failed, signature = _failure_signature(output)
        if failed is not None:
            found.append((command, failed, signature))
    return found


def rule_tool_exit(window, results_by_call, last_check):
    """Rule 4 — how a check the agent ran itself moved across this window.

    `last_check` is carried forward through the trial rather than rebuilt per
    window, because the transition is the signal and a single invocation in
    isolation says nothing.
    """
    verdict = None
    for command, failed, signature in _check_invocations(window, results_by_call):
        previous = last_check.get(command)
        last_check[command] = (failed, signature)
        if previous is None:
            continue
        was_failed, was_signature = previous
        if was_failed and not failed:
            verdict = (PRODUCTIVE, "tool_exit:fixed")
        elif not was_failed and failed:
            verdict = (UNPRODUCTIVE, "tool_exit:broke")
        elif was_failed and failed and was_signature == signature:
            verdict = (UNPRODUCTIVE, "tool_exit:repeated_failure")
    return verdict


def _is_no_op(window):
    """Rule 5 — nothing detectable happened.

    The spec's "zero non-read-only `tool_start`" is read here as "no shell
    call": every other built-in that changes the workspace emits a
    `file_change`, and the shell is the one that can change it without
    announcing anything. Reading it that way keeps this rule out of the
    business of maintaining a second copy of the tool catalog in a second
    language.
    """
    if window.of_type("file_change") or window.of_type("stage"):
        return False
    for start in window.of_type("tool_start"):
        call = start.get("call")
        if isinstance(call, dict) and call.get("name") == SHELL_TOOL:
            return False
    return True


def classify(window, later_by_path, results_by_call, last_check):
    """Run the rule chain in priority order and return `(direction, reason)`.

    The first rule whose precondition is met decides and no later rule is
    consulted — but rule 4 is still *evaluated* when an earlier rule fired,
    because it carries the check state forward and dropping one invocation's
    outcome would make the next window's transition wrong.
    """
    settled = rule_loop_and_retry(window)
    diff = rule_diff_convergence(window, later_by_path)
    exit_verdict = rule_tool_exit(window, results_by_call, last_check)

    if settled is not None:
        return settled
    if diff is not None:
        return diff
    if exit_verdict is not None:
        return exit_verdict
    if _is_no_op(window):
        return NEUTRAL, NO_MEASURABLE_EFFECT
    return NEUTRAL, UNCLASSIFIED_FLOOR


def _tool_results(events):
    """`call_id` -> `ToolOutput`, over the whole trial.

    Built once per trial rather than per window, because a call's result can
    land in a later window than its start and rule 4 attributes the check to
    the window that *made* the call.
    """
    results = {}
    for _seq, kind, payload in events:
        if kind != "tool_result":
            continue
        call_id = payload.get("call_id")
        output = payload.get("output")
        if isinstance(call_id, str) and isinstance(output, dict):
            results[call_id] = output
    return results


def _window_span(window, zero):
    """`(ts_start_ms, ts_end_ms, wall_clock_ms)`, price-clocked from `zero`."""
    if zero is None:
        return None, None, None
    first = next((_ts(p) for _s, _k, p in window.events if _ts(p) is not None), None)
    last = _ts(window.usage)
    ts_start = None if first is None else first - zero
    ts_end = None if last is None else last - zero
    wall = None if ts_start is None or ts_end is None else ts_end - ts_start
    return ts_start, ts_end, wall


def grade_trial(conn, trial_id, run_id, started_at, passed, version=GRADER_VERSION):
    """Write every `*_grades` row for one trial. Returns a small report."""
    events, unreadable = load_events(conn, trial_id)
    windows, outcomes = split_windows(events)
    zero = origin_ms(events)
    run_day = parse_timestamp(started_at)
    run_day = run_day.date() if run_day is not None else None
    graded_at = now()
    results_by_call = _tool_results(events)

    last_check = {}
    rows = []
    for index, window in enumerate(windows):
        direction, reason = classify(
            window, _later_changes_by_path(windows, index), results_by_call, last_check
        )
        ts_start, ts_end, wall = _window_span(window, zero)
        cost, cost_status = price_window(window.usage, run_day)
        rows.append(
            (
                f"{trial_id}:{window.turn_instance}:{window.step}:{window.call_index}:{version}",
                trial_id,
                window.turn_instance,
                window.step,
                window.call_index,
                window.seq_start,
                window.seq_end,
                ts_start,
                ts_end,
                wall,
                _int(window.usage.get("duration_ms")),
                cost,
                cost_status,
                len(window.of_type("tool_start")),
                direction,
                reason,
                DETERMINISTIC,
                DETERMINISTIC_CONFIDENCE,
                version,
                graded_at,
            )
        )

    conn.executemany(
        "INSERT OR REPLACE INTO step_grades (step_grade_id,trial_id,turn_instance,step,"
        "call_index,event_seq_start,event_seq_end,ts_start_ms,ts_end_ms,wall_clock_ms,"
        "model_time_ms,cost_usd,cost_norm_status,tool_calls_count,direction,"
        "direction_reason,direction_source,confidence,grader_version,graded_at)"
        " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        rows,
    )

    turn_rows = aggregate_turns(conn, trial_id, windows, outcomes, version, graded_at)
    aggregate_execution(conn, trial_id, run_id, turn_rows, passed, version, graded_at)
    return {
        "windows": len(windows),
        "turns": len(turn_rows),
        "unreadable_payloads": unreadable,
    }


def _sum_or_none(values):
    """Sum, or None when nothing was measurable.

    An absent measurement must never render as a computed zero — the rule
    `trials.cost_norm_status` exists for, applied to time as well.
    """
    present = [value for value in values if value is not None]
    return sum(present) if present else None


def _roll_up_cost(cells):
    """`(cost, status)` for a roll-up over `(cost, status)` pairs.

    `priced` only when every contributing row is priced. A partial sum
    published as a total is the failure this vocabulary exists to prevent, so a
    roll-up over any unpriced row carries that row's reason and no number.
    """
    if not cells:
        return None, COST_NORM_NO_TOKENS
    for cost, status in cells:
        if status != COST_NORM_PRICED or cost is None:
            return None, status
    return sum(cost for cost, _status in cells), COST_NORM_PRICED


def _ratio(productive, total):
    """Section 7's formula, with section 7's NULL rule.

    `None` rather than `0` when nothing was graded: a turn that never ran a step
    and a turn whose every step was unproductive are different facts, and `0.0`
    cannot tell them apart.
    """
    return productive / total if total else None


def aggregate_turns(conn, trial_id, windows, outcomes, version, graded_at):
    """The turn-level roll-up. Returns the rows it wrote."""
    by_turn = {}
    for window in windows:
        by_turn.setdefault(window.turn_instance, []).append(window)

    graded = {}
    for row in conn.execute(
        "SELECT turn_instance, step, call_index, direction, ts_start_ms, ts_end_ms,"
        " model_time_ms, cost_usd, cost_norm_status FROM step_grades"
        " WHERE trial_id = ? AND grader_version = ?",
        (trial_id, version),
    ):
        graded[(row[0], row[1], row[2])] = row[3:]

    rows = []
    for turn in sorted(by_turn):
        cells = [
            graded[(turn, window.step, window.call_index)]
            for window in by_turn[turn]
            if (turn, window.step, window.call_index) in graded
        ]
        directions = [cell[0] for cell in cells]
        productive = directions.count(PRODUCTIVE)
        unproductive = directions.count(UNPRODUCTIVE)
        neutral = directions.count(NEUTRAL)
        total = productive + unproductive + neutral
        cost, cost_status = _roll_up_cost([(cell[4], cell[5]) for cell in cells])
        ts_start = min((cell[1] for cell in cells if cell[1] is not None), default=None)
        ts_end = max((cell[2] for cell in cells if cell[2] is not None), default=None)
        rows.append(
            (
                f"{trial_id}:{turn}:{version}",
                trial_id,
                turn,
                ts_start,
                ts_end,
                None if ts_start is None or ts_end is None else ts_end - ts_start,
                _sum_or_none([cell[3] for cell in cells]),
                cost,
                cost_status,
                total,
                productive,
                unproductive,
                neutral,
                _ratio(productive, total),
                outcomes.get(turn, OUTCOME_IN_PROGRESS),
                version,
                graded_at,
            )
        )

    conn.executemany(
        "INSERT OR REPLACE INTO turn_grades (turn_grade_id,trial_id,turn_instance,"
        "ts_start_ms,ts_end_ms,wall_clock_ms,model_time_ms,cost_usd,cost_norm_status,"
        "step_count,productive_steps,unproductive_steps,neutral_steps,"
        "productive_step_ratio,outcome,grader_version,graded_at)"
        " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        rows,
    )
    return rows


def verdict_alignment(ratio, passed):
    """The sanity cross-check, never a scoring input.

    NULL when either half is unknown, because "we did not grade it" and "the
    grade disagreed with the verdict" are not the same reading.
    """
    if ratio is None or passed is None:
        return None
    high = ratio >= VERDICT_ALIGNMENT_RATIO_FLOOR
    return CONSISTENT if high == bool(passed) else DISCORDANT


def aggregate_execution(conn, trial_id, run_id, turn_rows, passed, version, graded_at):
    """The execution-level roll-up — the agent productive step ratio.

    Summed over the turns rather than recomputed from `step_grades`, so the
    identity section 7 pins — the execution ratio equals the `step_count`-
    weighted mean of its turns' ratios — holds by construction instead of by
    coincidence.
    """
    productive = sum(row[_TURN_PRODUCTIVE] for row in turn_rows)
    unproductive = sum(row[_TURN_UNPRODUCTIVE] for row in turn_rows)
    neutral = sum(row[_TURN_NEUTRAL] for row in turn_rows)
    total = productive + unproductive + neutral
    ts_start = min((row[_TURN_TS_START] for row in turn_rows if row[_TURN_TS_START] is not None), default=None)
    ts_end = max((row[_TURN_TS_END] for row in turn_rows if row[_TURN_TS_END] is not None), default=None)
    cost, cost_status = _roll_up_cost(
        [(row[_TURN_COST], row[_TURN_COST_STATUS]) for row in turn_rows]
    )
    ratio = _ratio(productive, total)

    conn.execute(
        "INSERT OR REPLACE INTO execution_grades (execution_grade_id,trial_id,run_id,"
        "ts_start_ms,ts_end_ms,wall_clock_ms,model_time_ms,cost_usd,cost_norm_status,"
        "turn_count,step_count,productive_steps,unproductive_steps,neutral_steps,"
        "agent_productive_step_ratio,verdict_alignment,grader_version,graded_at,notes)"
        " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            f"{trial_id}:{version}",
            trial_id,
            run_id,
            ts_start,
            ts_end,
            None if ts_start is None or ts_end is None else ts_end - ts_start,
            _sum_or_none([row[_TURN_MODEL_MS] for row in turn_rows]),
            cost,
            cost_status,
            len(turn_rows),
            total,
            productive,
            unproductive,
            neutral,
            ratio,
            verdict_alignment(ratio, passed),
            version,
            graded_at,
            None,
        ),
    )


def grade(conn, run=None, trial=None, version=GRADER_VERSION):
    """Grade every selected trial. Returns the per-trial reports."""
    query = "SELECT trial_id, run_id, started_at, passed FROM trials"
    clauses, params = [], []
    if run:
        clauses.append("run_id = ?")
        params.append(run)
    if trial:
        clauses.append("trial_id = ?")
        params.append(trial)
    if clauses:
        query += " WHERE " + " AND ".join(clauses)
    reports = {}
    for trial_id, run_id, started_at, passed in conn.execute(
        query + " ORDER BY trial_id", params
    ):
        reports[trial_id] = grade_trial(conn, trial_id, run_id, started_at, passed, version)
    return reports


def main() -> int:
    ap = argparse.ArgumentParser(description="Grade ingested Terminal-Bench trials.")
    ap.add_argument("--db", required=True)
    ap.add_argument("--run", default=None, help="grade only this run tag")
    ap.add_argument("--trial", default=None, help="grade only this trial id")
    ap.add_argument("--grader-version", default=GRADER_VERSION)
    a = ap.parse_args()

    conn = connect(a.db)
    reports = grade(conn, run=a.run, trial=a.trial, version=a.grader_version)
    conn.commit()

    windows = sum(report["windows"] for report in reports.values())
    unreadable = sum(report["unreadable_payloads"] for report in reports.values())
    print(f"{a.grader_version}: {len(reports)} trial(s), {windows} step window(s)")
    if unreadable:
        # Said out loud rather than left as a hole in a published ratio: these
        # are events this grader could not read at all.
        print(
            f"{a.grader_version}: {unreadable} event payload(s) did not parse"
            f" (truncated at ingest, {TRUNCATION_GAP})"
        )
    if not reports:
        print("no trials matched — nothing was graded")
    return 0


if __name__ == "__main__":
    sys.exit(main())
