# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Reading each arm out of the stream it was already writing.

The bias, as in :mod:`tests.test_telemetry`, is toward the paths where a bug
yields a *plausible* number: the message-id deduplication (a naive sum
overstates output tokens by 1.76x on measured data), the incremental cursor,
the token convention the archive's historical cache figures were computed
under, and — since #2519 and #2520 — the two ways an arm can be described in
the wrong vocabulary or credited from the wrong source.

No Docker, no network, no key: every fixture is a synthetic stream.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
from pathlib import Path

from arenabench.harness import (
    CLAUDE_CODE,
    DIALECTS,
    STELLA,
    STELLA_EVENTS_NAME,
    STREAM_NAME,
    USAGE_STREAM,
    USAGE_TRANSCRIPT,
    USAGE_UNOBSERVED,
    HarnessTotals,
    StreamReader,
    reduce_stream,
    summarize,
)
from arenabench.telemetry import MetricsReader

# --------------------------------------------------------------------------
# fixtures
# --------------------------------------------------------------------------


def init_event(**kwargs) -> dict:
    base = {
        "type": "system",
        "subtype": "init",
        "session_id": "s-1",
        "claude_code_version": "2.1.226",
        "model": "claude-opus-5",
        "permissionMode": "bypassPermissions",
        "apiKeySource": "ANTHROPIC_API_KEY",
        "output_style": "default",
        "cwd": "/app",
        "tools": ["Bash", "Read", "Edit"],
        "mcp_servers": [],
        "skills": ["debug"],
        "agents": ["Explore"],
        "slash_commands": ["/init", "/review"],
    }
    base.update(kwargs)
    return base


def assistant(
    message_id: str,
    *,
    blocks: list[dict] | None = None,
    usage: dict | None = None,
    timestamp: str = "2026-08-09T00:00:00.000Z",
    model: str = "claude-opus-5",
) -> dict:
    return {
        "type": "assistant",
        "timestamp": timestamp,
        "message": {
            "id": message_id,
            "role": "assistant",
            "model": model,
            "content": blocks if blocks is not None else [{"type": "text", "text": "hi"}],
            "usage": usage
            if usage is not None
            else {
                "input_tokens": 2,
                "output_tokens": 6,
                "cache_read_input_tokens": 100,
                "cache_creation_input_tokens": 10,
            },
        },
    }


def tool_result(call_id: str, *, timestamp: str, is_error: bool = False) -> dict:
    return {
        "type": "user",
        "timestamp": timestamp,
        "message": {
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": call_id, "is_error": is_error}
            ],
        },
    }


def result_event(**kwargs) -> dict:
    base = {
        "type": "result",
        "subtype": "success",
        "total_cost_usd": 0.6929,
        "num_turns": 25,
        "duration_ms": 447305,
        "duration_api_ms": 180235,
        "stop_reason": "end_turn",
        "terminal_reason": "completed",
        "is_error": False,
        "permission_denials": [],
    }
    base.update(kwargs)
    return base


def write_stream(path: Path, events: list[dict], *, tail: str = "") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        for event in events:
            handle.write(json.dumps(event) + "\n")
        if tail:
            handle.write(tail)


# --------------------------------------------------------------------------
# the projection bugs
# --------------------------------------------------------------------------


def test_one_message_emitted_per_block_is_counted_once(tmp_path: Path) -> None:
    """The stream re-emits a message per content block; the fold must not.

    Measured on a real trial: 48 assistant events for 20 distinct message ids,
    so summing events reports 208 output tokens where the truth is 118 — 1.76x
    — and 48 steps where the truth is 20. Neither number looks wrong, which is
    exactly why it needs a test rather than a reviewer.
    """
    path = tmp_path / STREAM_NAME
    usage = {
        "input_tokens": 2,
        "output_tokens": 6,
        "cache_read_input_tokens": 100,
        "cache_creation_input_tokens": 10,
    }
    write_stream(path, [init_event(), *[assistant("msg_a", usage=usage)] * 3])

    result = reduce_stream(path)
    assert result is not None
    _, totals = result
    assert totals.steps == 1
    assert totals.tokens_out == 6
    assert totals.tokens_in == 2
    assert totals.cache_read == 100
    assert totals.cache_write == 10


