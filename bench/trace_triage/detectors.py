"""The defect shapes `make triage-bench-traces` looks for, and the registry.

Every detector is a pure function from a loaded `Run` to a list of `Finding`s.
It reads `stella-events.jsonl` (plus `reward.txt`, `exception.txt` and
`results.json`) and returns nothing but observations with citations attached —
no GitHub, no network, no policy about what to do with what it found. That
split is what makes the whole set testable against a fixture trace, and it is
why adding a detector is one decorated function and nothing else.

## Adding a detector

    @detector(
        code="my-shape",                       # stable; part of the fingerprint
        title="what a new issue would be called",
        site="crates/stella-pipeline/src/verify.rs",   # or "" if none implicated
        search_terms=("phrase a reviewer would search",),
    )
    def _my_shape(run: Run) -> list[Finding]:
        ...

Register it by importing this module — the decorator appends to `DETECTORS`.
`tests/test_detectors.py::test_every_detector_declares_its_contract` then holds
you to the contract, so a detector cannot ship without a code, a title and the
search terms the de-duplication step needs.

Three rules a detector must obey, each of which this repository has paid for:

1. **`proof` discriminates on `step.kind`.** Use `trial.proofs(kind=...)`.
2. **Carry a reason string in FULL.** They are truncated in every projection
   and the truncated half is usually the point, so an occurrence's excerpt is
   the whole string. Never `[:80]`.
3. **`file_change` is observability, never evidence.** `bash` names no path, so
   a heredoc, `patch`, `make` or `>` mutates the tree without emitting one.
   A detector keyed on its absence states what it observed — "no `file_change`
   event was recorded" — and never concludes that the tree did not change. The
   `no-post-verdict-file-change` detector below carries that caveat in its own
   output so the sentence reaches the issue rather than dying here.
"""

from __future__ import annotations

import json
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from typing import Any

from fingerprint import Fingerprint, fingerprint_of
from run_trace import Run, Trial

# A `bash` error envelope whose message is at least this long, and this many
# lines, before its `[exit code: N]` trailer is carrying real output that the
# model now has to read as a failure.
_SUBSTANTIAL_OUTPUT_CHARS = 200
_SUBSTANTIAL_OUTPUT_LINES = 3


@dataclass(frozen=True)
class Occurrence:
    """One citable instance: where it happened and the bytes that show it."""

    trial_uuid: str
    task_id: str
    s3_key: str
    excerpt: str
    location: str = ""

    @property
    def task_name(self) -> str:
        """The dataset task, with the per-trial suffix stripped.

        `code-from-image__UTUWFRc` is one *attempt*; the suffix is minted fresh
        every run. A citation names the attempt (that is what a reader fetches),
        but anything compared across runs — the ledger's `tasks` list, and so
        every "a new affected task" claim — must use this. Keyed on the raw id,
        the novelty check reports all twenty tasks of a rerun as new, which is
        the loudest possible way to say nothing.
        """
        return self.task_id.split("__", 1)[0]

    def render(self) -> str:
        head = f"- **{self.task_id}** (trial `{self.trial_uuid}`)"
        if self.location:
            head += f" — {self.location}"
        return f"{head}\n  `{self.s3_key}`\n\n```\n{self.excerpt}\n```"


@dataclass
class Finding:
    """What one detector saw, with everything needed to file or comment on it."""

    detector: str
    site: str
    variant_source: str
    title: str
    summary: str
    occurrences: list[Occurrence]
    denominator: int
    search_terms: tuple[str, ...] = ()
    caveats: list[str] = field(default_factory=list)
    detail: str = ""

    @property
    def fingerprint(self) -> Fingerprint:
        return fingerprint_of(self.detector, self.site, self.variant_source)

    @property
    def count(self) -> int:
        return len(self.occurrences)

    @property
    def single_occurrence(self) -> bool:
        return self.count <= 1

    def rate_line(self) -> str:
        """The incidence, stated so it cannot be read as more than it is.

        A one- or two-trial difference at n=20 is noise, and a detector that
        fires once must say so rather than presenting `1/20 = 5%` as a rate —
        the percentage is arithmetically true and epistemically a lie.
        """
        if self.denominator <= 0:
            return f"{self.count} occurrence(s); no denominator recorded."
        if self.single_occurrence:
            return (
                f"**Single occurrence** — {self.count} of {self.denominator} trials examined. "
                "This is one trial, not a rate: do not read an incidence off it."
            )
        pct = 100.0 * self.count / self.denominator
        line = f"{self.count} of {self.denominator} trials examined ({pct:.0f}%)."
        if self.count <= 2:
            line += (
                " At this denominator two occurrences are within noise of one — treat the"
                " count as the observation and the percentage as decoration."
            )
        return line


