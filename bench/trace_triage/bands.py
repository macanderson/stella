"""The healthy band each per-trial metric sits in, measured off real arms.

The detectors next door answer "did this exact defect shape occur". This
module answers the other half — "how far is this arm from a run we already
know was fine" — and it exists because the two questions fail differently. A
shape detector is silent on a run that is merely *wrong*: an arm whose prompt
cache collapsed still emits well-formed events, and a broken roster produces
sixty-nine tool calls a trial without a single one of them being individually
suspicious.

**Every number here was measured, not chosen.** The survey covered nine arms
on 2026-08-12 — bare and pipeline, three gateways, four SUTs, healthy and
broken — and is recorded in :data:`SURVEY` so a reader can check a threshold
against the arms that produced it instead of taking it on faith:

    arm     solved  tools  identical  empty-ok  cache
    post1   16/20    30.0        0.0       2.0    93%   healthy
    s5b2    18/20    32.5        0.0       2.4   100%   healthy
    dec1    17/20    27.1        0.1       1.5   100%   healthy
    w10b    14/20    28.1        0.1       1.6    95%
    w10p     5/20    41.9        0.3       3.5    96%   pipeline losses
    f89p2   27/89    43.0        4.5       3.3    95%   pipeline losses
    w10f     4/20    69.7        0.9       9.2    52%   broken roster
    f89b2   50/88    32.7        1.1       1.8    94%

Two of those columns separate cleanly and the rest overlap, which is the whole
finding and the reason this module distinguishes them by type rather than by
prose. :attr:`Band.verdict_capable` marks the two — **identical consecutive
calls** (0.0-0.1 healthy against 0.9-4.5 unhealthy) and **cache hit** (52% on
the broken arm against 93%+ everywhere else). Tool-call count separates in the
same direction and overlaps at the edges (32.7 on an arm that solved 50/88,
32.5 on one that solved 18/20), so it is reported and never concluded from.

A band that cannot resolve a difference must say so, because the alternative
is a report that fires on every run. A detector that always fires is noise,
and noise is how a real finding gets ignored.

## Cache hit is computed the way ArenaBench computes it, deliberately

``cache_read / tokens_in``, from ``step_usage``'s ``cached_input_tokens`` and
``input_tokens`` (``arenabench/telemetry.py::TrialMetrics.cache_hit_rate``).
That is only correct if ``input_tokens`` is the **total** prompt count rather
than the uncached remainder, and it is: in the s5b2 trace, step 0 reads
``input_tokens=2, cached_input_tokens=0, cache_write_tokens=16582`` and step 1
reads ``input_tokens=16584, cached_input_tokens=16582`` — the second step's
input is the first step's write plus the two new tokens, so the field counts
everything sent. Adopting the same formula is not deference; it is what stops
this report and the match it is rendered on disagreeing about whether an arm's
cache is working.
"""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any

from run_trace import Run, Trial

__all__ = [
    "BANDS",
    "READ_TOOLS",
    "REREAD_LIMIT",
    "SURVEY",
    "ArmMetrics",
    "Band",
    "SurveyedArm",
    "TrialMetrics",
    "arm_metrics",
    "band_for",
    "cache_hit_rate",
    "read_target",
    "repeated_identical_adjacent",
    "trial_metrics",
]

#: Tool names whose call reads something without changing it. Named
#: exhaustively rather than inferred from a `read_only` flag, because the flag
#: is not on the wire: `tool_start` carries the name and the arguments and
#: nothing else. A tool missing from this list is simply not counted as a read
#: — the re-read metric under-reports rather than inventing a category.
READ_TOOLS = frozenset(
    {
        "read_file",
        "grep",
        "glob",
        "list_files",
        "semantic_code_search",
        "read_many_files",
    }
)

#: Argument keys a read tool names its target with, in precedence order.
_TARGET_KEYS = ("path", "file_path", "file", "pattern")

#: How many reads of one path inside one trial stop being orientation. The
#: worst single trial observed in the survey read one file 13 times; healthy
#: arms average 0.1-0.9 *re-reads per trial* across all paths, so a single path
#: crossing five is already several times any healthy arm's whole budget.
REREAD_LIMIT = 5


@dataclass(frozen=True)
class SurveyedArm:
    """One arm of the 2026-08-12 survey, kept so a threshold can be checked."""

    arm: str
    solved: str
    tools: float
    identical: float
    empty_ok: float
    cache: float
    note: str = ""