def test_a_revised_usage_credits_only_the_difference(tmp_path: Path) -> None:
    """Usage that grows across re-emissions of one message is not double-counted.

    The CLI accumulates usage as a turn streams, so the same message id can
    arrive several times with rising numbers. Crediting the delta is what makes
    an *incremental* reader agree with a one-shot one — "keep the last value"
    would need the whole file on every poll.
    """
    path = tmp_path / STREAM_NAME
    write_stream(
        path,
        [
            init_event(),
            assistant("msg_a", usage={"input_tokens": 1, "output_tokens": 4}),
            assistant("msg_a", usage={"input_tokens": 1, "output_tokens": 40}),
            assistant("msg_a", usage={"input_tokens": 1, "output_tokens": 400}),
        ],
    )
    result = reduce_stream(path)
    assert result is not None
    _, totals = result
    assert totals.tokens_out == 400
    assert totals.tokens_in == 1
    assert totals.steps == 1


def test_a_shrinking_usage_never_subtracts(tmp_path: Path) -> None:
    """A provider correcting itself downward must not un-credit real spend."""
    path = tmp_path / STREAM_NAME
    write_stream(
        path,
        [
            init_event(),
            assistant("msg_a", usage={"input_tokens": 1, "output_tokens": 400}),
            assistant("msg_a", usage={"input_tokens": 1, "output_tokens": 4}),
        ],
    )
    result = reduce_stream(path)
    assert result is not None
    assert result[1].tokens_out == 400


def test_prompt_tokens_keeps_atifs_convention(tmp_path: Path) -> None:
    """`prompt_tokens` folds both cache legs in; the wire counters stay apart.

    Every Claude Code cache-hit figure in the archive was computed with the
    cache legs folded into the input side, so the scoreboard keeps that
    convention. Losing the split would make the rate a ratio of a number to
    itself plus other things.
    """
    totals = HarnessTotals(tokens_in=37, cache_read=551_258, cache_write=18_006)
    assert totals.prompt_tokens == 569_301
    assert totals.tokens_in == 37


# --------------------------------------------------------------------------
# the incremental cursor
# --------------------------------------------------------------------------


def test_a_torn_final_line_is_held_not_dropped(tmp_path: Path) -> None:
    """A half-written event counts once, when the rest of it lands.

    The file is appended to while it is read, so a torn tail is the normal
    case. The cursor has already moved past those bytes, which is what makes
    dropping them permanent rather than merely late.
    """
    path = tmp_path / STREAM_NAME
    reader = StreamReader()
    partial = json.dumps(assistant("msg_a", usage={"output_tokens": 9}))
    write_stream(path, [init_event()], tail=partial[:40])

    first = reader.read(path)
    assert first is not None and first[1].tokens_out == 0

    with path.open("a", encoding="utf-8") as handle:
        handle.write(partial[40:] + "\n")

    second = reader.read(path)
    assert second is not None and second[1].tokens_out == 9


def test_incremental_drain_matches_a_single_pass(tmp_path: Path) -> None:
    """Reading in pieces and reading at once must agree.

    The whole reason for a cursor is cost, and a cursor that changes the answer
    is worse than the cost it saves.
    """
    path = tmp_path / STREAM_NAME
    reader = StreamReader()
    batches = [
        [init_event()],
        [
            assistant(
                "msg_a",
                blocks=[{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}],
                timestamp="2026-08-09T00:00:00.000Z",
            )
        ],
        [tool_result("t1", timestamp="2026-08-09T00:00:02.500Z")],
        [assistant("msg_b", usage={"output_tokens": 11})],
        [result_event()],
    ]
    for batch in batches:
        write_stream(path, batch)
        reader.read(path)
    incremental = reader.read(path)

    oneshot = reduce_stream(path)
    assert incremental is not None and oneshot is not None
    assert incremental[1].to_json() == oneshot[1].to_json()
    assert incremental[0].to_json() == oneshot[0].to_json()


def test_a_replaced_file_is_read_from_the_start(tmp_path: Path) -> None:
    """A rerun into the same directory resets rather than continues."""
    path = tmp_path / STREAM_NAME
    reader = StreamReader()
    write_stream(path, [init_event(), assistant("msg_a", usage={"output_tokens": 50})])
    assert reader.read(path)[1].tokens_out == 50

    path.write_text("", encoding="utf-8")
    write_stream(path, [init_event(), assistant("msg_z", usage={"output_tokens": 7})])
    assert reader.read(path)[1].tokens_out == 7


