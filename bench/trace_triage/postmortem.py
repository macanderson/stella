"""An automated read of a run's traces, rendered on the match it came from.

`make triage-bench-traces` files GitHub issues; this files nothing. Its job is
the other one the same evidence supports: **say, directionally, where a run
went wrong, on screen, before anyone decides what the run means.** The traces
are read by hand when someone remembers to; this is the half that happens when
nobody does.

It shares the triage path's detectors rather than growing a second set. Both
call `detectors.run_all` over a `run_trace.Run`, so the question "is this run
healthy" has exactly one implementation and the report cannot say a run is fine
while the triage run opens an issue about it. What this module adds is the
*band* half — `bands`, next door — which is a different question (how far is
this arm from one we know was fine) rather than a second answer to the same
one.

## Four rules the output obeys, each of which this project has paid for

**Directional, never a verdict.** It names what looks wrong, how far outside
the measured healthy band it sits, and which trials to open. It states no
cause it has not established. `bands.Band.verdict_capable` decides this
here: a metric whose healthy and unhealthy arms overlap is reported as
something to look at, never as a finding.

**Every line cites its evidence.** Trial ids, the event or file that shows it,
the artifact path to open. A claim with no citation is not a finding — this
repository has been burned repeatedly by conclusions drawn off summary fields,
including a `WITNESS CONFIRMED` that was a `ModuleNotFoundError` and a `✗
failed` cell on a trial that scored 1.0.

**"The run is broken" is separated from "the agent did badly", loudly.** A
trial that completed zero model calls measured nothing: a credential that never
authenticated writes reward files full of zeros, and scoring those as losses
hands the other arm a win it did not earn. That distinction is the single most
valuable thing this report makes, so it is the first section and it is stated
in trials rather than percentages.

**Nothing to say is said in one line.** A healthy run gets a short clean
report. A detector that always fires is noise, and noise is how a real finding
gets ignored.

## Small samples

At n=20 a one- or two-trial difference is noise. `Finding.rate_line` already
refuses to print a percentage off a single occurrence; this module carries the
same discipline into the band table, which prints the trial count beside every
mean so a reader can see what the mean is a mean of.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path

import bands
from detectors import Finding, run_all
from run_trace import Run, Trial, load_run

__all__ = [
    "Cohort",
    "Report",
    "build_report",
    "cohort_of",
    "main",
    "render_markdown",
    "report_paths",
    "write_report",
]

#: How many trials a finding names before the list is cut. The point of the
#: list is "open these next", and a reader who is going to open twenty is going
#: to read the traces anyway.
_CITED_TRIALS = 6

#: Under this many measured trials, every rate in the report is decoration.
_NOISE_FLOOR = 5


@dataclass(frozen=True)
class Cohort:
    """How the run's trials divide before any of them are scored.

    The three groups answer different questions and must never be pooled:
    ``void`` measured nothing, ``timed_out`` measured something and ran out of
    clock, and ``measured`` is the only cohort a solve rate may be quoted over.
    """

    total: int
    void: tuple[Trial, ...]
    timed_out: tuple[Trial, ...]
    graded: tuple[Trial, ...]
    ungraded: tuple[Trial, ...]

    @property
    def measured(self) -> int:
        return self.total - len(self.void)

    @property
    def solved(self) -> int:
        return sum(1 for t in self.graded if (t.reward or 0.0) > 0)


def cohort_of(run: Run) -> Cohort:
    """Split a run's trials by what they measured, before scoring any of them.

    ``void`` is "zero completed model calls" — the same observation the
    ``void-model-call`` detector fires on, read here as a cohort rather than as
    a defect because that is what makes it usable as a denominator correction.
    Timeouts come from each trial's **job-level** ``result.json``
    (``exception_stats``), never the trial-root ``results.json``: reading the
    wrong one reported zero exceptions on a run where 10 of 20 threw.
    """
    void = tuple(t for t in run.trials if not t.of_type("step_usage"))
    timed_out = tuple(t for t in run.trials if "AgentTimeoutError" in t.exception_types)
    graded = tuple(run.graded_trials)
    ungraded = tuple(t for t in run.trials if t.reward is None)
    return Cohort(
        total=len(run.trials),
        void=void,
        timed_out=timed_out,
        graded=graded,
        ungraded=ungraded,
    )


@dataclass
class Report:
    """One run's postmortem: cohorts, band exits, findings, and the citations."""

    run_id: str
    root: Path
    match_name: str
    sut_ref: str
    bare_loop: bool
    cohort: Cohort
    metrics: bands.ArmMetrics
    findings: list[Finding]
    #: (band, observed value) for every band this arm sits outside.
    band_exits: list[tuple[bands.Band, float]] = field(default_factory=list)

    @property
    def clean(self) -> bool:
        """Whether the report has nothing to say.

        Void trials count as something to say even with no findings: they are
        the case where the *numbers* are wrong rather than the agent.
        """
        return not self.findings and not self.band_exits and not self.cohort.void

    @property
    def noisy_sample(self) -> bool:
        return self.cohort.measured < _NOISE_FLOOR

    def to_json(self) -> dict:
        return {
            "schema": 1,
            "run_id": self.run_id,
            "match_name": self.match_name,
            "sut_ref": self.sut_ref,
            "bare_loop": self.bare_loop,
            "clean": self.clean,
            "cohort": {
                "trials": self.cohort.total,
                "measured": self.cohort.measured,
                "void": [t.task_id for t in self.cohort.void],
                "timed_out": [t.task_id for t in self.cohort.timed_out],
                "graded": len(self.cohort.graded),
                "ungraded": [t.task_id for t in self.cohort.ungraded],
                "solved": self.cohort.solved,
            },
            "bands": [
                {
                    "key": band.key,
                    "label": band.label,
                    "observed": self.metrics.value(band.key),
                    "healthy_low": band.low,
                    "healthy_high": band.high,
                    "verdict_capable": band.verdict_capable,
                    "outside": band.outside(self.metrics.value(band.key)),
                    "evidence": band.evidence,
                }
                for band in bands.BANDS
            ],
            "findings": [
                {
                    "detector": finding.detector,
                    "title": finding.title,
                    "site": finding.site,
                    "fingerprint": finding.fingerprint.digest,
                    "count": finding.count,
                    "denominator": finding.denominator,
                    "rate": finding.rate_line(),
                    "trials": [
                        {
                            "task_id": occurrence.task_id,
                            "trial": occurrence.trial_uuid,
                            "artifact": occurrence.s3_key,
                        }
                        for occurrence in finding.occurrences[:_CITED_TRIALS]
                    ],
                }
                for finding in self.findings
            ],
        }