@dataclass(frozen=True)
class Detector:
    code: str
    title: str
    site: str
    search_terms: tuple[str, ...]
    fn: Callable[[Run], list[Finding]]

    def run(self, run: Run) -> list[Finding]:
        return [f for f in self.fn(run) if f.occurrences]


DETECTORS: list[Detector] = []


def detector(
    *, code: str, title: str, site: str = "", search_terms: Iterable[str] = ()
) -> Callable[[Callable[[Run], list[Finding]]], Callable[[Run], list[Finding]]]:
    def wrap(fn: Callable[[Run], list[Finding]]) -> Callable[[Run], list[Finding]]:
        DETECTORS.append(
            Detector(
                code=code,
                title=title,
                site=site,
                search_terms=tuple(search_terms),
                fn=fn,
            )
        )
        return fn

    return wrap


def run_all(run: Run) -> list[Finding]:
    findings: list[Finding] = []
    for det in DETECTORS:
        findings.extend(det.run(run))
    return findings


def _brief(event: dict[str, Any], limit: int = 400) -> str:
    """A one-line event excerpt for a citation, with reasons left whole.

    The truncation here is on the *event*, never on a `reason`: a reason is
    lifted out first and printed in full, because it is the half every other
    surface cuts.
    """
    text = json.dumps(event, ensure_ascii=False)
    return text if len(text) <= limit else text[:limit] + " …(event truncated)"


# --------------------------------------------------------------------------
# 1. A trial that thought, and never completed a model call
# --------------------------------------------------------------------------


@detector(
    code="void-model-call",
    title="a trial streamed reasoning and completed zero model calls",
    site="crates/stella-model/src/provider_parity.rs",
    search_terms=("step_usage reasoning deltas zero", "agent timeout no completed step"),
)
def _void_model_call(run: Run) -> list[Finding]:
    """`reasoning` deltas with no `step_usage` at all.

    `step_usage` is emitted only when a step *completes*, so a trial with
    thousands of reasoning deltas and zero of them never got a single model call
    back. That is an infrastructure void — the run measured the harness, not the
    agent — and reporting it as a task loss is the flattering-artifact error.
    """
    occurrences = []
    for trial in run.trials:
        reasoning = len(trial.of_type("reasoning"))
        usage = len(trial.of_type("step_usage"))
        if reasoning > 0 and usage == 0:
            occurrences.append(
                Occurrence(
                    trial_uuid=trial.trial_uuid,
                    task_id=trial.task_id,
                    s3_key=trial.s3_key(),
                    location=f"{reasoning} reasoning deltas, 0 step_usage events",
                    excerpt=(
                        f"reasoning deltas : {reasoning}\n"
                        f"step_usage       : 0   <- no model call ever completed\n"
                        f"tool_start       : {len(trial.of_type('tool_start'))}\n"
                        f"harbor exception : {', '.join(trial.exception_types) or 'none recorded'}"
                    ),
                )
            )
    if not occurrences:
        return []
    return [
        Finding(
            detector="void-model-call",
            site="crates/stella-model/src/provider_parity.rs",
            variant_source="reasoning deltas present with zero completed model calls",
            title="a trial streamed reasoning and completed zero model calls",
            summary=(
                "These trials emitted `reasoning` deltas but no `step_usage` at all. "
                "`step_usage` fires only on a completed step, so not one model call "
                "returned. This is an infrastructure void, not a task loss: scoring "
                "these as agent failures measures the harness."
            ),
            occurrences=occurrences,
            denominator=len(run.trials),
            search_terms=("step_usage reasoning deltas zero", "agent timeout no completed step"),
            caveats=[
                "Reads only completed-step telemetry. A provider that streamed reasoning "
                "and then timed out server-side is indistinguishable here from one that "
                "never accepted the request; both are voids, but the cause is not decided "
                "by this signal alone."
            ],
        )
    ]