def test_the_reasoning_estimate_is_the_last_one_not_their_sum(tmp_path: Path) -> None:
    """`thinking_tokens` is cumulative and self-superseding.

    99% of a real stream's lines by count are these, which is why the drain
    recognises them by substring instead of parsing them — and why summing
    them would report a number tens of thousands of times too large.
    """
    path = tmp_path / STREAM_NAME
    write_stream(
        path,
        [init_event()]
        + [
            {
                "type": "system",
                "subtype": "thinking_tokens",
                "estimated_tokens": n,
                "estimated_tokens_delta": 100,
            }
            for n in (100, 200, 4619)
        ],
    )
    assert reduce_stream(path)[1].thinking_tokens == 4619


# --------------------------------------------------------------------------
# behaviour
# --------------------------------------------------------------------------


def test_a_tool_gets_its_wall_clock_from_the_join(tmp_path: Path) -> None:
    """Tool duration is the gap between the request and the result answering it."""
    path = tmp_path / STREAM_NAME
    write_stream(
        path,
        [
            init_event(),
            assistant(
                "msg_a",
                blocks=[
                    {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}},
                    {"type": "tool_use", "id": "t2", "name": "Read", "input": {}},
                ],
                timestamp="2026-08-09T00:00:00.000Z",
            ),
            tool_result("t1", timestamp="2026-08-09T00:00:02.500Z"),
            tool_result("t2", timestamp="2026-08-09T00:00:00.960Z", is_error=True),
        ],
    )
    _, totals = reduce_stream(path)
    assert totals.tools == 2
    assert totals.tool_calls == {"Bash": 1, "Read": 1}
    assert totals.tool_ms == {"Bash": 2500, "Read": 960}
    assert totals.tool_wall_ms == 3460
    assert totals.tool_errors == 1


def test_a_tool_that_never_returned_counts_but_has_no_duration(tmp_path: Path) -> None:
    """A trial killed mid-tool reports the call, not an invented elapsed time."""
    path = tmp_path / STREAM_NAME
    write_stream(
        path,
        [
            init_event(),
            assistant(
                "msg_a",
                blocks=[{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}],
            ),
        ],
    )
    _, totals = reduce_stream(path)
    assert totals.tool_calls == {"Bash": 1}
    assert totals.tool_ms == {}


def test_the_harness_discloses_its_own_configuration(tmp_path: Path) -> None:
    """The boot event is the evidence behind "both arms ran the same thing"."""
    path = tmp_path / STREAM_NAME
    write_stream(path, [init_event()])
    profile, _ = reduce_stream(path)
    assert profile.version == "2.1.226"
    assert profile.model == "claude-opus-5"
    assert profile.permission_mode == "bypassPermissions"
    assert profile.api_key_source == "ANTHROPIC_API_KEY"
    assert profile.tools == ("Bash", "Read", "Edit")
    assert profile.skills == ("debug",)
    assert profile.subagents == ("Explore",)
    assert profile.slash_commands == 2


def test_a_roster_of_objects_reads_the_same_as_a_roster_of_names(tmp_path: Path) -> None:
    """The CLI has spelled these three ways; all three must read alike."""
    path = tmp_path / STREAM_NAME
    write_stream(
        path,
        [
            init_event(
                tools=[{"name": "Bash"}, {"name": "Read"}],
                mcp_servers={"github": {}, "linear": {}},
            )
        ],
    )
    profile, _ = reduce_stream(path)
    assert profile.tools == ("Bash", "Read")
    assert profile.mcp_servers == ("github", "linear")


def test_the_final_verdict_carries_the_api_error(tmp_path: Path) -> None:
    """A swallowed 401/429 is the shape that scored real trials as losses.

    Harbor names such a trial `NonZeroAgentExitCodeError` — an *agent* failure
    — so the credential guard never sees it and the scoreboard reads "lost the
    task" when the agent never completed a call (#1480). The harness said so
    itself, in the file, all along.
    """
    path = tmp_path / STREAM_NAME
    write_stream(
        path,
        [
            init_event(apiKeySource="none"),
            result_event(
                api_error_status="403",
                terminal_reason="api_error",
                is_error=True,
                total_cost_usd=0.0,
                num_turns=1,
            ),
        ],
    )
    profile, totals = reduce_stream(path)
    assert profile.api_key_source == "none"
    assert totals.api_error == "403"
    assert totals.terminal_reason == "api_error"
    assert totals.is_error is True
    assert totals.complete is True


def test_no_stream_reads_as_absence_not_as_zero(tmp_path: Path) -> None:
    """Every non-Claude-Code arm has no such file, and that is not a measurement."""
    assert reduce_stream(tmp_path / STREAM_NAME) is None


# --------------------------------------------------------------------------
# the witness: a mid-run arm is no longer dark
# --------------------------------------------------------------------------


