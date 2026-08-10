# stella-pipeline

The orchestration plane above `stella-core::Engine`. It drives one prompt through the
staged turn flow — **evaluate → enhance → route → execute → deterministic verify → bounded
revise** — over injected ports, emitting an `AgentEvent` at every stage boundary. Live
verification is deterministic-only; historical witness/verdict stage tokens remain readable
but are never dispatched. This is the default `stella run` path.

Two boundaries define the crate. First, **no I/O**: it never imports a provider SDK, a
shell, a context store, or a terminal — everything crosses the traits in
[`src/ports.rs`](src/ports.rs), which the `stella-cli` glue implements against the real
subsystems. Second, **no decisions in the async code**: [`src/pipeline.rs`](src/pipeline.rs)
is I/O sequencing only, and every judgement it makes is delegated to a synchronous function
over owned data in a sibling module (`triage`, `plan`, `scope`, `verify`, `witness`,
`candidate`) — which is what keeps the hard logic property-testable.

## Where it sits

Depends on `stella-protocol` (wire types and `AgentEvent`) and `stella-core` (`Engine`,
`Router`, `BudgetGuard`, `ToolExecutor`, `Sleeper`), plus `tokio`, `async-trait`, `serde`,
`serde_json` and `thiserror`. Nothing in the workspace depends on it except `stella-cli`,
which owns the port implementations and the `Router` itself. It builds no binary.

## Boundary — does this change belong here?

This crate owns the staged flow itself: which stages run for a given prompt, in what
order, on what evidence — triage classification, plan shape, the scope gate, the flip oracle
and evidence ladder, deterministic failure receipts, the revision policy, best-of-N
selection. The decision rule: if a change alters what
happens between a prompt arriving and a verdict being declared — a stage added or
skipped, a gate's threshold, what a failure receipt may disclose, or what grants completion
authority — it lands here, with the judgement in a synchronous sibling module and only
the sequencing in [`src/pipeline.rs`](src/pipeline.rs), per the two intro boundaries.

Engine step-loop mechanics never land here. How one model-call/tool loop retries,
compacts, detects loops, or consults the budget between steps is
[`stella-core`](../stella-core)'s `Engine::run_turn`; the pipeline composes turns and
forwards their events, and a change that needs to reach *inside* a step — to see an
individual tool call before the turn settles — is a core change, not a new pipeline
stage. Likewise no concrete subsystem: a provider SDK, a shell, a store, a terminal
are all implementations of the traits in [`src/ports.rs`](src/ports.rs) and belong in
the `stella-cli` glue.

Two look-alikes route elsewhere. The `verify_done` *tool* — the shadow-worktree check
a model calls to prove its own change, `crates/stella-tools/src/verify.rs` — enforces
the same fail-to-pass contract as this crate's configured-command oracle, so it evolves in
[`stella-tools`](../stella-tools); this crate owns the typed final-state replay that consumes
its receipt, not the tool's filesystem implementation. And
fan-out of *many tasks* — a DAG with dependencies, claims, and a durable ledger — is
[`stella-fleet`](../stella-fleet); best-of-N here is many candidates for *one*
prompt, selected and discarded within a single run.

Resist splitting a new crate rather than extending this one. A new crate is justified
only when the functionality sits behind a port and would drag heavy new dependencies
into a crate that is deliberately light (this crate's short dependency list is a
feature), when it needs a dependency direction the current graph forbids (two crates
that must not know each other needing a shared home — how `stella-home` earns its
row), or when it is a genuinely separate deliverable with its own binary or release
cadence (`stella-serve`). Otherwise extend this crate: a new crate costs a
workspace-table row, an impacted-crates scope, CI time, and a README, and a wrong
split is harder to undo than a wrong merge. Adding one means updating AGENTS.md's
workspace table and the root `Cargo.toml` members in the same PR.

## God files — do not add lines

