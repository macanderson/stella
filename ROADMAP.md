# ROADMAP — Verification Pipeline

Improvement proposals for the verification half of `stella-pipeline` — the
flip oracle, the evidence ladder, witness authoring, the verifier escalation
path, and best-of-N candidate scoring (`verify.rs`, `witness.rs`,
`candidate.rs`, and the verify/revise wiring in `pipeline.rs`).

## Related tracks

This roadmap covers verification only. The **self-improvement** track — Stella
authoring its own tools, tuning its own policy from evals, maintaining its own
source, and distilling its own trajectories (issues #830–#836) — is documented
at [`website/content/docs/self-improvement.mdx`](website/content/docs/self-improvement.mdx),
published as [Self-improvement](https://stella.oxagen.sh/docs/self-improvement).

The two are coupled in one direction: every self-authored change in that track
has to clear the machinery described here. A self-authored tool needs a witness
that flips, and a self-authored PR needs the same evidence ladder as any other
work. Weakening verification weakens every self-improvement guarantee that
depends on it.

## Where we are today

The current design (L-E11) is deterministic-first and already avoids the two
classic failure modes: verifiers rubber-stamping plausible work, and "it passed,
ship it" false positives.

- **Flip oracle** (`verify::FlipOracle`): only a fail→pass flip of the *same
  normalized command* counts as deterministic verification.
- **Evidence ladder** (`verify::ladder_decision`): submit fast on strong
  evidence, revise on clear failure, escalate to the model verifier only when
  evidence is genuinely inconclusive.
- **Witness authoring** (`witness`): when no `--test-command` is armed, an
  independent model authors the failing witness test, with tamper exclusion
  at verify time.
- **Candidate scoring** (`candidate`): `DeterministicPass > VerifierPass >
  Unverified > Failed`, tie-broken by diff size.

The proposals below strengthen the *quality* of the evidence, not just its
presence.

## 1. Witness strength — a flip is necessary but not sufficient

A fail→pass flip proves the test *reacted* to the change; it does not prove
the test *constrains* the change well.

- **Mutation-check the witness (cheap variant).** *Done (#870).* The
  pre-submit audit breaks the candidate's changed lines one at a time
  (`verify::mutation`, ≤3 single-line mutants from the diff, witness files
  excluded) and re-runs the witness per mutant — in place with a byte-exact
  restore (`FsMutationProbe`; a failed restore fails the candidate closed).
  A witness that fails under any mutant keeps its credit (early exit at the
  first kill); one that stays green under every observed mutant is
  tautological: the fast-submit is withheld and the verifier decides with
  `witness_tautological=true` in evidence. Authored witnesses only.
- **Assertion-density heuristic on authored witnesses.** *Done (#863).*
  `witness::density::screen_witness_source` is the static "test must be able
  to fail *meaningfully*" check beside "test must fail first": it refuses a
  witness with no assertions, one asserting only over constants, one
  comparing a value to itself, and a bare `#[should_panic]` /
  `raises(Exception)`. Enforced at `create_witness_test` — the only path
  witness bytes take to disk — so the refusal lands *inside the author's own
  turn* and costs no extra model invocation, rather than after a baseline run
  and a repair turn as sketched here.
- **Diff-coverage overlap.** *Done (#1291).* A `CoverageProbe` port runs the
  tracked command under the workspace's own coverage tool in the pre-submit
  audit — `cargo llvm-cov` (LCOV) and `pytest --cov` (coverage.py JSON), the
  two dialects `verify::fingerprint` can already read test output for — and
  `verify::coverage` intersects the executed lines with the diff's added
  ones. Three-valued, and **neither non-`covered` answer is a pass**:
  - `not_covered` (measured, no overlap) withholds the deterministic credit
    and escalates to the verifier — the flip is a coincidence, which is worth a
    second opinion. Never a failure, never a deterministic red.
  - `unmeasured` (no tooling, no probe, an unreadable report) takes the
    fast-submit — no verifier call, no extra turn — but is **scored
    `Unverified`**, with the verdict summary leading `UNPROVEN` and the status
    on the ladder snapshot. The honest answer costs a ranking position rather
    than a model call, which is what makes it affordable by default; escalating
    instead would tax every workspace without coverage tooling (the #1295
    result). `require_diff_coverage` turns that stricter reading on for an
    operator who has the tooling and wants the overlap enforced.

  `PipelineOutcome::score` surfaces the grade so a host can see the
  distinction a `verdict.deterministic` flag alone would hide.

## 2. Flakiness — protect the oracle's invariant from nondeterminism

The oracle's `Flipped → Failing` regression edge is honest, but a flaky test
can produce a *false flip* (fails for an unrelated reason, then passes).

- **Confirmation run on flip.** *Done (#859).* Gated on the *decision*
  rather than the flip transition: only a run about to claim `SubmitFast`
  pays the one extra suite run. A failed (or infra, #860) confirmation moves
  the oracle to `Unstable` — not `Flipped`, not `Failing` — and the verifier is
  told `unstable_flip=true`.
- **Failure-fingerprint matching.** *Done (#867).* `FlipOracle::observe_run`
  records the failing test names each failing observation reports
  (`verify::fingerprint`: libtest + pytest, both orders); a pass that names
  its tests without naming the baseline's failures is `NoEvidence` — the
  fix-by-disappearance case (delete the failing test, suite exits 0). The
  refusal requires a demonstrably COMPLETE pass listing (the runner's own
  summary count must equal the names parsed), so a truncated tail can never
  fail an honest fix; every dark input degrades to the exit code.
- **Typed timeout/infra outcomes.** *Done (#860).* `CmdOutcome` carries
  `CmdKind {Completed, TimedOut, OutOfMemory, Infra}`; verification consumes
  `assertion_result()`, so an infra "failure" can neither lock the oracle
  nor read as a deterministic red (it escalates with `test_run=<label>` in
  evidence), and the witness fail-first gate degrades honestly. A
  segfaulting test stays `Completed` — that is a real failure.
- **Out-of-memory kills.** *Done (#1294).* The residual ambiguity #860 left
  is closed: `stella_pipeline::oom` classifies a run the machine killed for
  memory (`SIGKILL`, exit `137`, or a runtime's own allocation-failure
  message on a run that failed), the runner reports it as
  `CmdKind::OutOfMemory`, and every test run in the pipeline goes through
  one retrying entry point (`run_test_observed`, `test_oom_retries`, one
  retry by default). Retry rather than revise is the whole point: the run
  observed no assertion, so telling a worker its work failed asks it to
  "fix" code no test ever judged. A kill that survives its retry escalates
  with `test_run=out_of_memory` — its own word, never `infra_failure` and
  never a deterministic red. The kernel log is deliberately not read (see
  the module docs for why attribution there is unsafe).

## 3. Secondary deterministic evidence (without weakening L-E11)

Lint/typecheck are rightly excluded from the flip oracle. But they can still
*veto* and *inform*:

- **Regression veto.** *Done (#861).* A `LintProbe` port runs the
  workspace's own diagnostics plan; a set-difference against the
  pre-execution baseline (identity excludes line/col) vetoes fast-submit on
  new errors, warnings opt-in via `diagnostics_veto_warnings`. Runs only in
  the pre-submit audit — lint before the confirmation run, since a veto
  makes the confirmation moot — and degrades open on every unavailable
  path.
- **Impacted-test scope for Rust.** *Done at the tool level (#443, #862).*
  `run_tests scope=impacted` resolves Rust `use`/`mod` edges through the
  workspace module tree — including cross-crate paths — and narrows to the
  owning cargo packages; an unrelated crate is left out, and a missing or
  stale index still stands down loudly. What remains is **using** it as
  ladder evidence, and the constraint that shapes it: the oracle's identity
  is the *normalized command*, and the impacted selection is derived from
  the diff — which does not exist when the baseline pre-run fires. A
  per-turn narrowed command would differ between baseline and candidate and
  the oracle would rightly ignore the pass. The viable route is the
  isolated-candidate path, where the session tree stays pristine through
  execution: both halves of the flip can be observed at verify time with
  the *same* diff-derived `cargo test -p …` invocation. That is a
  restructuring of when the baseline is observed, not a bolt-on — design
  first, then build.
- **Touched-tests set widening.** *Superseded.* The ladder runs exactly one
  typed invocation per iteration (the configured `--test-command`, else the
  witness command) and re-runs it after every revise turn, so there is no
  per-file "touched set" left to widen — the concern this item named is
  covered by the per-iteration re-observation.

## 4. Verifier escalation — make the inconclusive path richer

- **Structured verifier evidence.** *Done (#864).* `VerifierEvidence` carries the
  full `LadderSnapshot` (oracle trace in observation order, diagnostics
  delta, tamper-check result, and every later audit finding), and the verifier
  prompt renders it compactly — one `oracle_trace=[…]` fragment and a ≤3-line
  lint sample, never a log dump.
- **Verifier verdict calibration telemetry.** *Done (#871), first slice.*
  Everything needed was already persisted (verdicts with snapshots, PR/CI
  observations in the same stream), so `replay::calibration` folds it and
  `stella calibration` reports the verifier's measured false-positive rate
  beside the deterministic cohort's — unmeasured stays unmeasured, never 0%.
- **An answer key: reverts, and verdicts that arrive late.** *Done (#1293).*
  Two gaps in the first slice are closed. `replay::ground_truth` reads git's
  own `This reverts commit <sha>` marker, so a human undoing Stella's work is
  a ground-truth source — counted apart from red CI, because a revert is a
  later and better-informed statement. And `calibration_pending` carries a
  session's unsettled passes out of the fold with the commits and PRs they
  cover, so a terminal verdict recorded in *another* session (or a revert
  landing weeks later) still reconciles them. The asymmetry is deliberate and
  enforced: a revert settles a pass as a false positive, while the ABSENCE of
  one confirms nothing and leaves the pass out of every denominator.
  Threshold auto-tuning still waits on real measured data — which this is the
  instrument for.
- **Distress guidance earlier for repeated identical failures.** *Already
  satisfied (#868, closed).* The trigger fires on the second consecutive
  deterministic failure unconditionally — the timing this item asked for —
  and cannot fire earlier, since there is no repetition to detect before a
  second failure exists. The oracle-level fingerprints (#867) are available
  if a future change relaxes the base timing.

## 5. Best-of-N scoring — refine within `DeterministicPass`

*Done (#869, completed by #870).* Within a rank, selection prefers
mutation-survived, then fewer new diagnostics, then the smaller diff — all
read off the verdict's own provenance snapshot, so selection needed no new
plumbing. The coarse ordering is untouched and pinned by test: a warning-free
verifier-pass never beats a warned deterministic pass.

## 6. Verification honesty & replay

- **Verdict provenance in `PipelineOutcome`.** *Done (#865).* Every verdict
  (and every emitted `VerifierVerdict` event) carries the `LadderSnapshot`
  frozen at decision time; `replay::verdict_provenance` renders "why?" from
  the recording alone, and a pre-snapshot stream reads as *not recorded*,
  never reconstructed.
- **Seal-check coverage.** *Already satisfied, now pinned.* The witness
  tamper exclusion runs every verify-loop iteration, so a revise turn that
  edits the witness is caught at the next iteration's check and hard-fails
  the candidate — pinned by
  `a_revise_turn_that_edits_the_witness_hard_fails_at_the_next_check`.

## Suggested sequencing

| Phase | Items | Status |
|-------|-------|--------|
| 1 | §2 confirmation run (#859), §2 typed infra outcomes (#860), §3 regression veto (#861) | **Done** — PR #1033 |
| 2 | §1 assertion-density check (#863), §4 structured verifier evidence (#864), §6 verdict provenance (#865) | **Done** — #863 earlier; PR #1035 |
| 3 | §2 failure fingerprints (#867), §4 early distress guidance (#868, already satisfied), §5 score refinement (#869) | **Done** — PR #1049 |
| 4 | §1 mutation check (#870), §4 calibration telemetry (#871) | **Done** — PRs #1052, #1055 |
| — | §1 diff-coverage (needs a coverage-tooling decision), §3 impacted selection as ladder evidence (see the design constraint above) | Remaining |

The per-PR degradation gate (`pipeline/tests/degradation_gate.rs`, PR #1038,
`docs/design/verification-gate.md`) now pins every decision and its spend, so each
later change to this pipeline lands against a matrix that fails loudly on
drift.

Each phase preserves the two core invariants: `Flipped` is reachable only
through `Failing` of the same normalized command, and the verifier never
overrides a deterministic failure.