def test_a_claude_code_trial_reports_its_metrics_before_it_finishes(
    tmp_path: Path,
) -> None:
    """**The witness.** A running Claude Code trial is measured, not blank.

    Fails on the code this replaced: ATIF is written once in
    `populate_context_post_run`, so a trial with no `trajectory.json` and no
    `result.json` yet — i.e. every Claude Code arm for the whole length of a
    match — reported 0 steps, 0 tools, 0 tokens and $0 while the agent worked.
    `_liveness_age` made it *look* alive off file mtimes; nothing measured it.
    """
    trial = tmp_path / "sqlite-with-gcov__abc"
    write_stream(
        trial / STREAM_NAME,
        [
            init_event(),
            assistant(
                "msg_a",
                blocks=[{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}],
                usage={
                    "input_tokens": 37,
                    "output_tokens": 118,
                    "cache_read_input_tokens": 551_258,
                    "cache_creation_input_tokens": 18_006,
                },
                timestamp="2026-08-09T00:00:00.000Z",
            ),
            tool_result("t1", timestamp="2026-08-09T00:00:02.500Z"),
        ],
    )
    # Deliberately no trajectory.json and no result.json: this is what the
    # directory looks like while the agent is still working.
    assert not (trial / "trajectory.json").exists()

    metrics = MetricsReader().read(trial, task="sqlite-with-gcov")

    assert metrics.status == "running"
    assert metrics.steps == 1
    assert metrics.tools == 1
    assert metrics.tokens_out == 118
    assert metrics.tokens_in == 569_301  # ATIF's convention: input + both cache legs
    assert metrics.cache_read == 551_258
    assert metrics.usage_measured is True
    assert metrics.harness is not None
    assert metrics.harness.version == "2.1.226"
    assert metrics.behaviour is not None
    assert metrics.behaviour.tool_calls == {"Bash": 1}
    assert metrics.behaviour.tool_ms == {"Bash": 2500}


def test_a_stella_trial_is_untouched_by_the_harness_reader(tmp_path: Path) -> None:
    """A Stella arm writes no such stream, so nothing about it changes."""
    trial = tmp_path / "sqlite-with-gcov__def"
    events = trial / "agent" / "stella-events.jsonl"
    events.parent.mkdir(parents=True, exist_ok=True)
    events.write_text(
        json.dumps(
            {
                "type": "step_usage",
                "model": "anthropic/claude-opus-5",
                "input_tokens": 500,
                "output_tokens": 40,
            }
        )
        + "\n",
        encoding="utf-8",
    )
    metrics = MetricsReader().read(trial, task="sqlite-with-gcov")
    assert metrics.harness is None
    assert metrics.behaviour is None
    assert metrics.tokens_in == 500
    assert metrics.tokens_out == 40


# --------------------------------------------------------------------------
# the operator's view
# --------------------------------------------------------------------------


def test_the_harness_command_reports_an_arm(tmp_path: Path, capsys) -> None:
    """`arenabench harness <match>` prints the evidence, not the claim."""
    from arenabench.cli import _cmd_harness

    trial = tmp_path / "matches" / "m1" / "jobs" / "m1-claude-code" / "t__a"
    write_stream(
        trial / STREAM_NAME,
        [
            init_event(),
            assistant(
                "msg_a",
                blocks=[{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}],
                usage={"input_tokens": 5, "output_tokens": 9},
                timestamp="2026-08-09T00:00:00.000Z",
            ),
            tool_result("t1", timestamp="2026-08-09T00:00:01.000Z"),
            result_event(),
        ],
    )
    args = argparse.Namespace(workspace=str(tmp_path), match="m1")
    assert _cmd_harness(args) == 0
    out = capsys.readouterr().out
    assert "2.1.226" in out
    assert "Bashx1" in out
    assert "bypassPermissions" in out


def test_the_harness_command_says_so_when_no_arm_streams(
    tmp_path: Path, capsys
) -> None:
    """A Stella-only match is not an error; it just has no such stream."""
    from arenabench.cli import _cmd_harness

    (tmp_path / "matches" / "m1" / "jobs" / "m1-stella").mkdir(parents=True)
    args = argparse.Namespace(workspace=str(tmp_path), match="m1")
    assert _cmd_harness(args) == 0
    assert "no arm published a harness stream" in capsys.readouterr().out


