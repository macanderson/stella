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

Beside the trials, the *runner* leaves one artifact of its own per job:
``<job>.seat.json``, the seat's launch record — which provider api and
route-qualified model the job was actually launched against. It exists because
no model id the agent or Harbor records can carry that fact (the launch
spelling differs by route), and pricing needs it (#1498). A job without one —
every archive predating the record — is read exactly as before.

Two readers sit on top. :class:`MetricsReader` reduces a trial to the seven
scoreboard dimensions and re-parses a file only when its size changes, so an
idle dashboard costs ``stat()`` calls. :class:`TranscriptReader` keeps a byte
offset per file and yields only what is new, which is what the SSE endpoint
streams.

:mod:`arenabench.monitor` reduces the same metrics one step further, to
failure detections a supervising process can act on while the match runs.
"""

from __future__ import annotations

import json
import time
from collections.abc import Iterable
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from stat import S_ISDIR
from typing import Any

from .pricing import price_for, price_for_route, trial_cost

__all__ = [
    "EVENTS_NAME",
    "INFRASTRUCTURE_FAILURES",
    "LIVENESS_NAMES",
    "TRAJECTORY_NAME",
    "MetricsReader",
    "TranscriptReader",
    "TrialMetrics",
    "aggregate",
    "leaders",
    "seat_manifest_path",
]

EVENTS_NAME = "agent/stella-events.jsonl"
TRAJECTORY_NAME = "agent/trajectory.json"
RESULT_NAME = "result.json"
SEAT_MANIFEST_SUFFIX = ".seat.json"

#: Per-trial paths written incrementally *while the agent runs*, probed —
#: never read — for liveness. Stella appends its event stream per event;
#: Harbor appends ``trial.log`` as the trial progresses; a Claude Code arm
#: tees its stdout to ``claude-code.txt`` and appends session transcripts
#: under ``sessions/``. Deliberately an allowlist rather than the directory
#: tree: the verifier and the arena write into the same trial directory, and
#: their activity must not read as the agent's pulse (#1571).
LIVENESS_NAMES: tuple[str, ...] = (
    EVENTS_NAME,
    "trial.log",
    "agent/claude-code.txt",
    "agent/sessions",
)


def seat_manifest_path(job_dir: Path) -> Path:
    """Where the runner records a job's seat launch data.

    A sibling of the job directory rather than a file inside it: Harbor owns
    that tree's creation, and the record exists before Harbor has made
    anything. Defined here, next to the reader, so the writer in ``runner.py``
    and :class:`MetricsReader` can never disagree about the location.
    """
    return job_dir.with_name(job_dir.name + SEAT_MANIFEST_SUFFIX)

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


def _liveness_age(trial_dir: Path) -> float | None:
    """Seconds since the trial last wrote any :data:`LIVENESS_NAMES` artifact.

    The freshest mtime across the allowlist wins, and a directory entry is
    probed recursively: appending to a transcript under ``agent/sessions/``
    touches only that file's mtime, never a directory's. Before #1571 this
    was the event stream's mtime alone, which made every trajectory-only arm
    — i.e. every non-Stella contestant — silent by construction: a hung
    Claude Code trial could burn its whole time allowance without the stall
    rule ever seeing it. ``None`` when no artifact exists yet — a trial that
    has not started has no liveness to be stale.
    """
    freshest: float | None = None
    for name in LIVENESS_NAMES:
        root = trial_dir / name
        try:
            probe = root.stat()
        except OSError:
            continue
        mtime = probe.st_mtime
        if S_ISDIR(probe.st_mode):
            for child in root.rglob("*"):
                try:
                    mtime = max(mtime, child.stat().st_mtime)
                except OSError:
                    continue
        freshest = mtime if freshest is None else max(freshest, mtime)
    if freshest is None:
        return None
    return max(0.0, time.time() - freshest)


# --------------------------------------------------------------------------
# Metrics
# --------------------------------------------------------------------------

#: Harbor exceptions that mean **the agent never got a fair attempt** — the
#: container did not start, its toolchain did not install, credentials were
#: wrong, or the verifier itself fell over and never returned a verdict.
#:
#: These are held apart from agent failures because conflating them corrupts a
#: head-to-head in whichever direction the infrastructure happened to break.
#: An agent whose per-trial installer needs the network is fragile to an outage
#: in a way that says nothing about how well it codes; scoring those trials 0
#: hands its opponent a win nobody can defend when the logs are opened. The
#: reverse is just as bad — a host DNS drop mid-run should not read as the
#: agent giving up.
#:
#: The split is deliberately conservative. Anything that happened *while the
#: agent was running and in control* stays an agent failure, including
#: `AgentTimeoutError` (it had its time), `NonZeroAgentExitCodeError` (it ran
#: and quit), and the context/output/memory ceilings (it chose what to spend).
#: Being wrong in that direction under-credits a contestant, which is the safe
#: way to be wrong about your own result.
INFRASTRUCTURE_FAILURES: frozenset[str] = frozenset(
    {
        # The trial never reached a state where the agent could act.
        "AgentSetupTimeoutError",
        "EnvironmentStartTimeoutError",
        "HealthcheckError",
        "AddTestsDirError",
        "DownloadVerifierDirError",
        "MissingExtraError",
        # Credentials are an operator setting, not a capability.
        "AuthenticationError",
        "OAuthCallbackError",
        "RefreshTokenExpiredError",
        # The run was killed out from under the agent. Distinct from
        # `AgentTimeoutError`, where the agent had its full budget and spent
        # it: here the clock was stopped early by something outside the
        # contest, so the trial has no result to report in either direction.
        # Observed for real — an operator restarted the match server and every
        # in-flight trial landed as a loss against a contestant that was still
        # working, which is the exact mis-scoring this set exists to prevent.
        "CancelledError",
        # The verifier broke, so no verdict exists to record either way.
        "VerifierTimeoutError",
        "VerifierOutputParseError",
        "RewardFileNotFoundError",
        "RewardFileEmptyError",
        "MultimodalExportError",
    }
)


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
    #: The verifier's raw score, when it gave one. Kept alongside
    #: :attr:`resolved` because a task graded 0.6 is a different result from
    #: one graded 0, and a pass/fail bit throws that away — partial credit is
    #: the only signal a scoreboard has about *solution quality* rather than
    #: mere completion.
    reward: float | None = None
    failure: str = ""
    #: ``True`` when :attr:`failure` names a harness or host fault rather than
    #: something the agent did. Such a trial is left *unjudged* — see
    #: :data:`INFRASTRUCTURE_FAILURES` for why that is not the same as a loss.
    infrastructure: bool = False
    steps: int = 0
    tools: int = 0
    tokens_in: int = 0
    tokens_out: int = 0
    cache_read: int = 0
    cache_write: int = 0
    #: What the agent said it spent. Kept for the record but never compared
    #: across seats: each agent prices from its own table, and two tables
    #: subtracted from each other is not a cost difference.
    total_cost: float = 0.0
    #: List-price cost recomputed from this trial's own token counts using one
    #: shared table. ``None`` when the model is unpriced — missing beats wrong.
    priced_cost: float | None = None
    clock_time: float = 0.0
    #: Seconds since the trial last wrote any :data:`LIVENESS_NAMES`
    #: artifact — its liveness, whichever files this arm happens to write.
    age_s: float | None = None
    cap_hits: int = 0
    models: tuple[str, ...] = ()
    has_video: bool = False
    #: ``True`` when the agent's own event stream said ``complete`` — the
    #: agent *claimed* to be finished. Distinct from :attr:`status`, which
    #: ``result.json`` also forces to "done": a trial Harbor tore down is
    #: done, but only the agent can have declared itself complete.
    declared_complete: bool = False
    #: ``usage_incomplete`` events observed — model calls whose tokens and
    #: cost never made it into the totals (#1467). A nonzero count means
    #: every spend figure on this trial is a floor, not a measurement.
    usage_incomplete: int = 0
    #: The last ``error`` message that no further step followed — the run's
    #: final word, when that word was an error. Empty when the agent worked
    #: on after its most recent error.
    late_error: str = ""

    @property
    def cache_hit_rate(self) -> float | None:
        """Share of prompt tokens served from cache, 0-100.

        Reported as a rate rather than a raw count because the count rewards
        being verbose: an agent that sends twice the context gets twice the
        cache reads while paying more in absolute terms. The rate asks the
        question that actually matters — of everything this agent put in front
        of the model, how much did it avoid paying full price for.
        """
        if self.tokens_in <= 0:
            return None
        return min(100.0, self.cache_read / self.tokens_in * 100.0)

    def to_json(self) -> dict[str, Any]:
        return {
            "task": self.task,
            "trial": self.trial,
            "status": self.status,
            "resolved": self.resolved,
            "reward": self.reward,
            "failure": self.failure,
            "infrastructure": self.infrastructure,
            "steps": self.steps,
            "tools": self.tools,
            "tokens_in": self.tokens_in,
            "tokens_out": self.tokens_out,
            "cache_read": self.cache_read,
            "cache_write": self.cache_write,
            "total_cost": round(self.total_cost, 6),
            "priced_cost": (
                round(self.priced_cost, 6) if self.priced_cost is not None else None
            ),
            "cache_hit_rate": self.cache_hit_rate,
            "clock_time": round(self.clock_time, 2),
            "age_s": self.age_s,
            "cap_hits": self.cap_hits,
            "models": list(self.models),
            "has_video": self.has_video,
            "declared_complete": self.declared_complete,
            "usage_incomplete": self.usage_incomplete,
            "late_error": self.late_error,
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
        "verifier_passed": None,
        "models": [],
        "usage_incomplete": 0,
        "late_error": "",
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
                    # A step after an error means the agent kept working, so
                    # that error was not the run's last word.
                    totals["late_error"] = ""
                elif kind == "error":
                    totals["late_error"] = str(event.get("message") or "")[:400]
                elif kind == "usage_incomplete":
                    totals["usage_incomplete"] += 1
                elif kind == "verdict":
                    passed = event.get("passed")
                    totals["verifier_passed"] = None if passed is None else bool(passed)
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
        self._seats: dict[Path, dict[str, Any] | None] = {}

    def _seat_route(self, trial_dir: Path) -> str:
        """The route-qualified model this trial's seat was launched with.

        ``""`` when the job has no launch record — every archive predating
        it. Cached without invalidation: the record is written once, before
        the job's first trial exists, so by the time this reader has a trial
        directory to ask about, the record's presence and content are final.
        """
        try:
            path = seat_manifest_path(trial_dir.parent)
        except ValueError:
            return ""
        if path not in self._seats:
            self._seats[path] = _load_json(path)
        seat = self._seats[path]
        return str(seat.get("qualified_model") or "") if seat else ""

    def _events_summary(self, path: Path) -> dict[str, Any] | None:
        try:
            stat = path.stat()
        except OSError:
            return None
        cached = self._events.get(path)
        if cached is not None and cached[0] == stat.st_size:
            return cached[1]
        summary = _reduce_events(path)
        if summary is not None:
            self._events[path] = (stat.st_size, summary)
        return summary

    def read(self, trial_dir: Path, task: str) -> TrialMetrics:
        """Everything knowable about one trial, from its artifacts alone."""
        metrics = TrialMetrics(task=task, trial=trial_dir.name)
        events = self._events_summary(trial_dir / EVENTS_NAME)
        metrics.age_s = _liveness_age(trial_dir)

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
                # ATIF spells these `total_prompt_tokens` /
                # `total_completion_tokens`. The `total_input_*` / `total_output_*`
                # spellings do not exist in the format, and reading them silently
                # yields zero — so an agent that publishes only a trajectory
                # (i.e. every non-Stella contestant) would score a real run at
                # 0 tokens and $0 and look astonishingly efficient.
                metrics.tokens_in = int(final.get("total_prompt_tokens") or 0)
                metrics.tokens_out = int(final.get("total_completion_tokens") or 0)
                metrics.cache_read = int(final.get("total_cached_tokens") or 0)
                metrics.total_cost = float(final.get("total_cost_usd") or 0.0)
                # Cache writes ride in `extra`, not on the metrics object.
                extra = final.get("extra")
                if isinstance(extra, dict):
                    metrics.cache_write = int(extra.get("total_cache_write_tokens") or 0)

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
            metrics.status = "done" if events["complete"] else "running"
            metrics.declared_complete = events["complete"]
            metrics.usage_incomplete = events["usage_incomplete"]
            metrics.late_error = events["late_error"]
        elif metrics.age_s is not None:
            # Liveness artifacts and no verdict yet mean the trial is in
            # flight even though nothing streams — every trajectory-only arm
            # mid-run looks like this. Without it the stall rule could never
            # see such an arm hang: a "pending" trial is not expected to be
            # writing, so its silence would never be suspicious (#1571).
            # `result.json` below still forces "done" once Harbor concludes.
            metrics.status = "running"

        configured = ""
        result = _load_json(trial_dir / RESULT_NAME)
        if result is not None:
            metrics.status = "done"
            verdict = result.get("verifier_result")
            verdict = verdict if isinstance(verdict, dict) else result
            # Harbor 0.6.1 nests the score: `verifier_result.rewards.reward`.
            # Reading only the flat `verifier_result.reward` finds nothing, and
            # nothing is indistinguishable from "not judged yet" — so a solved
            # task sat unjudged forever and the solve rate stayed 0% while the
            # verifier had already said 1.0 on disk. Both shapes are accepted
            # because the flat one is what older bundles carry.
            rewards = verdict.get("rewards")
            reward = (
                rewards.get("reward")
                if isinstance(rewards, dict)
                else verdict.get("reward")
            )
            if isinstance(reward, (int, float)):
                metrics.reward = float(reward)
                metrics.resolved = reward >= 1
            exception = result.get("exception_info")
            if isinstance(exception, dict) and exception.get("exception_type"):
                metrics.failure = str(exception["exception_type"])
                metrics.infrastructure = metrics.failure in INFRASTRUCTURE_FAILURES
                if metrics.resolved is None and not metrics.infrastructure:
                    # An agent that ran and failed scores 0, as every
                    # leaderboard counts it. One that never started stays
                    # `None`: unjudged, excluded from the solve rate, and
                    # counted where a reader can see it.
                    metrics.resolved = False
            wall = _iso_seconds(result.get("started_at"), result.get("finished_at"))
            if wall is not None:
                metrics.clock_time = wall
            # Harbor records the model it was launched with; prefer it over the
            # stream's, which can name a role's model rather than the seat's.
            configured = (
                ((result.get("config") or {}).get("agent") or {}).get("model_name") or ""
            )
            if configured and configured not in metrics.models:
                metrics.models = (*metrics.models, configured)

        # A trial that "ran" without one observed token was never a fair
        # attempt. Some agents swallow their own 401/429 and exit nonzero, so
        # Harbor names the trial `NonZeroAgentExitCodeError` — deliberately an
        # agent failure — and the `AuthenticationError` guard in
        # :data:`INFRASTRUCTURE_FAILURES` never sees the credential problem.
        # The scoreboard then reads "lost N tasks" when the truth is "never
        # made a single model call", which is the inverted conclusion a
        # benchmark must not produce (match dd52a57a6f49: three such trials,
        # `api_error_status:429`, scored as Claude Code losses — #1480).
        # Classified on *observed spend* rather than on the exception name
        # because the exception name is exactly what lied.
        if (
            metrics.resolved is False
            and metrics.steps > 0
            and metrics.tokens_in == 0
            and metrics.tokens_out == 0
            and metrics.total_cost == 0.0
        ):
            metrics.infrastructure = True
            metrics.resolved = None

        # One table, both seats. See pricing.py for why self-reported cost is
        # recorded but never compared. The seat's launch record outranks every
        # model id read back from the artifacts: launch spelling varies with
        # routing (a directly-routed seat is launched with the bare id), so
        # only the record can say which route served the trial, and every
        # recorded string prices route-blind at the gateway tier (#1498).
        # Among the recorded ids the configured seat model is tried first: a
        # single process can log several roles' `step_usage` (verifier, triage)
        # into one stream, so the stream's first-seen model may name a role
        # rather than the seat — pricing on that would mis-cost the whole trial.
        price = price_for_route(self._seat_route(trial_dir))
        if price is None:
            for name in (configured, *metrics.models):
                price = price_for(name)
                if price is not None:
                    break
        if price is not None:
            metrics.priced_cost = trial_cost(
                price,
                tokens_in=metrics.tokens_in,
                tokens_out=metrics.tokens_out,
                cache_read=metrics.cache_read,
                cache_write=metrics.cache_write,
            )

        # A running trial has no finish timestamp, so wall clock has to come
        # from its artifacts' own span rather than from Harbor. The earliest
        # liveness artifact stands in for the start — for a streaming arm the
        # event file's creation, as before #1571; for a trajectory-only arm
        # the tee of its stdout, without which a trial this change surfaces
        # as "running" would show a clock frozen at zero.
        if metrics.status == "running" and metrics.clock_time == 0.0:
            started = None
            for name in LIVENESS_NAMES:
                try:
                    stat = (trial_dir / name).stat()
                except OSError:
                    continue
                created = getattr(stat, "st_birthtime", stat.st_ctime)
                started = created if started is None else min(started, created)
            if started is not None:
                metrics.clock_time = max(0.0, time.time() - started)

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

    ``infrastructure`` counts trials that never reached a verdict because the
    harness or host broke. They are excluded from the solve rate on purpose,
    and reported separately so a reader can tell "won 8 of 10" from "won 8 of
    the 8 that managed to start". A scoreboard that hides the difference is
    the one nobody can defend afterwards.
    """
    trials = list(trials)
    judged = [t for t in trials if t.resolved is not None]
    passed = [t for t in judged if t.resolved]
    return {
        "trials": len(trials),
        "infrastructure": sum(1 for t in trials if t.infrastructure),
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
        # Comparable spend: every seat priced from the same table. `None` when
        # no trial had a priced model, so the column stays empty rather than
        # reading as a free run.
        "priced_cost": (
            sum(t.priced_cost for t in trials if t.priced_cost is not None)
            if any(t.priced_cost is not None for t in trials)
            else None
        ),
        "cache_hit_rate": (
            sum(t.cache_read for t in trials) / sum(t.tokens_in for t in trials) * 100.0
            if sum(t.tokens_in for t in trials) > 0
            else None
        ),
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

    A dimension a contestant has **no number** for excludes only that
    contestant, and only there. ``priced_cost`` is ``None`` for a seat on an
    unpriced model, and reading that as zero would hand it the cost crown for
    the one reason nobody would accept: that nobody knows what it spent. If no
    seat has a number, the dimension crowns nobody at all.
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
            raw = totals.get(dimension.key)
            if raw is None:
                continue
            value = float(raw)
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

        if kind == "verdict":
            passed = event.get("passed")
            return [
                entry(
                    self._next_seq(state),
                    "verdict",
                    "verifier: pass" if passed else "verifier: fail",
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
