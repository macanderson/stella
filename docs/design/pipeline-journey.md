# The journey of a prompt — full pipeline mode, in plain language

**Status:** Living document. Describes the staged pipeline as of 0.6.x.

This is the maintainer's companion to the user-facing
[Inference Pipeline](../website/content/docs/inference-pipeline.mdx) page. That
page explains *what* the pipeline promises; this one walks a single prompt from
the moment it enters `stella run` to the moment the run reports done, naming
the module that owns each step so you can find the code in one hop. Everything
here lives in `stella-pipeline` unless stated otherwise; the async orchestrator
is `src/pipeline.rs`, and every *decision* it makes is a pure synchronous
function in a sibling module (`triage`, `plan`, `scope`, `verify`, `witness`,
`candidate`) — that split is the crate's core invariant.

The one-line itinerary:

> **prompt → triage (+ recall, concurrently) → [conversational fast path?] →
> plan → scope review → execute → diff probe → witness warrant → witness
> authoring (on demand) → evidence ladder → { submit fast | revise | judge |
> abstain } → complete.**

A detail worth calling out immediately, because older docs got it backwards:
**the witness test is authored *after* execution, not before it.** Authoring
waits until the warrant (`witness::warrant`) has read the actual diff, so a
docs-only change never buys a test, and the author is kept *blind* to the diff
(it works in a pristine snapshot of the pre-execution tree) so it cannot write
a test that merely restates the patch.

---

## 0. What "full pipeline mode" is

Stella has two ways to run a prompt:

- **The raw step loop** (`stella run --no-pipeline`, and interactive chat):
  `stella-core::Engine::run_turn` — the model proposes tool calls, tools run,
  results feed back, repeat until the model stops or a budget/backstop fires.
  No stages, no verification.
- **The staged pipeline** (`stella run`, the default; per-turn in the Command
  Deck while `/pipeline` is ON): the step loop wrapped in the stages below,
  with deterministic verification and terminal-event ownership moved up into
  `Pipeline::run`.

"Full pipeline mode" is the second one. The engine still does all the actual
work — reading files, editing, running commands — but the pipeline decides
*when* the engine runs, *what evidence* is collected around it, and *whether
the result may be called done*.