def test_zeroed_stream_usage_is_disclosed_not_printed_as_free(
    tmp_path: Path, capsys
) -> None:
    """A gateway-routed seat streams zeros; that is not "this trial was free".

    Printing `0 tokens` beside a nonzero self-reported cost reads as a
    contradiction at best and as an efficiency claim at worst — and the seats
    it happens to are exactly the ones a cost comparison is about.
    """
    from arenabench.cli import _cmd_harness

    trial = tmp_path / "matches" / "m1" / "jobs" / "m1-claude-code" / "t__a"
    write_stream(
        trial / STREAM_NAME,
        [
            init_event(model="glm-5.2"),
            assistant("msg_a", usage={"input_tokens": 0, "output_tokens": 0}),
            result_event(total_cost_usd=3.02),
        ],
    )
    args = argparse.Namespace(workspace=str(tmp_path), match="m1")
    assert _cmd_harness(args) == 0
    out = capsys.readouterr().out
    assert "stream reported no usage" in out
    assert "3.0200" in out


# --------------------------------------------------------------------------
# the Stella arm (#2519)
# --------------------------------------------------------------------------


def stella_tool_start(call_id: str, name: str, *, ts: float) -> dict:
    return {
        "type": "tool_start",
        "ts": ts,
        "call": {"call_id": call_id, "name": name, "input": {"command": "ls"}},
    }


def stella_tool_result(
    call_id: str, *, ts: float, duration_ms: int | None = None, error: str = ""
) -> dict:
    # `ToolOutput` is externally tagged and carries no `is_error` field: the
    # tag IS the verdict, which is the trap `arenabench.toolout` exists for.
    output = {"error": {"message": error}} if error else {"ok": {"content": "ok"}}
    event: dict = {"type": "tool_result", "ts": ts, "call_id": call_id, "output": output}
    if duration_ms is not None:
        event["duration_ms"] = duration_ms
    return event


def stella_step_usage(**kwargs) -> dict:
    base = {
        "type": "step_usage",
        "ts": 1_000.0,
        "step": 0,
        "role": "worker",
        "model": "anthropic/claude-opus-5",
        "input_tokens": 120,
        "output_tokens": 40,
        "cached_input_tokens": 900,
        "cache_write_tokens": 30,
        "cost_usd": 0.0125,
        "duration_ms": 3100,
    }
    base.update(kwargs)
    return base


def stella_stream() -> list[dict]:
    """One small Stella trial: two tools, one of them failed, two model calls."""
    return [
        {"type": "stage", "name": "execute"},
        stella_step_usage(),
        stella_tool_start("c1", "bash", ts=1_000.0),
        stella_tool_result("c1", ts=3_500.0, duration_ms=2_500),
        stella_tool_start("c2", "read_file", ts=3_600.0),
        stella_tool_result("c2", ts=4_560.0, duration_ms=960, error="no such file"),
        stella_step_usage(step=1, role="verifier", model="openai/gpt-5", cost_usd=0.004),
        {"type": "complete", "model": "anthropic/claude-opus-5", "cost_usd": 0.0165},
    ]


def test_a_stella_stream_folds_into_the_shared_vocabulary(tmp_path: Path) -> None:
    """**The witness for #2519.** A Stella arm reduces to `HarnessTotals`.

    Fails on the code this replaces: `harness.py` could fold exactly one
    spelling — Claude Code's stream-json tee — so a Stella trial produced no
    reading at all and the one command whose whole purpose is "compare the two
    harnesses" could describe one of them.
    """
    path = tmp_path / STELLA_EVENTS_NAME
    write_stream(path, stella_stream())

    result = reduce_stream(path, STELLA)
    assert result is not None
    _, totals = result
    assert totals.tools == 2
    assert totals.tool_calls == {"bash": 1, "read_file": 1}
    assert totals.tool_ms == {"bash": 2500, "read_file": 960}
    assert totals.tool_wall_ms == 3460
    assert totals.tool_errors == 1
    assert totals.steps == 2
    assert totals.tokens_in == 240
    assert totals.tokens_out == 80
    assert totals.cache_read == 1800
    assert totals.cache_write == 60
    assert totals.models == ("anthropic/claude-opus-5", "openai/gpt-5")
    assert totals.total_cost == 0.0165
    assert totals.complete is True
    assert totals.usage_source == USAGE_STREAM