# --------------------------------------------------------------------------
# 2. Trials the harness killed
# --------------------------------------------------------------------------


@detector(
    code="agent-timeout",
    title="the run is dominated by AgentTimeoutError — it measures the timeout, not the agent",
    site="",
    search_terms=("AgentTimeoutError", "agent execution timed out"),
)
def _agent_timeout(run: Run) -> list[Finding]:
    """Trials harbor recorded as `AgentTimeoutError`.

    Taken from `results.json`'s `exception_stats` — harbor's own classification
    — rather than by regexing the traceback, so the detector agrees with the
    runner about what killed the trial instead of parsing prose about it.
    """
    occurrences = []
    for trial in run.trials:
        if "AgentTimeoutError" not in trial.exception_types:
            continue
        tail = trial.exception_text.strip().splitlines()[-3:]
        occurrences.append(
            Occurrence(
                trial_uuid=trial.trial_uuid,
                task_id=trial.task_id,
                s3_key=trial.s3_key("exception.txt"),
                location=(
                    f"{len(trial.of_type('step_usage'))} completed steps, "
                    f"{len(trial.of_type('tool_start'))} tool calls, reward "
                    f"{trial.reward if trial.reward is not None else 'unrecorded'}"
                ),
                excerpt="\n".join(tail) or "exception.txt absent; class from results.json",
            )
        )
    if not occurrences:
        return []
    return [
        Finding(
            detector="agent-timeout",
            site="",
            variant_source="harbor recorded AgentTimeoutError for the trial",
            title=(
                "the run is dominated by AgentTimeoutError — it measures the timeout, "
                "not the agent"
            ),
            summary=(
                "Harbor killed these trials at the agent timeout. A run whose trials "
                "predominantly end this way has an operational abort as its outcome "
                "distribution: its solve rate is a measurement of the ceiling, and "
                "comparing it against another arm compares two ceilings."
            ),
            occurrences=occurrences,
            denominator=len(run.trials),
            search_terms=("AgentTimeoutError", "agent execution timed out"),
            caveats=[
                "A timeout is not by itself a defect — it is a defect when it is the "
                "run's dominant outcome, or when the trace shows work finished before "
                "the kill. Read the per-trial step counts above before concluding."
            ],
        )
    ]


# --------------------------------------------------------------------------
# 3. The witness that was never authored
# --------------------------------------------------------------------------


@detector(
    code="witness-unavailable",
    title="witness authoring failed",
    site="crates/stella-pipeline/src/verify.rs",
    search_terms=("witness_unavailable", "witness author", "no TEST_COMMAND"),
)
def _witness_unavailable(run: Run) -> list[Finding]:
    """`proof` steps with `kind == "witness_unavailable"`, split by reason.

    One finding per distinct **normalized** reason, because the reasons name
    different defects: "produced no TEST_COMMAND line" is a parse contract
    failure, "emitted a test command but created no file" is an authoring
    failure, and "turn aborted: stuck-loop detected" is a loop defect. Filing
    them under one ticket buries two of the three.
    """
    from fingerprint import normalize

    if run.bare_loop:
        return []  # no witness stage exists on the bare loop; see below.

    groups: dict[str, list[tuple[Trial, str]]] = {}
    for trial in run.trials:
        for step in trial.proofs("witness_unavailable"):
            reason = str(step.get("reason") or "(no reason recorded)")
            groups.setdefault(normalize(reason), []).append((trial, reason))

    authored = sum(len(t.proofs("witness_authored")) for t in run.trials)
    attempted = authored + sum(len(g) for g in groups.values())

    findings = []
    for _, members in sorted(groups.items()):
        reason_full = members[0][1]
        occurrences = [
            Occurrence(
                trial_uuid=t.trial_uuid,
                task_id=t.task_id,
                s3_key=t.s3_key(),
                location="proof step.kind == witness_unavailable",
                # The reason is reproduced whole, deliberately: it is truncated
                # in the deck, in result.json and in every summary view.
                excerpt=f"reason (full):\n{r}",
            )
            for t, r in members
        ]
        rate = (
            f"Run-level witness authoring: {authored} authored, "
            f"{attempted - authored} unavailable, of {attempted} attempts."
        )
        findings.append(
            Finding(
                detector="witness-unavailable",
                site="crates/stella-pipeline/src/verify.rs",
                variant_source=reason_full,
                title=f"witness authoring failed: {reason_full[:90]}",
                summary=(
                    "The witness stage recorded `witness_unavailable`, so the run had "
                    "no fail->pass oracle and the verification ladder could not credit "
                    "a flip. The full reason is reproduced with each occurrence below."
                ),
                occurrences=occurrences,
                denominator=len(run.trials),
                search_terms=("witness_unavailable", "witness author", "no TEST_COMMAND"),
                detail=rate,
            )
        )
    return findings


