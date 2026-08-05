# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Watching a live match for the failures its artifacts already reveal.

Every failure mode this module detects was diagnosed by hand, hours after a
finished match, from data the run had been writing since its first minute. The
expensive one is silent and inverted: an arm whose credentials never worked
shows on the scoreboard as an agent that *lost*, and a run that produces a
wrong conclusion is worse than a run that crashes. So the rules here are not
telemetry decoration — each one is a one-line reduction over
:class:`~arenabench.telemetry.TrialMetrics` that turns "someone should have
noticed" into a line a supervising process can act on while the match is still
running.

The observation rule is inherited from the rest of the arena: everything is
read from files the run already writes, nothing talks to a container, an
agent, or a provider, and watching a contestant can never change its number.

What a detection looks like on the wire is deliberately **not** ArenaBench's
own: it is the *agent monitor protocol* (``doc:agent-monitor-protocol``), a
line-delimited JSON envelope any run-watching tool can emit and any supervisor
(CI, a ``Monitor``, the fullauto loop, a shell pipe) can consume without
knowing what kind of run it is watching. ArenaBench is one emitter; the
protocol is the contract. This module owns the rules and the envelope;
``arenabench watch`` (:mod:`arenabench.cli`) owns the process and its exit
code.
"""

from __future__ import annotations

import json
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field, replace
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .telemetry import MetricsReader, TrialMetrics

__all__ = [
    "ALL_RULES",
    "DEFAULT_RULES",
    "PROTOCOL",
    "PROTOCOL_VERSION",
    "Detection",
    "MatchWatcher",
    "Thresholds",
    "stream_event",
]

#: The protocol name every emitted event carries. Consumers key on this and
#: on :data:`PROTOCOL_VERSION`, never on which tool happened to emit it.
PROTOCOL = "agent-monitor"

#: Version 1. Evolution is additive: a consumer must ignore fields and event
#: kinds it does not recognise, so new rules and new evidence never require a
#: version bump — only a change to the meaning of an existing field does.
PROTOCOL_VERSION = 1

#: The rules armed when the operator names none. ``usage-incomplete`` is
#: deliberately excluded: it marks the *totals* as a floor rather than the
#: trial as suspect, and firing it by default would bury the four detections
#: that actually invalidate or explain a result under bookkeeping notices.
DEFAULT_RULES = ("zero-token", "premature-complete", "late-verdict", "stall")

ALL_RULES = (*DEFAULT_RULES, "usage-incomplete")


@dataclass(frozen=True)
class Thresholds:
    """Tunable floors, conservative by default.

    Rules key on *outcome plus shape*, never shape alone — a 3-step pass is a
    real result and must not fire anything. The floors only say when a shape
    is short enough to be worth explaining given that the outcome was already
    a failure.
    """

    #: Below this many steps, a completed-and-failed trial is a confident zero.
    min_steps: int = 5
    #: Below this many output tokens, likewise.
    min_output_tokens: int = 300
    #: A running trial silent for longer than this is burning its allowance.
    stall_minutes: float = 10.0


@dataclass(frozen=True)
class Detection:
    """One rule firing once, for one agent on one task."""

    rule: str
    #: ``notice`` | ``warning`` | ``critical``. Critical means the observed
    #: run's number is invalid and must not be published — it is what turns
    #: into a nonzero exit from a one-shot watch.
    severity: str
    agent: str
    task: str
    #: One human-readable line of evidence. The numbers it cites also ride
    #: structured in :attr:`data`, so a consumer never parses this string.
    evidence: str
    data: dict[str, Any] = field(default_factory=dict)

    @property
    def key(self) -> tuple[str, str, str]:
        """Identity for deduplication: a detection is a fact about a trial,
        and a fact does not become more true by being re-observed."""
        return (self.agent, self.task, self.rule)

    def to_event(self, *, source: str, run: str, ts: str | None = None) -> dict[str, Any]:
        """This detection as an agent monitor protocol event."""
        return {
            "protocol": PROTOCOL,
            "v": PROTOCOL_VERSION,
            "kind": "detection",
            "ts": ts or _now_iso(),
            "source": source,
            "run": run,
            "agent": self.agent,
            "task": self.task,
            "rule": self.rule,
            "severity": self.severity,
            "evidence": self.evidence,
            "data": self.data,
        }

    def to_line(self) -> str:
        """The one-line text form: ``<severity> <agent> <task> <rule> <evidence>``."""
        return (
            f"{self.severity:<8} {self.agent:<28} {self.task:<28} "
            f"{self.rule:<18} {self.evidence}"
        )


def _now_iso() -> str:
    return datetime.now(UTC).isoformat(timespec="seconds")


def stream_event(kind: str, *, source: str, run: str, **fields: Any) -> dict[str, Any]:
    """A non-detection protocol event (``watching``, ``end``).

    Lifecycle framing is what makes a subscription practical: ``watching``
    tells the consumer the stream is alive and which rules are armed, ``end``
    tells it the stream is complete and carries the severity totals — so
    "no detections" and "the watcher died" are never the same silence.
    """
    return {
        "protocol": PROTOCOL,
        "v": PROTOCOL_VERSION,
        "kind": kind,
        "ts": _now_iso(),
        "source": source,
        "run": run,
        **fields,
    }


# --------------------------------------------------------------------------
# Rules
# --------------------------------------------------------------------------
#
# Each rule reduces one trial's metrics to at most one detection. They fire
# only on facts a wrong conclusion would be built from, which is why every
# guard below asks about the outcome before it asks about the shape.


def _zero_token(m: TrialMetrics, t: Thresholds) -> Detection | None:
    """Nonzero steps, zero observed spend: the arm never made a model call.

    The signature of a credential or rate-limit failure the agent swallowed
    (`api_error_status:429` exits nonzero, so Harbor's exception says *agent*
    failure). Critical because it inverts the scoreboard — this exact shape
    scored three rate-limited trials as Claude Code losses in match
    dd52a57a6f49. Keyed on observed spend, not the exception name, because
    the exception name is what lied.
    """
    if m.status != "done" or m.resolved is True:
        return None
    if m.steps <= 0 or m.tokens_in != 0 or m.tokens_out != 0 or m.total_cost != 0.0:
        return None
    return Detection(
        rule="zero-token",
        severity="critical",
        agent="",
        task="",
        evidence=(
            f"{m.steps} steps, 0 tokens, $0.00 observed — the arm never made a "
            f"model call ({m.failure or 'no exception recorded'}); "
            "an infrastructure void, not a loss"
        ),
        data={"steps": m.steps, "failure": m.failure},
    )


def _premature_complete(m: TrialMetrics, t: Thresholds) -> Detection | None:
    """The agent declared itself done almost immediately, and was wrong.

    A confident zero: ``stage: complete`` after a handful of steps and a few
    hundred output tokens, on a trial the verifier then failed. Requires the
    agent's *own* completion claim (``declared_complete``) so an arm that was
    torn down early — which also looks short — does not fire; and requires
    ``resolved is False`` so a legitimately short pass never does.
    """
    if not m.declared_complete or m.resolved is not False:
        return None
    if m.steps >= t.min_steps and m.tokens_out >= t.min_output_tokens:
        return None
    return Detection(
        rule="premature-complete",
        severity="warning",
        agent="",
        task="",
        evidence=(
            f"declared complete after {m.steps} steps / {m.tokens_out} output "
            f"tokens (floor: {t.min_steps} steps, {t.min_output_tokens} tokens) "
            "and did not pass"
        ),
        data={
            "steps": m.steps,
            "tokens_out": m.tokens_out,
            "min_steps": t.min_steps,
            "min_output_tokens": t.min_output_tokens,
        },
    )


def _late_verdict(m: TrialMetrics, t: Thresholds) -> Detection | None:
    """The run's final word was "verification failed" — caught too late to act.

    The probe found a false success at the very end: no step followed the
    error, so the agent shut down on a failure it never got (or took) a turn
    to repair. Keys on ``late_error`` — an error message nothing but shutdown
    followed — rather than on any error occurring, because a mid-run
    verification failure the agent then fixes is the system working.
    """
    if not m.declared_complete or m.resolved is True:
        return None
    if "verification failed" not in m.late_error.lower():
        return None
    first_line = m.late_error.splitlines()[0][:160]
    return Detection(
        rule="late-verdict",
        severity="warning",
        agent="",
        task="",
        evidence=f"final event is an error: {first_line}",
        data={"late_error": m.late_error},
    )


def _stall(m: TrialMetrics, t: Thresholds) -> Detection | None:
    """A running trial that has written nothing for the threshold window.

    A hung agent burns its time allowance in silence and surfaces as an
    ordinary timeout loss an hour later. ``age_s`` is the freshest write
    across the trial's liveness allowlist
    (:data:`~arenabench.telemetry.LIVENESS_NAMES`), not the event stream
    alone — so an arm that only tees its stdout, i.e. every non-Stella
    contestant, is watched exactly like one that streams (#1571).
    """
    if m.status != "running" or m.age_s is None:
        return None
    if m.age_s < t.stall_minutes * 60.0:
        return None
    return Detection(
        rule="stall",
        severity="warning",
        agent="",
        task="",
        evidence=(
            f"no event for {m.age_s / 60.0:.1f} min "
            f"(threshold {t.stall_minutes:g} min) on a running trial"
        ),
        data={"age_s": round(m.age_s, 1), "stall_minutes": t.stall_minutes},
    )


def _usage_incomplete(m: TrialMetrics, t: Thresholds) -> Detection | None:
    """Model calls whose tokens and cost never reached the totals (#1467).

    A notice, not a warning: it does not make the trial suspect, it makes
    every spend figure on it a floor rather than a measurement — which a
    reader comparing costs needs to know.
    """
    if m.usage_incomplete <= 0:
        return None
    return Detection(
        rule="usage-incomplete",
        severity="notice",
        agent="",
        task="",
        evidence=(
            f"{m.usage_incomplete} usage_incomplete event(s) — token and cost "
            "totals for this trial are a floor, not a measurement"
        ),
        data={"count": m.usage_incomplete},
    )


_RULES: dict[str, Callable[[TrialMetrics, Thresholds], Detection | None]] = {
    "zero-token": _zero_token,
    "premature-complete": _premature_complete,
    "late-verdict": _late_verdict,
    "stall": _stall,
    "usage-incomplete": _usage_incomplete,
}


# --------------------------------------------------------------------------
# The watcher
# --------------------------------------------------------------------------


class MatchWatcher:
    """Applies the armed rules to a match directory, once or repeatedly.

    Stateful for the same reason :class:`~arenabench.telemetry.TranscriptReader`
    is: a subscription wants *new* facts. Each :meth:`scan` re-reads whatever
    trial directories currently exist (cheap — the metrics reader caches by
    file size) and returns only detections not yet reported, so a follow loop
    emits each one exactly once no matter how often it polls.
    """

    def __init__(
        self,
        match_dir: Path,
        *,
        rules: Iterable[str] = DEFAULT_RULES,
        thresholds: Thresholds | None = None,
    ) -> None:
        self.match_dir = match_dir
        #: The run identity carried on every emitted event.
        self.run = match_dir.name
        self.rules = tuple(rules)
        unknown = sorted(set(self.rules) - set(ALL_RULES))
        if unknown:
            raise ValueError(
                f"unknown rule(s): {', '.join(unknown)} "
                f"(known: {', '.join(ALL_RULES)})"
            )
        self.thresholds = thresholds or Thresholds()
        self._metrics = MetricsReader()
        self._reported: set[tuple[str, str, str]] = set()
        self._counts = {"notice": 0, "warning": 0, "critical": 0}

    # -- reading -----------------------------------------------------------

    def agents(self) -> dict[str, Path]:
        """Observed agent name -> its job directory.

        The runner names a job ``<match-id>-<contestant-slug>``; the agent a
        detection blames is the slug. A job directory named some other way is
        reported under its full name rather than dropped — an unrecognised
        arm is still an arm.
        """
        jobs_root = self.match_dir / "jobs"
        if not jobs_root.is_dir():
            return {}
        prefix = f"{self.run}-"
        found: dict[str, Path] = {}
        for entry in sorted(jobs_root.iterdir()):
            if entry.is_dir():
                name = entry.name
                agent = name[len(prefix):] if name.startswith(prefix) else name
                found[agent] = entry
        return found

    @staticmethod
    def _trial_dirs(job_dir: Path) -> dict[str, Path]:
        """Task name -> trial directory (attempts collapsed, as the arena does)."""
        found: dict[str, Path] = {}
        for entry in sorted(job_dir.iterdir()):
            if entry.is_dir():
                found.setdefault(entry.name.rsplit("__", 1)[0], entry)
        return found

    # -- scanning ----------------------------------------------------------

    def scan(self) -> list[Detection]:
        """Detections not yet reported, in (agent, task, rule) order."""
        fresh: list[Detection] = []
        for agent, job_dir in self.agents().items():
            for task, trial_dir in self._trial_dirs(job_dir).items():
                metrics = self._metrics.read(trial_dir, task)
                for rule in self.rules:
                    hit = _RULES[rule](metrics, self.thresholds)
                    if hit is None:
                        continue
                    # Rules speak about one trial and do not know whose it
                    # is; the watcher owns the attribution.
                    detection = replace(hit, agent=agent, task=task)
                    if detection.key in self._reported:
                        continue
                    self._reported.add(detection.key)
                    self._counts[detection.severity] += 1
                    fresh.append(detection)
        return fresh

    @property
    def counts(self) -> dict[str, int]:
        """Detections reported so far, by severity."""
        return dict(self._counts)

    @property
    def invalidating(self) -> bool:
        """Whether anything reported so far invalidates the match's numbers."""
        return self._counts["critical"] > 0

    def summary_json(self) -> str:
        """The ``end`` event for this watcher, as one JSONL line."""
        return json.dumps(
            stream_event(
                "end",
                source="arenabench",
                run=self.run,
                detections=self._counts,
                invalidating=self.invalidating,
            )
        )