def build_report(run: Run) -> Report:
    """Read a loaded run into a report. Pure: no filesystem writes, no network."""
    metrics = bands.arm_metrics(run)
    exits: list[tuple[bands.Band, float]] = []
    for band in bands.BANDS:
        value = metrics.value(band.key)
        if value is not None and band.outside(value):
            exits.append((band, value))
    return Report(
        run_id=run.run_id,
        root=run.root,
        match_name=run.match_name,
        sut_ref=run.sut_ref,
        bare_loop=run.bare_loop,
        cohort=cohort_of(run),
        metrics=metrics,
        findings=run_all(run),
        band_exits=exits,
    )


# --------------------------------------------------------------------------
# rendering
# --------------------------------------------------------------------------


def _number(value: float | None, unit: str) -> str:
    if value is None:
        return "not measured"
    return f"{value:.1f}{unit}" if unit == "%" else f"{value:.1f} {unit}".strip()


def _cohort_section(report: Report) -> list[str]:
    """The section that must be impossible to miss, so it goes first."""
    cohort = report.cohort
    lines = [
        "## Did the run work, or did the agent fail?",
        "",
        f"- {cohort.total} trial(s) on disk; {cohort.measured} completed at least "
        "one model call.",
    ]
    if cohort.void:
        names = ", ".join(f"`{t.task_id}`" for t in cohort.void[:_CITED_TRIALS])
        more = "" if len(cohort.void) <= _CITED_TRIALS else " …"
        lines += [
            "",
            f"> **{len(cohort.void)} trial(s) measured nothing.** No `step_usage` "
            "event exists in their traces, so no model call ever completed — a "
            "credential that never authenticated and a gateway that never "
            "answered both land here. Their reward files hold zeros, and those "
            "zeros are the absence of a measurement, not a loss. **Scoring them "
            "as losses hands the other arm a win it did not earn.**",
            "",
            f"  Trials: {names}{more}",
            "  Evidence: `agent/stella-events.jsonl` in each — census `step_usage`"
            " and find none.",
        ]
    if cohort.timed_out:
        names = ", ".join(f"`{t.task_id}`" for t in cohort.timed_out[:_CITED_TRIALS])
        more = "" if len(cohort.timed_out) <= _CITED_TRIALS else " …"
        lines += [
            "",
            f"- {len(cohort.timed_out)} trial(s) ended in `AgentTimeoutError`. "
            "That is a trial that ran out of clock, which is a different thing "
            "from one that got the answer wrong.",
            f"  Trials: {names}{more}",
            "  Evidence: the **job-level** `result.json` "
            "(`stats.evals.*.exception_stats`) — the trial-root `results.json` "
            "does not carry it, and reading that one reported zero exceptions on "
            "a run where half the trials threw.",
        ]
    if cohort.ungraded:
        lines.append(
            f"- {len(cohort.ungraded)} trial(s) carry no `verifier/reward.txt`; "
            "they are in no denominator here."
        )
    if cohort.graded:
        lines.append(
            f"- Solve rate over the graded cohort: {cohort.solved}/"
            f"{len(cohort.graded)}."
            + (
                "  At this denominator a one- or two-trial difference is noise, "
                "not a rate."
                if len(cohort.graded) <= 20
                else ""
            )
        )
    return lines