# --------------------------------------------------------------------------
# 4. A flip the oracle saw and the grader did not
# --------------------------------------------------------------------------


@detector(
    code="oracle-flip-ungraded",
    title="the oracle recorded a baseline->candidate flip on a trial the grader scored 0",
    site="crates/stella-pipeline/src/verify.rs",
    search_terms=("oracle flip reward 0", "flip achieved grader rejected"),
)
def _oracle_flip_ungraded(run: Run) -> list[Finding]:
    """`proof` `step.kind == "oracle"` showing a flip, on a trial the grader failed.

    The oracle is the pipeline's own proof that the change did something: a
    `baseline` run that did not pass and a `candidate` run that did. When the
    verifier then scores the trial 0, the witness proved something the grader
    does not accept — either the witness is weaker than the task, or the proven
    work never reached the graded tree. Both are defects, and the trace cannot
    tell them apart on its own, which is stated rather than guessed.
    """
    if run.bare_loop:
        return []  # no oracle exists on the bare loop; see below.

    occurrences = []
    for trial in run.trials:
        if trial.reward is None or trial.reward > 0:
            continue
        oracles = trial.proofs("oracle")
        baseline_failed = any(
            o.get("tree") == "baseline" and o.get("passed") is False for o in oracles
        )
        candidate_passed = any(
            o.get("tree") == "candidate" and o.get("passed") is True for o in oracles
        )
        if not (baseline_failed and candidate_passed):
            continue
        occurrences.append(
            Occurrence(
                trial_uuid=trial.trial_uuid,
                task_id=trial.task_id,
                s3_key=trial.s3_key(),
                location=f"reward.txt = {trial.reward:g}",
                excerpt="\n".join(_brief(o) for o in oracles),
            )
        )
    if not occurrences:
        return []
    return [
        Finding(
            detector="oracle-flip-ungraded",
            site="crates/stella-pipeline/src/verify.rs",
            variant_source="oracle observed baseline fail and candidate pass on a trial graded 0",
            title="the oracle recorded a baseline->candidate flip on a trial the grader scored 0",
            summary=(
                "The witness oracle observed the flip it exists to observe — the check "
                "failed on the baseline tree and passed on the candidate tree — and the "
                "task verifier still scored the trial 0. The pipeline therefore held a "
                "proof of work that the grader does not accept as solving the task."
            ),
            occurrences=occurrences,
            denominator=len(run.graded_trials),
            search_terms=("oracle flip reward 0", "flip achieved grader rejected"),
            caveats=[
                "The trace cannot distinguish a witness weaker than the task from proven "
                "work that never reached the graded tree. Deciding which needs the "
                "witness body (`tool_start.call.input.content` on the `create_witness_test` "
                "call — `witness_author` has `output_text: null`, so its product is only "
                "in its tool calls) read against the task's own verifier."
            ],
        )
    ]


# --------------------------------------------------------------------------
# 5. An ERROR envelope wrapped around output the model needed
# --------------------------------------------------------------------------


