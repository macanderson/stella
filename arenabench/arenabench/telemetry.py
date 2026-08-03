# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Reading a running contest out of the files it is already writing.

Everything the arena displays comes from trial artifacts on disk. Nothing here
talks to a container, an agent, or a provider — which is why the same code
renders a live match and an archive from six months ago, and why watching a
match cannot perturb it.

Per trial, Harbor and the agent leave:

``result.json``
    Harbor's verdict: ``verifier_result.reward``, start/finish timestamps, and
    an ``exception_info`` when the agent itself failed.
``agent/trajectory.json``
    ATIF — the cross-agent trajectory format. Written once, at the end, and
    supported by every Harbor agent that opts in. The portable fallback.
``agent/stella-events.jsonl``
    Stella's own event stream, appended and flushed per event. Richer than
    ATIF and *current* rather than final, so it is preferred wherever both
    speak — it is what makes a live transcript possible at all.

Two readers sit on top. :class:`MetricsReader` reduces a trial to the seven
scoreboard dimensions and re-parses a file only when its size changes, so an
idle dashboard costs ``stat()`` calls. :class:`TranscriptReader` keeps a byte
offset per file and yields only what is new, which is what the SSE endpoint
streams.
"""

from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Iterable

__all__ = [
    "EVENTS_NAME",
    "TRAJECTORY_NAME",
    "MetricsReader",
    "TranscriptReader",
    "TrialMetrics",
    "aggregate",
    "leaders",
]

EVENTS_NAME = "agent/stella-events.jsonl"
TRAJECTORY_NAME = "agent/trajectory.json"
RESULT_NAME = "result.json"

#: A step that spent its entire output allowance and called no tool is the
#: signature of output-cap truncation — the "zero-tool" failure class. Matched
#: on the reported count rather than a config lookup so an archived bundle from
#: another configuration still classifies.
_CAP_HIT_MIN_OUTPUT = 16384


def _load_json(path: Path) -> dict[str, Any] | None:
    try:
        loaded = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return loaded if isinstance(loaded, dict) else None


def _iso_seconds(started: Any, finished: Any) -> float | None:
    if not started or not finished:
        return None
    try:
        delta = datetime.fromisoformat(str(finished)) - datetime.fromisoformat(str(started))
    except (TypeError, ValueError):
        return None
    return max(0.0, delta.total_seconds())


# --------------------------------------------------------------------------
# Metrics
# --------------------------------------------------------------------------


@dataclass
class TrialMetrics:
    """One contestant's attempt at one task, reduced to what a scoreboard needs."""

    task: str
    trial: str
    status: str = "pending"  # pending | running | done
    #: ``True`` = verifier awarded the reward, ``False`` = it did not,
    #: ``None`` = not judged yet. Never coerced: "not yet" and "no" are
    #: different answers and a solve rate that conflates them is wrong.
    resolved: bool | None = None
    failure: str = ""
    steps: int = 0
    tools: int = 0
    tokens_in: int = 0
    tokens_out: int = 0
    cache_read: int = 0
    cache_write: int = 0
    total_cost: float = 0.0
    clock_time: float = 0.0
    #: Seconds since the trial last wrote anything — its liveness.
    age_s: float | None = None
    cap_hits: int = 0
    models: tuple[str, ...] = ()
    has_video: bool = False

    def to_json(self) -> dict[str, Any]:
        return {
            "task": self.task,
            "trial": self.trial,
            "status": self.status,
            "resolved": self.resolved,
            "failure": self.failure,
            "steps": self.steps,
            "tools": self.tools,
            "tokens_in": self.tokens_in,
            "tokens_out": self.tokens_out,
            "cache_read": self.cache_read,
            "cache_write": self.cache_write,
            "total_cost": round(self.total_cost, 6),
            "clock_time": round(self.clock_time, 2),
            "age_s": self.age_s,
            "cap_hits": self.cap_hits,
            "models": list(self.models),
            "has_video": self.has_video,
        }