def test_a_stella_tool_prefers_the_engines_own_duration(tmp_path: Path) -> None:
    """`duration_ms` is a measurement; the timestamp gap is only a fallback.

    The gap between two journal stamps also charges the tool for the time its
    result spent being written. The engine measured the call itself, so its
    figure wins — and a stream that carries no duration still gets the join,
    rather than losing the call's wall clock entirely.
    """
    path = tmp_path / STELLA_EVENTS_NAME
    write_stream(
        path,
        [
            stella_tool_start("c1", "bash", ts=1_000.0),
            stella_tool_result("c1", ts=9_000.0, duration_ms=2_500),
            stella_tool_start("c2", "grep", ts=10_000.0),
            stella_tool_result("c2", ts=10_400.0),
        ],
    )
    _, totals = reduce_stream(path, STELLA)
    assert totals.tool_ms == {"bash": 2500, "grep": 400}


def test_discarded_speculation_counts_its_call_and_invents_no_duration(
    tmp_path: Path,
) -> None:
    """A speculative call whose answer was thrown away still really ran.

    It is not an error — the tool worked, the model never saw the result — and
    it has no observed duration, so neither is fabricated.
    """
    path = tmp_path / STELLA_EVENTS_NAME
    write_stream(
        path,
        [
            stella_tool_start("c1", "read_file", ts=1_000.0),
            {
                "type": "speculation_discarded",
                "ts": 1_200.0,
                "call_id": "c1",
                "name": "read_file",
                "reason": "harvest_mismatch",
            },
        ],
    )
    _, totals = reduce_stream(path, STELLA)
    assert totals.tool_calls == {"read_file": 1}
    assert totals.tool_ms == {}
    assert totals.tool_errors == 0


def test_a_stella_stream_reads_incrementally_like_a_single_pass(tmp_path: Path) -> None:
    """The cursor is shared machinery, so it must hold for the new arm too."""
    path = tmp_path / STELLA_EVENTS_NAME
    reader = StreamReader()
    for event in stella_stream():
        write_stream(path, [event])
        reader.read(path, STELLA)
    incremental = reader.read(path, STELLA)
    oneshot = reduce_stream(path, STELLA)
    assert incremental is not None and oneshot is not None
    assert incremental[1].to_json() == oneshot[1].to_json()


def test_stella_publishes_no_boot_disclosure_rather_than_an_invented_one(
    tmp_path: Path,
) -> None:
    """The profile stays empty: Stella's stream has no boot event to read.

    Back-filling `model` from the first `step_usage` would report a pipeline
    role's model as the seat's, and back-filling `tools` from the calls the run
    happened to make would report "used" as "offered". Both are the sort of
    plausible wrong number this module refuses.
    """
    path = tmp_path / STELLA_EVENTS_NAME
    write_stream(path, stella_stream())
    profile, _ = reduce_stream(path, STELLA)
    assert STELLA.boot_disclosure is False
    assert profile.model == ""
    assert profile.tools == ()


def test_every_dialect_declares_only_fields_it_leaves_alone() -> None:
    """`unpublished` is a claim about the fold, and it is checked against it.

    Two halves, because either alone can rot: every name must be a real
    `HarnessTotals` field (so a rename cannot strand an entry), and folding a
    stream that exercises the dialect must leave each of those fields at its
    default (so a dialect cannot declare a column absent while quietly filling
    it in).
    """
    fields = {f.name: f for f in dataclasses.fields(HarnessTotals)}
    streams = {CLAUDE_CODE.arm: [], STELLA.arm: stella_stream()}
    for dialect in DIALECTS:
        fold = dialect.new_fold()
        for event in streams[dialect.arm]:
            fold.consume([json.dumps(event).encode()])
        for name in dialect.unpublished:
            assert name in fields, f"{dialect.arm} declares unknown field {name}"
            assert getattr(fold.totals, name) == fields[name].default


def test_a_seat_summary_unions_models_and_never_sums_a_boolean(tmp_path: Path) -> None:
    """One seat's trials fold to one block: counters add, model ids union."""
    reader = StreamReader()
    readings = []
    for index in (1, 2):
        trial = tmp_path / f"t{index}"
        write_stream(trial / STELLA_EVENTS_NAME, stella_stream())
        readings.append(reader.read_arm(trial, STELLA))
    seat = summarize(readings)
    assert seat is not None
    assert seat.trials == 2
    assert seat.totals["tools"] == 4
    assert seat.totals["tool_calls"] == {"bash": 2, "read_file": 2}
    assert seat.totals["models"] == ["anthropic/claude-opus-5", "openai/gpt-5"]
    assert "complete" not in seat.totals
    assert seat.usage_sources == (USAGE_STREAM,)
    assert summarize([]) is None