@detector(
    code="tool-error-envelope",
    title="a tool_result returned an ERROR envelope around substantial useful output",
    site="crates/stella-tools/src/registry.rs",
    search_terms=("tool_result error envelope exit code", "bash non-zero exit useful output"),
)
def _tool_error_envelope(run: Run) -> list[Finding]:
    """`tool_result` carrying an error envelope whose message is real output.

    The `bash` shape: a chained command whose last element exits non-zero
    returns the whole transcript inside `output.error.message`, so the model is
    handed a page of correct, useful output labelled as a failure.

    **The variant is a constant here, and that is the point.** A variant exists
    to separate defects, not payloads, and this detector's payload is command
    output — pure run data, different in every trial. Keying the fingerprint on
    it shattered one defect into 23 tickets on the first run this was pointed
    at. So the predicate *is* the identity: everything it matches is the same
    defect, and the discriminator is spent on nothing.

    The `[exit code:` trailer is required rather than optional for the same
    reason. A guard refusal (`refused before running: … graded tree …`) is also
    an error envelope, and it is a different defect with its own issue — it
    carries no useful output, only a refusal, so demanding the trailer keeps the
    two apart without asking the normalizer to tell them apart by prose.
    """
    matches: list[tuple[Trial, Any]] = []
    for trial in run.trials:
        for call in trial.tool_calls:
            message = call.error_message
            if not message or "[exit code:" not in message:
                continue
            body = message.rsplit("[exit code:", 1)[0].strip()
            if len(body) < _SUBSTANTIAL_OUTPUT_CHARS:
                continue
            if body.count("\n") + 1 < _SUBSTANTIAL_OUTPUT_LINES:
                continue
            matches.append((trial, call))
    if not matches:
        return []

    occurrences = []
    seen_trials = set()
    for trial, call in matches:
        if trial.trial_uuid in seen_trials:
            continue
        seen_trials.add(trial.trial_uuid)
        occurrences.append(
            Occurrence(
                trial_uuid=trial.trial_uuid,
                task_id=trial.task_id,
                s3_key=trial.s3_key(),
                location=(
                    f"tool `{call.name}`, call_id `{call.call_id}`, "
                    f"stella-events.jsonl:{call.result_index}"
                ),
                excerpt=(
                    "call.input:\n"
                    + json.dumps(call.input, ensure_ascii=False, indent=2)[:800]
                    + "\n\noutput.error.message (full):\n"
                    + call.error_message
                ),
            )
        )
    return [
        Finding(
            detector="tool-error-envelope",
            site="crates/stella-tools/src/registry.rs",
            variant_source=(
                "error envelope wrapping substantial output before a nonzero exit trailer"
            ),
            title="a tool_result returned an ERROR envelope around substantial useful output",
            summary=(
                "These `tool_result` events carry an error envelope whose message is "
                "mostly successful output — the `&&`/`;`-chain shape, where one element "
                "exits non-zero and the whole transcript is relabelled a failure. The "
                "model is handed correct information marked as an error: it cannot use "
                "the output, and it retries a command that already answered."
            ),
            occurrences=occurrences,
            denominator=len(run.trials),
            search_terms=(
                "tool_result error envelope exit code",
                "bash non-zero exit useful output",
            ),
            detail=(
                f"{len(matches)} matching tool results across "
                f"{len(seen_trials)} trial(s) — one occurrence shown per trial."
            ),
            caveats=[
                "Requires the `[exit code: N]` trailer, so a guard refusal is not counted "
                "here: it is an error envelope too, but it wraps a refusal rather than "
                "output, and it is a different defect."
            ],
        )
    ]


# --------------------------------------------------------------------------
# 6. The same call, twice, with the same answer
# --------------------------------------------------------------------------


@detector(
    code="repeated-identical-tool-call",
    title="consecutive identical (tool, args) calls returned byte-identical output",
    site="crates/stella-core/src/driver.rs",
    search_terms=("identical consecutive tool call", "stuck loop repeated tool"),
)
def _repeated_identical_tool_call(run: Run) -> list[Finding]:
    """Adjacent tool calls with the same `(tool, args)` and the same output.

    Byte-identical on both sides is the strict form deliberately: a repeat with
    a *different* answer is a legitimate poll (a build finishing, a port coming
    up). A repeat with the same answer bought nothing and paid a full model call
    for it.
    """
    occurrences = []
    for trial in run.trials:
        calls = [c for c in trial.tool_calls if c.result is not None]
        for previous, current in zip(calls, calls[1:], strict=False):
            if previous.signature() != current.signature():
                continue
            if previous.output_signature() != current.output_signature():
                continue
            occurrences.append(
                Occurrence(
                    trial_uuid=trial.trial_uuid,
                    task_id=trial.task_id,
                    s3_key=trial.s3_key(),
                    location=(
                        f"tool `{current.name}`, calls `{previous.call_id}` then "
                        f"`{current.call_id}` (stella-events.jsonl:"
                        f"{previous.result_index} and {current.result_index})"
                    ),
                    excerpt=(
                        "identical call.input:\n"
                        + json.dumps(current.input, ensure_ascii=False, indent=2)[:800]
                        + "\n\nidentical output digest:\n"
                        + current.output_signature()[:400]
                    ),
                )
            )
    if not occurrences:
        return []
    return [
        Finding(
            detector="repeated-identical-tool-call",
            site="crates/stella-core/src/driver.rs",
            variant_source="consecutive identical tool call with byte-identical output",
            title="consecutive identical (tool, args) calls returned byte-identical output",
            summary=(
                "The engine issued the same tool call twice in a row and got the same "
                "bytes back. The second call bought no information and cost a full model "
                "call to request, so loop detection did not fire on a repeat it should "
                "see."
            ),
            occurrences=occurrences,
            denominator=len(run.trials),
            search_terms=("identical consecutive tool call", "stuck loop repeated tool"),
            caveats=[
                "Restricted to *adjacent* calls with identical output. A repeat separated "
                "by other calls, or one whose answer changed, is not counted — polling a "
                "build or a port is legitimate."
            ],
        )
    ]