def _reduce_events(path: Path) -> dict[str, Any] | None:
    """Fold Stella's event stream into per-trial totals.

    Tolerant of a torn final line: a live file is being appended to while this
    reads it, so an unparseable tail is expected rather than exceptional.
    """
    totals = {
        "steps": 0,
        "tools": 0,
        "tokens_in": 0,
        "tokens_out": 0,
        "cache_read": 0,
        "cache_write": 0,
        "total_cost": 0.0,
        "model_ms": 0,
        "cap_hits": 0,
        "complete": False,
        "judge_passed": None,
        "models": [],
    }
    seen_models: set[str] = set()
    try:
        with path.open(encoding="utf-8", errors="replace") as handle:
            for line in handle:
                try:
                    event = json.loads(line)
                except ValueError:
                    continue
                if not isinstance(event, dict):
                    continue
                kind = str(event.get("type", ""))
                if kind == "tool_start":
                    totals["tools"] += 1
                elif kind == "step_usage":
                    out = int(event.get("output_tokens") or 0)
                    totals["steps"] += 1
                    totals["tokens_in"] += int(event.get("input_tokens") or 0)
                    totals["tokens_out"] += out
                    totals["cache_read"] += int(event.get("cached_input_tokens") or 0)
                    totals["cache_write"] += int(event.get("cache_write_tokens") or 0)
                    totals["total_cost"] += float(event.get("cost_usd") or 0.0)
                    totals["model_ms"] += int(event.get("duration_ms") or 0)
                    if out >= _CAP_HIT_MIN_OUTPUT and not event.get("tool_calls"):
                        totals["cap_hits"] += 1
                    model = str(event.get("model") or "")
                    if model and model not in seen_models:
                        seen_models.add(model)
                        totals["models"].append(model)
                elif kind == "judge_verdict":
                    passed = event.get("passed")
                    totals["judge_passed"] = None if passed is None else bool(passed)
                elif kind == "complete":
                    totals["complete"] = True
    except OSError:
        return None
    return totals


class MetricsReader:
    """Caches per-trial reductions, invalidated by file size.

    Size rather than mtime because the files are append-only: a changed size is
    exactly a new event, and mtime granularity on some filesystems is coarser
    than the interval a live dashboard polls at.
    """

    def __init__(self) -> None:
        self._events: dict[Path, tuple[int, dict[str, Any]]] = {}

    def _events_summary(self, path: Path) -> tuple[dict[str, Any] | None, float | None]:
        try:
            stat = path.stat()
        except OSError:
            return None, None
        cached = self._events.get(path)
        if cached is not None and cached[0] == stat.st_size:
            summary: dict[str, Any] | None = cached[1]
        else:
            summary = _reduce_events(path)
            if summary is not None:
                self._events[path] = (stat.st_size, summary)
        age = max(0.0, time.time() - stat.st_mtime)
        return summary, age

    def read(self, trial_dir: Path, task: str) -> TrialMetrics:
        """Everything knowable about one trial, from its artifacts alone."""
        metrics = TrialMetrics(task=task, trial=trial_dir.name)
        events, age = self._events_summary(trial_dir / EVENTS_NAME)

        # ATIF first: portable across agents, but written once at the end.
        trajectory = _load_json(trial_dir / TRAJECTORY_NAME)
        if trajectory is not None:
            steps = trajectory.get("steps")
            if isinstance(steps, list):
                metrics.steps = len(steps)
                metrics.tools = sum(
                    len(step.get("tool_calls") or [])
                    for step in steps
                    if isinstance(step, dict)
                )
            final = trajectory.get("final_metrics")
            if isinstance(final, dict):
                metrics.tokens_in = int(final.get("total_input_tokens") or 0)
                metrics.tokens_out = int(final.get("total_output_tokens") or 0)
                metrics.cache_read = int(final.get("total_cached_tokens") or 0)
                metrics.cache_write = int(final.get("total_cache_write_tokens") or 0)
                metrics.total_cost = float(final.get("total_cost_usd") or 0.0)

        # Stella's own stream is richer and current, so it wins where it speaks.
        if events is not None:
            metrics.steps = events["steps"] or metrics.steps
            metrics.tools = events["tools"] or metrics.tools
            metrics.tokens_in = events["tokens_in"] or metrics.tokens_in
            metrics.tokens_out = events["tokens_out"] or metrics.tokens_out
            metrics.cache_read = events["cache_read"] or metrics.cache_read
            metrics.cache_write = events["cache_write"] or metrics.cache_write
            metrics.total_cost = events["total_cost"] or metrics.total_cost
            metrics.cap_hits = events["cap_hits"]
            metrics.models = tuple(events["models"])
            metrics.age_s = age
            metrics.status = "done" if events["complete"] else "running"

        result = _load_json(trial_dir / RESULT_NAME)
        if result is not None:
            metrics.status = "done"
            verdict = result.get("verifier_result")
            verdict = verdict if isinstance(verdict, dict) else result
            reward = verdict.get("reward")
            if isinstance(reward, (int, float)):
                metrics.resolved = reward >= 1
            exception = result.get("exception_info")
            if isinstance(exception, dict) and exception.get("exception_type"):
                metrics.failure = str(exception["exception_type"])
                if metrics.resolved is None:
                    metrics.resolved = False
            wall = _iso_seconds(result.get("started_at"), result.get("finished_at"))
            if wall is not None:
                metrics.clock_time = wall

        # A running trial has no finish timestamp, so wall clock has to come
        # from the events file's own span rather than from Harbor.
        if metrics.status == "running" and metrics.clock_time == 0.0:
            events_path = trial_dir / EVENTS_NAME
            try:
                stat = events_path.stat()
                created = getattr(stat, "st_birthtime", stat.st_ctime)
                metrics.clock_time = max(0.0, time.time() - created)
            except OSError:
                pass

        metrics.has_video = (trial_dir / "arena" / "recording.mp4").exists()
        return metrics