def test_the_harness_command_reports_both_arms(tmp_path: Path, capsys) -> None:
    """**The witness for #2519, at the operator's surface.**

    A match with one Stella job and one Claude Code job prints two blocks in
    one layout, with comparable `tool_calls` and `tool_ms`. Before this the
    same match printed "no arm published a harness stream" for the Stella half
    — the comparison command describing one side of a comparison.
    """
    from arenabench.cli import _cmd_harness

    jobs = tmp_path / "matches" / "m1" / "jobs"
    write_stream(
        jobs / "m1-claude-code" / "t__a" / STREAM_NAME,
        [
            init_event(),
            assistant(
                "msg_a",
                blocks=[{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}],
                usage={"input_tokens": 5, "output_tokens": 9},
                timestamp="2026-08-09T00:00:00.000Z",
            ),
            tool_result("t1", timestamp="2026-08-09T00:00:01.000Z"),
            result_event(),
        ],
    )
    write_stream(jobs / "m1-stella" / "t__a" / STELLA_EVENTS_NAME, stella_stream())

    args = argparse.Namespace(workspace=str(tmp_path), match="m1")
    assert _cmd_harness(args) == 0
    out = capsys.readouterr().out

    assert "m1-claude-code" in out and "m1-stella" in out
    # Comparable behaviour, in one vocabulary: both arms report their mix and
    # their per-tool wall clock.
    assert "Bashx1 (1s)" in out
    assert "bashx1 (2s)" in out
    assert "read_filex1 (1s)" in out
    # …and the report says where the vocabularies genuinely differ, so nobody
    # puts the two step counts in one column.
    assert "one assistant message per model turn" in out
    assert "one model call per pipeline role" in out
    # Stella's stream carries no boot event and no reasoning estimate; both are
    # disclosed as absent rather than printed as zero.
    assert "not disclosed on this arm's stream" in out
    assert "thinking_tokens" in out
    assert "0 reasoning (est.)" not in out.split("m1-stella")[1]


# --------------------------------------------------------------------------
# a gateway-routed seat's tokens (#2520)
# --------------------------------------------------------------------------

#: Where Claude Code appends its own session transcript inside a trial, which
#: is where the tokens are while the stdout stream reports zeros. Harbor points
#: `CLAUDE_CONFIG_DIR` at `/logs/agent/sessions`.
TRANSCRIPT_NAME = "agent/sessions/projects/-app-repo/e1b7.jsonl"


def zeroed_stream() -> list[dict]:
    """What a seat routed through `ANTHROPIC_BASE_URL` to a gateway streams.

    Measured on a real archived trial: 87 assistant messages carrying usage, of
    which zero had a nonzero counter.
    """
    return [
        init_event(model="glm-5.2"),
        assistant(
            "msg_a",
            blocks=[{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}],
            usage={"input_tokens": 0, "output_tokens": 0},
            timestamp="2026-08-09T00:00:00.000Z",
        ),
        tool_result("t1", timestamp="2026-08-09T00:00:02.000Z"),
        assistant("msg_b", usage={"input_tokens": 0, "output_tokens": 0}),
    ]


def transcript_lines() -> list[dict]:
    """The same two messages as the CLI's own session file records them.

    One line per content block, each carrying the message's *accumulated*
    usage — the same shape as the stdout stream, and the same over-counting
    trap: a naive sum here reports 1.76x the output tokens.
    """
    return [
        {"type": "summary", "summary": "make mips interpreter"},
        assistant("msg_a", usage={"input_tokens": 11, "output_tokens": 40}),
        assistant(
            "msg_a",
            usage={
                "input_tokens": 11,
                "output_tokens": 118,
                "cache_read_input_tokens": 551_258,
                "cache_creation_input_tokens": 18_006,
            },
        ),
        assistant("msg_b", usage={"input_tokens": 26, "output_tokens": 7}),
    ]


def test_a_gateway_routed_seat_reads_its_tokens_from_the_transcript(
    tmp_path: Path,
) -> None:
    """**The witness for #2520.** Live tokens for a seat the stream zeroes.

    Fails on the code this replaces: the stdout stream was the only source, so
    a gateway-routed arm's token and cache columns stayed dark until the ATIF
    trajectory landed at teardown — deliberately absent here, which is what a
    trial directory looks like while the agent is still working.
    """
    trial = tmp_path / "make-mips-interpreter__t9J"
    write_stream(trial / STREAM_NAME, zeroed_stream())
    write_stream(trial / TRANSCRIPT_NAME, transcript_lines())
    assert not (trial / "agent" / "trajectory.json").exists()

    metrics = MetricsReader().read(trial, task="make-mips-interpreter")

    assert metrics.behaviour is not None
    assert metrics.behaviour.usage_source == USAGE_TRANSCRIPT
    # Credited per message id, not summed per line: 118 + 7, never 165.
    assert metrics.behaviour.tokens_out == 125
    assert metrics.behaviour.tokens_in == 37
    assert metrics.behaviour.cache_read == 551_258
    assert metrics.behaviour.cache_write == 18_006
    assert metrics.tokens_out == 125
    assert metrics.tokens_in == 569_301  # ATIF's convention: input + both legs
    assert metrics.usage_measured is True
    # The behaviour the stdout stream already carried is unchanged by the
    # fallback — the transcript is consulted for the counters and nothing else.
    assert metrics.steps == 2
    assert metrics.tools == 1
    assert metrics.behaviour.tool_ms == {"Bash": 2000}


