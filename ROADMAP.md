# ROADMAP — Verification Pipeline

Improvement proposals for the verification half of `stella-pipeline` — the
flip oracle, the evidence ladder, witness authoring, the judge escalation
path, and best-of-N candidate scoring (`verify.rs`, `witness.rs`,
`candidate.rs`, and the verify/revise wiring in `pipeline.rs`).

## Where we are today

The current design (L-E11) is deterministic-first and already avoids the two
classic failure modes: judges rubber-stamping plausible work, and "it passed,
ship it" false positives.

- **Flip oracle** (`verify::FlipOracle`): only a fail→pass flip of the *same
  normalized command* counts as deterministic verification.
- **Evidence ladder** (`verify::ladder_decision`): submit fast on strong
  evidence, revise on clear failure, escalate to the model judge only when
  evidence is genuinely inconclusive.
- **Witness authoring** (`witness`): when no `--test-command` is armed, an
  independent model authors the failing witness test, with tamper exclusion
  at verify time.
- **Candidate scoring** (`candidate`): `DeterministicPass > JudgePass >
  Unverified > Failed`, tie-broken by diff size.

The proposals below strengthen the *quality* of the evidence, not just its
presence.

## 1. Witness strength — a flip is necessary but not sufficient

A fail→pass flip proves the test *reacted* to the change; it does not prove
the test *constrains* the change well.

- **Mutation-check the witness (cheap variant).** After a flip, apply 1–3
  trivial mutations to the changed lines (negate a condition, off-by-one a
  bound) in a scratch candidate workspace and re-run the witness. If the
  witness stays green under every mutant, downgrade the evidence from
  `DeterministicPass` toward the judge path — the witness is likely
  tautological. Bounded cost: one extra test run per mutant, only on the
  winning candidate.