SURVEY: tuple[SurveyedArm, ...] = (
    SurveyedArm("post1", "16/20", 30.0, 0.0, 2.0, 93.0, "healthy"),
    SurveyedArm("s5b2", "18/20", 32.5, 0.0, 2.4, 100.0, "healthy"),
    SurveyedArm("dec1", "17/20", 27.1, 0.1, 1.5, 100.0, "healthy"),
    SurveyedArm("w10b", "14/20", 28.1, 0.1, 1.6, 95.0),
    SurveyedArm("w10p", "5/20", 41.9, 0.3, 3.5, 96.0, "pipeline losses"),
    SurveyedArm("f89p2", "27/89", 43.0, 4.5, 3.3, 95.0, "pipeline losses"),
    SurveyedArm("w10f", "4/20", 69.7, 0.9, 9.2, 52.0, "broken roster"),
    SurveyedArm("f89b2", "50/88", 32.7, 1.1, 1.8, 94.0),
)


@dataclass(frozen=True)
class Band:
    """What a metric looked like on arms already known to be fine.

    ``low``/``high`` bound the healthy arms; ``worse_is`` says which direction
    leaves the band. ``verdict_capable`` is the honest half: false means the
    healthy and unhealthy arms overlap on this metric, so a reading outside the
    band is a thing to look at and never a thing to conclude.
    """

    key: str
    label: str
    low: float
    high: float
    worse_is: str  # "higher" | "lower"
    verdict_capable: bool
    unit: str = ""
    evidence: str = ""

    def outside(self, value: float | None) -> bool:
        if value is None:
            return False
        return value > self.high if self.worse_is == "higher" else value < self.low

    def distance(self, value: float | None) -> float:
        """How far outside the band, in the metric's own units. 0 when inside."""
        if value is None or not self.outside(value):
            return 0.0
        return value - self.high if self.worse_is == "higher" else self.low - value


BANDS: tuple[Band, ...] = (
    Band(
        key="identical_adjacent",
        label="identical consecutive calls",
        low=0.0,
        high=0.1,
        worse_is="higher",
        verdict_capable=True,
        unit="per trial",
        evidence="0.0-0.1 on three healthy arms; 0.9-4.5 on three unhealthy ones",
    ),
    Band(
        key="cache_hit",
        label="prompt cache hit",
        low=93.0,
        high=100.0,
        worse_is="lower",
        verdict_capable=True,
        unit="%",
        evidence="93-100% on every arm surveyed except the broken roster, at 52%",
    ),
    Band(
        key="tools",
        label="tool calls",
        low=27.0,
        high=33.0,
        worse_is="higher",
        verdict_capable=False,
        unit="per trial",
        evidence=(
            "27-33 on healthy arms, 41.9-69.7 on unhealthy ones — but 32.7 on an "
            "arm that solved 50/88, so the bands touch"
        ),
    ),
)

#: Deliberately **not** a band: the survey's "empty ok-results" column
#: (1.5-2.4 healthy, 3.3-9.2 unhealthy) could not be reproduced from the
#: traces. Replaying s5b2 — the arm the survey scores at 2.4 — gives 0.0 under
#: "ok content is empty or whitespace", 0.0 under "ok envelope carries no
#: content", and 1.3 under "under twenty characters", of which the actual
#: strings are `(no matches)` and a bare `[exit code: 0]` trailer. Three of the
#: survey's four columns replay exactly (tools 32.5, identical 0.0, cache
#: 100%), so the disagreement is this metric's definition and not the loader.
#:
#: A threshold whose measurement cannot be reproduced is a number that will
#: fire or stay silent for reasons nobody can check, so it ships as an
#: observation on :class:`TrialMetrics` and as nothing else until the
#: definition is settled — see #3023.
EMPTY_OK_UNRESOLVED = (
    "the survey's empty-ok column has no reproducible definition yet (#3023)"
)


def band_for(key: str) -> Band | None:
    return next((band for band in BANDS if band.key == key), None)


# --------------------------------------------------------------------------
# per-trial measurement
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class TrialMetrics:
    """Everything the bands are read against, for one trial."""

    trial_uuid: str
    task_id: str
    steps: int
    tools: int
    identical_adjacent: int
    empty_ok_results: int
    prompt_tokens: int
    cached_tokens: int
    elapsed_seconds: float
    #: Path -> how many times a read-shaped tool call named it.
    read_counts: dict[str, int]

    @property
    def cache_hit(self) -> float | None:
        return cache_hit_rate(self.cached_tokens, self.prompt_tokens)

    @property
    def steps_per_minute(self) -> float | None:
        if self.elapsed_seconds <= 0 or not self.steps:
            return None
        return self.steps / (self.elapsed_seconds / 60.0)

    @property
    def void(self) -> bool:
        """No model call ever completed in this trial.

        The single most consequential distinction the report makes. A trial
        that completed zero model calls measured **nothing** — a credential
        that never authenticated, a gateway that never answered — and scoring
        its zero as a loss hands the other arm a win it did not earn.
        """
        return self.steps == 0

    @property
    def worst_reread(self) -> tuple[str, int] | None:
        if not self.read_counts:
            return None
        path, count = max(self.read_counts.items(), key=lambda kv: (kv[1], kv[0]))
        return (path, count) if count > 1 else None