def test_the_transcript_is_a_fallback_and_never_an_addend(tmp_path: Path) -> None:
    """Both sources describe the same messages, so a sum would double them."""
    trial = tmp_path / "t__a"
    write_stream(
        trial / STREAM_NAME,
        [init_event(), assistant("msg_a", usage={"input_tokens": 3, "output_tokens": 9})],
    )
    write_stream(
        trial / TRANSCRIPT_NAME,
        [assistant("msg_a", usage={"input_tokens": 3, "output_tokens": 9})],
    )
    reading = StreamReader().read_arm(trial, CLAUDE_CODE)
    assert reading is not None
    assert reading.totals.usage_source == USAGE_STREAM
    assert reading.totals.tokens_out == 9


def test_a_transcript_read_in_pieces_credits_each_message_once(tmp_path: Path) -> None:
    """The fallback is incremental too: the transcript is stream-sized."""
    trial = tmp_path / "t__a"
    write_stream(trial / STREAM_NAME, zeroed_stream())
    reader = StreamReader()
    for line in transcript_lines():
        write_stream(trial / TRANSCRIPT_NAME, [line])
        reader.read_arm(trial, CLAUDE_CODE)
    incremental = reader.read_arm(trial, CLAUDE_CODE)

    fresh = StreamReader().read_arm(trial, CLAUDE_CODE)
    assert incremental is not None and fresh is not None
    assert incremental.totals.to_json() == fresh.totals.to_json()
    assert incremental.totals.tokens_out == 125


def test_no_source_publishing_usage_reads_as_unobserved_not_as_zero(
    tmp_path: Path,
) -> None:
    """The distinction the whole fix turns on, and the one a `0` destroys."""
    trial = tmp_path / "t__a"
    write_stream(trial / STREAM_NAME, zeroed_stream())
    reading = StreamReader().read_arm(trial, CLAUDE_CODE)
    assert reading is not None
    assert reading.totals.usage_source == USAGE_UNOBSERVED
    assert reading.totals.tokens_out == 0
    # …and the scoreboard still refuses to price it, because nothing measured
    # it — the guard that keeps a real $0.79 from being published as $0.00.
    metrics = MetricsReader().read(trial, task="t")
    assert metrics.usage_measured is False
    assert metrics.priced_cost is None


def test_an_arm_on_a_working_route_never_opens_the_transcript(tmp_path: Path) -> None:
    """The fallback costs the arm it is not for exactly nothing.

    Asserted by making the transcript say something the totals must not
    contain: a fold that drained it would report 4000 output tokens.
    """
    trial = tmp_path / "t__a"
    write_stream(
        trial / STREAM_NAME,
        [init_event(), assistant("msg_a", usage={"input_tokens": 3, "output_tokens": 9})],
    )
    write_stream(
        trial / TRANSCRIPT_NAME,
        [assistant("msg_z", usage={"input_tokens": 1, "output_tokens": 4000})],
    )
    reading = StreamReader().read_arm(trial, CLAUDE_CODE)
    assert reading is not None
    assert reading.totals.tokens_out == 9


def test_the_harness_command_says_which_source_the_tokens_came_from(
    tmp_path: Path, capsys
) -> None:
    """A number a reader cannot source is a number they cannot defend."""
    from arenabench.cli import _cmd_harness

    trial = tmp_path / "matches" / "m1" / "jobs" / "m1-claude-code" / "t__a"
    write_stream(trial / STREAM_NAME, zeroed_stream())
    write_stream(trial / TRANSCRIPT_NAME, transcript_lines())
    args = argparse.Namespace(workspace=str(tmp_path), match="m1")
    assert _cmd_harness(args) == 0
    out = capsys.readouterr().out
    assert "125 out" in out
    assert "read from the session transcript" in out
    assert "stream reported no usage" not in out