def _band_section(report: Report) -> list[str]:
    lines = [
        "## Distance from the measured healthy band",
        "",
        f"Means over the {len(report.metrics.trials)} trial(s) that measured "
        "something; void trials are excluded, because averaging a trial that "
        "never reached the model reports an infrastructure failure as the agent "
        "being terse.",
        "",
        "| metric | this run | healthy | reads as |",
        "| --- | --- | --- | --- |",
    ]
    for band in bands.BANDS:
        value = report.metrics.value(band.key)
        healthy = (
            f"≥ {band.low:g}{band.unit}"
            if band.worse_is == "lower"
            else f"≤ {band.high:g} {band.unit}".strip()
        )
        if not band.outside(value):
            verdict = "in band"
        elif band.verdict_capable:
            verdict = f"**outside** by {band.distance(value):.1f} — a separator"
        else:
            verdict = "outside — directional only, the bands overlap here"
        lines.append(
            f"| {band.label} | {_number(value, band.unit)} | {healthy} | {verdict} |"
        )
    lines += [
        "",
        "Only the metrics marked *a separator* discriminated cleanly across the "
        "nine surveyed arms. The rest moved in the right direction and overlap "
        "at the edges, so they are context and never a conclusion — see "
        "`bench/trace_triage/bands.py` for the arms each band was measured off.",
    ]
    if report.noisy_sample:
        lines += [
            "",
            f"> Only {len(report.metrics.trials)} "
            "measured trial(s): every mean above is one or two trials wide. Read "
            "them as the trials they are, not as rates.",
        ]
    return lines


def _findings_section(report: Report) -> list[str]:
    if not report.findings:
        return [
            "## Detector findings",
            "",
            "None. The eight-plus shared detectors — the same set "
            "`make triage-bench-traces` files issues from — found nothing in "
            "this run's traces.",
        ]
    lines = ["## Detector findings", ""]
    for finding in report.findings:
        lines += [
            f"### {finding.title}",
            "",
            f"- detector `{finding.detector}` · fingerprint "
            f"`{finding.fingerprint.digest}`"
            + (f" · site `{finding.site}`" if finding.site else ""),
            f"- {finding.rate_line()}",
            "",
            finding.summary,
            "",
            "Open these:",
        ]
        for occurrence in finding.occurrences[:_CITED_TRIALS]:
            lines.append(
                f"- `{occurrence.task_id}` (trial `{occurrence.trial_uuid}`) — "
                f"`{occurrence.s3_key}`"
                + (f" — {occurrence.location}" if occurrence.location else "")
            )
        if len(finding.occurrences) > _CITED_TRIALS:
            lines.append(
                f"- …and {len(finding.occurrences) - _CITED_TRIALS} more."
            )
        for caveat in finding.caveats:
            lines += ["", f"> {caveat}"]
        lines.append("")
    return lines