The pipeline owns the terminal events. `Engine::run_turn` emits its own
`Stage { Complete }` per turn, which is correct for one turn but wrong for a
multi-step or revising run — so the pipeline gives each engine turn a private
channel, forwards everything except the engine's stage/terminal events, and is
the single authority for `Complete` / terminal `Error` (see the "Event
ownership" section of the `pipeline.rs` module docs).

## 1. Intake: triage and recall run side by side

Entry point: `Pipeline::run` (`src/pipeline.rs`).

Two things start at once, because neither depends on the other:

- **Triage** (`Pipeline::triage` + the pure `triage` module): one cheap model
  call, on the triage role's configured model, that classifies the prompt.
  It answers two independent questions — *is this even a software task?*
  (`conversational`), and *how much ceremony does it deserve?* (`TaskClass`:
  `SimpleLookup` / `SingleTask` / `MultiStep`) — plus two optional opinions:
  whether an authored witness is warranted and whether a judge review is
  warranted. The call runs under a hard latency ceiling
  (`triage_latency_ceiling`, default 10 s); if the model doesn't answer in
  time the pipeline falls through to the full path rather than waiting.
  A **deterministic floor** (`triage::resolve_task_class`) pattern-matches the
  goal and can only *raise* the class toward more planning, never lower it —
  a misclassified complex task must still complete, just with less
  scaffolding, never fail outright.
- **Context recall** (`ContextRecallPort`): advisory memory/graph recall over
  the goal, bounded by its own latency ceiling (`recall_latency_ceiling`,
  default 5 s) and degraded to "no frames" on expiry. Recalled frames are
  bounded at the source (`bound_recalled_frames`) and ride as a **volatile
  user message after the byte-stable system prefix** — never mutated into the
  system block — so prompt-cache hits on the stable prefix survive across
  turns (invariant 7, L-E8).

## 2. The conversational fast path

If triage said "this is chat, not work" — and the deterministic floor saw no
task signal to overrule it — the pipeline answers in **one plain, tool-less
completion** under a conversational system prompt and exits
(`Pipeline::run_conversational`). This is the escape hatch that keeps a bare
`hi` from being planned, executed, and witness-tested.

## 3. Plan, then scope review (multi-step only)

Only a `MultiStep` class plans (`TaskClass::plans`). The planner
(`plan::build_planner_prompt` / `parse_plan`) turns the goal, the recalled
frames, and a repository-structure summary into explicit steps. A plan that
fails to parse gets one bounded repair attempt (`plan_repair_prompt`), then
degrades to a single-step plan — a stubborn planner never blocks the work.

If the plan's blast radius crosses any configured threshold
(`scope::ScopeThresholds`: more than 5 steps, more than 8 estimated files, or
estimated cost over $1 by default; all strict `>`), the run pauses for **scope
review** (`pipeline/scope_stage.rs`). Interactively, you approve, trim to the
largest under-threshold prefix, abort, or send the plan back to the planner
with a note (bounded at `MAX_SCOPE_REVISIONS` re-plans). Headless runs must
opt in to bypass the gate (`headless_bypass_scope_review` — the
`headless_scope_bypass` engine-config toggle); without it, an over-threshold
plan ends the run rather than silently auto-approving.

## 4. Deciding where the work happens

Before executing, the pipeline resolves *whose tree* the candidate works in
(`worktree_decision_without_asking` + `Pipeline::isolate_in_worktree`):

- **Best-of-N** (`candidates = Some(n)`) always isolates: each candidate runs
  in a snapshot of the current tree (HEAD + uncommitted + untracked, via a
  detached git worktree), and only the winner's changes are adopted.
- **An authored witness** always requires a disposable candidate, even at
  N = 1, so authoring can never mutate the session tree.
- Otherwise the **worktree policy** (`create_worktrees`: always / never / ask)
  decides — and it is consulted only when the run will actually change files,
  and only when isolation is available (a plain directory can't offer it; a
  configured `always` that can't be honoured says so out loud).

There is a "never choose nothing" backstop: a candidate that aborts in *setup*
(no isolation port, unsnapshottable tree, no independent witness author)
degrades to a bare worker run on the working tree
(`Pipeline::degrade_to_bare_execution`) — the fancy path being unavailable is
a reason to do less, never a reason to do nothing. Genuine execution aborts
(budget, loop detection, step caps) keep their stop.

## 5. Baselines: the flip oracle arms before execution

For any class that verifies, and only when the user supplied `--test-command`,
the candidate runs that command **once before executing anything**
(`Pipeline::run_candidate`) and records the result in the **flip oracle**
(`verify::FlipOracle`). The oracle is a state machine keyed on the normalized
command string: it locks onto the first *failure* it observes, and only a
later *pass of that same command* moves it to `Flipped`. A suite that was
already green can never produce a flip — which structurally excludes the
"it passed, ship it" false positive.

Details that keep the oracle honest:

- **Typed outcomes (#860):** a baseline that timed out or never spawned
  observed no assertion and does not lock the oracle — infra noise plus a
  merely-faster candidate must not read as a verified flip.
- **Failure fingerprints (#867):** a failing baseline contributes the *names*
  of its failing tests; a later pass that names its tests without naming the
  baseline's failures is no evidence — the fix-by-disappearance case (delete
  the failing test, suite exits 0) is refused.

Two other baselines are captured here: a lint/typecheck snapshot for the
regression veto (#861), and content fingerprints of untracked files so the
diff probe can tell this turn's new files from pre-existing dirt.

## 6. Execute: the engine does the work

`Pipeline::execute_plan` emits `Stage { Execute }` and runs the worker:

- **Simple / single-task:** one engine turn against the goal.
- **Multi-step:** one engine turn per plan step, each fed
  `Step i/n: <description>` on top of the shared conversation.

Each turn is `stella-core::Engine::run_turn` — the full tool loop with
compaction, loop detection, and hooks — running on the **worker** role's
model. The pipeline counts two things as the turn runs: `FileChange` events
(observed file touches) and **mutating actions** (dispatched tool calls whose
tool is not advertised read-only; unknown tools count as mutating, because
`bash` is how most real work lands). The budget guard is consulted only at
safe boundaries — between model calls, never mid-tool (invariant 6).

## 7. The diff probe — engineered to be incapable of lying

After execution the pipeline reads what changed (`Pipeline::gather_diff`):
tracked changes via the configured diff diagnostic (default `git diff`), plus
per-file `--no-index` numstats for created/modified untracked files (bounded
concurrency). The probe's output is deliberately three-valued:

- a real diff;
- "the tree changed but the diff could not be captured" — when `FileChange`
  events are positive but the diff came back empty
  (`verification_honest_diff`), which downstream readers must treat as
  *couldn't verify*, never *verified nothing*;
- "the probe could not read the tree at all" (`DIFF_PROBE_FAILED`), with a
  separately named case for "this is not a git repository, so `git diff` can
  never answer here" (`DIFF_PROBE_NOT_A_REPO` — the permanent condition of a
  Terminal-Bench task image, #973).

The distinction exists because an ambiguous empty diff once convinced a judge
that "no changes were made" — the archetypal verification lie.

Simple lookups exit here if they touched nothing: a clean lookup has nothing
to verify. A lookup that unexpectedly *did* touch files has its judge-skip
revoked (the zero-diff guard) and continues into verification like any other
change.

## 8. The witness warrant, then on-demand authoring

If no `--test-command` is armed, verification still wants a deterministic
oracle — but only when the change *warrants* one. The **warrant**
(`witness::warrant`) reads the diff and answers "does this change need a
witness test, and if not, why not":

- `NothingChanged`, `DocsOnly`, `TestsOnly`, `ConfigOnly`, `CommentsOnly`,
  `PureRemoval` — each a *stated reason*, recorded in the verdict, mirroring
  the contributor rule ("ship a witness test, or a stated reason there isn't
  one"). Test-only changes and pure removals still warrant an independent
  review even though no test is warranted.
- Anything mixed, unrecognized, or unreadable **fails closed to Required**:
  an unnecessary witness costs one model call; a missing one ships unverified
  behavior.

When a witness is required, the **witness author** — an independent model,
resolved from the judge's slot and refused if it would be the same model as
the worker — writes a minimal *failing* test (`pipeline/witness_stage.rs`).
The author works in a **pristine snapshot of the pre-execution tree**, blind
to the diff, so the test pins the *intended behavior* rather than restating
the patch. The authored test must:

- fail on the old code first (the fail-first gate — a test that passes before
  the change proves nothing);
- pass a static assertion-density screen (`witness::density`, #863): no
  assertion-free tests, no constant-only assertions, no self-comparisons, no
  bare `#[should_panic]`;
- survive tamper checks every verify iteration: a worker (or revise turn)
  that edits the witness files hard-fails the candidate
  (`witness::airlock`).

Its command then arms the same flip oracle. The witness is scaffolding for
this one run — it lives and dies with the candidate workspace unless
`--keep-witness` promotes it.

If no independent author can be resolved, the run degrades to the unauthored
ladder with a warning — unless `require_independent_witness` is set, in which
case the run refuses up front (#1147: a benchmark arm whose manifest names an
independent author must not silently produce a number without one).

## 9. Verify: the evidence ladder

`Pipeline::verify_candidate` re-runs the tracked command (the configured one,
else the witness's), feeds every observation back to the oracle, and hands the
pure `verify::ladder_decision` one `LadderInputs` snapshot: flip state,
touched-test result, diff size and availability, file-touch count, mutating
actions, new lint errors/warnings, and whether the witness proved
tautological. The ladder answers **in this order**:

1. **Touched tests red → `Revise`.** Already a deterministic failure; never
   spend a judge call confirming it.
2. **Nothing attempted → `NothingAttempted`.** The turn dispatched zero
   mutating calls and nothing observed a change: the model narrated a solution
   and wrote none of it down. This rung is *knowledge*, not abstention — the
   revision turn gets a blunt nudge ("reasoning about a solution, or stating
   one in prose, does not perform it"), and a run that never acts ends
   `passed: false`. Before this rung existed, eleven untouched Terminal-Bench
   tasks were reported as successes.
3. **Every channel blind → `Unverifiable`.** No flip, no test result, an
   unreadable tree, no recorded touch: the ladder *abstains*. No judge call —
   a judge asked to rule on an empty record once answered with a confident
   `FAIL` naming a file that existed. The run is scored unverified, never
   passed or failed.
4. **Flip + green + diff within budget (default ≤ 400 lines) + no new lint
   errors + witness not tautological → `SubmitFast`.** The full deterministic
   pass. The model judge is skipped entirely. Two audits run before the
   submit is final: a **confirmation run** (#859 — one extra suite run; a
   flake demotes the oracle to `Unstable` and escalates instead) and the
   **mutation check** (#870 — break the changed lines one at a time; a
   witness that stays green under every mutant is tautological and loses its
   fast-submit).
5. **Otherwise → `ModelJudge`.** Genuinely inconclusive, but at least one
   channel saw something.

## 10. Judge: asymmetric trust in a second opinion

When the ladder escalates, the **judge** — a separate model call, by
preference from a different model *family* than the worker — reviews the
goal, the honest diff, and a compact structured evidence snapshot
(`JudgeEvidence` carrying the full `LadderSnapshot`, #864: oracle trace in
observation order, diagnostics delta, tamper result, audit findings), and
answers with a leading `PASS` or `FAIL`. It never sees the worker's narration.
A failed judge call degrades to a conservative heuristic verdict
(`verify::heuristic_fallback`) rather than hanging.

Trust in the judge is deliberately asymmetric, because its authority was
measured and found wanting: across an 89-task Terminal-Bench run where the
witness rung couldn't fire (single-model posture), the judge agreed with the
benchmark's own grader 46% of the time, and its false passes cost tasks
outright. So a judge "not yet" is always actionable (costs one revision), but
a judge "done" **standing alone** — no flip, no green test behind it — is
scored *unverified* rather than passed (`judge_pass_stands_alone`). The judge
never overrides a deterministic failure.

Before recording that unverified pass, the pipeline asks for the missing
evidence once (`judge_evidence_demand`, #1295): one revision turn telling the
worker to make the tracked command observe its change. The ask is raised **only
where a tracked command exists**, and that precondition is what makes it
affordable — the two facts that would clear `judge_pass_stands_alone` are both
observations of that command, so with none resolved no worker could satisfy the
ask on any turn. That is exactly what the feature's first measurement ran into:
on Terminal-Bench, with no `--test-command` and the authored-witness rung unable
to fire under a single-model posture, the condition held on nearly every turn
and the extra turn bought nothing on all of them. Capped at one ask per
candidate and drawn from the same `max_revisions` budget a real failure spends.
The measurement that switched it back on is in
`bench/evidence/judge-evidence-demand-1295/`.

## 11. Revise: bounded retries with escalating candor

A `Revise` decision (or a judge `FAIL`) sends the evidence back into a fresh
worker turn (`Pipeline::revise_candidate`), up to `max_revisions` times
(default 2). On the **second consecutive** deterministic failure, the pipeline
spends one judge call on **distress guidance** (`verify::guidance_prompt`) —
a course-correction note that rides with the next revision prompt instead of
letting the worker dig the same hole. Repeated identical failures also widen
what the revision prompt discloses about the failure
(`witness::airlock::grain_for_repeats` — sealed failure output is scrubbed of
secrets and disclosed at coarser or finer grain by repeat count). After every
revise turn the tracked command is re-run and the ladder re-decides; the
witness tamper check runs every iteration, so a revise turn that edits the
witness is caught at the next check.

## 12. Best-of-N selection and adoption

With `candidates = Some(n)`, each candidate runs steps 5–11 in its own
isolated snapshot (steered apart by `candidate_steering::SteeringFanOut`) and
is scored (`candidate::score_from_verification`):

> `DeterministicPass > JudgePass > Unverified > Failed`

Ties break within a rank by mutation-survival, then fewer new diagnostics,
then smaller diff (#869/#870). Only the winner's changes are adopted into the
real tree — atomically, failing loudly with the conflicting paths named if
you edited the same files mid-run. Losing candidates leave no residue.

## 13. Complete: one terminal event, honest accounting

`Pipeline::run` adopts the winning candidate's message trajectory and emits
exactly one terminal signal:

- **`Complete`** (with the worker model's label and the run's total spend)
  when verification passed or was legitimately not needed;
- a non-retryable **`Error`** when verification stayed red after the revision
  budget (`PipelineStatus::VerificationFailed`) or the run aborted;
- hard infrastructure failures return out of band as `PipelineRunError`.

The `PipelineOutcome` records the task class, final text, total cost, revision
count, how many candidates actually *ran* (not how many were configured), and
the verdict — including `deterministic: true/false`, so a headless caller can
tell a flip-oracle pass from a judge's opinion, and the frozen
`LadderSnapshot` (#865), so `stella replay` can answer "why did this run
fast-submit / revise / judge?" from the recording alone without re-deriving.

Every stage boundary along the way emitted a `Stage` event, every model call
was metered into the budget guard, and every proof-relevant step (oracle
observations, warrant, tamper checks, audits) landed on the proof rail
(`ProofStep`) — which is what makes a pipeline run *replayable* and its
verdict *auditable* after the fact.

---

## Who runs on which model

Four roles, all configured through `agent_engine_config` (see the README's
"Agent engine config" section):

| Role | What it does in the journey | Default resolution |
|---|---|---|
| **triage** | Stage 1 classification call | `pipeline_triage_model` → `default_model` |
| **worker** | Execute turns, revise turns (plan and conversational ride this tier too) | `pipeline_worker_model` → `default_model` |
| **judge** | Judge verdicts, distress guidance | `pipeline_judge_model` → `default_model`; prefers a different model *family* than the worker |
| **witness author** | Authors the failing witness test | Resolves from the judge slot; **refused** if it would equal the worker's model — Stella will not let the worker write the test that proves the worker |

A configured role model whose provider has no resolvable key degrades softly
to the worker with a notice — configuration can never turn a runnable
pipeline into an error. The one exception is deliberate:
`require_independent_witness` makes a missing independent author a refusal
instead of a degradation, for callers whose published posture claims one.

## Where the knobs live

`PipelineConfig` (`src/pipeline.rs`) is the single tuning surface: latency
ceilings, scope thresholds, headless behavior, `test_command`,
`witness_writer`, `keep_witness`, `distress_guidance`, `diff_budget_lines`
(400), `diagnostics_veto_warnings`, `max_revisions` (2),
`judge_evidence_demand`, `require_independent_witness`, `candidates`, and
`create_worktrees`. The
verification half's design history and remaining work are tracked in
[`ROADMAP.md`](../ROADMAP.md); every decision and its spend is pinned by the
per-PR degradation gate (`crates/stella-pipeline/src/pipeline/tests/degradation_gate.rs`,
`docs/design/verification-gate.md`).
