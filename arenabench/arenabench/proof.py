"""What actually proved a trial — the witness, the oracle flip, and who ran.

Stella's pipeline publishes a *proof rail* alongside its ordinary telemetry:
a running account of whether the change it made was demanded to prove itself,
whether an independent model authored a failing test, whether that test then
flipped fail→pass, and — when none of that happened — the sentence saying why.
The rail is the whole point of the agent. It is the machinery that stops a
model from announcing "done" on work that is not done.

Every one of those events was already in ``stella-events.jsonl`` and none of
them reached a scoreboard. :mod:`arenabench.telemetry` folded the stream for
tokens, cost and a pass/fail bit and dropped the rest on the floor; the web
UI shipped a filter chip for ``proof`` entries that nothing has ever produced.
This module is the missing reduction.

**Why it is a separate module.** The distillation is a pure fold over parsed
events with no I/O of its own, which makes it directly testable against
recorded fixtures — and :mod:`arenabench.telemetry` is already close enough to
the 1500-line ratchet that growing it is the wrong move regardless.

**The one thing to understand before reading a grade.** The pipeline emits
``passed: true`` for three different situations: a real deterministic flip, an
``unverifiable`` outcome where every channel was blind, and a ``waived`` one
where triage skipped review. Only :attr:`TrialProof.rung` and the honesty
prefix on the verdict summary tell them apart. A reader that trusts ``passed``
alone cannot distinguish proven work from an unexamined claim, which is the
exact confusion the proof rail exists to prevent — so
:attr:`TrialProof.claimed_without_proof` is computed here rather than left to
each caller to rediscover.

Wire contract: ``crates/stella-protocol/src/proof.rs`` (the ``ProofStep``
enum), ``crates/stella-protocol/src/ladder.rs`` (``LadderSnapshot``,
``OracleObservation``, ``LadderRung``) and
``crates/stella-protocol/src/event/call_role.rs`` (``ModelCallRole``). The
generated schema in ``docs/wire/agentevent.schema.json`` is authoritative.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from typing import Any

__all__ = [
    "FLIP_ACHIEVED",
    "FLIP_NOT_ACHIEVED",
    "FLIP_UNKNOWN",
    "FLIP_UNOBSERVED",
    "GRADES",
    "OracleRun",
    "RoleCall",
    "TrialProof",
    "distill",
    "flip_outcome",
]

#: The flip oracle's finding, as three wire tokens plus an absence.
#: ``crates/stella-protocol/src/ladder.rs::FlipOutcome`` is the contract.
#:
#: **A bool could not carry this, and the missing state is the one that
#: matters** (#2556). ``flip_achieved: false`` meant both "a tracked command
#: ran and did not go fail→pass" — a real negative about the work — and "no
#: command was ever tracked, so nothing could have flipped", which is a
#: statement about the instrument. Reading the first as the second is how a
#: scoreboard reports a shortfall it never measured.
FLIP_ACHIEVED = "achieved"
FLIP_NOT_ACHIEVED = "not_achieved"
FLIP_UNOBSERVED = "unobserved"
#: No ladder said anything at all — no verdict reached this trial. Distinct
#: from :data:`FLIP_UNOBSERVED`, which is a ladder actively reporting that its
#: oracle never armed.
FLIP_UNKNOWN = ""

_FLIP_TOKENS = frozenset({FLIP_ACHIEVED, FLIP_NOT_ACHIEVED, FLIP_UNOBSERVED})


def flip_outcome(ladder: Mapping[str, Any]) -> str:
    """The ladder's flip finding as a token, or :data:`FLIP_UNKNOWN`.

    Reads ``flip``; falls back to the pre-#2556 ``flip_achieved`` bool for
    records written before the tri-state existed — every committed fixture
    under ``tests/fixtures/matches/`` is one of those, and a reader that
    dropped them would be trading one silent misread for another.

    The legacy mapping is **the reference decoder's**, deliberately: a legacy
    ``true`` is unambiguous, and a legacy ``false`` reads
    :data:`FLIP_NOT_ACHIEVED` — the value it was written to mean — because
    nothing in that field can resolve the conflation it embodies. Joining
    against ``tracked_command`` could resolve it for a reader who needs to, and
    is deliberately not done here: two decoders of one wire contract disagreeing
    is the class of defect this function exists to close, not to widen.

    An unrecognised token is :data:`FLIP_UNKNOWN` rather than a guess. It must
    never fall through to "not achieved": that would turn a token this build has
    not learned yet into a negative finding about an agent's work, which is the
    one direction a benchmark must not be wrong in.
    """
    token = ladder.get("flip")
    if isinstance(token, str):
        return token if token in _FLIP_TOKENS else FLIP_UNKNOWN
    # `flip` is typed as the bool on a record old enough to predate the token,
    # and Rust's own deserializer accepts that shape under the new name too.
    legacy = token if isinstance(token, bool) else ladder.get("flip_achieved")
    if isinstance(legacy, bool):
        return FLIP_ACHIEVED if legacy else FLIP_NOT_ACHIEVED
    return FLIP_UNKNOWN

#: Verdict-summary prefixes the pipeline stamps when it is telling you the
#: result is *not* backed by proof. Ordered most- to least-specific; the first
#: match wins. Sourced from ``crates/stella-pipeline/src/verify.rs`` — the
#: constructors that build each summary — and deliberately matched against the
#: head of the string rather than anywhere in it, because a model's own prose
#: quotes these words often enough to matter.
_HONESTY_MARKERS: tuple[tuple[str, str], ...] = (
    ("NO WORK ATTEMPTED", "nothing_attempted"),
    ("UNVERIFIABLE", "unverifiable"),
    ("UNVERIFIED", "uncorroborated"),
    ("UNPROVEN", "unproven"),
)

#: How far into a summary a marker may sit and still count as a prefix. The
#: constructors put it first, but some paths prepend a rung word, so this is a
#: little slack rather than an exact ``startswith``.
_MARKER_WINDOW = 90

#: The rungs whose evidence is deterministic — a test was actually run and
#: observed. ``crates/stella-protocol/src/ladder.rs::is_deterministic``.
#: Notably ``waived`` is *not* here despite riding a ``deterministic: true``
#: evidence flag: a waiver is an unexamined claim, not an observation.
_DETERMINISTIC_RUNGS = frozenset({"submit_fast", "revise"})

#: Grades in descending order of how much the result is actually worth,
#: with the sentence a reader needs to interpret one. This is the single
#: badge the scoreboard shows per trial.
GRADES: dict[str, str] = {
    "flip": "an independently authored witness test failed, then passed",
    "oracle": "a tracked test command flipped fail→pass",
    "refuted": "the oracle ran and never passed — the claim was caught",
    "unproven": "proof was warranted and no witness could be authored",
    "waived": "verification was waived; nothing checked the work",
    "unwarranted": "no proof was demanded of this change",
    "model": "a model asserted the result with no deterministic backing",
    "heuristic": "not even a model — a fallback rule decided",
    "none": "no proof rail ran for this trial",
}


@dataclass(frozen=True)
class OracleRun:
    """One observation of the tracked test command.

    ``tree`` is ``"baseline"`` (the unmodified code, expected to fail) or
    ``"candidate"`` (the agent's change, expected to pass). An authored
    witness's failing baseline is folded into the oracle *after the fact* by
    the witness stage rather than published as its own step, so a run of
    candidate-only observations beginning with a failure is the ordinary
    shape of a genuine flip — not a missing baseline.
    """

    tree: str
    passed: bool
    #: The command, when the observation came from a ``proof.oracle`` event.
    #: Empty for observations recovered from the verdict's ``oracle_trace``,
    #: which carries only ``tree`` and ``passed``.
    command: str = ""

    def to_json(self) -> dict[str, Any]:
        return {"tree": self.tree, "passed": self.passed, "command": self.command}


@dataclass(frozen=True)
class RoleCall:
    """Every model call one pipeline role made, and what it cost.

    The census axis is ``step_usage.role``, which names the model that
    *actually served* the call rather than the one configured for it. That
    distinction is the entire point: the witness author rides the verifier's
    resolution and is then independence-filtered against the worker, so a
    config file cannot tell you who wrote the test and this can.
    """

    role: str
    model: str
    calls: int = 0
    tokens_in: int = 0
    tokens_out: int = 0
    cost_usd: float = 0.0

    def to_json(self) -> dict[str, Any]:
        return {
            "role": self.role,
            "model": self.model,
            "calls": self.calls,
            "tokens_in": self.tokens_in,
            "tokens_out": self.tokens_out,
            "cost_usd": round(self.cost_usd, 6),
        }


@dataclass
class TrialProof:
    """The proof rail of one trial, reduced to what a reader must decide on."""

    # -- was proof demanded at all? ----------------------------------------
    #: ``proof.warrant.required``. ``None`` when no warrant was published,
    #: which is every non-pipeline seat and every run that ended before
    #: execute.
    warranted: bool | None = None
    #: Why no proof was demanded, when it was not. One of six fixed sentences
    #: from ``crates/stella-pipeline/src/witness/warrant.rs`` — "documentation
    #: only; prose has no runtime behavior to flip" and its siblings.
    warrant_reason: str = ""
    #: Diff size the warrant saw. Its budget lives on the ladder.
    diff_lines: int | None = None

    # -- was a witness authored? -------------------------------------------
    #: A ``stage: witness`` event was seen. When proof was warranted and this
    #: is ``False``, the stage never started — a different failure from one
    #: that started and could not finish.
    stage_ran: bool = False
    witness_path: str = ""
    witness_command: str = ""
    witness_fingerprint: str = ""
    #: Full, untruncated reasons a witness could not be produced. Plural
    #: because authoring gets one repair attempt, so a run can fail twice for
    #: different reasons and only the pair explains what happened.
    witness_unavailable: tuple[str, ...] = ()

    # -- did it flip? -------------------------------------------------------
    oracle_runs: tuple[OracleRun, ...] = ()
    #: The pipeline's own flip finding from the ladder — one of
    #: :data:`FLIP_ACHIEVED` / :data:`FLIP_NOT_ACHIEVED` /
    #: :data:`FLIP_UNOBSERVED`, or :data:`FLIP_UNKNOWN` when no verdict
    #: reached this trial. Preferred over re-deriving one from
    #: :attr:`oracle_runs`, which sees only the observations that reached the
    #: stream.
    #:
    #: A token rather than a bool for the same reason :attr:`diff_coverage` is
    #: one: the third value is the whole point (#2556).
    flip: str = FLIP_UNKNOWN
    #: The command flipped, when one was tracked.
    tracked_command: str = ""
    #: The same command passed *and* failed across runs — a flip that proves
    #: flakiness rather than a fix.
    unstable_flip: bool = False
    #: The baseline failed for a different reason than the witness asserts,
    #: so the pipeline declined to credit the flip.
    flip_refused: bool = False
    #: ``Some(true)`` when every witness artifact still matched its pinned
    #: identity at verdict time. Never ``False`` on the wire — tampering
    #: aborts the candidate — so its presence is the stated proof the
    #: tamper check ran.
    witness_intact: bool | None = None
    touched_tests_passed: bool | None = None

    # -- what backed the verdict? ------------------------------------------
    rung: str = ""
    verdict_passed: bool | None = None
    verdict_summary: str = ""
    #: Which honesty marker the summary carries, or ``""`` for a plain claim.
    honesty: str = ""
    #: ``proof.assurance`` — what the pipeline itself said corroborated the
    #: result, published at triage before any of it happened.
    assurance_witness: bool | None = None
    assurance_verifier: bool | None = None
    #: Whether the verifier resolved to a different model than the worker.
    #: ``False`` means the run graded its own homework.
    verifier_independent: bool | None = None
    #: ``covered`` / ``not_covered`` / ``unmeasured`` — whether the flipping
    #: test actually exercises the lines that changed.
    diff_coverage: str = ""
    verdict_degraded: tuple[str, ...] = ()
    verification_unavailable: tuple[str, ...] = ()

    # -- who ran ------------------------------------------------------------
    roles: tuple[RoleCall, ...] = ()

    # -- computed -----------------------------------------------------------
    grade: str = "none"

    @property
    def witness_authored(self) -> bool:
        """Whether a witness test was accepted, grafted and pinned.

        ``proof.witness_authored`` fires only past every integrity check, so
        this is the real thing rather than an attempt.
        """
        return bool(self.witness_path)

    @property
    def failed_attempts(self) -> int:
        """How many times the model thought it was done and a test disagreed.

        Counts the failing oracle observations before the first passing one —
        and *all* of them when none ever passed, so that ``failed_attempts >
        0`` selects every trial where the agent guessed wrong, whether or not
        it recovered. Scoping it to "before the first pass" alone would make
        the refuted trials, which are the starkest cases of exactly this,
        report zero. Read it beside :attr:`grade`, which says whether the
        recovery happened.
        """
        failures = 0
        for run in self.oracle_runs:
            if run.passed:
                break
            failures += 1
        return failures

    @property
    def deterministic(self) -> bool:
        """Whether a test was actually run and observed for this verdict."""
        return self.rung in _DETERMINISTIC_RUNGS

    @property
    def claimed_without_proof(self) -> bool:
        """A passing verdict that nothing deterministic backs.

        The honesty trap, computed once here so no caller has to rediscover
        it: ``unverifiable`` and ``waived`` both publish ``passed: true``, and
        a model verdict is an assertion rather than an observation. A trial
        where this is ``True`` said it was done and produced no evidence that
        it was.
        """
        return self.verdict_passed is True and not self.deterministic

    def to_json(self) -> dict[str, Any]:
        return {
            "grade": self.grade,
            "warranted": self.warranted,
            "warrant_reason": self.warrant_reason,
            "diff_lines": self.diff_lines,
            "stage_ran": self.stage_ran,
            "witness_authored": self.witness_authored,
            "witness_path": self.witness_path,
            "witness_command": self.witness_command,
            "witness_fingerprint": self.witness_fingerprint,
            "witness_unavailable": list(self.witness_unavailable),
            "oracle_runs": [run.to_json() for run in self.oracle_runs],
            "failed_attempts": self.failed_attempts,
            "flip": self.flip,
            "tracked_command": self.tracked_command,
            "unstable_flip": self.unstable_flip,
            "flip_refused": self.flip_refused,
            "witness_intact": self.witness_intact,
            "touched_tests_passed": self.touched_tests_passed,
            "rung": self.rung,
            "deterministic": self.deterministic,
            "claimed_without_proof": self.claimed_without_proof,
            "verdict_passed": self.verdict_passed,
            "verdict_summary": self.verdict_summary,
            "honesty": self.honesty,
            "assurance_witness": self.assurance_witness,
            "assurance_verifier": self.assurance_verifier,
            "verifier_independent": self.verifier_independent,
            "diff_coverage": self.diff_coverage,
            "verdict_degraded": list(self.verdict_degraded),
            "verification_unavailable": list(self.verification_unavailable),
            "roles": [role.to_json() for role in self.roles],
        }


def _honesty_of(summary: str) -> str:
    head = summary[:_MARKER_WINDOW]
    for marker, label in _HONESTY_MARKERS:
        if marker in head:
            return label
    return ""


def _grade_of(proof: TrialProof) -> str:
    """The single badge, from the strongest evidence the trial produced.

    Ordered by what a reader should conclude, not by which field was easiest
    to reach. A refutation outranks every soft outcome below it because it is
    the rail working: the agent claimed done, the oracle disagreed, and the
    claim did not ship.
    """
    achieved = proof.flip == FLIP_ACHIEVED
    if achieved and proof.witness_authored:
        return "flip"
    if achieved:
        return "oracle"
    # "The oracle ran and never passed" — so it must actually have run. An
    # `unobserved` flip means nothing armed, and grading that `refuted` would
    # publish "the claim was caught" about a claim nothing examined: a negative
    # finding manufactured out of a blind instrument, which is precisely the
    # conflation the tri-state was introduced to end (#2556). The observations
    # can arrive from `proof.oracle` events independently of the ladder, so
    # this is an explicit guard rather than something the data shape provides.
    if (
        proof.flip != FLIP_UNOBSERVED
        and proof.oracle_runs
        and not any(run.passed for run in proof.oracle_runs)
    ):
        return "refuted"
    if proof.witness_unavailable:
        return "unproven"
    if proof.rung == "waived":
        return "waived"
    if proof.warranted is False:
        return "unwarranted"
    if proof.rung == "heuristic_fallback":
        return "heuristic"
    if proof.rung in ("model_verdict", "model_judge") or proof.verdict_summary:
        return "model"
    return "none"


def _roles_from(usage: Mapping[tuple[str, str], list[float]]) -> tuple[RoleCall, ...]:
    """Role/model pairs, heaviest first, with ties broken by name.

    Sorted by call count rather than first appearance so the roster reads as
    "who did the work" — and deterministically, so a golden fixture of this
    output stays stable across runs.
    """
    calls = [
        RoleCall(
            role=role,
            model=model,
            calls=int(totals[0]),
            tokens_in=int(totals[1]),
            tokens_out=int(totals[2]),
            cost_usd=totals[3],
        )
        for (role, model), totals in usage.items()
    ]
    calls.sort(key=lambda call: (-call.calls, call.role, call.model))
    return tuple(calls)


def distill(events: Iterable[Mapping[str, Any]]) -> TrialProof:
    """Fold a trial's event stream into its proof rail.

    Tolerant by construction: every field is optional, unknown event types are
    ignored, and both legacy spellings the protocol still reads back are
    accepted — ``judge_verdict`` for the verdict event, ``judge`` for the
    verifier flag on an assurance step and for the verdict *role*. An archive
    recorded before a rename must not silently lose its proof.
    """
    proof = TrialProof()
    oracle_runs: list[OracleRun] = []
    witness_unavailable: list[str] = []
    verification_unavailable: list[str] = []
    degraded: list[str] = []
    usage: dict[tuple[str, str], list[float]] = {}

    for event in events:
        if not isinstance(event, Mapping):
            continue
        kind = str(event.get("type") or "")

        if kind == "stage":
            if str(event.get("name") or "") == "witness":
                proof.stage_ran = True

        elif kind == "step_usage":
            role = str(event.get("role") or "unknown")
            if role == "judge":  # the pre-rename spelling of `verdict`
                role = "verdict"
            model = str(event.get("model") or "")
            totals = usage.setdefault((role, model), [0.0, 0.0, 0.0, 0.0])
            totals[0] += 1
            totals[1] += float(event.get("input_tokens") or 0)
            totals[2] += float(event.get("output_tokens") or 0)
            totals[3] += float(event.get("cost_usd") or 0.0)

        elif kind == "proof":
            step = event.get("step")
            if not isinstance(step, Mapping):
                continue
            step_kind = str(step.get("kind") or "")
            if step_kind == "warrant":
                proof.warranted = bool(step.get("required"))
                reason = step.get("reason")
                if reason:
                    proof.warrant_reason = str(reason)
                if step.get("diff_lines") is not None:
                    proof.diff_lines = int(step.get("diff_lines") or 0)
            elif step_kind == "assurance":
                proof.assurance_witness = bool(step.get("witness"))
                # `judge` is the pre-rename field name for `verifier`.
                verifier = step.get("verifier")
                if verifier is None:
                    verifier = step.get("judge")
                proof.assurance_verifier = None if verifier is None else bool(verifier)
            elif step_kind == "witness_authored":
                proof.witness_path = str(step.get("path") or "")
                proof.witness_command = str(step.get("command") or "")
                proof.witness_fingerprint = str(step.get("fingerprint") or "")
            elif step_kind == "witness_unavailable":
                witness_unavailable.append(str(step.get("reason") or ""))
            elif step_kind == "verification_unavailable":
                verification_unavailable.append(str(step.get("reason") or ""))
            elif step_kind == "oracle":
                oracle_runs.append(
                    OracleRun(
                        tree=str(step.get("tree") or ""),
                        passed=bool(step.get("passed")),
                        command=str(step.get("command") or ""),
                    )
                )
            elif step_kind == "verdict_degraded":
                degraded.append(str(step.get("reason") or ""))

        elif kind in ("verdict", "judge_verdict"):
            passed = event.get("passed")
            proof.verdict_passed = None if passed is None else bool(passed)
            evidence = event.get("evidence")
            if not isinstance(evidence, Mapping):
                continue
            proof.verdict_summary = str(evidence.get("summary") or "")
            proof.honesty = _honesty_of(proof.verdict_summary)
            ladder = evidence.get("ladder")
            if not isinstance(ladder, Mapping):
                continue
            rung = str(ladder.get("rung") or "")
            # `model_judge` is the pre-rename spelling of `model_verdict`.
            proof.rung = "model_verdict" if rung == "model_judge" else rung
            proof.flip = flip_outcome(ladder)
            proof.unstable_flip = bool(ladder.get("unstable_flip"))
            proof.flip_refused = bool(ladder.get("flip_refused_different_failure"))
            proof.tracked_command = str(ladder.get("tracked_command") or "")
            proof.diff_coverage = str(ladder.get("diff_coverage") or "")
            for source, target in (
                ("witness_intact", "witness_intact"),
                ("touched_tests_passed", "touched_tests_passed"),
                ("verifier_independent", "verifier_independent"),
            ):
                value = ladder.get(source)
                if value is not None:
                    setattr(proof, target, bool(value))
            # The ladder's count wins over the warrant's. Both are real, but
            # they measure at different moments: the warrant sees the diff as
            # it decides whether to demand proof, the ladder sees the final
            # one at verdict time. A reader asking "how big was this change"
            # means the second — and a warrant that fired before the diff
            # settled publishes a 0 that would otherwise shadow it.
            if ladder.get("diff_lines") is not None:
                proof.diff_lines = int(ladder.get("diff_lines") or 0)
            # The ladder's trace is the pipeline's own record and includes
            # observations that never reached the stream as `proof.oracle`
            # steps — notably an authored witness's failing baseline, which
            # the witness stage folds in after the fact. Prefer it, and keep
            # the commands the proof steps carried.
            trace = ladder.get("oracle_trace")
            if isinstance(trace, list) and trace:
                command = proof.tracked_command or next(
                    (run.command for run in oracle_runs if run.command), ""
                )
                oracle_runs = [
                    OracleRun(
                        tree=str(item.get("tree") or ""),
                        passed=bool(item.get("passed")),
                        command=command,
                    )
                    for item in trace
                    if isinstance(item, Mapping)
                ]

    proof.oracle_runs = tuple(oracle_runs)
    proof.witness_unavailable = tuple(witness_unavailable)
    proof.verification_unavailable = tuple(verification_unavailable)
    proof.verdict_degraded = tuple(degraded)
    proof.roles = _roles_from(usage)
    proof.grade = _grade_of(proof)
    return proof