def cache_hit_rate(cached: int, prompt: int) -> float | None:
    """``cache_read / tokens_in``, capped, or ``None`` with nothing sent.

    ``None`` rather than ``0.0`` for a trial that sent no prompt tokens: no
    cache reads out of no prompt is the absence of a measurement, and a 0%
    printed beside 94% is the loudest possible way to report nothing.
    """
    if prompt <= 0:
        return None
    return min(100.0, 100.0 * cached / prompt)


def read_target(name: str, arguments: Any) -> str | None:
    """The path a read-shaped call named, or ``None`` if it is not one."""
    if name not in READ_TOOLS or not isinstance(arguments, dict):
        return None
    for key in _TARGET_KEYS:
        value = arguments.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


def repeated_identical_adjacent(trial: Trial) -> int:
    """Adjacent tool calls with identical `(tool, args)` **and** identical output.

    The same strictness as the ``repeated-identical-tool-call`` detector, and
    for the same reason: a repeat whose answer *changed* is a legitimate poll.
    The two must agree — a band that counted loosely would call an arm unwell
    that the detector says is fine, and one of them would then be wrong in
    public.
    """
    calls = [c for c in trial.tool_calls if c.result is not None]
    return sum(
        1
        for previous, current in zip(calls, calls[1:], strict=False)
        if previous.signature() == current.signature()
        and previous.output_signature() == current.output_signature()
    )


def trial_metrics(trial: Trial) -> TrialMetrics:
    """Measure one trial. Pure: reads the parsed events and nothing else."""
    usage = trial.of_type("step_usage")
    prompt = sum(int(e.get("input_tokens") or 0) for e in usage)
    cached = sum(int(e.get("cached_input_tokens") or 0) for e in usage)

    stamps = [int(e["ts"]) for e in trial.events if isinstance(e.get("ts"), int)]
    elapsed = (max(stamps) - min(stamps)) / 1000.0 if len(stamps) > 1 else 0.0

    empty = 0
    reads: dict[str, int] = {}
    for call in trial.tool_calls:
        if call.result is not None and not call.error:
            content = call.ok_content
            if isinstance(content, str) and not content.strip():
                empty += 1
        target = read_target(call.name, call.input)
        if target is not None:
            reads[target] = reads.get(target, 0) + 1

    return TrialMetrics(
        trial_uuid=trial.trial_uuid,
        task_id=trial.task_id,
        steps=len(usage),
        tools=len(trial.tool_calls),
        identical_adjacent=repeated_identical_adjacent(trial),
        empty_ok_results=empty,
        prompt_tokens=prompt,
        cached_tokens=cached,
        elapsed_seconds=elapsed,
        read_counts=reads,
    )


# --------------------------------------------------------------------------
# per-arm aggregation
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class ArmMetrics:
    """The run's per-trial means, over the trials that measured anything.

    Void trials are excluded from every mean and counted separately. Averaging
    a trial that never reached the model into a tool-call rate reports the
    infrastructure failure as the agent being terse.
    """

    trials: tuple[TrialMetrics, ...]
    void: tuple[TrialMetrics, ...]

    def mean(self, key: str) -> float | None:
        values = [getattr(t, key) for t in self.trials]
        values = [float(v) for v in values if v is not None]
        return sum(values) / len(values) if values else None

    @property
    def cache_hit(self) -> float | None:
        """Pooled, not a mean of rates: a big trial and a tiny one differ."""
        return cache_hit_rate(
            sum(t.cached_tokens for t in self.trials),
            sum(t.prompt_tokens for t in self.trials),
        )

    def value(self, key: str) -> float | None:
        if key == "cache_hit":
            return self.cache_hit
        return self.mean(key)


def arm_metrics(run: Run | Iterable[Trial]) -> ArmMetrics:
    trials = run.trials if isinstance(run, Run) else list(run)
    measured = [trial_metrics(t) for t in trials]
    return ArmMetrics(
        trials=tuple(m for m in measured if not m.void),
        void=tuple(m for m in measured if m.void),
    )