The gate's file-size guard (`scripts/check-file-size.sh`) enforces a 1500-line
ratchet: a *new* file over the limit is a hard failure with no baseline escape, and
the two files below are grandfathered at a recorded ceiling in
`scripts/file-size-baseline.txt`. They are god files — already too big, closed to
growth. Plan changes so no new line lands in them: new stage logic goes in a
submodule under [`src/pipeline/`](src/pipeline), the crate's own precedent
(`fanout_stage.rs`, `scope_stage.rs`, `stage_budget.rs` were all
split out of [`src/pipeline.rs`](src/pipeline.rs) this way); new tests go in a child
module under [`src/pipeline/tests/`](src/pipeline/tests) rather than in
[`src/pipeline/tests.rs`](src/pipeline/tests.rs), which keeps only the shared fakes;
and code you touch in either file is a candidate to extract.

- [`src/pipeline.rs`](src/pipeline.rs)
- [`src/pipeline/tests.rs`](src/pipeline/tests.rs)

A ceiling can move only via `make file-size-update`, which lands as a reviewable
baseline diff justified like any other change — treat it as an escape hatch for an
irreducible line (a module declaration that must live in the oversized file), never
as a planning assumption.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The index of design lessons the crate encodes (L-E2, L-E5–E8, L-E11, L-M4) and the public re-exports. Read it first — every claim below is stated there in one line. |
| [`src/pipeline.rs`](src/pipeline.rs) | `Pipeline::run` and the whole stage sequence, `PipelineConfig`, `PipelineOutcome`. Open it when you need the *order* things happen in. |
| [`src/pipeline/revision.rs`](src/pipeline/revision.rs) | The bounded command/exit/stdout/stderr receipt handed to the worker after a completed failed test. |
| [`src/pipeline/raw_usage.rs`](src/pipeline/raw_usage.rs), [`src/pipeline/run_error.rs`](src/pipeline/run_error.rs), [`src/pipeline/stage_budget.rs`](src/pipeline/stage_budget.rs) | The metered direct-completion path for live management roles such as triage; `PipelineError`/`PipelineRunError`; the budget-abort translation. Historical role shapes remain for replay compatibility. |
| [`src/ports.rs`](src/ports.rs) | Every trait the pipeline orchestrates over, plus the no-op defaults (`NoContextRecall`, `NoRepoStructure`, `NoRepoStatus`, `AlwaysAbortGate`). |
| [`src/triage.rs`](src/triage.rs) | `TaskClass`, `TaskAssessment`, the response parser, and the deterministic pattern floor. |
| [`src/plan.rs`](src/plan.rs) | The planner's split context (`build_planner_prompt`) and the JSON-then-numbered-list `parse_plan`. |
| [`src/scope.rs`](src/scope.rs) | `ScopeThresholds` and the pure `needs_scope_review` / `apply_trim` / `build_proposal`. |
| [`src/witness.rs`](src/witness.rs) | Historical witness prompt/parsing and artifact-validation compatibility helpers; no live model stage calls them. |
| [`src/witness/airlock.rs`](src/witness/airlock.rs) | Historical disclosure and redaction helpers still used for bounded operator evidence, never as model-authored worker guidance. |
| [`src/verify.rs`](src/verify.rs) | `FlipOracle`, `ladder_decision`, deterministic evidence, plus compatibility-only verifier prompt/parsing helpers. |
| [`src/reward.rs`](src/reward.rs) | The ladder verdict as a training label (#1043): `RewardPolicy` (the per-workspace weights), `label`, and the `DiscardReason` set. Pure. Read it before changing what a rung is worth — most of the file is the argument for what the module refuses to claim. |
| [`src/candidate.rs`](src/candidate.rs) | `CandidateScore` and `select_best_candidate` — best-of-N selection, pure. |
| [`src/candidate_fanout.rs`](src/candidate_fanout.rs) | `fan_out_width` and `FanOutBudget` — how wide a fan-out runs and how one turn's money is split between candidates spending at the same time. Pure; the normative statement of the overshoot window. |
| [`src/pipeline/fanout_stage.rs`](src/pipeline/fanout_stage.rs) | The concurrent dispatch itself: workspace creation (serialized), `buffer_unordered` over the candidates, results re-ordered by index. |
| [`src/mcp_prefetch.rs`](src/mcp_prefetch.rs) | `fold`: shared MCP context gathered once at the top of a fan-out instead of N times. |
| [`src/replay.rs`](src/replay.rs), [`src/replay/golden.rs`](src/replay/golden.rs) | `validate_stream` / `structural_diff` / `parse_jsonl`, and the golden fixture format with its provenance manifest. |

## Key concepts

**The stage flow, and who owns the terminal event.** `stella-core::Engine::run_turn` emits
its own `Stage { Execute }`, `Stage { Complete }` and `Complete` — correct for one turn,
wrong for a multi-step plan or a revise loop. So the pipeline gives each `run_turn` a
private channel and forwards everything *except* the engine's `Stage`/`Complete`, then emits
the terminal `Complete` or a non-retryable `Error` itself. Triage and context recall are
overlapped with `tokio::join!` (no data dependency); the recall latency ceiling sits inside
the recall future, but triage's ceiling deliberately still awaits the paid call so its usage
cannot vanish from accounting through cancellation.

**Triage decides how much ceremony to buy** ([`src/triage.rs`](src/triage.rs)). `TaskClass`
is ordered `SimpleLookup < SingleTask < MultiStep` and the derived `Ord` is load-bearing:
`deterministic_floor` takes the `max` of the model's class and a pattern floor, so the floor
can only ever *add* planning. A misclassified task must still complete, just more slowly —
that is why `SimpleLookup`'s verification skip self-revokes through the zero-diff guard
(the `files_touched` / `should_verify` pair in `Pipeline::run_candidate`: a lookup is
verified after all if `file_changes > 0` or the diff is non-empty). `TaskAssessment` carries `conversational` on its own field rather than as a
fourth class, because "is this even a task" is a different axis from "how big is it"; a bare
`hi` takes one plain completion and skips plan → execute → verify entirely.

**The flip oracle.** Only a fail→pass flip of the *same normalized
command* counts (`FlipOracle` in [`src/verify.rs`](src/verify.rs)): `none → failing →
flipped`, locking onto the first command it sees fail. A pass with no prior failure is
`NoEvidence` and does not even lock a command. Whitespace is normalized; token order is not,
because a pass of `cargo test -p a` must never be credited to a failure of `cargo test -p b`.
The second deterministic path is the concrete built-in `verify_done` capability. The
registry records only a request that actually confirmed, the pipeline drains that typed
request, seals the candidate, and replays it against the final state. An arbitrary tool that
prints the historical success marker has no authority, and a later edit cannot inherit a
stale confirmation.

**The evidence ladder never spends a verifier call.** `ladder_decision` has three outcomes:
a configured-command or replayed `verify_done` flip → `SubmitFast`; a completed candidate
test failure → `Revise`; everything else → `Unverifiable`. A green candidate test without a
failing baseline, an unavailable runner, a timeout, OOM, unreadable diff, lint, and coverage
are observations, not correctness verdicts. Historical verifier roles and ladder rungs
remain decodable, but no live orchestration call site can dispatch them.

**A failed test tells the worker exactly what happened.** Before another solution is tried,
[`src/pipeline/revision.rs`](src/pipeline/revision.rs) renders the runner's bounded normalized
command, exit code, stdout, and stderr. No verifier prose, distress guidance, or model-written
test can enter that prompt. If the process did not complete, there is no execution result to
hand off and the pipeline ends unverified instead of asking the worker to fix an invented
failure.

**A best-of-N fan-out runs its isolated candidates at once**
([`src/pipeline/fanout_stage.rs`](src/pipeline/fanout_stage.rs)). Isolation already
guaranteed siblings never see each other's edits, so the only thing making
`candidates = Some(n)` cost the *sum* of n runtimes rather than the slowest was the
`&mut BudgetGuard` threaded through `run_candidate`. Three consequences worth knowing:

- **Money is split, not shared.** Each candidate spends against a `BudgetGuard::carve` of
  `headroom / width` and settles it back on the way out, gated before dispatch against the
  shared parent. Aggregate spend stays at the cap plus one in-flight window — at most
  `width` model calls, since the budget is consulted between steps and never mid-call.
  `BudgetGuard::carve` puts its ceiling on the *session* axis by contract, so a candidate
  that trips a turn cap now names the session axis in its abort text; the money is the same.
- **Only the isolated path is concurrent.** The shared-tree degradation (`candidates > 1`
  with no `CandidateWorkspacePort`) stays sequential — those candidates execute into one
  working tree. `git worktree` creation is serialized too.
- **Live previews are muted while the lane is shared.** `TextDelta` and `Reasoning` are
  the two events the wire contract already calls best-effort, and splicing three models'
  fragments into one paragraph is worse than showing none. Every durable event — each
  candidate's authoritative `Text`, its tool calls, file changes and proof steps — still
  goes out live, and the fan-out narrates the mute so a quiet stream is not mistaken for a
  stalled one. `candidate_concurrency: Some(1)` restores strictly sequential dispatch.

**Degrade, never do nothing.** `Pipeline::run` re-runs a bare worker turn when candidate
setup failed before the worker ever ran. Execution aborts (budget, loop, step-cap) keep their
stop — the worker did run.

## Gotchas

- **The flip baseline is a real observation from this candidate's own pre-execution
  snapshot**, recorded before execution and restored only when a resumed run names the same
  normalized command. It is never seeded from another candidate's surface or assumed.
- **An empty diff is never reported as "nothing changed" when `FileChange` events fired.**
  `verification_honest_diff` substitutes an explicit "the change is real; the diff is blind
  to it" note so stored evidence distinguishes an empty change from an unavailable probe.
- **A review waived by triage scores `Unverified`, not `DeterministicPass`** — claiming the
  strongest score would let a waived candidate tie a flip-verified sibling in best-of-N and
  then win the smaller-diff tiebreak.
- **The pipeline holds `&Router`, so it reads resolutions but never feeds the breaker.**
  `record_success`/`record_failure` need `&mut Router`; that feedback belongs to the glue
  that owns the router. A headless run crossing the scope thresholds with no bypass is
  likewise a named error (`PipelineError::ScopeReviewRequiredHeadless`), never a silent
  auto-approve.
- **`docs/*.md` paths cited from rustdoc here are gated.** `make doc-citations` fails if a
  cited path — or a cited `§N` — does not resolve; `src/replay.rs` and
  `src/replay/golden.rs` both cite `docs/spec/replay-golden-trajectories.md`.

## Testing

```bash
cargo test -p stella-pipeline
make record-golden            # STELLA_REFRESH_GOLDEN=1 … --lib golden; review the diff
```

There is no `make test-pipeline` target. Everything runs offline against scripted provider
and port doubles — no API key, no network.

- **Unit tests** sit beside the code ([`src/pipeline/tests.rs`](src/pipeline/tests.rs) and
  [`src/pipeline/tests/`](src/pipeline/tests)), split by concern: `verification_boundary`,
  `best_of_n` (and its child `fanout_concurrency`), `terminal_outcomes`, `usage`,
  `telemetry`, `management_accounting`, and `mcp_prefetch`. They are child modules so they reach `CandidateSurface` and the other
  private surface via `super::*`; the shared fakes (`run_isolated`, `FakeWorkspacePort`)
  stay in the common ancestor `tests.rs`. `chaos.rs` enumerates the config × environment
  cross-product — provider behavior × isolation × task class × single/multi-model × budget
  × headless bypass — asserting the loop invariants for every combination;
  exhaustive rather than `proptest`-driven because the space is small enough.
- **Property tests** (`proptest`) cover the flip oracle in
  [`src/verify.rs`](src/verify.rs) — `flip_requires_a_prior_failing_observation` is the one
  proving `Flipped` is unreachable without a prior `Failing` of the same command.
- **Replay fixtures** are two distinct things, deliberately kept apart.
  [`tests/fixtures/`](tests/fixtures) holds *synthetic*, hand-authored streams
  (`single_task_flip.jsonl`, `verifier_escalation.jsonl`, `torn_tail.jsonl`) exercising the
  invariants and the differ, driven from
  [`tests/replay_fixtures.rs`](tests/replay_fixtures.rs).
  [`tests/fixtures/golden/`](tests/fixtures/golden) holds real recordings of this
  pipeline's own event stream, asserted by `src/pipeline/tests/golden.rs`. Both sides of
  that comparison are the same code, so goldens are a **drift baseline** — they catch a
  stage that stopped being emitted or a tool that changed name — not independent evidence.
  A recording parsing to a different length than its manifest's `event_count` is a
  `GoldenError::Truncated`: `parse_jsonl`'s torn-tail tolerance is right for a live reader
  and wrong for a committed fixture.
  [`tests/reference_conformance.rs`](tests/reference_conformance.rs) pins the adapter
  contract an *independent* engine must satisfy before its runs could join them.

## Extending it

**Adding a test-runner form to the witness vocabulary** is the most common change, and it
touches three functions in [`src/witness.rs`](src/witness.rs) that must agree:

1. `parse_test_invocation` — accept the program/argv shape, and add its path-escape and
   working-directory flags to `validate_local_args` (`--manifest-path`, `--cwd`, `--rootdir`
   and friends are rejected so a test command cannot retarget another tree).
2. `validate_witness_invocation` — require the command to name the accepted artifact and a
   single exact test. A form that can run a whole suite would credit a flip the witness did
   not earn.
3. `is_witness_test_path` — teach it the language's test-file shape so the artifact is
   recognized at all. Accept a filename-only form (`test_*.py`, `*_test.go`) **only** if
   that runner really collects the file wherever it sits. Rust does not: cargo runs an
   integration test only from `tests/`, so the `rs` arm requires a recognized test
   directory. Accepting `src/backdoor_test.rs` would let a production file ride in as a
   witness whose required `cargo test --test <stem>` can then never pass.

**Adding a port**: define the trait in [`src/ports.rs`](src/ports.rs), add the field to
`PipelinePorts`, and choose deliberately between a no-op default and an `Option` — a port
that can honestly answer "nothing" gets a default; one whose absence *changes what the run
does* (candidate isolation, MCP pre-fetch) is an `Option`, so degradation stays visible.

**Adding a stage**: add the variant to `StageKind` in `stella-protocol`, give it a rank in
`stage_rank` ([`src/replay.rs`](src/replay.rs)) — `validate_stream` rejects any backward
transition that is not the Verify/Verdict → Execute revise back-edge — then re-record the
goldens and read the fixture diff, because a change there changes the observable event
contract.

## See also

- [`../../AGENTS.md`](../../AGENTS.md) — "The definition of done: witness tests" for the
  deterministic contract this crate enforces at runtime; "Architecture: ports, not concretions" for the inherited
  no-I/O and byte-stable-prompt rules.
- [`../../website/content/docs/inference-pipeline.mdx`](../../website/content/docs/inference-pipeline.mdx)
  — the full stage flow, exact failure-receipt revision loop, and the `/pipeline` deck toggle.
- [`../../docs/spec/replay-golden-trajectories.md`](../../docs/spec/replay-golden-trajectories.md) — the
  recording procedure and the reference-engine adapter contract.
- [`../stella-core`](../stella-core) — `Engine::run_turn`, the loop this crate composes.