- **Assertion-density heuristic on authored witnesses.** *Done (#863).*
  `witness::density::screen_witness_source` is the static "test must be able
  to fail *meaningfully*" check beside "test must fail first": it refuses a
  witness with no assertions, one asserting only over constants, one
  comparing a value to itself, and a bare `#[should_panic]` /
  `raises(Exception)`. Enforced at `create_witness_test` — the only path
  witness bytes take to disk — so the refusal lands *inside the author's own
  turn* and costs no extra model invocation, rather than after a baseline run
  and a repair turn as sketched here.
- **Diff-coverage overlap.** Where coverage tooling is available (e.g.
  `cargo llvm-cov`), check that the witness actually executes some of the
  changed lines. A flip whose test never touches the diff is a coincidence,
  not evidence. Make this an *optional* ladder input so environments without
  coverage tooling degrade gracefully.

## 2. Flakiness — protect the oracle's invariant from nondeterminism

The oracle's `Flipped → Failing` regression edge is honest, but a flaky test
can produce a *false flip* (fails for an unrelated reason, then passes).

- **Confirmation run on flip.** On the transition to `Flipped`, re-run the
  tracked command once. Pass again → confirmed flip. Fail → mark the command
  *unstable* and route to the judge with that fact in evidence instead of
  crediting a deterministic pass.
- **Failure-fingerprint matching.** Record a fingerprint of the failing
  observation (failed test names / panic message extracted from runner
  output). At flip time, verify the *same tests* that failed are now passing.
  A pass that "fixes" a different failure than the one observed should score
  as `NoEvidence`, mirroring the existing same-command rule at the level of
  same-*failure*.
- **Typed timeout/infra outcomes.** Distinguish "test failed" from "runner
  timed out / OOM / toolchain missing" in `TestRunner`'s outcome. Today an
  infra failure can lock the oracle onto a command that never had a real
  failing assertion.

## 3. Secondary deterministic evidence (without weakening L-E11)

Lint/typecheck are rightly excluded from the flip oracle. But they can still
*veto* and *inform*:

- **Regression veto.** Diff `DiagnosticRunner` results before/after the
  candidate: new errors (or new warnings, configurable) block fast-submit
  even when the flip holds. A flipped witness plus a fresh type error in an
  untested module is exactly the inconclusive case the judge exists for.
- **Impacted-test scope for Rust.** *Done at the tool level (#443, #862).*
  `run_tests scope=impacted` resolves Rust `use`/`mod` edges through the
  workspace module tree — including cross-crate paths — and narrows to the
  owning cargo packages; an unrelated crate is left out, and a missing or
  stale index still stands down loudly. What remains is **using** it as
  ladder evidence: `observe_touched_tests` runs exactly one typed invocation
  (the configured `--test-command`, else the witness command), so the ladder
  still chooses between one witness command and a full workspace run. Feeding
  it the impacted selection means composing a *typed* `cargo test -p …`
  invocation for the ladder, not shelling the tool.
- **Touched-tests set widening.** After revise turns, re-derive the
  touched-tests set from the *final* diff, not the first one — revisions can
  touch files whose tests were never consulted.

## 4. Judge escalation — make the inconclusive path richer

- **Structured judge evidence.** Extend `JudgeEvidence` with the oracle
  trace (observations in order, normalized commands, outcomes), the
  diagnostics delta, and the witness-tamper check result — not just the diff
  and a summary. A judge that sees *why* the ladder was inconclusive makes a
  better call than one shown a diff cold.
- **Judge verdict calibration telemetry.** Persist (locally) each
  `JudgePass` verdict alongside later ground truth when it arrives (CI
  results via `ci_status`, subsequent user revert/rollback of the adopted
  change). Over time this measures the judge's false-positive rate and can
  auto-tune when the ladder escalates vs. revises.
- **Distress guidance earlier for repeated identical failures.** Guidance
  currently triggers on the second consecutive red verification. If two
  consecutive failures have the *same* failure fingerprint, the worker is
  looping — trigger guidance immediately rather than spending another
  identical revise turn.

## 5. Best-of-N scoring — refine within `DeterministicPass`

`CandidateScore` ties within a rank break on diff size alone. Within
`DeterministicPass`, prefer:

1. survived the mutation check (§1) over not-run/not-survived,
2. no new diagnostics over new warnings,
3. smaller diff (current tie-break).

This keeps the existing coarse ordering intact while making the winner among
several verified candidates the *best-verified* one, not merely the smallest.

## 6. Verification honesty & replay

- **Verdict provenance in `PipelineOutcome`.** Attach the full ladder input
  snapshot (flip state + tracked command, touched-tests status, diff size,
  diagnostics delta) to `Verdict`, so `replay` can answer "why did this run
  fast-submit?" without re-deriving it.
- **Seal-check coverage.** The post-verification seal check (worktree changed
  after verification → abort) exists for candidates; extend the same
  fingerprint discipline to the *witness file itself* between the revise
  turns of a single candidate, closing the window where a worker edits the
  witness mid-loop.

## Suggested sequencing

| Phase | Items | Rationale |
|-------|-------|-----------|
| 1 | §2 confirmation run, §2 typed infra outcomes, §3 regression veto | Small, pure-logic changes in `verify.rs`/ports; directly reduce false `DeterministicPass` |
| 2 | ~~§1 assertion-density check~~ (done, #863), §4 structured judge evidence, §6 verdict provenance | Improves authored-witness quality and judge inputs with no new tooling deps |
| 3 | §2 failure fingerprints, §4 early distress guidance, §5 score refinement | Builds on the fingerprint machinery from phase 1–2 |
| 4 | §1 mutation check, §1 diff-coverage, ~~§3 Rust impacted selection~~ (done at the tool level, #443/#862 — the ladder wiring remains), §4 calibration telemetry | Larger investments; each degrades gracefully when unavailable |

Each phase preserves the two core invariants: `Flipped` is reachable only
through `Failing` of the same normalized command, and the judge never
overrides a deterministic failure.