# --------------------------------------------------------------------------
# 7. The census that disagrees with the launch configuration
# --------------------------------------------------------------------------


@detector(
    code="role-model-census-mismatch",
    title="the (role, model) census disagrees with the run's declared seat configuration",
    site="crates/stella-core/src/router.rs",
    search_terms=("role model census mismatch", "router resolved unexpected model"),
)
def _role_model_mismatch(run: Run) -> list[Finding]:
    """`(role, model)` from `step_usage` against the seat's declared roles.

    The declaration is read from the run's own `results.json`
    (`match.contestants[].engine`), which is the artifact copy of the match
    file, so the comparison is against what the run carried rather than against
    a config on someone's laptop. Model names are compared on their unqualified
    tail — the declaration writes `openrouter/deepseek/deepseek-chat` and the
    telemetry writes `deepseek/deepseek-chat` — because a provider prefix is a
    spelling difference, not a routing defect.
    """
    if not run.declared_roles and not run.declared_worker_model:
        return []

    def tail(model: str) -> str:
        parts = model.split("/")
        return "/".join(parts[-2:]) if len(parts) > 2 else model

    declared = {role: tail(model) for role, model in run.declared_roles.items()}
    if run.declared_worker_model:
        declared["worker"] = tail(run.declared_worker_model)

    occurrences = []
    for trial in run.trials:
        for (role, model), count in sorted(trial.role_census().items()):
            want = declared.get(role)
            if want is None or tail(model) == want:
                continue
            occurrences.append(
                Occurrence(
                    trial_uuid=trial.trial_uuid,
                    task_id=trial.task_id,
                    s3_key=trial.s3_key(),
                    location=f"role `{role}` ran {count} completed step(s)",
                    excerpt=(
                        f"declared (results.json match.contestants[].engine): {want}\n"
                        f"observed (step_usage.model)                      : {tail(model)}\n"
                        f"completed steps at the observed model            : {count}"
                    ),
                )
            )
    if not occurrences:
        return []
    return [
        Finding(
            detector="role-model-census-mismatch",
            site="crates/stella-core/src/router.rs",
            variant_source="step_usage role model disagrees with the declared seat roles",
            title="the (role, model) census disagrees with the run's declared seat configuration",
            summary=(
                "A `(role, model)` census taken off `step_usage` — completed calls only — "
                "does not match the roles the run was launched with. Every conclusion "
                "drawn from this run about a role's behaviour is a conclusion about a "
                "different model than the one named in the match."
            ),
            occurrences=occurrences,
            denominator=len(run.trials),
            search_terms=("role model census mismatch", "router resolved unexpected model"),
            caveats=[
                "Compared on the unqualified model tail, so a provider prefix difference "
                "is not reported. A role the seat does not declare is skipped rather than "
                "reported as unexpected — an undeclared role falls back by design."
            ],
        )
    ]


# --------------------------------------------------------------------------
# 8. A candidate that was never seen to be delivered
# --------------------------------------------------------------------------