# --------------------------------------------------------------------------
# Aggregation and the leaderboard
# --------------------------------------------------------------------------


def aggregate(trials: Iterable[TrialMetrics]) -> dict[str, Any]:
    """Sum one contestant's trials into scoreboard totals.

    ``solve_rate`` divides by *judged* trials, not by attempted ones. Early in
    a match most trials are unjudged, and dividing by them would show every
    contestant near zero and climbing — an artifact of progress, not of skill.
    """
    trials = list(trials)
    judged = [t for t in trials if t.resolved is not None]
    passed = [t for t in judged if t.resolved]
    return {
        "trials": len(trials),
        "running": sum(1 for t in trials if t.status == "running"),
        "done": sum(1 for t in trials if t.status == "done"),
        "judged": len(judged),
        "passed": len(passed),
        "solve_rate": (len(passed) / len(judged) * 100.0) if judged else 0.0,
        "clock_time": sum(t.clock_time for t in trials),
        "tokens_in": sum(t.tokens_in for t in trials),
        "tokens_out": sum(t.tokens_out for t in trials),
        "cache_read": sum(t.cache_read for t in trials),
        "cache_write": sum(t.cache_write for t in trials),
        "total_cost": sum(t.total_cost for t in trials),
        "tools": sum(t.tools for t in trials),
        "steps": sum(t.steps for t in trials),
        "cap_hits": sum(t.cap_hits for t in trials),
    }


def leaders(
    totals_by_contestant: dict[str, dict[str, Any]],
    dimensions: Iterable[Any],
) -> dict[str, list[str]]:
    """Who leads each dimension. Ties return every tied contestant.

    Contestants with no judged trial yet are excluded from every dimension: a
    seat that has not spent a token "leads" cost, tokens and clock time, and
    crowning it would make the scoreboard actively misleading for the first
    several minutes of every match.
    """
    eligible = {
        name: totals
        for name, totals in totals_by_contestant.items()
        if totals.get("judged")
    }
    if not eligible:
        return {}
    result: dict[str, list[str]] = {}
    for dimension in dimensions:
        if dimension.direction == "neutral":
            continue
        best: float | None = None
        winners: list[str] = []
        for name, totals in eligible.items():
            value = float(totals.get(dimension.key) or 0.0)
            if best is None or dimension.better(value, best):
                best, winners = value, [name]
            elif value == best:
                winners.append(name)
        if winners:
            result[dimension.key] = winners
    return result


# --------------------------------------------------------------------------
# Transcripts
# --------------------------------------------------------------------------


@dataclass
class TranscriptState:
    """Cursor into one trial's transcript."""

    offset: int = 0
    seq: int = 0
    started: float | None = None
    #: Open text/reasoning entries being accumulated from deltas, by kind.
    open_entries: dict[str, int] = field(default_factory=dict)
    #: ``call_id`` -> entry sequence, so a tool result can find its call.
    tool_index: dict[str, int] = field(default_factory=dict)


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

    def _entries_for(
        self, event: dict[str, Any], state: TranscriptState, path_key: str
    ) -> list[dict[str, Any]]:
        kind = str(event.get("type", ""))
        if state.started is None:
            state.started = time.time()
        now = round(time.time() - state.started, 2)

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
        if kind in ("text", "text_delta", "reasoning"):
            bucket = "reasoning" if kind == "reasoning" else "text"
            fragment = str(event.get("delta") or event.get("text") or "")
            if not fragment:
                return []
            seq = state.open_entries.get(bucket)
            if seq is None:
                seq = self._next_seq(state)
                state.open_entries[bucket] = seq
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

        if kind == "judge_verdict":
            passed = event.get("passed")
            return [
                entry(
                    self._next_seq(state),
                    "verdict",
                    "judge: pass" if passed else "judge: fail",
                    str(event.get("reasoning") or ""),
                    passed=passed,
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