def render_markdown(report: Report) -> str:
    """The whole report. Short and clean when there is nothing to say."""
    head = [
        f"# Postmortem — {report.run_id}",
        "",
        f"- match: {report.match_name or '(unnamed)'}",
        f"- SUT: `{report.sut_ref or 'unpinned'}`"
        + ("  ·  bare loop (no pipeline)" if report.bare_loop else ""),
        f"- artifacts: `{report.root}`",
        "",
        "Directional, not a verdict: this names what looks wrong and which "
        "trials to open. It establishes no cause. Read the traces it cites "
        "before concluding anything from it.",
        "",
    ]
    if report.clean:
        return "\n".join(
            head
            + [
                f"## Clean — {report.cohort.measured} of {report.cohort.total} "
                "trial(s) measured, nothing outside the healthy band, no "
                "detector findings.",
                "",
                f"Solve rate over the graded cohort: {report.cohort.solved}/"
                f"{len(report.cohort.graded)}."
                if report.cohort.graded
                else "No trial carried a verifier reward.",
                "",
            ]
        )
    body = (
        _cohort_section(report)
        + [""]
        + _band_section(report)
        + [""]
        + _findings_section(report)
    )
    return "\n".join(head + body) + "\n"


# --------------------------------------------------------------------------
# where the report lands
# --------------------------------------------------------------------------


def report_paths(run_dir: Path) -> tuple[Path, Path]:
    """Where the markdown and JSON go: **on the match**, when there is one.

    `arenabench assemble` folds a fetched cloud run into a single match under
    `<run>/matches/<id>/`. Writing the report there is what puts it beside the
    contest rather than beside the download — and when the run has not been
    assembled (or has been assembled into several), it falls back to the run
    directory rather than guessing which match the report belongs to.
    """
    matches = sorted(p for p in (run_dir / "matches").glob("*") if p.is_dir())
    home = matches[0] if len(matches) == 1 else run_dir
    return home / "postmortem.md", home / "postmortem.json"


def write_report(run_dir: Path, report: Report) -> tuple[Path, Path]:
    markdown, payload = report_paths(run_dir)
    markdown.parent.mkdir(parents=True, exist_ok=True)
    markdown.write_text(render_markdown(report), encoding="utf-8")
    payload.write_text(
        json.dumps(report.to_json(), indent=2) + "\n", encoding="utf-8"
    )
    return markdown, payload


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="postmortem",
        description=(
            "Probe a fetched bench run's traces and render a directional "
            "postmortem on the match assembled from it."
        ),
    )
    parser.add_argument(
        "run_dir", help="a run directory fetched by `arenabench cloud fetch`"
    )
    parser.add_argument("--run-id", help="override the run id in the report")
    parser.add_argument(
        "--stdout",
        action="store_true",
        help="print the markdown instead of writing it beside the match",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    run_dir = Path(args.run_dir).expanduser()
    if not run_dir.is_dir():
        print(f"error: {run_dir} is not a directory", file=sys.stderr)
        return 2
    run = load_run(run_dir, args.run_id or run_dir.resolve().name)
    if not run.trials:
        print(
            f"error: no trial artifacts under {run_dir} — a results-only fetch "
            "carries no traces. Re-fetch with `arenabench cloud fetch "
            "<run-id> --artifacts`.",
            file=sys.stderr,
        )
        return 2
    report = build_report(run)
    if args.stdout:
        print(render_markdown(report))
        return 0
    markdown, payload = write_report(run_dir, report)
    print(render_markdown(report))
    print(f"written: {markdown}")
    print(f"written: {payload}")
    return 0


if __name__ == "__main__":  # pragma: no cover — the CLI entry point
    raise SystemExit(main())
