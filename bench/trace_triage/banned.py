"""The banned-behavior ledger: which detector findings stop a bench run.

A **banned behavior** is a bug we can measure, that we previously fixed, and
should not see again. This module is the third consumer of the one detector
registry in :mod:`detectors` — `triage_bench_traces.py` files issues from it,
`postmortem.py` renders a report from it, and this decides whether the run is
allowed to keep spending money. Same predicates, three dispositions. A second
detector suite would mean two surfaces that can disagree about whether a run is
healthy, and then neither is trustworthy.

## Why a ledger and not "any finding aborts"

Because a healthy run fires detectors. Measured on `post1` — 16 of 20 solved,
the run everything about it says is fine — `tool-error-envelope` fires on 40% of
trials and `agent-timeout` on one. A gate that aborted on any finding would have
killed it. That is not a hypothetical: it is the whole reason a gate gets
switched off, and a switched-off gate is worth less than no gate, because
somebody believes it is running.

So every detector in the registry carries a **disposition** here, and the set is
**total over `detectors.DETECTORS`** — `tests/test_banned.py` fails if a
detector exists with no row. That is the forcing function, in the shape
AGENTS.md invariant #10 uses for event consumers: you cannot add a detector
without answering "does this stop a run?", because the test asks. Its honest
limit is that it forces the decision when a *detector* is added, not when a
*bug* is fixed — see the module docstring of `detectors.py` for the other half
of that workflow.

## The warrant

Each `Banned` row cites what puts it there, and there are exactly two kinds:

* `FIXED_REGRESSION` — a merged PR or closed issue fixed this, and seeing it
  again means the fix is absent from the binary under test. The citation is
  auditable: the number names a diff someone can read.
* `MEASUREMENT_INVARIANT` — no single fix behind it, but the run cannot produce
  a number that means anything while it holds. A trial that completed no model
  call measured nothing; a run whose `(role, model)` census disagrees with its
  own configuration measured a different model than the one it names.

Both warrants justify the same action for the same reason — continuing spends
money on a number that cannot be used — but they are never confused, because
the audit question "what fix is behind this row?" has to have an answer, and
for the second kind the honest answer is "none, and here is why it still
aborts". A row with neither warrant is rejected by the constructor.

## The trigger

Per-behavior, and the distinction is **not** how bad the behavior is. It is
whether the signature is a property of the *run* or of the *trial*:

* A signature that is a fact about the configuration — an absent fix, a
  misrouted model, a credential that does not work — cannot be unlucky. The
  next trial runs the same binary against the same gateway with the same
  config, so one trial is proof and `FIRST_TRIAL` is correct.
* A signature that is a *distribution* genuinely can be unlucky on one task.
  `agent-timeout` fires on 1 of 20 trials of a healthy run, so a first-trial
  trip on it would abort healthy runs; it needs a denominator, and gets
  `Rate(3, 5)` — three of the first five settled trials.

Waiting for a denominator costs trials, which is the thing this exists to save.
That cost is paid only by the rows that cannot be decided without one.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path

import detectors
from detectors import Finding
from run_trace import Run, load_run

#: The exit code that tells a caller "a banned behavior was observed". Any other
#: non-zero exit means the gate itself failed, which must NOT abort a run: a
#: broken checker killing healthy runs is exactly how a gate gets disabled.
BANNED_EXIT = 9

FIXED_REGRESSION = "fixed-regression"
MEASUREMENT_INVARIANT = "measurement-invariant"


@dataclass(frozen=True)
class Warrant:
    """What entitles a row to stop a run that is costing money."""

    kind: str
    citation: str
    note: str

    def __post_init__(self) -> None:
        if self.kind not in (FIXED_REGRESSION, MEASUREMENT_INVARIANT):
            raise ValueError(f"unknown warrant kind {self.kind!r}")
        if self.kind == FIXED_REGRESSION and not self.citation:
            raise ValueError(
                "a fixed-regression warrant must cite the PR or issue that fixed it; "
                "an entry with no fix behind it is not a banned behavior"
            )
        if not self.note:
            raise ValueError("a warrant must say why in prose a reviewer can check")

    def render(self) -> str:
        head = self.citation or "no single fix"
        return f"{self.kind} ({head}) — {self.note}"


@dataclass(frozen=True)
class Trip:
    """How many trials must show the behavior before the run is stopped.

    `at_least` counts **distinct trials**, never occurrences: a detector that
    fires eleven times inside one trial has seen one trial, and reading that as
    eleven would turn any per-call detector into a first-trial trip by accident.
    """

    at_least: int
    of_first: int

    def __post_init__(self) -> None:
        if self.at_least < 1 or self.of_first < self.at_least:
            raise ValueError(f"incoherent trip {self.at_least}/{self.of_first}")

    def render(self) -> str:
        if self == FIRST_TRIAL:
            return "the first trial that shows it"
        return f"{self.at_least} of the first {self.of_first} settled trials"


#: Decidable on one trial, because the signature is a fact about the run.
FIRST_TRIAL = Trip(1, 1)


@dataclass(frozen=True)
class Banned:
    """A detector whose finding stops the run."""

    warrant: Warrant
    trip: Trip
    evidence: str


@dataclass(frozen=True)
class ReportOnly:
    """A detector that is reported and never stops the run, and why."""

    reason: str


Disposition = Banned | ReportOnly


# --------------------------------------------------------------------------
# The ledger. Total over `detectors.DETECTORS` — see `tests/test_banned.py`.
# --------------------------------------------------------------------------

LEDGER: dict[str, Disposition] = {
    "grep-ere-false-negative": Banned(
        warrant=Warrant(
            kind=FIXED_REGRESSION,
            citation="#2989",
            note=(
                "the POSIX grep fallback ran BRE, so every alternation the model "
                "wrote matched nothing and the tool reported the false negative as "
                "the fact `(no matches)`. Seeing a bare zero-match on an ERE pattern "
                "again means the binary under test predates the fix."
            ),
        ),
        trip=FIRST_TRIAL,
        evidence=(
            "run s5b2 (SUT 62a0cb5048f0, which does not contain fix commit b58e29a6): "
            "22 occurrences, 0 disclosures. Run post1 (SUT 018852a97387, which does): "
            "0 occurrences, 4 of 4 empties disclosed."
        ),
    ),
    "role-model-census-mismatch": Banned(
        warrant=Warrant(
            kind=MEASUREMENT_INVARIANT,
            citation="",
            note=(
                "the `(role, model)` census taken off completed `step_usage` events "
                "disagrees with the seat the run was launched with. Every number the "
                "run produces is then about a different model than the one it names, "
                "so continuing buys trials that cannot be reported."
            ),
        ),
        trip=FIRST_TRIAL,
        evidence=(
            "SILENCE VERIFIED, FIRING NOT: silent on both real runs mirrored with "
            "agent artifacts (s5b2, post1). It fired on 20 of 20 s5b2 trials until "
            "this gate was pointed at it and the finding turned out to be a "
            "comparison bug — a declared `anthropic/claude-sonnet-5` read as "
            "unequal to an observed `claude-sonnet-5`, which is the same model. "
            "The firing half is covered by fixture only; no real trace in this "
            "repository has ever shown a true census mismatch."
        ),
    ),
    "void-model-call": Banned(
        warrant=Warrant(
            kind=MEASUREMENT_INVARIANT,
            citation="",
            note=(
                "`step_usage` fires only on a completed step, so a trial with "
                "reasoning deltas and none of them never got one model call back. "
                "The trial measured the harness, and scoring its zero as a task loss "
                "is the flattering-artifact error inverted. This is the shape an "
                "expired credential takes: it is decidable on trial one, because the "
                "next trial uses the same credential."
            ),
        ),
        trip=FIRST_TRIAL,
        evidence=(
            "silent on both real runs mirrored with agent artifacts — 20 of 20 trials "
            "completed at least one model call in each of s5b2 and post1."
        ),
    ),
    "agent-timeout": Banned(
        warrant=Warrant(
            kind=FIXED_REGRESSION,
            citation="#2956",
            note=(
                "a gateway serving the worker far below its measured rate pushes the "
                "whole run into the agent timeout — 85 of 88 trials, once. A run whose "
                "trials predominantly end this way measures the ceiling, and comparing "
                "it against another arm compares two ceilings."
            ),
        ),
        # NOT first-trial, and this is the row that proves the rule is needed.
        trip=Trip(3, 5),
        evidence=(
            "run post1 is healthy (16 of 20 solved) and still fires this on 1 of 20 "
            "trials. A first-trial trip would have aborted it. Three of the first five "
            "settled trials is a distribution no healthy arm mirrored here produces."
        ),
    ),
    # -- reported, never aborting -------------------------------------------
    "tool-error-envelope": ReportOnly(
        reason=(
            "fires on 40% of the healthy post1 run and 60% of s5b2. It is a real "
            "defect and a bad one, but at that incidence it is the background rate of "
            "every arm we have, so aborting on it aborts everything."
        )
    ),
    "repeated-identical-tool-call": ReportOnly(
        reason=(
            "a band metric with a measured healthy ceiling (`bands.py`), not a fixed "
            "regression. post1 records 0.0 per trial; a run drifting above the band is "
            "worth reading, not worth killing."
        )
    ),
    "repeated-file-read": ReportOnly(
        reason=(
            "same shape as the band metrics above — 2 of 20 trials on s5b2. It "
            "describes an agent doing badly, which is a result, not a broken run."
        )
    ),
    "cache-collapse": ReportOnly(
        reason=(
            "`bands.py` records this as a pooled measurement across nine arms whose "
            "healthy and broken ranges overlap at the edges. A metric that cannot "
            "separate a healthy arm from a broken one must not be allowed to stop one."
        )
    ),
    "witness-unavailable": ReportOnly(
        reason=(
            "a reason-split diagnostic over a closed set the engine authors, not a "
            "fixed regression. The banned shape #2915 describes — a run ending within "
            "seconds of `witness_unavailable` — is a different predicate that does not "
            "exist yet and could not be verified against a trace here; see #3045."
        )
    ),
    "oracle-flip-ungraded": ReportOnly(
        reason=(
            "needs a pipeline trace to verify a trip threshold against, and no run "
            "mirrored in this repository carries one — the three runs holding agent "
            "artifacts are bare-loop arms that emit no `proof` or `verdict` at all. "
            "Shipping an abort rule unverified is the false abort this gate must not "
            "have; see #3045."
        )
    ),
    "no-post-verdict-file-change": ReportOnly(
        reason=(
            "this is close to the #2943 shape, but the detector's own caveat governs: "
            "`file_change` is observability, never evidence, because `bash` names no "
            "path and a heredoc mutates the tree silently. An abort keyed on an absent "
            "event would fire on work that happened; see #3045."
        )
    ),
}


# --------------------------------------------------------------------------
# Evaluation
# --------------------------------------------------------------------------


def trials_affected(finding: Finding) -> int:
    """Distinct trials in a finding's occurrences.

    `Finding.count` is occurrences, and several detectors emit one per tool
    call. Counting those as trials would silently turn every per-call detector
    into a first-trial trip, which is the arithmetic that makes a gate lie.
    """
    return len({occurrence.trial_uuid for occurrence in finding.occurrences})


@dataclass(frozen=True)
class Tripped:
    """One banned behavior the run showed, with everything a dispute needs."""

    code: str
    title: str
    warrant: Warrant
    trip: Trip
    trials: int
    settled: int
    evidence: str
    excerpt: str
    task_ids: tuple[str, ...]

    def as_dict(self) -> dict[str, object]:
        return {
            "code": self.code,
            "title": self.title,
            "warrant": {
                "kind": self.warrant.kind,
                "citation": self.warrant.citation,
                "note": self.warrant.note,
            },
            "trip": {
                "at_least": self.trip.at_least,
                "of_first": self.trip.of_first,
                "reads_as": self.trip.render(),
            },
            "trials_affected": self.trials,
            "trials_settled": self.settled,
            "ledger_evidence": self.evidence,
            "occurrence": self.excerpt,
            "tasks": list(self.task_ids),
        }

    def render(self) -> str:
        return (
            f"BANNED  {self.code}\n"
            f"  {self.title}\n"
            f"  warrant : {self.warrant.render()}\n"
            f"  trips on: {self.trip.render()}\n"
            f"  observed: {self.trials} of {self.settled} settled trial(s) — "
            f"{', '.join(self.task_ids)}\n"
            f"  ledger  : {self.evidence}\n"
            f"  trace   :\n{_indent(self.excerpt)}"
        )


def _indent(text: str, prefix: str = "    | ") -> str:
    return "\n".join(prefix + line for line in text.splitlines())


@dataclass(frozen=True)
class Verdict:
    """What the gate concluded about the trials it could see."""

    tripped: tuple[Tripped, ...]
    waived: tuple[str, ...]
    settled: int

    @property
    def banned(self) -> bool:
        return bool(self.tripped)

    @property
    def codes(self) -> tuple[str, ...]:
        return tuple(t.code for t in self.tripped)

    def as_dict(self) -> dict[str, object]:
        return {
            "banned": self.banned,
            "codes": list(self.codes),
            "waived": list(self.waived),
            "trials_settled": self.settled,
            "tripped": [t.as_dict() for t in self.tripped],
        }

    def render(self) -> str:
        lines = []
        if self.waived:
            lines.append(
                "waived for this run (--allow): "
                + ", ".join(self.waived)
                + "  — a waived behavior is recorded, never invisible."
            )
        if not self.tripped:
            lines.append(
                f"clear — {self.settled} settled trial(s) showed no banned behavior."
            )
            return "\n".join(lines)
        lines.extend(t.render() for t in self.tripped)
        return "\n\n".join(lines)


def evaluate(run: Run, *, allowed: Sequence[str] = ()) -> Verdict:
    """Judge the trials on disk against the ledger.

    The run passed here is whatever has settled so far, which is what makes the
    gate early: with one Batch job per trial, the first job to finish is a
    complete one-trial `Run`, and a `FIRST_TRIAL` row is decidable on it.
    """
    waived = tuple(sorted({code for code in allowed if code in LEDGER}))
    settled = len(run.trials)
    tripped: list[Tripped] = []
    for finding in detectors.run_all(run):
        disposition = LEDGER.get(finding.detector)
        if not isinstance(disposition, Banned):
            continue
        if finding.detector in waived:
            continue
        affected = trials_affected(finding)
        trip = disposition.trip
        # A rate cannot be read off fewer trials than its denominator. Refusing
        # to conclude is the whole point: the alternative is calling 1 of 1 a
        # 100% rate, which is the arithmetic that aborts healthy runs.
        if settled < trip.of_first or affected < trip.at_least:
            continue
        tripped.append(
            Tripped(
                code=finding.detector,
                title=finding.title,
                warrant=disposition.warrant,
                trip=trip,
                trials=affected,
                settled=settled,
                evidence=disposition.evidence,
                excerpt=finding.occurrences[0].render(),
                task_ids=tuple(sorted({o.task_id for o in finding.occurrences})),
            )
        )
    return Verdict(tripped=tuple(tripped), waived=waived, settled=settled)


# --------------------------------------------------------------------------
# CLI — the contract the arenabench gate seam invokes
# --------------------------------------------------------------------------


def main(argv: Sequence[str] | None = None) -> int:
    """Judge a mirrored run directory; exit `BANNED_EXIT` if it must stop.

    Stdout is the machine channel and carries the verdict as JSON, so the
    caller can name the codes in its own abort record without parsing prose.
    The human rendering goes to stderr, so redirecting one never costs the
    other.
    """
    parser = argparse.ArgumentParser(
        prog="banned",
        description=(
            "Judge a mirrored ArenaBench run against the banned-behavior ledger. "
            f"Exits 0 when clear, {BANNED_EXIT} when the run must stop, and 2 when "
            "the run could not be read — which is not a banned behavior and must "
            "not abort anything."
        ),
    )
    parser.add_argument("run_dir", type=Path, help="a mirrored run or trial directory")
    parser.add_argument(
        "--allow",
        action="append",
        default=[],
        metavar="CODE",
        help=(
            "waive one banned behavior for this invocation. The waiver is echoed "
            "into the verdict so a run that muted a check cannot later be read as "
            "clean. This is the escape hatch for a wrong ledger entry — use it, then "
            "fix the entry; do not switch the gate off."
        ),
    )
    parser.add_argument(
        "--list", action="store_true", help="print the ledger and exit"
    )
    args = parser.parse_args(argv)

    if args.list:
        print(render_ledger())
        return 0

    try:
        run = load_run(args.run_dir)
    except Exception as exc:  # noqa: BLE001 — any read failure is the same answer
        print(
            f"gate: could not read {args.run_dir}: {exc}\n"
            "This is a gate failure, not a banned behavior. The run continues.",
            file=sys.stderr,
        )
        return 2

    verdict = evaluate(run, allowed=args.allow)
    print(json.dumps(verdict.as_dict(), indent=2))
    print(verdict.render(), file=sys.stderr)
    return BANNED_EXIT if verdict.banned else 0


def render_ledger() -> str:
    """The whole ledger as text, so `--list` is the readable audit surface."""
    lines = ["banned — the run stops:"]
    for code, disposition in sorted(LEDGER.items()):
        if not isinstance(disposition, Banned):
            continue
        lines.append(f"  {code}")
        lines.append(f"    warrant : {disposition.warrant.render()}")
        lines.append(f"    trips on: {disposition.trip.render()}")
        lines.append(f"    evidence: {disposition.evidence}")
    lines.append("")
    lines.append("reported, never aborting:")
    for code, disposition in sorted(LEDGER.items()):
        if not isinstance(disposition, ReportOnly):
            continue
        lines.append(f"  {code}")
        lines.append(f"    {disposition.reason}")
    return "\n".join(lines)


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