@detector(
    code="no-post-verdict-file-change",
    title="a trial recorded no file_change after its verdict",
    site="crates/stella-cli/src/candidate_ws/adopt.rs",
    search_terms=("candidate adoption post-verdict file_change", "delivery emits no event"),
)
def _no_post_verdict_file_change(run: Run) -> list[Finding]:
    """Two variants, deliberately kept apart.

    A trial with **no `verdict` event at all** was killed before the pipeline
    reached its single delivery point; a trial **with** a verdict and no
    `file_change` after it reached delivery and recorded nothing. Those are
    different defects with different fixes, and the repository has already split
    them into two issues, so collapsing them into one finding would suppress
    exactly the residual half that the nothing-left-behind rule exists to keep.

    The caveat below is not decoration. `bash` names no path, so a heredoc, a
    `patch`, a `make` or a `>` mutates the tree with no `file_change` emitted at
    all. This detector reports **an absent event**, never an unchanged tree.
    """
    if run.bare_loop:
        # The bare turn loop has no verify stage, so it emits no `verdict` and
        # adopts no candidate. Running this detector against it reports 20/20 of
        # an absence that is the arm working exactly as designed — a finding
        # that is arithmetically true and entirely meaningless, and the kind of
        # number that gets quoted once and never withdrawn.
        return []

    absent_verdict: list[Occurrence] = []
    silent_delivery: list[Occurrence] = []
    for trial in run.trials:
        verdicts = trial.of_type("verdict")
        mutations = [e for e in trial.of_type("file_change") if e.get("kind") != "read"]
        if not verdicts:
            if not trial.events:
                continue
            last = trial.events[-1]
            absent_verdict.append(
                Occurrence(
                    trial_uuid=trial.trial_uuid,
                    task_id=trial.task_id,
                    s3_key=trial.s3_key(),
                    location=(
                        f"{len(trial.events)} events, no `verdict`; "
                        f"{trial.malformed_lines} unreadable line(s) at the tail"
                    ),
                    excerpt="last event on the stream:\n" + _brief(last),
                )
            )
            continue
        verdict_ts = verdicts[-1].get("ts")
        if not isinstance(verdict_ts, int):
            continue
        after = [e for e in mutations if isinstance(e.get("ts"), int) and e["ts"] >= verdict_ts]
        if after:
            continue
        silent_delivery.append(
            Occurrence(
                trial_uuid=trial.trial_uuid,
                task_id=trial.task_id,
                s3_key=trial.s3_key(),
                location=(
                    f"verdict at ts={verdict_ts}, "
                    f"{len(mutations)} mutating file_change events, 0 of them after it"
                ),
                excerpt="verdict event:\n" + _brief(verdicts[-1], limit=700),
            )
        )

    blindness = (
        "`file_change` is observability, never evidence: `bash` names no path, so a "
        "heredoc, `patch`, `make` or `>` mutates the tree and emits nothing. This "
        "finding reports an ABSENT EVENT. It does not, and cannot, establish that the "
        "tree was unchanged or that delivery did not happen."
    )

    findings = []
    if absent_verdict:
        findings.append(
            Finding(
                detector="no-post-verdict-file-change",
                site="crates/stella-cli/src/candidate_ws/adopt.rs",
                variant_source="trial recorded no verdict event at all",
                title="a trial ended with no verdict event — the delivery point was never reached",
                summary=(
                    "These trials carry no `verdict` event. The pipeline delivers a "
                    "candidate once, after the verdict, so a trial killed before that "
                    "point ends with its work still in the candidate workspace."
                ),
                occurrences=absent_verdict,
                denominator=len(run.trials),
                search_terms=("killed run never delivers", "no verdict event truncated"),
                caveats=[blindness],
            )
        )
    if silent_delivery:
        findings.append(
            Finding(
                detector="no-post-verdict-file-change",
                site="crates/stella-cli/src/candidate_ws/adopt.rs",
                variant_source="verdict recorded and no mutating file_change followed it",
                title="a trial recorded a verdict and no file_change after it",
                summary=(
                    "These trials reached a verdict and recorded no mutating "
                    "`file_change` afterwards. Adoption emits no event of its own, so a "
                    "withheld candidate and a delivered one that touched nothing are "
                    "indistinguishable on the stream."
                ),
                occurrences=silent_delivery,
                denominator=len(run.trials),
                search_terms=(
                    "candidate adoption post-verdict file_change",
                    "delivery emits no event",
                ),
                caveats=[blindness],
            )
        )
    return findings
