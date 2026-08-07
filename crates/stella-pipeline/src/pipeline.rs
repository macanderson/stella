//! The orchestrator: the staged turn flow that sits
//! *above* `stella-core::Engine`. It sequences evaluate → enhance → route →
//! execute → witness → verify → verdict → revise over the injected ports,
//! emitting a `Stage` event at every boundary and owning terminal
//! success-or-failure signaling for outcome-producing runs (`Complete` or a
//! non-retryable `Error`). Hard infrastructure failures return out of band as
//! [`PipelineRunError`].
//!
//! Everything here is I/O sequencing; every *decision* it makes is delegated
//! to a pure function in a sibling module (`triage`, `plan`, `scope`,
//! `verify`, `candidate`) so the hard logic stays synchronous and
//! property-testable. The orchestrator's own job is only to call the ports in
//! the right order and thread the budget through.
//!
//! # Event ownership
//!
//! `stella-core::Engine::run_turn` emits its own `Stage { Execute }`, a
//! terminal `Stage { Complete }`, and a `Complete` — correct for *one turn*,
//! but a multi-step plan or a revise loop runs several turns. The pipeline is
//! the **single authority** for stage boundaries and the terminal event on an
//! outcome-producing run: it gives each `run_turn` a private channel, then
//! forwards every event to the consumer *except* the engine's
//! `Stage`/`Complete` (which would otherwise falsely signal "done" after step
//! one). The pipeline emits `Complete` for success or a non-retryable `Error`
//! for terminal failure; hard [`PipelineRunError`] exits remain typed return
//! values for the caller to close out. This mirrors the one-emission-point
//! discipline of L-E1/L-T5.
//!
//! # Cache discipline (L-E8)
//!
//! Recalled context rides as a **volatile message after the byte-stable system
//! prefix**, never mutated into the system block itself, so prompt-cache hits
//! on the stable prefix survive across turns. See `assemble_user_message`.
//!
//! # Breaker feedback boundary
//!
//! The pipeline holds `&Router` (per its constructor contract) and so *reads*
//! resolutions and surfaces `ProviderFallback` events, but does not feed
//! call outcomes back into the breaker (`record_success`/`record_failure`
//! need `&mut Router`). That feedback is the responsibility of the glue that
//! owns the `Router` — documented here so the boundary is explicit.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use std::sync::Arc;

use futures_util::StreamExt as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use stella_core::hooks::{HookRunner, Hooks};
use stella_core::receipts::RECEIPT_SEQ_ALLOCATED_BASE;
use stella_core::retry::{RetryPolicy, Sleeper};
use stella_core::router::FallbackInfo;
use stella_core::{AbortKind, BudgetGuard, Engine, EngineConfig, EventSender, Router, TurnOutcome};
use stella_protocol::{
    AgentEvent, CompletionMessage, LadderRung, LadderSnapshot, MessageRole, ModelCallRole,
    ModelRef, OracleObservation, ProofStep, ProofTree, Provider, Role, StageKind, VerdictEvidence,
};

use crate::candidate::{
    CandidateScore, CandidateSummary, score_from_verification, select_best_candidate,
};
use crate::candidate_fanout::fan_out_width;
use crate::candidate_narration::{
    self, candidate_fanout_notice, candidate_start_notice, candidate_winner_notice,
};
use crate::candidate_steering::SteeringFanOut;
use crate::plan::{PlanStep, build_planner_prompt, parse_plan, plan_repair_prompt};
use crate::ports::{
    ApprovalGate, CandidateWorkspace, CandidateWorkspacePort, CmdOutcome, ContextRecallPort,
    CoverageProbe, DiagnosticInvocation, DiagnosticRunner, FileTouchPort, LintProbe, LintRecord,
    McpPrefetchPort, MutantOutcome, MutationProbe, PipelinePorts, ProviderResolver, Recall,
    RecalledFrame, RepoStatusPort, RepoStructurePort, ScopeDecision, TestInvocation, TestRunner,
    WorkspaceError,
};
use crate::scope::{
    MAX_SCOPE_REVISIONS, PlannedScope, ScopeEstimate, ScopeVerdict, apply_trim, build_proposal,
    needs_scope_review,
};
use crate::triage::{
    TaskAssessment, TaskClass, parse_triage_response, resolve_conversational, resolve_task_class,
    resolve_witness, triage_prompt,
};
use stella_core::driver::TurnHalt;
use stella_protocol::ToolOutput;

use crate::flip_halt::{FlipHalt, command_of};
use crate::management_prompt::ManagementPrompt;
use crate::verify::coverage::DiffCoverage;
use crate::verify::diff_render::DiffContext;
use crate::verify::{
    FlipOracle, LadderDecision, LadderInputs, Verdict as ModelVerifierVerdict,
    deterministic_fail_evidence, deterministic_pass_evidence, evidence_demand_is_worth_a_turn,
    guidance_prompt, heuristic_fallback, ladder_decision, model_verdict_evidence,
    nothing_attempted_evidence, parse_verifier_response, uncorroborated_pass_evidence,
    unverifiable_evidence, verifier_prompt,
};
use crate::witness::airlock::{
    DisclosureGrain, FailureFingerprint, SealedFailure, grain_for_repeats, redact, scrub,
};
use crate::witness::warrant::{ChangeSignals, warrant};
use crate::witness::{
    Witness, parse_test_invocation, parse_witness_command, validate_witness_artifact,
    validate_witness_identity, validate_witness_invocation, witness_identity_matches,
    witness_prompt, witness_repair_prompt,
};
mod authored;
mod candidate_result;
mod disclosure;
mod evidence;
mod fanout_stage;
mod plan_steps;
mod raw_usage;
mod repair_gate;
mod run_error;
mod scope_stage;
mod stage_budget;
mod task_frame;
mod verifier_stage;
mod verify_probes;
use verify_probes::DiffProbe;
mod witness_stage;
use candidate_result::{CandidateAbort, CandidateResult, TurnAbort, escape_abort_reason};
use fanout_stage::SerialCreates;
use raw_usage::{RawCall, RawCallError};
pub use run_error::{PipelineError, PipelineRunError};
use run_error::{RoleResolveError, WitnessAuthorIndependence};
use stage_budget::{PipelineBudgetAbort, Spend, budget_abort};
use task_frame::TaskFrame;
use witness_stage::{BoundHookRunner, WitnessAuthoring};
/// Make a diff that verification hands downstream *incapable of lying*.
///
/// What a [`LadderDecision::NothingAttempted`] revision turn is told.
///
/// Says only what was observed and what is required, and deliberately offers no
/// theory about *why* the turn stopped — the pipeline does not know, and a
/// wrong guess ("you seem to have thought the task was complete") is an
/// invitation to argue with the premise instead of acting on it. The observed
/// fact is the whole message: no tool ran, so nothing changed.
///
/// The last clause matters more than it looks. The turns this fires on ended
/// with the model narrating a finished solution it never wrote down — on
/// Terminal-Bench, 123 reasoning events and zero tool calls — so the one thing
/// worth saying is that describing the work is not doing it.
const NOTHING_ATTEMPTED_NUDGE: &str = "This turn ended without calling a single tool, so the \
     workspace is exactly as it was. Nothing has been written yet, whatever the answer above \
     describes. Carry out the task now with tool calls that change the workspace — writing the \
     file, running the command, applying the edit. Reasoning about a solution, or stating one in \
     prose, does not perform it.";

/// Minimal fallback when the caller supplies no stable system prefix.
const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a precise, careful software engineering agent. Make the smallest correct change.";

/// The system prompt for the conversational fast path. Swapped in for
/// [`DEFAULT_SYSTEM_PROMPT`] when triage classified the input as chat so the
/// reply reads as a normal, brief conversational turn rather than a work plan.
/// How many trailing messages the conversational reply is given, beyond the
/// leading system message.
///
/// Chat needs the thread of the conversation, not the engineering transcript
/// under it. Twelve messages is roughly the last half-dozen exchanges — enough
/// that "and the other one?" still resolves, and enough that a follow-up reads
/// as continuous.
const CONVERSATIONAL_HISTORY_MESSAGES: usize = 12;

/// The messages a conversational reply is dispatched with: the leading system
/// message, then the last [`CONVERSATIONAL_HISTORY_MESSAGES`].
///
/// Unbounded, this call re-billed the whole running transcript at the full
/// input rate for a two-sentence answer. It dispatches with `tools:
/// Vec::new()` and a replaced system message, so not one byte of the prefix
/// matches the worker's cached one — a 90k-token session where the user types
/// "thanks" paid for all 90k plus the 1.25x cache-write premium, and every
/// chat interjection in a long session paid it again (#1840).
///
/// Bounding the input is the fix that holds regardless of caching. The
/// issue's other option — keep the worker's system message so the prefix
/// matches — cannot deliver a hit on its own while the tool array is empty:
/// the tools block is part of the same cached prefix, so a matching system
/// message with no tools still misses. That half is worth doing, and needs
/// adapter-level work to verify rather than assume.
///
/// The system message is kept because the caller's first message may not be
/// one (`run` seeds it when history is empty, but nothing enforces it for a
/// caller that seeded its own); dropping a non-system first message here
/// would silently destroy context the window is meant to preserve.
fn conversational_window(messages: &[CompletionMessage]) -> Vec<CompletionMessage> {
    let leads_with_system = messages
        .first()
        .is_some_and(|message| matches!(message.role, MessageRole::System));
    let (head, rest) = if leads_with_system {
        messages.split_at(1)
    } else {
        messages.split_at(0)
    };
    let tail = rest.len().saturating_sub(CONVERSATIONAL_HISTORY_MESSAGES);
    head.iter().chain(&rest[tail..]).cloned().collect()
}

pub(crate) const CONVERSATIONAL_SYSTEM_PROMPT: &str = "You are Stella, a careful software engineering agent. The user's latest \
     message is a greeting, small talk, or a question about you — not a coding \
     task. Reply briefly and warmly in plain prose: no tools, no code, no plan, \
     no test. Do not invent a task. If it fits, add one short line inviting \
     them to describe a change, bug, or question about their codebase.";

/// Per-role request overrides for the pipeline's raw completion calls
/// (triage / verifier / guidance), resolved by the caller from
/// `agent_engine_config`. Every field is optional and falls through to the
/// engine config's value — the worker's settings are the pipeline-wide
/// base; these refine one role. `prompt`, when set, is prepended as a
/// system message to the role's built-in task prompt (the task prompt
/// carries the output contract — `PASS`/`FAIL`, the triage token — so it
/// is never replaced outright).
#[derive(Debug, Clone, Default)]
pub struct RoleCallOverrides {
    pub prompt: Option<String>,
    pub effort: Option<stella_protocol::ReasoningEffort>,
    pub reasoning: Option<bool>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub params: Option<stella_protocol::GenerationParams>,
}

/// The pipeline's per-role override set. Worker (and plan, which rides the
/// worker's tier) is configured through [`PipelineConfig::engine`] directly;
/// only the two roles with their own models get their own request shaping.
/// The witness author/repair engines ride the verifier's model, so they take
/// the `verifier` row's shaping too (#1785) — everything except `prompt`,
/// which stays scoped to the raw verdict/guidance calls
/// (`Pipeline::witness_engine_config` says why).
#[derive(Debug, Clone, Default)]
pub struct PipelineRoleOverrides {
    pub triage: RoleCallOverrides,
    pub verifier: RoleCallOverrides,
}

/// Tuning for the whole staged flow.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Passed to `stella-core::Engine` for every execute turn.
    pub engine: EngineConfig,
    /// Wall clock this whole pipeline RUN may spend, measured from
    /// [`Pipeline::new`] — the repair gate's clock axis (#1479, #1507).
    ///
    /// Deliberately not [`EngineConfig::turn_budget`], whose contract is one
    /// engine turn: a multi-step plan runs one engine turn per step plus the
    /// witness author's and every revision's, so metering the run's elapsed
    /// time against a per-turn allowance under-reported the remaining clock
    /// and refused repairs a long run could still afford (#1507). Like
    /// `turn_budget` it enforces nothing — no future is cancelled when it
    /// elapses — and `None` means "nobody is measuring": the clock axis
    /// abstains rather than inventing a deadline the caller never declared.
    pub run_budget: Option<Duration>,
    /// Per-role request overrides (`agent_engine_config`) for the raw
    /// triage/verifier completion calls.
    pub role_overrides: PipelineRoleOverrides,
    /// Decision latency ceiling on the triage classification call (L-M4): if
    /// it doesn't answer within this, the in-flight call is dropped and
    /// triage falls through to the full path. The expiry is not silent in
    /// accounting: `run_accounted_call` records a content-free
    /// `UsageIncomplete` envelope for the abandoned attempt (its provider-side
    /// spend is unknowable once the response never lands).
    pub triage_latency_ceiling: Duration,
    /// Latency ceiling on the context-recall port. Recall runs concurrently
    /// with triage and is advisory (L-C6), never a gate — but nothing bounded
    /// it, so a wedged embedding call or an unresponsive CGP host hung the
    /// whole turn before the first stage completed, with no event after
    /// `Stage { ContextRecall }` to say why. Past this, recall degrades to
    /// [`crate::ports::Recall::default`] (no frames) and the turn proceeds.
    pub recall_latency_ceiling: Duration,
    /// Thresholds above which a plan triggers interactive scope review (L-E5).
    pub scope_thresholds: crate::scope::ScopeThresholds,
    /// Whether this run is headless (no interactive approver available).
    pub headless: bool,
    /// User-invoked plan mode (#1264): show the plan and wait for approval
    /// **regardless of size**.
    ///
    /// The thresholds in [`Self::scope_thresholds`] answer "is this plan big
    /// enough to be worth interrupting for?", which is a question about the
    /// *plan*. This answers a different one — "do I want to see the plan
    /// before anything happens?" — which is a question about the *user*, and
    /// no estimate can answer it. A one-step plan in unfamiliar code is
    /// exactly the case the thresholds wave through and a human most wants to
    /// read first.
    ///
    /// Forcing the gate is the whole mechanism, because the gate already sits
    /// ahead of the execute stage — the only stage that touches the working
    /// tree. Declining therefore leaves the tree untouched by construction
    /// rather than by the model's good behaviour.
    pub plan_mode: bool,
    /// If headless and a plan crosses the scope-review thresholds, this must
    /// be explicitly `true` to proceed. The bypass skips the gate outright —
    /// `Pipeline::scope_review` never consults the approval port for it —
    /// otherwise the run is a named error rather than a silent auto-approve.
    pub headless_bypass_scope_review: bool,
    /// The test command the flip oracle tracks (run before and after execute).
    /// `None` hands the flip oracle to the witness author (when
    /// `witness_writer` is on) — an explicit user command always wins over an
    /// authored one.
    pub test_command: Option<String>,
    /// Witness authoring (L-E11 front half): when no `test_command` is
    /// configured and the task class verifies unconditionally, an independent
    /// model authors a failing witness test whose command arms the flip
    /// oracle, with tamper exclusion at verify time ([`crate::witness`]).
    /// Costs one engine turn + up to two test runs per *candidate*: each
    /// best-of-N candidate authors its own witness inside its own snapshot,
    /// because a witness written against a sibling's tree witnesses nothing
    /// about this one's work. At the default `candidates = None` that is once.
    pub witness_writer: bool,
    /// Whether an authored witness is adopted into the real tree along with
    /// the work it verified. Off by default: the witness is scaffolding for
    /// one run, not a change the user asked for.
    ///
    /// A witness is written to *fail* — it encodes a moment ("this code does
    /// not do X yet"), not an invariant, which is the opposite of what a
    /// durable regression test encodes. Adopting one by default dropped an
    /// untracked, already-satisfied test into the project's real test tree,
    /// where the runner picks it up forever and nobody reviews it because it
    /// was never in a diff. They accumulate: the witness executor creates
    /// exclusively (`create_new`), so each run that lands on a taken filename
    /// simply picks another.
    ///
    /// Turning this on is the explicit promotion step — the run's verdict is
    /// unchanged either way, since the witness has already done its job by
    /// the time adoption happens.
    pub keep_witness: bool,
    /// Distress-triggered course-correction: on a candidate's *second*
    /// deterministic verification failure — cumulative, not necessarily
    /// consecutive (#868) — spend one verifier call for guidance that rides
    /// with the next revision prompt ([`crate::verify::guidance_prompt`]).
    /// Event-triggered by design — never a fixed mid-run checkpoint. Bounded by
    /// `max_revisions` (at most `max_revisions - 1` guidance calls per candidate).
    pub distress_guidance: bool,
    /// The closed diagnostic that reports what the turn changed. `None`
    /// disables diff-size and zero-diff inspection.
    pub diff_diagnostic: Option<DiagnosticInvocation>,
    /// The diff-size budget in changed lines: a diff at or under this is
    /// "small enough" to trust deterministic evidence without a verifier (L-E11).
    pub diff_budget_lines: u32,
    /// Regression veto strictness (#861): when `true`, NEW lint/typecheck
    /// warnings (not just errors) also block a deterministic fast-submit and
    /// route to the verifier. Off by default — a chatty linter would otherwise
    /// tax every submit — while new *errors* always veto.
    pub diagnostics_veto_warnings: bool,
    /// Maximum revision turns per candidate when verification fails.
    pub max_revisions: u32,
    /// How many times a test run that died of an out-of-memory kill (#1294)
    /// is re-run before the pipeline accepts the non-observation.
    ///
    /// Retry, not revise, is the whole point: an OOM kill says nothing about
    /// the code, so feeding it back as a failure asks a model to "fix" work
    /// that may be correct. Re-running is what a human does by hand, and it
    /// is the only response that can produce the observation the pipeline
    /// actually wanted.
    ///
    /// `1` by default — one retry converts the common case (a transient peak,
    /// a sibling process that happened to be the fatter target) while keeping
    /// the worst case bounded at two runs of a suite that may be minutes
    /// long. `0` disables the retry and reports the first non-observation,
    /// which stays honest: the outcome is still `out_of_memory`, never a
    /// failing assertion.
    pub test_oom_retries: u32,
    /// Escalate to the verifier when the diff-coverage overlap could not be
    /// *measured* (#1291) — the strictest reading of "did the test run the
    /// changed lines?".
    ///
    /// Off by default, and note what the default does NOT mean: an unmeasured
    /// overlap is already scored `Unverified` rather than `DeterministicPass`
    /// (see the `SubmitFast` arm), so it never ships as a verified pass. What
    /// this adds is *spending a verifier call* on it.
    ///
    /// The three positions, so the choice is legible:
    ///
    /// - measured overlap → deterministic pass, either way;
    /// - measured NON-overlap → verifier, either way (the flip is a coincidence,
    ///   and that is worth a second opinion);
    /// - unmeasured → scored unproven and shipped (off, the default), or
    ///   escalated to the verifier (on).
    ///
    /// Off is the default for the reason `verify::coverage` records at
    /// length: most workspaces have no coverage tooling, so escalating would
    /// route nearly every deterministic pass through a paid verifier call to be
    /// told what the evidence already said — the "a gate that fires
    /// everywhere is a tax" result #1295 measured. Turning it on is for an
    /// operator who has the tooling and wants the overlap enforced rather
    /// than merely scored.
    pub require_diff_coverage: bool,
    /// Ask for corroboration when a model verifier passes and nothing
    /// deterministic stands behind it (#1295): spend one revision demanding
    /// the evidence instead of recording the pass as UNVERIFIED on the spot.
    ///
    /// Bounded to **one** demand per candidate, spending a revision turn the
    /// repair gate does NOT count against `max_revisions` (#1509 — the demand
    /// corroborates a passing verdict; a repair fixes a refuted one, and the
    /// two are not substitutes), and — the part that
    /// decides whether this is worth having at all — only raised when
    /// `Pipeline::effective_test_command` resolved to something. Without a
    /// tracked command the ladder has no channel that can *ever* answer the
    /// ask: `touched_tests_passed` stays `None` by construction and the flip
    /// oracle never observes a candidate run, so
    /// [`crate::verify::LadderInputs::verifier_pass_stands_alone`] is true no
    /// matter what the worker does with the turn. That is the whole of why
    /// this was measured as a loss the first time (#1211 §1) — on
    /// Terminal-Bench, with no `--test-command` and the authored-witness rung
    /// unable to fire under a single-model posture, the condition held on
    /// nearly every turn and the extra turn bought nothing on all of them.
    /// Gating on the command is what turns "fires everywhere, answerable
    /// nowhere" into "fires only where an answer exists".
    pub verifier_evidence_demand: bool,
    /// Refuse the run when no witness author independent of the worker can
    /// be resolved, instead of degrading to the unauthored verify ladder.
    ///
    /// Off by default, and that default is the one almost every caller wants:
    /// a missing author costs the run its authored witness, never the task
    /// (see `Pipeline::can_author_independent_witness`). Turning it on is
    /// for the host that has already made the independence claim OUTSIDE the
    /// run — a benchmark arm whose hashed posture names a second model, a
    /// manifest that will be read as "this number was produced with an
    /// independent author". There, the silent degradation is worse than no
    /// number at all: the arm's own digest describes a configuration the run
    /// did not have (#1147).
    pub require_independent_witness: bool,
    /// Refuse the run when the VERDICT call would resolve to the worker's own
    /// model (#1795) — the "independent code reviewer" grading the code it
    /// wrote — instead of proceeding with the once-per-run prose caveat.
    ///
    /// Off by default for the same reason its witness sibling above is: a
    /// single-provider BYOK seat is the common case and must keep working.
    /// On or off, the verdict's ladder snapshot records grader independence
    /// as a structured fact (`LadderSnapshot::verifier_independent`), so a
    /// stored verdict states it without the transcript. Checked before spend,
    /// like the witness gate: a refusal after the trajectory is bought is a
    /// trajectory the caller must throw away.
    pub require_independent_verifier: bool,
    /// Best-of-N (L-E7). `None` or `Some(1)` is single-shot (the default);
    /// `Some(n)` generates n candidate executions — each in an isolated
    /// snapshot of the current tree state when a
    /// [`crate::ports::CandidateWorkspacePort`] is wired — and selects the
    /// best, adopting only the winner's changes into the real tree. Paid for
    /// with n× the execution *cost* — opt-in only. Not n× the wall clock: see
    /// [`Self::candidate_concurrency`].
    pub candidates: Option<u32>,
    /// How many **isolated** candidates execute at once (#1215).
    ///
    /// `None` — the default — runs the whole fan-out together, so
    /// `candidates = Some(n)` costs the slowest candidate in wall clock rather
    /// than the sum of all n. `Some(1)` is the strictly sequential behaviour
    /// that predates concurrency, and is the setting for a host that needs one
    /// candidate's events to arrive without a sibling's interleaved on the
    /// same stream.
    ///
    /// Only the isolated path is ever concurrent. The shared-tree degradation
    /// (`candidates > 1` with no [`crate::ports::CandidateWorkspacePort`]
    /// wired) stays sequential whatever this says: those candidates execute
    /// into one working tree and would overwrite each other's files.
    ///
    /// The budget is divided rather than shared — see
    /// `crate::candidate_fanout` for the aggregate bound this keeps and the
    /// one-window overshoot it accepts.
    pub candidate_concurrency: Option<u32>,
    /// Whether this run does its work in a throwaway worktree rather than the
    /// user's checkout. Consulted once — after planning, before execution,
    /// using the task class triage resolved — and only when the run is going
    /// to change files.
    ///
    /// The precedence is: a run that changes nothing is never isolated; a
    /// workspace with no candidate isolation cannot offer it (and an `Always`
    /// that cannot be honoured says so); only then does this policy decide.
    pub create_worktrees: crate::ports::WorktreePolicy,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            engine: EngineConfig::default(),
            run_budget: None,
            role_overrides: PipelineRoleOverrides::default(),
            triage_latency_ceiling: Duration::from_secs(10),
            // Half the triage ceiling, and recall runs concurrently with
            // triage — so this can never extend the critical path, it only
            // stops recall from becoming it. A remote CGP embedding round
            // trip is 100-500ms and the local path is single-digit ms, so
            // this is an order of magnitude above the realistic worst case.
            recall_latency_ceiling: Duration::from_secs(5),
            scope_thresholds: crate::scope::ScopeThresholds::default(),
            headless: false,
            headless_bypass_scope_review: false,
            plan_mode: false,
            test_command: None,
            witness_writer: true,
            keep_witness: false,
            distress_guidance: true,
            diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
            diff_budget_lines: 400,
            diagnostics_veto_warnings: false,
            max_revisions: 2,
            test_oom_retries: 1,
            require_diff_coverage: false,
            // On (#1295). Off is the historical setting, and the reason it was
            // off — the ask fired on turns that could never answer it — is now
            // a precondition of raising it at all rather than a property of
            // the workload. See the field's docs and `bench/evidence/verifier-
            // evidence-demand-1295/README.md` for the measurement.
            verifier_evidence_demand: true,
            require_independent_witness: false,
            require_independent_verifier: false,
            candidates: None,
            candidate_concurrency: None,
            create_worktrees: crate::ports::WorktreePolicy::default(),
        }
    }
}

impl PipelineConfig {
    /// The effective candidate count (`candidates`, floored at 1).
    fn candidate_count(&self) -> u32 {
        self.candidates.unwrap_or(1).max(1)
    }
}

/// What the operator is asked under [`crate::ports::WorktreePolicy::Ask`].
///
/// Phrased as what changes for *them*, not as what the implementation does:
/// the cost of isolation is that `git status` stays quiet while the run works,
/// and the benefit is that an abandoned run leaves nothing behind.
const WORKTREE_QUESTION: &str = "Do this run's work in a throwaway git worktree? Nothing reaches your checkout until the run \
     finishes and adopts it.";

/// The worktree decision, before anyone is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decided {
    Yes,
    No,
    /// No, and the operator configured something they are not getting.
    NoAndSayWhy(&'static str),
    /// The policy defers to the human.
    MustAsk,
}

/// The half of the worktree decision that needs no gate — split out so the
/// precedence is testable without a live approval port, exactly as
/// `DeckApprovalGate::decide` splits the keypress mapping out of its channel.
///
/// Order matters and is not arbitrary:
///
/// 1. A run that changes nothing is never isolated, whatever the policy says.
///    There is nothing to protect, and asking would put a prompt in front of
///    the commonest thing anyone does — a question about relocating work that
///    is not going to happen.
/// 2. Without isolation available there is nothing to offer. Silent under
///    `ask`/`never` (neither wanted it), but `always` is told, because that
///    operator configured something this workspace cannot give them.
/// 3. Only then does the policy speak.
fn worktree_decision_without_asking(
    policy: crate::ports::WorktreePolicy,
    run_changes_files: bool,
    isolation_available: bool,
) -> Decided {
    use crate::ports::WorktreePolicy;
    if !run_changes_files {
        return Decided::No;
    }
    if !isolation_available {
        return match policy {
            WorktreePolicy::Always => Decided::NoAndSayWhy(
                "\"create_worktrees\": \"always\" is configured, but this workspace offers no \
                 candidate isolation (it is not a git working tree) — this run's changes will \
                 land in the working tree",
            ),
            _ => Decided::No,
        };
    }
    match policy {
        WorktreePolicy::Always => Decided::Yes,
        WorktreePolicy::Never => Decided::No,
        WorktreePolicy::Ask => Decided::MustAsk,
    }
}

/// The final verification verdict a pipeline run produced, if verification
/// ran. `deterministic` distinguishes a flip-oracle/ladder verdict from a
/// model/heuristic verifier's opinion (never conflated, L-E11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub passed: bool,
    pub deterministic: bool,
    pub summary: String,
    /// The ladder input snapshot this verdict was decided from (#865), when
    /// verification ran far enough to take one. `replay` answers "why did
    /// this run fast-submit / revise / verifier?" from here without
    /// re-deriving.
    pub ladder: Option<Box<stella_protocol::LadderSnapshot>>,
}

impl Verdict {
    fn from_evidence(passed: bool, evidence: &VerdictEvidence) -> Self {
        Self {
            passed,
            deterministic: evidence.deterministic,
            summary: evidence.summary.clone(),
            ladder: evidence.ladder.clone(),
        }
    }
}

/// How a pipeline run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineStatus {
    Completed,
    /// Verification remained red after the revision budget was exhausted.
    VerificationFailed {
        verdict: Verdict,
    },
    /// The run ended early: a step aborted (budget/loop/step-cap), or the user
    /// aborted at scope review. `kind` is the half a consumer branches on —
    /// the engine stopping on purpose vs. the run falling over — and is what
    /// lets the process boundary exit a deliberate stop distinctly from a
    /// crash (#1524).
    Aborted {
        reason: String,
        kind: AbortKind,
    },
}

/// The result of one [`Pipeline::run`].
// No `Eq`: `total_cost_usd` is an `f64`. `PartialEq` is enough for assertions.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineOutcome {
    pub status: PipelineStatus,
    /// The task class triage resolved (after the deterministic floor).
    pub task_class: TaskClass,
    /// The final assistant text of the selected candidate.
    pub final_text: String,
    /// Total spend across every stage of this run, in USD.
    pub total_cost_usd: f64,
    /// The final verification verdict, if verification ran.
    pub verdict: Option<Verdict>,
    /// How strongly the selected candidate was verified (#1291).
    ///
    /// The same grade best-of-N ranks on, surfaced because it is where the
    /// system's *claim* about a run lives, and one claim it makes is not
    /// visible anywhere else: a run whose test flipped and went green but
    /// whose diff-coverage overlap could not be measured is
    /// [`CandidateScore::Unverified`], not `DeterministicPass`. That
    /// distinction — "a test passed, and nobody could check it touched this
    /// change" — is the whole of #1291's honest-degradation case, and a host
    /// that could only read `verdict.deterministic` would see a pass and
    /// nothing else.
    ///
    /// `None` when verification never ran far enough to grade the work.
    pub score: Option<CandidateScore>,
    /// How many revision turns the selected candidate took.
    pub revisions: u32,
    /// How many candidates actually reached the worker (1 for single-shot).
    ///
    /// This is what RAN, not what was configured: a candidate that aborted in
    /// setup — no isolation port, a tree that could not be snapshotted, no
    /// independent witness author — never dispatched a model call, and is not
    /// counted. A `candidates = 4` run where three failed isolation reports 1.
    /// (Named for a `--candidates` flag that was never built; the knob is set
    /// through `agent_engine_config.pipeline_candidates` — #1211 §6.8.)
    pub candidates_run: u32,
}

/// A role resolved to a concrete provider.
struct ResolvedRole<'a> {
    model_ref: ModelRef,
    provider: &'a dyn Provider,
    fallback: Option<FallbackInfo>,
    /// The router's resolution-quality caveat (L-M8) — today, only the verifier
    /// degrading to the worker's own provider family because no second family
    /// is healthy. Surfaced by [`Pipeline::warn_verifier_caveat`] at the calls
    /// whose independence it undermines; dropping it silently is exactly the
    /// "fallback is always visible" rule (L-M7) applied to a softer signal.
    caveat: Option<String>,
}

/// The candidate-local mutable state one execute+verify+revise pass threads
/// through its phases — grouped so [`Pipeline::run_candidate`]'s sub-methods
/// take one argument instead of seven. Every exit path moves it into the
/// returned [`CandidateResult`].
struct CandidateState {
    messages: Vec<CompletionMessage>,
    final_text: String,
    /// What this candidate's engine turns did, accumulated in the exact form
    /// [`warrant`] and the ladder read it — each field's meaning is documented
    /// on [`ChangeSignals`] itself. One typed field rather than three loose
    /// `u32`s so the counts can never be transposed on their way to the
    /// warrant (the #1701 recurrence a projection method used to guard
    /// against by hand).
    signals: ChangeSignals,
    /// Ends an engine turn — execute, or a revision that gets the latch via
    /// [`FlipHalt::unfired`] — as soon as the tracked test goes fail→pass.
    ///
    /// `None` while there is nothing to watch: no failing configured-command
    /// baseline and no authored witness yet. `witness_on_demand` arms it the
    /// moment a witness's failing baseline is credited (#1793). See
    /// [`crate::flip_halt`] for why stopping is separate from crediting.
    flip_halt: Option<Arc<FlipHalt>>,
    oracle: FlipOracle,
    /// The oracle's observations in the order they were made (#864) —
    /// baseline (only for a configured `--test-command`; the authored-witness
    /// baseline feeds the flip oracle directly without a trace entry),
    /// per-iteration candidate runs, the pre-submit confirmation.
    /// Mirrors the emitted `ProofStep::Oracle` events, accumulated here so
    /// the verdict can carry its own trace without replaying the stream.
    oracle_trace: Vec<OracleObservation>,
    /// Untracked-file fingerprints snapshotted before the first turn, so
    /// every diff gather can exclude pre-existing dirty state.
    untracked_before: HashMap<String, String>,
    diff_lines: u32,
    diff_text: String,
    /// Whether the diff probe could READ the working tree this round. `false`
    /// is "I could not look" — never "I looked and it was empty". A
    /// Terminal-Bench task directory is not a git repository, so `git diff`
    /// there is permanently `false` (#973).
    diff_available: bool,
    /// The recorder's mutation tally as it stood before this candidate's first
    /// turn. Every later reading is a delta from here, which is what makes a
    /// monotonic session-wide counter usable per candidate.
    touch_baseline: u64,
    /// Pre-execution lint records for the regression veto (#861). Populated
    /// eagerly only on the in-place path, whose baseline tree is destroyed
    /// by execution; isolated candidates read theirs lazily at audit time
    /// from the still-pristine session tree. `None` also covers "probe
    /// unavailable", and the veto degrades open either way.
    lint_baseline: Option<Vec<LintRecord>>,
    /// The mutation audit's finding (#870): Some(true) = the witness
    /// failed under at least one mutant (it constrains the change);
    /// Some(false) = it stayed green under every observed mutant
    /// (tautological — the fast-submit was withheld); None = never run.
    witness_mutation: Option<bool>,
    /// The diff-coverage audit's finding (#1291): whether the passing test
    /// run executed the lines this candidate added. `Unmeasured` both before
    /// the audit runs and wherever it cannot be made — the two are the same
    /// claim, which is none.
    diff_coverage: DiffCoverage,
    /// How the authored witness's arming failure presented (#1790): the
    /// airlock's symptom class of the failing baseline run, recorded only
    /// when it was a build failure — a flip armed by a compile error is
    /// legitimate for a missing-API goal but weaker evidence than an
    /// assertion failure, and the verifier deserves to see which it was.
    witness_baseline_symptom: Option<&'static str>,
    revisions: u32,
    /// How many of those revisions were spent asking for corroboration of a
    /// standalone verifier pass rather than fixing a failure (#1295). Capped at
    /// one per candidate: the second ask would be to a worker that already
    /// answered "there is no test surface here", and paying for that answer
    /// twice is the cost this feature was switched off for the first time.
    evidence_demands: u32,
    /// Paths of the witness artifact grafted into this candidate, if the
    /// warrant bought one after execution. Empty until then, and empty forever
    /// when the change had nothing to prove.
    witness_paths: Vec<String>,
    /// Every deterministic failure this candidate has produced, in order.
    /// The airlock reads it to tell "stuck on the same thing" from "made
    /// progress and hit something new" — the signal that decides how much the
    /// next revision is told (`witness::airlock`).
    failures: Vec<FailureFingerprint>,
    /// The last model verdict this candidate bought, keyed by a digest of the
    /// exact inputs that produced it (#1431). A revision that changed nothing
    /// the verdict depends on reuses the opinion instead of re-buying it —
    /// the verifier is stateless across rounds, so identical inputs are the
    /// same question, and paying twice buys only sampling noise.
    last_verdict: Option<(u64, crate::verify::Verdict)>,
    /// The stripped diff text the model verifier last read on this candidate
    /// — the delta-framing baseline (#1431), distinct from the whole-verdict
    /// reuse pin above: reuse fires only on byte-identical *inputs*, while
    /// this lets a round whose diff partially moved render the unchanged file
    /// sections as stat lines instead of re-buying their bodies. `None` until
    /// a model verdict has been bought, and never set by a heuristic
    /// fallback: a verdict no model read must not let the next round claim
    /// its diff was already reviewed.
    last_verdict_diff: Option<String>,
}

impl CandidateState {
    /// Finish this candidate with a verification verdict — every verified
    /// exit (deterministic or judged, passed or failed) funnels through here.
    fn into_verified(
        self,
        passed: bool,
        evidence: &VerdictEvidence,
        score: CandidateScore,
    ) -> CandidateResult {
        CandidateResult {
            messages: self.messages,
            final_text: self.final_text,
            aborted: None,
            degradable: false,
            verdict: Some(Verdict::from_evidence(passed, evidence)),
            score,
            diff_lines: self.diff_lines,
            revisions: self.revisions,
            witness_paths: self.witness_paths,
        }
    }

    /// Finish a clean lookup: verification skipped, no verdict to report.
    fn into_unverified(self) -> CandidateResult {
        CandidateResult {
            messages: self.messages,
            final_text: self.final_text,
            aborted: None,
            degradable: false,
            verdict: None,
            score: CandidateScore::Unverified,
            diff_lines: self.diff_lines,
            revisions: self.revisions,
            witness_paths: self.witness_paths,
        }
    }
}

/// The working-tree surface one candidate executes and verifies against: the
/// session ports (the real tree) on single-shot and shared-tree runs, an
/// isolated snapshot's ports under best-of-N isolation. Grouped and `Copy`
/// so the candidate phases thread one value instead of two borrows.
#[derive(Clone, Copy)]
struct CandidateSurface<'c> {
    diagnostics: &'c dyn DiagnosticRunner,
    tests: &'c dyn TestRunner,
    /// The witness mutation check (#870); rooted per surface via `cwd`.
    mutation: Option<&'c dyn MutationProbe>,
    /// The diff-coverage check (#1291); rooted per surface via `cwd`.
    coverage: Option<&'c dyn CoverageProbe>,
    /// The lint probe for the regression veto (#861); rooted per surface
    /// via `cwd`.
    lint: Option<&'c dyn LintProbe>,
    repo_status: &'c dyn RepoStatusPort,
    cwd: Option<&'c str>,
    hook_runner: Option<&'c dyn HookRunner>,
    workspace: Option<&'c dyn CandidateWorkspace>,
}

/// The staged orchestrator. Holds only borrowed ports + an owned event sender
/// and config; carries no per-run state (the caller owns the message history
/// and budget, exactly as `stella-core::Engine` does).
pub struct Pipeline<'a> {
    router: &'a Router,
    providers: &'a dyn ProviderResolver,
    tools: &'a dyn stella_core::ToolExecutor,
    recall: &'a dyn ContextRecallPort,
    repo: &'a dyn RepoStructurePort,
    repo_status: &'a dyn RepoStatusPort,
    /// The file recorder's mutation tally — the evidence channel that answers
    /// "did this turn touch anything" when the diff probe cannot.
    touches: &'a dyn FileTouchPort,
    diagnostics: &'a dyn DiagnosticRunner,
    tests: &'a dyn TestRunner,
    lint: Option<&'a dyn LintProbe>,
    mutation: Option<&'a dyn MutationProbe>,
    coverage: Option<&'a dyn CoverageProbe>,
    approvals: &'a dyn ApprovalGate,
    sleeper: &'a dyn Sleeper,
    hooks: Option<(&'a Hooks, &'a dyn HookRunner)>,
    candidate_workspaces: Option<&'a dyn CandidateWorkspacePort>,
    mcp_prefetch: Option<&'a dyn McpPrefetchPort>,
    steering: Option<&'a dyn stella_core::ports::TurnSteering>,
    /// Boundary pause gate ([`Pipeline::with_turn_gate`]): attached to every
    /// engine this pipeline builds and consulted before every management
    /// call, so a paused pipeline-driven worker parks instead of spending.
    turn_gate: Option<&'a dyn stella_core::ports::TurnGate>,
    events: EventSender,
    config: PipelineConfig,
    configured_test: Result<Option<TestInvocation>, crate::witness::TestInvocationError>,
    /// Monotonic `StepManifest::call_seq` for the management roles that call
    /// providers directly (`metered_raw_call`). They sit outside any step loop,
    /// so nothing else keys their receipts apart — several verifier calls in one
    /// run would otherwise all land on the same primary key and overwrite each
    /// other. Starts at [`RECEIPT_SEQ_ALLOCATED_BASE`], above the seats the
    /// engine's worker and summarizer reserve.
    raw_call_seq: AtomicU64,
    /// Whether the verifier's same-family degradation caveat (L-M8) has been
    /// surfaced this run — see [`Pipeline::warn_verifier_caveat`].
    verifier_caveat_warned: AtomicBool,
    /// Whether a verdict's silent degradation to the deterministic heuristic
    /// has been surfaced this run — see [`Pipeline::warn_verifier_fallback`].
    verifier_fallback_warned: AtomicBool,
    /// Whether more than one candidate is currently writing to [`Self::events`]
    /// — set for the duration of a concurrent best-of-N fan-out and false
    /// everywhere else.
    ///
    /// Read only by [`Pipeline::run_engine_turn`], to mute the two event kinds
    /// the wire contract already calls best-effort previews (`TextDelta`,
    /// `Reasoning`). Interleaving those from N models produces one paragraph
    /// of spliced sentences; every durable event — including each candidate's
    /// authoritative `Text` — still goes out live. See
    /// `pipeline::fanout_stage`'s module doc.
    shared_event_lane: AtomicBool,
    /// When this pipeline was constructed — the turn's own elapsed clock, one
    /// pipeline being one turn. Read only by [`repair_gate`].
    started: std::time::Instant,
}

impl<'a> Pipeline<'a> {
    /// Construct a pipeline over the given ports, event sink, and config.
    pub fn new(
        ports: PipelinePorts<'a>,
        events: impl Into<EventSender>,
        config: PipelineConfig,
    ) -> Self {
        let configured_test = config
            .test_command
            .as_deref()
            .map(parse_test_invocation)
            .transpose();
        Self {
            router: ports.router,
            providers: ports.providers,
            tools: ports.tools,
            recall: ports.recall,
            repo: ports.repo,
            repo_status: ports.repo_status,
            touches: ports.touches,
            diagnostics: ports.diagnostics,
            tests: ports.tests,
            lint: ports.lint,
            mutation: ports.mutation,
            coverage: ports.coverage,
            approvals: ports.approvals,
            sleeper: ports.sleeper,
            hooks: ports.hooks,
            candidate_workspaces: ports.candidate_workspaces,
            mcp_prefetch: ports.mcp_prefetch,
            steering: ports.steering,
            turn_gate: None,
            events: events.into(),
            config,
            configured_test,
            raw_call_seq: AtomicU64::new(RECEIPT_SEQ_ALLOCATED_BASE),
            verifier_caveat_warned: AtomicBool::new(false),
            verifier_fallback_warned: AtomicBool::new(false),
            shared_event_lane: AtomicBool::new(false),
            started: std::time::Instant::now(),
        }
    }

    /// Attach a boundary pause gate. Every engine the pipeline builds — the
    /// worker's execute/revise turns and the witness author's — parks at its
    /// step boundaries while the gate holds, and every management call
    /// (triage, verifier, guidance) parks before dispatch: the same safe
    /// boundary as budget aborts, never mid-tool.
    ///
    /// This is the seam that lets a supervisor's pause reach a
    /// pipeline-driven worker at all. Without it only the raw step-loop path
    /// held a gate, so `Fleet::pause_task` on a pipeline worker silently did
    /// nothing — the named follow-up in `fleet_cmd`.
    #[must_use]
    pub fn with_turn_gate(mut self, gate: &'a dyn stella_core::ports::TurnGate) -> Self {
        self.turn_gate = Some(gate);
        self
    }

    /// Drive one prompt through the full staged flow. `messages` is the
    /// caller-owned history: seed it with the stable system prefix (the cached
    /// prompt prefix, L-E8); the pipeline appends the volatile recall+goal
    /// message and every execution turn, and on return holds the selected
    /// candidate's trajectory. `budget` accumulates spend across every stage.
    pub async fn run(
        &self,
        goal: &str,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
    ) -> Result<PipelineOutcome, PipelineRunError> {
        let mut total_cost = 0.0f64;
        if let Err(error) = &self.configured_test {
            return Err(PipelineRunError::new(
                PipelineError::InvalidTestCommand(error.to_string()),
                total_cost,
            ));
        }
        // Checked HERE — before triage, before recall, before a single paid
        // call — because the whole point of the refusal is that the run must
        // not produce a number under a configuration it does not have. A
        // check further down would still refuse, but only after buying the
        // very trajectory the caller must now throw away (#1147).
        if self.config.require_independent_witness
            && let WitnessAuthorIndependence::Unavailable(reason) =
                self.witness_author_independence()
        {
            return Err(PipelineRunError::new(
                PipelineError::WitnessAuthorUnavailable(reason),
                total_cost,
            ));
        }
        // Same probe, second consequence (#1795): the VERDICT grader must be
        // independent too when the caller says so. The probe compares the
        // worker's and verifier's resolved model refs, which is exactly
        // "would the verdict resolve to the worker's model".
        if self.config.require_independent_verifier
            && let WitnessAuthorIndependence::Unavailable(reason) =
                self.witness_author_independence()
        {
            return Err(PipelineRunError::new(
                PipelineError::VerifierNotIndependent(reason),
                total_cost,
            ));
        }
        if messages.is_empty() {
            messages.push(CompletionMessage::system(DEFAULT_SYSTEM_PROMPT));
        }

        // --- 1+2. Evaluate (triage) + context recall, overlapped. ----------
        // Triage's class first gates stage 3 and recall consumes only the
        // goal — no data dependency — so the triage model call and the
        // recall embedding/store scan run concurrently instead of paying
        // both latencies back-to-back on every prompt. Stage-event order is
        // unchanged: join polls triage first (it emits Stage::Triage before
        // its first await), then the recall future emits
        // Stage::ContextRecall before its own first await.
        let recall_future = async {
            self.emit(AgentEvent::Stage {
                name: StageKind::ContextRecall,
            });
            // The ceiling goes INSIDE the future, not around the join: the
            // join must still poll triage to completion so its outcome —
            // including the `UsageIncomplete` envelope its own ceiling emits
            // on expiry (`run_accounted_call`) — cannot disappear from
            // accounting. Recall is unbilled and advisory, so cancelling it
            // on expiry needs no such envelope.
            tokio::time::timeout(self.config.recall_latency_ceiling, self.recall.recall(goal))
                .await
                .unwrap_or_default()
        };
        let (assessment, mut recalled) =
            tokio::join!(self.triage(goal, budget, &mut total_cost), recall_future);
        // Bounded at the source (#616), so every consumer — the user message,
        // the planner prompt, the witness prompt — inherits one budget, and
        // the ContextRecall event reports the frames the turn actually pays
        // for. A mis-tuned recall port must not silently inflate every
        // subsequent turn (N candidates × every revision) past the window.
        let frames = bound_recalled_frames(std::mem::take(&mut recalled.frames));
        self.emit_context_recall(&frames, &recalled);
        let assessment = match assessment {
            Ok(assessment) => assessment,
            Err(abort) => {
                return Ok(self.aborted_before_execute(
                    resolve_task_class(None, goal),
                    total_cost,
                    &abort.reason,
                    AbortKind::DeliberateStop,
                ));
            }
        };
        let task_class = assessment.class;
        // The volatile recall+goal message rides AFTER the stable system
        // prefix (L-E8) — see assemble_user_message. The verification
        // contract rides only on turns that will actually be verified: a
        // conversational turn has no oracle, and a simple lookup is verified
        // only if it unexpectedly touches files — telling either "make this
        // test pass" would invent work.
        let verified_by = self
            .config
            .test_command
            .as_deref()
            .filter(|_| !assessment.conversational && task_class.verifies_unconditionally());
        // The authored-witness decision, taken HERE — before the user message
        // is assembled — so a run with no oracle and no independent author
        // can tell the worker up front that its own failing test is the only
        // deterministic evidence the run will carry (test-first is cheap
        // exactly at the start and unaffordable after the diff exists). The
        // conjunction short-circuits in the same order it did at the
        // single-shot/best-of-N split below, so `can_author…` — which
        // announces the degradation — is still never consulted for a turn
        // that would not have authored a witness anyway.
        let authored_witness = !assessment.conversational
            && self.config.test_command.is_none()
            && self.config.witness_writer
            && assessment.wants_witness()
            && task_class.verifies_unconditionally()
            && self.can_author_independent_witness();
        let contract = match verified_by {
            Some(command) => VerificationContract::Oracle(command),
            None if !assessment.conversational
                && task_class.verifies_unconditionally()
                && !authored_witness =>
            {
                VerificationContract::WorkerTestFirst
            }
            None => VerificationContract::None,
        };
        messages.push(CompletionMessage::user(assemble_user_message(
            goal, &frames, contract,
        )));

        // --- Conversational fast path. -------------------------------------
        // Triage classified this as chat, not a software task (a greeting,
        // small talk, a question about the agent), and the deterministic floor
        // saw no task signal to overrule it (triage::resolve_conversational).
        // Answer in one plain, tool-less completion and skip plan → execute →
        // witness → verify entirely. This is the fix for "typing `hi` authored
        // a witness test": a non-task must never enter the work pipeline.
        if assessment.conversational {
            return self
                .run_conversational(messages, budget, &mut total_cost)
                .await;
        }

        // --- 3+4. Plan, then scope review — one phase, because a reviewer who
        // asks for a different scope sends us back to the planner. -----------
        let plan: Option<Vec<PlanStep>> = if task_class.plans() {
            match self
                .plan_with_review(goal, &frames, budget, &mut total_cost)
                .await
            {
                Ok(PlannedScope::Steps(steps)) => Some(steps),
                Ok(PlannedScope::Ended { reason }) => {
                    return Ok(self.aborted_before_execute(
                        task_class,
                        total_cost,
                        &reason,
                        AbortKind::DeliberateStop,
                    ));
                }
                Err(cause) => return Err(PipelineRunError::new(cause, total_cost)),
            }
        } else {
            None
        };

        // --- 5. Witness + execute + verify (single-shot or best-of-N). ------
        let n = self.config.candidate_count();
        let base_messages = messages.clone();
        // The one frame every candidate stage below reads (#1809). Built here
        // because this is where its last field settles: nothing after this
        // point changes the goal, the staged prefix, the plan, or the class.
        let frame = TaskFrame {
            goal,
            base_messages: &base_messages,
            plan: plan.as_deref(),
            assessment,
        };
        // Decided above, before the user message was assembled (the worker's
        // test-first contract keys off it) and before this single-shot/
        // best-of-N split, because an authored witness is the *only* reason a
        // single candidate needs disposable isolation. Resolving independence
        // later would commit the run to snapshot machinery it then discovers
        // it cannot use — and candidate isolation requires a git working
        // tree, so on a plain directory that is a hard failure rather than an
        // unused cost.
        // The third reason a single candidate needs isolation: the operator
        // asked for this run's work to happen in a worktree rather than in
        // their checkout. Resolved here, beside the other two, so all three
        // reach the same one decision.
        // Asked ONLY when the answer can change what happens. Best-of-N and an
        // authored witness already require a disposable candidate, so the
        // branch below is taken regardless of this value — prompting there
        // would put a question to the operator whose answer is then discarded,
        // which teaches them their choices do not matter. `false` is the safe
        // value in that case precisely because it is unreachable.
        let isolate = if n == 1 && !authored_witness {
            self.isolate_in_worktree(task_class).await
        } else {
            false
        };
        // Single-shot (the default) runs directly over the session ports —
        // zero snapshot/adoption machinery only when the user supplied the
        // test invocation (or witness authoring is otherwise disabled).
        // Authored witnesses always require a disposable candidate, even at
        // N=1, so authoring can never mutate the session tree.
        // Best-of-N runs every candidate in an isolated snapshot of the
        // current tree state and adopts only the winner's changes (L-E7).
        let (best, worker_model_label, candidates_run) = if n == 1 && !authored_witness && !isolate
        {
            let worker = match self.resolve_provider(Role::Worker) {
                Ok(worker) => worker,
                Err(error) => {
                    return Err(PipelineRunError::new(
                        error.into_pipeline_error(),
                        total_cost,
                    ));
                }
            };
            if let Some(fallback) = &worker.fallback {
                self.emit_fallback(fallback);
            }
            let worker_model_label = worker.model_ref.to_string();
            let mut single = self
                .run_shared_candidates(
                    frame,
                    &worker,
                    1,
                    &mut Spend {
                        budget: &mut *budget,
                        total: &mut total_cost,
                    },
                )
                .await;
            let ran = executed_count(&single);
            let best = single
                .pop()
                .expect("run_shared_candidates returns one result per requested candidate");
            (best, Some(worker_model_label), ran)
        } else {
            match self
                .run_best_of_n(
                    frame,
                    n,
                    &frames,
                    authored_witness,
                    &mut Spend {
                        budget: &mut *budget,
                        total: &mut total_cost,
                    },
                )
                .await
            {
                Ok(result) => result,
                Err(cause) => return Err(PipelineRunError::new(cause, total_cost)),
            }
        };

        // The "never choose nothing" backstop. A candidate that aborted
        // BEFORE the worker ever ran — a setup failure (no isolation port, no
        // independent witness author, a tree that could not be snapshotted) —
        // must not end the turn having executed nothing. The fancy path being
        // unavailable is a reason to do LESS, never a reason to do nothing:
        // degrade to a bare worker run on the working tree. Execution aborts
        // (budget, loop, step-cap) and true resource limits keep their stop —
        // there the worker DID run, so the abort is honest.
        let (best, candidates_run) = if best.aborted.is_some() && best.degradable {
            match self
                .degrade_to_bare_execution(
                    frame,
                    &mut Spend {
                        budget: &mut *budget,
                        total: &mut total_cost,
                    },
                )
                .await
            {
                // The bare run is the one candidate that actually executed.
                Some(executed) => (executed, candidates_run + 1),
                // Even the bare run could not start (no resolvable worker) —
                // that is a genuine impossibility, so keep the setup abort.
                None => (best, candidates_run),
            }
        } else {
            (best, candidates_run)
        };

        // Adopt the winning candidate's trajectory.
        *messages = best.messages;

        // --- 6. Complete. --------------------------------------------------
        if let Some(abort) = best.aborted {
            // One abort is one `error` event. A turn-originated abort already
            // crossed the bus inside the worker turn (or, for a soft stop /
            // host cancel, deliberately did not — a decision is not a
            // failure); only a pipeline-originated abort still owes the
            // stream its event (#1524).
            if !abort.from_turn {
                self.emit(AgentEvent::Error {
                    message: abort.reason.clone(),
                    retryable: false,
                });
            }
            return Ok(PipelineOutcome {
                status: PipelineStatus::Aborted {
                    reason: abort.reason,
                    kind: abort.kind,
                },
                task_class,
                final_text: best.final_text,
                total_cost_usd: total_cost,
                verdict: best.verdict,
                score: Some(best.score),
                revisions: best.revisions,
                candidates_run,
            });
        }

        self.emit(AgentEvent::Stage {
            name: StageKind::Complete,
        });
        let status = match &best.verdict {
            Some(verdict) if !verdict.passed => PipelineStatus::VerificationFailed {
                verdict: verdict.clone(),
            },
            _ => PipelineStatus::Completed,
        };
        match &status {
            PipelineStatus::Completed => self.emit(AgentEvent::Complete {
                // The label is `None` only when the candidate path returned
                // before it resolved a worker (a setup abort that then
                // degraded to a bare run). Re-resolve rather than emit
                // `Complete { model: "" }` — a terminal event that names no
                // model reads to every consumer as "no model ran", which is
                // exactly backwards on a path that did the work.
                model: worker_model_label
                    .or_else(|| {
                        self.resolve_provider(Role::Worker)
                            .ok()
                            .map(|worker| worker.model_ref.to_string())
                    })
                    .unwrap_or_default(),
                cost_usd: total_cost,
            }),
            PipelineStatus::VerificationFailed { verdict } => {
                self.emit(AgentEvent::Error {
                    message: format!("verification failed: {}", verdict.summary),
                    retryable: false,
                });
            }
            PipelineStatus::Aborted { .. } => {
                unreachable!("aborted candidates return before terminal verification")
            }
        }
        Ok(PipelineOutcome {
            status,
            task_class,
            final_text: best.final_text,
            total_cost_usd: total_cost,
            verdict: best.verdict,
            score: Some(best.score),
            revisions: best.revisions,
            candidates_run,
        })
    }

    // Stage: triage

    async fn triage(
        &self,
        goal: &str,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<TaskAssessment, PipelineBudgetAbort> {
        self.emit(AgentEvent::Stage {
            name: StageKind::Triage,
        });
        // Deterministic short-circuit, BEFORE the paid call.
        //
        // `resolve_conversational` is a disjunction whose first term ignores
        // the model entirely, so a `true` here with `model_says_chat = false`
        // means the greeting arm fired — and no triage answer could change the
        // outcome. Classifying `hi` used to cost a full round-trip plus, on a
        // wedged provider, up to `triage_latency_ceiling` of dead air, for a
        // route the module docs already describe as never depending on a model
        // answer. This is the same assessment the resolution-failure arm below
        // builds; it just stops paying for it first.
        if resolve_conversational(false, goal) {
            return Ok(TaskAssessment {
                conversational: true,
                ..TaskAssessment::from_class(resolve_task_class(None, goal))
            });
        }
        let resolved = match self.resolve_provider(Role::Triage) {
            Ok(r) => r,
            // Triage resolution failure is soft: fall through to the full path
            // via the deterministic floor. Never fail the run on triage.
            // The conversational route is still resolved deterministically here
            // (`resolve_conversational(false, goal)`) — a bare greeting must
            // route to chat even when the triage provider can't be resolved,
            // since it never depends on a model answer.
            Err(_) => {
                return Ok(TaskAssessment {
                    conversational: resolve_conversational(false, goal),
                    ..TaskAssessment::from_class(resolve_task_class(None, goal))
                });
            }
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }

        let assessment = match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::Triage,
                    resolved: &resolved,
                    messages: triage_prompt(goal, &self.repo.structure_summary().await)
                        .into_messages(),
                    policy: RetryPolicy::deterministic(),
                    overrides: &self.config.role_overrides.triage,
                    timeout: Some(self.config.triage_latency_ceiling),
                },
                budget,
                total,
            )
            .await
        {
            Ok(result) => parse_triage_response(&result.text),
            Err(RawCallError::Budget(abort)) => return Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => None,
        };
        // The class still goes through `resolve_task_class` so a failed or
        // unparseable triage lands on the deterministic floor exactly as
        // before; a real assessment keeps its own assurance flags.
        // Resolve the conversational route once, up front: it must hold even
        // when the triage model call failed/was unparseable (the `None` arm),
        // because a bare greeting is deterministic and should never depend on a
        // model answer. `resolve_conversational` also applies the floor veto to
        // an over-eager model `chat` — a goal with real task signal is work.
        //
        // A headless run never routes to chat on the model's opinion: its goal
        // arrived from a script, a CI job, or a benchmark harness, so there is
        // nobody chatting, and the chat path is terminal no-work — a misroute
        // there silently drops the task with no revision possible. The
        // deterministic greeting arm above stays (`stella run "thanks"` is
        // still not a task); only the model's say is withheld.
        let model_says_chat =
            !self.config.headless && assessment.map(|a| a.conversational).unwrap_or(false);
        let conversational = resolve_conversational(model_says_chat, goal);
        // The witness decision is resolved here for the same reason as the
        // conversational one: it must hold even when the triage call failed or
        // was unparseable. `resolve_witness` is the deterministic *ceiling* —
        // the mirror of the floor above, and the only thing allowed to move
        // assurance down. It fires on one shape (a bare deletion of a named
        // artifact) where an authored witness has nothing to fail against and
        // the author can only invent something vacuous.
        let resolved = match assessment {
            Some(assessment) => {
                let class = resolve_task_class(Some(assessment.class), goal);
                TaskAssessment {
                    class,
                    conversational,
                    require_witness: Some(resolve_witness(assessment.require_witness, class, goal)),
                    ..assessment
                }
            }
            None => {
                let class = resolve_task_class(None, goal);
                TaskAssessment {
                    conversational,
                    require_witness: Some(resolve_witness(None, class, goal)),
                    ..TaskAssessment::from_class(class)
                }
            }
        };
        // The turn's assurance PLAN, published the moment it is decided and
        // before any later stage can fail, abort, or decline to run.
        //
        // Every other proof step reports something that happened, so the most
        // common outcome by far — triage deciding this change does not warrant
        // a test — used to produce no steps at all and leave the surface with
        // nothing to say about the thing it exists to say. A declared plan
        // makes "we chose not to" a statement rather than an absence.
        //
        // Not emitted for a conversational turn: there is no work, so there is
        // no assurance question, and answering an unasked one is noise.
        if !resolved.conversational {
            self.emit_proof(ProofStep::Assurance {
                witness: resolved.wants_witness(),
                verifier: resolved.wants_verifier(),
            });
        }
        Ok(resolved)
    }

    // Conversational fast path

    /// Answer a non-task (greeting / small talk / meta question) in a single
    /// plain completion, then return. No plan, no witness, no execute turn, no
    /// verify — the whole reason this path exists is that a bare `hi` must
    /// never enter the work pipeline. Runs the worker provider with a
    /// conversational system prompt; [`Self::metered_raw_call`] carries no
    /// tools, so the model cannot touch the tree even if it tried.
    async fn run_conversational(
        &self,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        total_cost: &mut f64,
    ) -> Result<PipelineOutcome, PipelineRunError> {
        let resolved = match self.resolve_provider(Role::Worker) {
            Ok(r) => r,
            Err(error) => {
                return Err(PipelineRunError::new(
                    error.into_pipeline_error(),
                    *total_cost,
                ));
            }
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }
        // Keep the running conversation for coherent multi-turn small talk, but
        // swap the leading engineering system prompt for the conversational one
        // so the reply is a chat turn. Only ever *replace* a system message:
        // `run` seeds one when the caller's history is empty, but a caller that
        // seeded a non-system first message (the contract asks for a stable
        // system prefix; nothing enforces it) must not have that message
        // silently destroyed — prepend in front of it instead. The swap is
        // local to `convo`, so the caller's own prefix — and its prompt-cache
        // hits, L-E8 — survive the turn untouched.
        let mut convo = conversational_window(messages);
        let leads_with_system = convo
            .first()
            .is_some_and(|message| matches!(message.role, MessageRole::System));
        if leads_with_system {
            convo[0] = CompletionMessage::system(CONVERSATIONAL_SYSTEM_PROMPT);
        } else {
            convo.insert(0, CompletionMessage::system(CONVERSATIONAL_SYSTEM_PROMPT));
        }

        let overrides = RoleCallOverrides::default();
        let reply = match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::Worker,
                    resolved: &resolved,
                    messages: convo,
                    policy: RetryPolicy::standard(),
                    overrides: &overrides,
                    timeout: self.config.engine.model_timeout,
                },
                budget,
                total_cost,
            )
            .await
        {
            Ok(result) => result.text,
            Err(RawCallError::Budget(abort)) => {
                return Ok(self.aborted_before_execute(
                    TaskClass::SimpleLookup,
                    *total_cost,
                    &abort.reason,
                    AbortKind::DeliberateStop,
                ));
            }
            // Even chat needs the model; if it is down, abort cleanly rather
            // than fabricate a reply.
            Err(RawCallError::Provider | RawCallError::Timeout) => {
                return Ok(self.aborted_before_execute(
                    TaskClass::SimpleLookup,
                    *total_cost,
                    "conversational reply unavailable",
                    AbortKind::Failure,
                ));
            }
        };

        // Adopt the assistant turn into the running trajectory (the same thing
        // the worked path does via `*messages = best.messages`) so a follow-up
        // keeps context.
        messages.push(CompletionMessage::assistant(reply.clone()));
        self.emit(AgentEvent::Text {
            text: reply.clone(),
        });
        self.emit(AgentEvent::Stage {
            name: StageKind::Complete,
        });
        self.emit(AgentEvent::Complete {
            model: resolved.model_ref.to_string(),
            cost_usd: *total_cost,
        });
        Ok(PipelineOutcome {
            status: PipelineStatus::Completed,
            task_class: TaskClass::SimpleLookup,
            final_text: reply,
            total_cost_usd: *total_cost,
            verdict: None,
            // The simple-lookup fast path never verifies, so there is nothing
            // to grade — reported as ungraded rather than as a weak pass.
            score: None,
            revisions: 0,
            candidates_run: 1,
        })
    }

    // Stage: plan

    /// `revision` is the reviewer's note from a rejected scope card, or `None`
    /// for a turn's first plan.
    async fn plan_stage(
        &self,
        goal: &str,
        recall: &[RecalledFrame],
        repo_structure: &str,
        revision: Option<&str>,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<Vec<PlanStep>, PipelineBudgetAbort> {
        self.emit(AgentEvent::Stage {
            name: StageKind::Plan,
        });
        let fallback_plan = || vec![PlanStep::new(goal)];

        let resolved = match self.resolve_provider(Role::Plan) {
            Ok(r) => r,
            Err(_) => return Ok(fallback_plan()),
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }

        let prompt = build_planner_prompt(goal, recall, repo_structure, revision);
        // Plan rides the worker's settings (same router tier, same tuning).
        let worker_overrides = RoleCallOverrides::default();
        let result = match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::Plan,
                    resolved: &resolved,
                    messages: vec![CompletionMessage::user(prompt)],
                    policy: RetryPolicy::standard(),
                    overrides: &worker_overrides,
                    timeout: self.config.engine.model_timeout,
                },
                budget,
                total,
            )
            .await
        {
            Ok(r) => r,
            Err(RawCallError::Budget(abort)) => return Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => return Ok(fallback_plan()),
        };

        if let Some(steps) = parse_plan(&result.text) {
            return Ok(steps);
        }

        // One bounded JSON-repair retry (L-V2), deterministic (no retry-hang).
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::PlanRepair,
                    resolved: &resolved,
                    messages: vec![CompletionMessage::user(plan_repair_prompt(&result.text))],
                    policy: RetryPolicy::deterministic(),
                    overrides: &worker_overrides,
                    timeout: self.config.engine.model_timeout,
                },
                budget,
                total,
            )
            .await
        {
            Ok(repair) => {
                if let Some(steps) = parse_plan(&repair.text) {
                    return Ok(steps);
                }
            }
            Err(RawCallError::Budget(abort)) => return Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => {}
        }

        // Degrade to a single-step plan rather than failing — a planner that
        // won't produce a parseable plan must still let the work proceed.
        Ok(fallback_plan())
    }

    // Stage: execute + verify — candidate generation and selection

    /// Last-resort execution when candidate setup failed before the worker
    /// ever ran. Runs exactly one worker turn on the session tree — no
    /// isolation, no authored witness, the simplest path that still does the
    /// work. Returns `None` only when there is no resolvable worker provider
    /// (a true impossibility, not a degradable setup failure), in which case
    /// the caller keeps the original setup abort.
    async fn degrade_to_bare_execution(
        &self,
        frame: TaskFrame<'_>,
        spend: &mut Spend<'_>,
    ) -> Option<CandidateResult> {
        let worker = self.resolve_provider(Role::Worker).ok()?;
        if let Some(fallback) = &worker.fallback {
            self.emit_fallback(fallback);
        }
        self.warn(
            "candidate setup failed before any execution; running a bare worker turn on the \
             working tree so the turn still does the work it was asked to"
                .to_string(),
        );
        self.run_shared_candidates(frame, &worker, 1, spend)
            .await
            .pop()
    }

    /// Run `n` candidates sequentially over the session ports (the real
    /// working tree): the single-shot path, and the shared-tree degradation
    /// of best-of-N when no [`CandidateWorkspacePort`] is wired.
    async fn run_shared_candidates(
        &self,
        frame: TaskFrame<'_>,
        worker: &ResolvedRole<'a>,
        n: u32,
        spend: &mut Spend<'_>,
    ) -> Vec<CandidateResult> {
        let surface = CandidateSurface {
            diagnostics: self.diagnostics,
            tests: self.tests,
            lint: self.lint,
            mutation: self.mutation,
            coverage: self.coverage,
            repo_status: self.repo_status,
            cwd: None,
            hook_runner: None,
            workspace: None,
        };
        // One mirror, one cursor per candidate — see `candidate_steering`.
        let fan = self.steering.map(SteeringFanOut::new);
        let mut results: Vec<CandidateResult> = Vec::with_capacity(n as usize);
        for i in 0..n {
            self.emit_text(candidate_start_notice(i, n, false));
            // Per candidate, because its steering view is (cheap: no provider setup).
            let mut engine = Engine::with_sleeper(
                worker.provider,
                self.tools,
                self.config.engine.clone(),
                self.sleeper,
            );
            if let Some((hooks, runner)) = self.hooks {
                engine = engine.with_hooks(hooks, runner);
            }
            if let Some(gate) = self.turn_gate {
                engine = engine.with_gate(gate);
            }
            let view = fan.as_ref().map(|fan| fan.candidate());
            if let Some(view) = view.as_ref() {
                engine = engine.with_steering(view);
            }
            results.push(
                self.run_candidate(
                    frame,
                    // A shared-tree run has no workspace to graft into and no
                    // pristine snapshot to author blind in, so it never buys a
                    // witness — exactly as before, when it was passed `None`.
                    None, &engine, surface, spend,
                )
                .await,
            );
        }
        results
    }

    /// Best-of-N over isolated workspaces (L-E7): each candidate executes and
    /// verifies inside its own snapshot of the current tree state, so
    /// siblings never see each other's edits and losers leave no residue —
    /// only the winner's changes are applied to the real tree. Cleanup is
    /// unconditional (success or failure) with one deliberate exception: a
    /// winner whose adoption failed keeps its workspace, named in the error,
    /// so completed work is never destroyed.
    async fn run_best_of_n(
        &self,
        frame: TaskFrame<'_>,
        n: u32,
        frames: &[RecalledFrame],
        author_witness: bool,
        spend: &mut Spend<'_>,
    ) -> Result<(CandidateResult, Option<String>, u32), PipelineError> {
        // Orchestrator pre-fetch (issue #248) — see `crate::mcp_prefetch::fold`.
        let prefetched = crate::mcp_prefetch::fold(self.mcp_prefetch, n, frame.base_messages).await;
        let frame = TaskFrame {
            base_messages: prefetched.as_deref().unwrap_or(frame.base_messages),
            ..frame
        };
        let Some(port) = self.candidate_workspaces else {
            if author_witness {
                return Ok((
                    CandidateResult::setup_aborted(
                        frame.base_messages.to_vec(),
                        "authored witness requires candidate isolation, but no candidate \
                         workspace port is available"
                            .to_string(),
                    ),
                    None,
                    // Nothing was dispatched: the isolation port is missing, so
                    // no candidate ever reached a model.
                    0,
                ));
            }
            // No isolation port (non-git workspace, or a caller that never
            // wired one): the historical shared-tree behavior, made loud —
            // candidates see each other's residue and losers' edits stay on
            // disk.
            self.warn(format!(
                "best-of-N candidate isolation is unavailable (no candidate workspace port): \
                 {n} candidates will run sequentially in the shared working tree, and losing \
                 candidates' file changes will not be rolled back"
            ));
            let worker = self
                .resolve_provider(Role::Worker)
                .map_err(RoleResolveError::into_pipeline_error)?;
            if let Some(fallback) = &worker.fallback {
                self.emit_fallback(fallback);
            }
            let label = worker.model_ref.to_string();
            let candidates = self.run_shared_candidates(frame, &worker, n, spend).await;
            let best_idx = best_index(&candidates);
            let ran = executed_count(&candidates);
            self.emit_text(candidate_winner_notice(best_idx, n, ran));
            return Ok((
                candidates
                    .into_iter()
                    .nth(best_idx)
                    .expect("best_index returns an in-range index"),
                Some(label),
                ran,
            ));
        };

        // Resolve both identities before creating a workspace or dispatching
        // the witness. Authored verification is meaningful only when its
        // author is actually independent from the worker.
        let worker = self
            .resolve_provider(Role::Worker)
            .map_err(RoleResolveError::into_pipeline_error)?;
        if let Some(fallback) = &worker.fallback {
            self.emit_fallback(fallback);
        }
        let worker_label = worker.model_ref.to_string();
        // Losing the independent author costs the run its authored witness —
        // it must never cost the run the whole task. A single-model
        // configuration (every role pinned to one model, as benchmark and
        // solo-provider setups do) previously aborted here after one model
        // call, having done no work at all. Degrade to the unauthored verify
        // ladder instead, and say so once.
        // Whether this is the `create_worktrees` caller — a plain single-shot
        // run the operator asked to happen in a worktree — as opposed to
        // best-of-N or an authored witness. Read from the CALLER's argument,
        // before the degradation below shadows it, because a requested-then-
        // degraded authored witness is still not this caller.
        //
        // Exact by construction: `run` takes the direct path when
        // `n == 1 && !authored_witness && !isolate`, so reaching here with
        // `n == 1 && !author_witness` means `isolate` was the only reason.
        let single_shot_isolation = n == 1 && !author_witness;
        // `can_author_independent_witness` already gated `author_witness` and
        // announced any degradation, so this is the invariant guard for that
        // decision — silent on purpose, never a second warning.
        let mut author_witness = author_witness;
        let witness_author = match author_witness
            .then(|| self.resolve_provider(Role::Verifier))
            .and_then(Result::ok)
            .filter(|author| author.model_ref != worker.model_ref)
        {
            Some(author) => {
                if let Some(fallback) = &author.fallback {
                    self.emit_fallback(fallback);
                }
                Some(author)
            }
            None => {
                author_witness = false;
                None
            }
        };

        // Isolation is created in index order even when the candidates then
        // run together, and so is the second snapshot a witness author needs —
        // `SerialCreates` is what makes the second one obey too. See
        // `fanout_stage`'s module doc for why git, and not the model calls, is
        // the thing worth serializing.
        let serialized = SerialCreates::new(port);
        let port: &dyn CandidateWorkspacePort = &serialized;
        let width = fan_out_width(n, self.config.candidate_concurrency);
        self.emit_text(candidate_fanout_notice(n, width));
        // Index-aligned with the results below, so adoption can pair a
        // workspace with the result that produced it (and with the witness
        // paths that result carries). `Err` marks a candidate that never got a
        // workspace, and carries the reason it will be scored with.
        let workspaces = self.create_candidate_workspaces(port, n).await;
        // Handed to the candidate rather than spent here: whether a run buys a
        // witness is not knowable until the candidate has executed and its diff
        // can be read.
        let authoring = author_witness.then(|| WitnessAuthoring {
            port,
            author: witness_author
                .as_ref()
                .expect("authored witness identity is resolved before dispatch"),
            frames,
        });
        let candidates = self
            .dispatch_isolated_candidates(frame, &worker, authoring, &workspaces, width, spend)
            .await;

        let best_idx = best_index(&candidates);
        // Counted before the winner is moved out — the report is about the
        // whole fan-out, not the one result that survives it.
        let ran = executed_count(&candidates);
        self.emit_text(candidate_winner_notice(best_idx, n, ran));
        // Winner adoption + cleanup. An aborted winner adopts nothing — an
        // aborted best-of-N run leaves the real tree untouched.
        let mut adopt_failure: Option<WorkspaceError> = None;
        for (i, slot) in workspaces.into_iter().enumerate() {
            let Ok(ws) = slot else {
                continue;
            };
            if i == best_idx
                && candidates[best_idx]
                    .verdict
                    .as_ref()
                    .is_some_and(|verdict| verdict.passed)
            {
                // The witness has already done its whole job by now — it armed
                // the flip oracle and the flip was observed — so withholding it
                // cannot change the verdict, only what lands in the tree.
                let withhold: &[String] = if self.config.keep_witness {
                    &[]
                } else {
                    &candidates[best_idx].witness_paths
                };
                match ws.adopt(withhold).await {
                    Ok(adopted) => {
                        // Surface the adopted changes on the event stream: the
                        // winner's edits happened inside the snapshot, so no
                        // FileChange was emitted for the real tree yet. The
                        // adoption measured them (git's numstat + patch), so
                        // these rows carry a real delta — they used to arrive
                        // with `diff: None` and no counts, which the Files tab
                        // rendered as `+0 -0` for every adopted file.
                        for change in adopted {
                            self.emit(AgentEvent::FileChange {
                                path: change.path,
                                kind: change.kind,
                                added: change.added,
                                removed: change.removed,
                                diff: change.diff,
                            });
                        }
                        ws.remove().await;
                    }
                    // Keep the workspace: the error names its path and the
                    // conflicting files; the winning work stays recoverable.
                    Err(e) => adopt_failure = Some(e),
                }
            } else if single_shot_isolation {
                // The `create_worktrees` run that did not pass. Discarding is
                // right for best-of-N — the operator asked for the best of
                // several and none was good, and the losers were never meant to
                // survive. It is also right for an authored-witness abort,
                // where the candidate is *poisoned* rather than merely
                // unverified (a tampered artifact, an author that edited
                // production code) and `witness_isolation`'s tests pin its
                // removal.
                //
                // It is wrong for this third caller. That operator asked where
                // the work should *happen*, not that it be thrown away unless
                // it verified — and without this they end up strictly worse off
                // than with isolation off, where a failed run at least leaves
                // its changes in the tree to look at. So keep the snapshot and
                // name it: the same posture as the adopt-failure arm just
                // above, and for the same reason — unverified is not worthless.
                self.warn(format!(
                    "this run's changes did not verify, so they were not adopted into your \
                     working tree — they are kept at {} for you to inspect or salvage",
                    ws.root()
                ));
            } else {
                ws.remove().await;
            }
        }
        let mut best = candidates
            .into_iter()
            .nth(best_idx)
            .expect("best_index returns an in-range index");
        if let Some(e) = adopt_failure {
            best.aborted = Some(CandidateAbort {
                reason: e.to_string(),
                kind: AbortKind::Failure,
                from_turn: false,
            });
        }
        Ok((best, Some(worker_label), ran))
    }

    // Stages: execute + verify + revise (one candidate)

    async fn run_candidate(
        &self,
        frame: TaskFrame<'_>,
        authoring: Option<WitnessAuthoring<'_>>,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        spend: &mut Spend<'_>,
    ) -> CandidateResult {
        // Flip oracle: for classes we always verify, take a pre-execute
        // baseline of the test command so a later pass counts as a genuine
        // fail→pass flip (L-E11). Simple lookups skip the baseline — they are
        // only verified at all if the zero-diff guard trips, and then the
        // absence of a baseline correctly leaves the oracle unflipped.
        //
        // This is the *configured* command only. A user who names a test
        // command names one that exists now, so its baseline is a real
        // observation of this candidate's own surface, taken here. An authored
        // witness has no command at this point — it is not written until after
        // execution, once the warrant has established there is something to
        // prove — and its baseline is observed where it is written, in a
        // pristine snapshot of this same pre-execution tree.
        let mut oracle = FlipOracle::new();
        let mut oracle_trace = Vec::new();
        // Armed below, and only by a baseline that actually FAILED. A command
        // that was already green cannot flip, so arming on it would hand the
        // engine a stop signal for work the turn never did.
        let mut flip_halt: Option<Arc<FlipHalt>> = None;
        if frame.assessment.class.verifies_unconditionally()
            && let Some(cmd) = self.effective_test_command(None)
        {
            let pre = self.run_test_observed(surface.tests, cmd.invocation).await;
            // #860: only a completed run is an oracle observation. A baseline
            // that timed out or never spawned observed no assertion, so it
            // must not lock the oracle's `Failing` precondition — that is how
            // infra noise plus a merely-faster candidate used to read as a
            // verified fail→pass flip.
            if let Some(passed) = pre.assertion_result() {
                // Output rides along for the same-failure rule (#867): a
                // failing baseline contributes the test names a later flip
                // must actually fix.
                let output = format!("{}\n{}", pre.stdout_tail, pre.stderr_tail);
                oracle.observe_run(cmd.command, passed, &output);
                oracle_trace.push(OracleObservation {
                    tree: ProofTree::Baseline,
                    passed,
                });
                self.emit_proof(ProofStep::Oracle {
                    command: cmd.command.to_string(),
                    passed,
                    tree: ProofTree::Baseline,
                    run: None,
                    runs_required: None,
                    seed: None,
                });
                if !passed {
                    flip_halt = Some(Arc::new(FlipHalt::new(cmd.command)));
                }
            }
        }

        // Baseline lint snapshot for the regression veto (#861) — eager only
        // where it must be. An in-place candidate executes into the session
        // tree, so its pre-execution diagnostics are unreadable after this
        // point; an isolated candidate leaves the session tree pristine, so
        // its baseline is read lazily at audit time and a candidate that
        // never reaches a fast-submit pays no lint run at all. Gated on the
        // classes that verify: a fast-submit needs a flip, so where no flip
        // is possible the snapshot would be spend without a consumer.
        let lint_baseline = if surface.workspace.is_none()
            && frame.assessment.class.verifies_unconditionally()
            && (self.effective_test_command(None).is_some() || self.config.witness_writer)
        {
            match surface.lint {
                Some(probe) => probe.snapshot(surface.cwd).await,
                None => None,
            }
        } else {
            None
        };

        // Snapshot untracked files (with content fingerprints) BEFORE
        // executing so `gather_diff` can tell files this turn created OR
        // modified from pre-existing dirty state — a stale untracked file with
        // an unchanged fingerprint is not this turn's work, but one the turn
        // edited (fingerprint changed) is.
        let untracked_before = surface.repo_status.untracked_fingerprints().await;

        let mut state = CandidateState {
            messages: candidate_narration::messages_rooted_at(
                frame.base_messages,
                surface.workspace,
            ),
            final_text: String::new(),
            signals: ChangeSignals::default(),
            flip_halt,
            oracle,
            oracle_trace,
            untracked_before,
            diff_lines: 0,
            diff_text: String::new(),
            diff_available: true,
            touch_baseline: {
                // Bracket the turn at the same instant the baseline is taken,
                // so the before-image and the count describe one moment.
                self.touches.begin_workspace_probe();
                self.touches.mutations_recorded()
            },
            lint_baseline,
            witness_mutation: None,
            diff_coverage: DiffCoverage::Unmeasured,
            revisions: 0,
            evidence_demands: 0,
            witness_paths: Vec::new(),
            witness_baseline_symptom: None,
            failures: Vec::new(),
            last_verdict: None,
            last_verdict_diff: None,
        };

        if let Err(abort) = self
            .execute_plan(frame.plan, engine, spend, &mut state)
            .await
        {
            return CandidateResult::turn_aborted(state.messages, abort);
        }

        // Decide whether to verify: unconditional for single/multi; for a
        // simple lookup, only if the turn unexpectedly touched files (the
        // zero-diff guard, L-E2). "Touched files" = FileChange events observed
        // OR a non-empty diff from a turn that dispatched something able to
        // write. The second conjunct is #1553: the diff reads the WORKING
        // TREE, and in a shared worktree the tree can move under a run —
        // a human editing beside it. `mutating_actions` is counted off the
        // calls this pipeline dispatched, not off any look at the world, so
        // a lookup that dispatched nothing that could write cannot own the
        // motion the diff shows, and must not be dragged into verification
        // over someone else's edit.
        let probe = self.gather_diff(surface, &state.untracked_before).await;
        self.absorb_probe(&mut state, probe);
        let files_touched = state.signals.file_changes > 0
            || (state.signals.mutating_actions > 0 && !state.diff_text.trim().is_empty());
        let should_verify = frame.assessment.class.verifies_unconditionally()
            || (frame.assessment.class == TaskClass::SimpleLookup && files_touched);
        if !should_verify {
            // A clean lookup: nothing to verify.
            return state.into_unverified();
        }

        // The warrant's answer, published from the one place that always runs
        // for a verifying candidate. `witness_on_demand` and
        // `warranted_completion` each re-ask it (the call is pure and cheap),
        // but neither is reached on every path — authoring is skipped whenever
        // there is no independent author, and the completion shortcut returns
        // early when a test IS required. Emitting from either would make the
        // rail's first row appear only on some runs, which is the failure this
        // whole surface exists to end.
        let warrant = warrant(&state.diff_text, state.signals);
        self.emit_proof(ProofStep::Warrant {
            required: warrant.is_required(),
            reason: warrant.reason().map(|r| r.sentence().to_string()),
            diff_lines: state.diff_lines,
        });

        // Buy the witness now, or not at all. Everything above this line has
        // already happened, so the diff is evidence rather than a prediction —
        // which is the whole reason authoring waits until here.
        let witness = match self
            .witness_on_demand(frame.goal, authoring, surface, &mut state, spend)
            .await
        {
            Ok(witness) => witness,
            // A witness-stage budget stop: pipeline policy, so this abort is
            // deliberate and still owes the stream its one error event.
            Err(reason) => {
                return CandidateResult::aborted(state.messages, reason, AbortKind::DeliberateStop);
            }
        };

        self.verify_candidate(frame, witness.as_ref(), engine, surface, spend, state)
            .await
    }

    /// Execute stage: one turn for simple/single-task; one turn per plan step
    /// for multi-step (each step guides a fresh engine turn). The last turn's
    /// text lands in `state.final_text`; `Err` is the first aborted turn's
    /// reason and kind, kept typed so the driver-side emit is not repeated.
    async fn execute_plan(
        &self,
        plan: Option<&[PlanStep]>,
        engine: &Engine<'_>,
        spend: &mut Spend<'_>,
        state: &mut CandidateState,
    ) -> Result<(), TurnAbort> {
        self.emit(AgentEvent::Stage {
            name: StageKind::Execute,
        });
        // Borrowed, not collected: the steps are only read, so materializing a
        // `Vec<&PlanStep>` per candidate bought nothing.
        let steps: &[PlanStep] = plan.unwrap_or_default();
        if steps.is_empty() {
            match self
                .run_engine_turn(
                    engine,
                    &mut state.messages,
                    spend.budget,
                    &mut state.signals,
                    state.flip_halt.clone(),
                )
                .await
            {
                TurnOutcome::Completed { text, cost_usd } => {
                    state.final_text = text;
                    *spend.total += cost_usd;
                }
                TurnOutcome::Aborted {
                    reason,
                    kind,
                    cost_usd,
                } => {
                    *spend.total += cost_usd;
                    return Err(TurnAbort { reason, kind });
                }
            }
        } else {
            let n = steps.len();
            for (i, step) in steps.iter().enumerate() {
                state
                    .messages
                    .push(CompletionMessage::user(plan_steps::step_prompt(
                        i,
                        n,
                        &step.description,
                    )));
                match self
                    .run_engine_turn(
                        engine,
                        &mut state.messages,
                        spend.budget,
                        &mut state.signals,
                        state.flip_halt.clone(),
                    )
                    .await
                {
                    TurnOutcome::Completed { text, cost_usd } => {
                        *spend.total += cost_usd;
                        // #1702: a worker that declares the whole goal done
                        // ends the walk — the remaining steps could only
                        // re-confirm finished work, and a false declaration
                        // is the verify stage's to refute, not this loop's.
                        let closed_out = plan_steps::goal_declared_complete(&text);
                        state.final_text = text;
                        if closed_out {
                            break;
                        }
                    }
                    TurnOutcome::Aborted {
                        reason,
                        kind,
                        cost_usd,
                    } => {
                        *spend.total += cost_usd;
                        return Err(TurnAbort { reason, kind });
                    }
                }
            }
        }
        Ok(())
    }

    /// Verify + bounded revise loop over an executed candidate: observe the
    /// tests, take the deterministic ladder decision (L-E11), and either
    /// finish with a verdict, escalate to the model verifier, or spend one of
    /// `max_revisions` on a revise pass and re-observe. Owns `state` because
    /// every exit moves it into the returned [`CandidateResult`].
    async fn verify_candidate(
        &self,
        frame: TaskFrame<'_>,
        witness: Option<&Witness>,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        spend: &mut Spend<'_>,
        mut state: CandidateState,
    ) -> CandidateResult {
        self.emit(AgentEvent::Stage {
            name: StageKind::Verify,
        });
        let effective_cmd = self.effective_test_command(witness);
        let witness_paths = Self::witness_paths(witness);
        let meter = repair_gate::RepairMeter::start(*spend.total);
        loop {
            if let Some(workspace) = surface.workspace {
                if let Err(error) = workspace.seal().await {
                    return CandidateResult::aborted(
                        state.messages,
                        format!("candidate could not be sealed for verification: {error}"),
                        AbortKind::Failure,
                    );
                }
                // #1538: the seal just pinned this candidate's final bytes — a
                // real tree already holding them means the candidate wrote
                // through its isolation. Fail it in the round that caused it,
                // not at the winner's adoption.
                let escaped = workspace.escaped_paths().await;
                if !escaped.is_empty() {
                    return CandidateResult::aborted(
                        state.messages,
                        escape_abort_reason(&escaped),
                        AbortKind::Failure,
                    );
                }
            }
            let (touched_tests_passed, test_tail, test_infra) = self
                .observe_touched_tests(surface, effective_cmd, &mut state)
                .await;
            // Tamper exclusion is an authority boundary, not evidence for a
            // model to weigh. Any post-baseline witness mutation hard-fails
            // the candidate before a verifier can override it.
            let mut tampered = Vec::new();
            if let Some(witness) = witness {
                for (path, expected) in &witness.files {
                    let current = surface.repo_status.artifact_identity(path).await;
                    if !witness_identity_matches(expected, current.as_ref()) {
                        tampered.push(path.clone());
                    }
                }
                tampered.sort();
            }
            if !tampered.is_empty() {
                return CandidateResult::aborted(
                    state.messages,
                    format!(
                        "witness artifact changed after its accepted baseline: {}",
                        tampered.join(", ")
                    ),
                    AbortKind::Failure,
                );
            }
            if let Some(workspace) = surface.workspace {
                match workspace.sealed_is_unchanged().await {
                    Ok(true) => {}
                    Ok(false) => {
                        return CandidateResult::aborted(
                            state.messages,
                            "candidate worktree changed after verification".to_string(),
                            AbortKind::Failure,
                        );
                    }
                    Err(error) => {
                        return CandidateResult::aborted(
                            state.messages,
                            format!("could not validate the verified candidate seal: {error}"),
                            AbortKind::Failure,
                        );
                    }
                }
            }
            let mut inputs = LadderInputs {
                flip_achieved: state.oracle.is_flipped(),
                touched_tests_passed,
                diff_lines: state.diff_lines,
                diff_budget: self.config.diff_budget_lines,
                diff_available: state.diff_available,
                file_change_events: state.signals.file_changes,
                mutating_actions: state.signals.mutating_actions,
                // Filled by the pre-submit audit below (#861, #870, #1291) —
                // the lint, coverage and mutation probes only run when a
                // fast-submit is imminent.
                new_diag_errors: 0,
                new_diag_warnings: 0,
                veto_warnings: self.config.diagnostics_veto_warnings,
                witness_tautological: false,
                diff_coverage: DiffCoverage::Unmeasured,
                require_diff_coverage: self.config.require_diff_coverage,
            };

            // Pre-submit audit (#859, #861): a deterministic pass is about
            // to be credited, so spend the two cheap checks that can refute
            // it — gated on the DECISION, not on the flip transition, so
            // paths already headed to the verifier pay nothing extra and the
            // cost is bounded to one lint pass + one suite run per
            // verification round (a revised candidate re-enters the audit),
            // paid only where a credit is about to be spent.
            let mut lint_sample = String::new();
            if matches!(ladder_decision(&inputs), LadderDecision::SubmitFast)
                && let Some(cmd) = effective_cmd
            {
                // Regression veto first (#861): the lint delta is cheaper
                // than a suite re-run, and a veto makes the confirmation
                // moot. New errors (or opted-in warnings) drop the
                // fast-submit rung and the run escalates to the verifier with
                // the delta in evidence.
                let (new_errors, new_warnings, sample) = self.lint_delta(surface, &state).await;
                inputs.new_diag_errors = new_errors;
                inputs.new_diag_warnings = new_warnings;
                lint_sample = sample;

                // Confirmation run (#859), only if the veto left the
                // fast-submit standing: re-run the tracked command once on
                // the same sealed tree. A failed or infra confirmation moves
                // the oracle to `Unstable`; the re-derived decision below
                // then escalates instead of fast-submitting, with
                // `unstable_flip=true` in the evidence.
                if matches!(ladder_decision(&inputs), LadderDecision::SubmitFast) {
                    // Through the retrying runner (#1294) for a reason worth
                    // stating: an OOM'd confirmation demotes the oracle to
                    // `Unstable` below, which is a real cost paid for a run
                    // that observed nothing. Retrying first is what keeps a
                    // memory kill from silently withdrawing a deterministic
                    // pass the candidate had earned.
                    let confirmation = self.run_test_observed(surface.tests, cmd.invocation).await;
                    match confirmation.assertion_result() {
                        Some(passed) => {
                            state.oracle.confirm(passed);
                            state.oracle_trace.push(OracleObservation {
                                tree: ProofTree::Candidate,
                                passed,
                            });
                            self.emit_proof(ProofStep::Oracle {
                                command: cmd.command.to_string(),
                                passed,
                                tree: ProofTree::Candidate,
                                run: None,
                                runs_required: None,
                                seed: None,
                            });
                        }
                        // An unobservable confirmation confirms nothing:
                        // demote without fabricating a pass/fail proof step
                        // (#860).
                        None => state.oracle.confirm(false),
                    }
                    inputs.flip_achieved = state.oracle.is_flipped();
                }

                // Diff coverage (#1291): did the passing run execute the
                // lines this candidate added? Ordered after the confirmation
                // (a flip that could not be reproduced makes the question
                // moot) and before the mutation check (one instrumented run
                // beats up to three witness runs). Every unavailable path —
                // no probe wired, no tooling for this dialect, an unreadable
                // report — leaves the status `Unmeasured`, which withholds
                // nothing unless the operator asked for strictness. A
                // measured non-overlap withholds the deterministic credit and
                // escalates: unproven, never failed.
                if matches!(ladder_decision(&inputs), LadderDecision::SubmitFast)
                    && let Some(probe) = surface.coverage
                {
                    let changed =
                        crate::verify::coverage::changed_lines(&state.diff_text, &witness_paths);
                    let report = probe.covered_lines(surface.cwd, cmd.invocation).await;
                    inputs.diff_coverage =
                        crate::verify::coverage::overlap(&changed, report.as_ref());
                    state.diff_coverage = inputs.diff_coverage;
                }

                // Mutation check (#870), last because it is the most
                // expensive (one witness run per mutant, ≤3) and only for
                // AUTHORED witnesses — a user-configured suite is not the
                // artifact whose tautology this audits. Break the changed
                // lines one at a time; a witness that fails under any mutant
                // has proven it constrains the change (stop early, credit
                // stands). One that stays green under every mutant reacts to
                // the change without constraining it — the flip may not buy
                // a deterministic pass, and the verifier is told why.
                if matches!(ladder_decision(&inputs), LadderDecision::SubmitFast)
                    && witness.is_some()
                    && let Some(probe) = surface.mutation
                {
                    let mutants = crate::verify::mutation::mutants_from_diff(
                        &state.diff_text,
                        &witness_paths,
                    );
                    let mut observed = 0u32;
                    let mut killed = false;
                    for mutant in &mutants {
                        match probe.run_mutant(surface.cwd, mutant, cmd.invocation).await {
                            MutantOutcome::Witness { passed } => {
                                observed += 1;
                                if !passed {
                                    killed = true;
                                    break;
                                }
                            }
                            // Neither evidence for nor against — the check
                            // degrades open on unavailable mutants.
                            MutantOutcome::Unavailable => {}
                            // The original bytes could not be restored: the
                            // tree is no longer the verified candidate.
                            // Fail closed — shipping a mutated tree is
                            // strictly worse than losing the candidate.
                            MutantOutcome::TreePoisoned => {
                                return CandidateResult::aborted(
                                    state.messages,
                                    format!(
                                        "mutation audit could not restore {} after a mutant \
                                         run; the candidate tree is no longer verified",
                                        mutant.path
                                    ),
                                    AbortKind::Failure,
                                );
                            }
                        }
                    }
                    if observed > 0 {
                        state.witness_mutation = Some(killed);
                        inputs.witness_tautological = !killed;
                    }
                }
            }

            // The verdict's provenance (#865): the ladder inputs frozen at
            // decision time, attached to every evidence value emitted below.
            // `witness_intact` states that the tamper exclusion above RAN
            // and passed — a tampered witness never reaches a verdict.
            let snapshot =
                Self::ladder_snapshot(&inputs, &state, test_infra, witness.map(|_| true));

            // Everything the verification side knows about this round's
            // failure. Both failing arms disclose from it, and nothing reaches
            // the worker except through `Pipeline::airlock_forward` or a
            // `redact` of this value.
            let sealed = SealedFailure {
                command: effective_cmd.map_or("", |cmd| cmd.command),
                invocation: effective_cmd.map(|cmd| cmd.invocation),
                output: &test_tail,
                witness_paths: &witness_paths,
            };

            match ladder_decision(&inputs) {
                LadderDecision::SubmitFast => {
                    // Deterministic pass — verifier SKIPPED (L-E11).
                    let mut evidence = deterministic_pass_evidence(
                        state.oracle.tracked_command(),
                        state.diff_lines,
                        state.diff_coverage,
                    );
                    evidence.ladder = Some(Box::new(snapshot.clone()));
                    self.emit(AgentEvent::Verdict {
                        passed: true,
                        evidence: evidence.clone(),
                    });
                    // #1291: the deterministic *badge* is earned only when
                    // something confirmed the test ran the changed lines. An
                    // unmeasured overlap keeps the fast-submit — no verifier
                    // call, no extra turn, the run completes exactly as
                    // before — but scores `Unverified`, because "a test
                    // passed and nobody could check it touched this change"
                    // is unproven, and the score is what the claim is made
                    // in. Cheap by construction: the downgrade costs a
                    // ranking position, never a model call.
                    //
                    // `evidence.deterministic` deliberately stays `true`:
                    // the flip WAS a real test observation, and the
                    // calibration cohorts (#871) partition by evidence kind,
                    // not by coverage. Only the score moves.
                    let proven = state.diff_coverage != DiffCoverage::Unmeasured;
                    return state.into_verified(
                        true,
                        &evidence,
                        score_from_verification(proven, None),
                    );
                }
                LadderDecision::NothingAttempted => {
                    // The turn ended without dispatching one call that could
                    // write anything. Unlike the arm below, no probe failed
                    // here and no evidence is missing — the pipeline is
                    // reporting its own dispatch record — so this reports a
                    // plain `passed: false` and pushes the worker to act.
                    //
                    // Before this arm existed the state fell through to
                    // `Unverifiable`, whose four dark channels it satisfies,
                    // and abstaining reported `passed: true`: eleven
                    // Terminal-Bench trials completed "successfully" having
                    // never touched the task, and Harbor scored every one 0.0.
                    let mut evidence = nothing_attempted_evidence(&inputs);
                    evidence.ladder = Some(Box::new(snapshot.clone()));
                    self.emit(AgentEvent::Verdict {
                        passed: false,
                        evidence: evidence.clone(),
                    });
                    if !self.affords_repair(&state, &meter, *spend.total, spend.budget) {
                        return state.into_verified(
                            false,
                            &evidence,
                            score_from_verification(false, Some(false)),
                        );
                    }
                    if let Err(abort) = self
                        .revise_candidate(
                            engine,
                            surface,
                            NOTHING_ATTEMPTED_NUDGE,
                            spend,
                            &mut state,
                        )
                        .await
                    {
                        return CandidateResult::turn_aborted(state.messages, abort);
                    }
                }
                LadderDecision::Unverifiable => {
                    // The turn went unobserved — every channel blind, or every
                    // channel clear-eyed and empty over dispatched mutating
                    // calls (#1701). The verifier is not asked, because the
                    // only thing it could do is guess from an empty record —
                    // which in the wild it did, returning `FAIL … the file
                    // likely does not exist` about a file that was in the
                    // container (#973).
                    //
                    // `passed: true` because a run is not failed by the absence
                    // of a way to check it, and this shape already exists for
                    // the review-waived path. What keeps it from reading as a
                    // pass is the pair beside it: the summary says UNVERIFIABLE
                    // in its first word, and the score is `Unverified`, so this
                    // candidate can never tie a genuinely verified sibling in
                    // best-of-N and then win the smaller-diff tiebreak.
                    //
                    // The arm above is what makes this defensible. Reaching
                    // here means the turn *did* dispatch mutating calls and no
                    // channel saw their effect — a real state: a Terminal-Bench
                    // trial that wrote its answer through shell redirects
                    // recorded no touch, could not be diffed, landed here, and
                    // scored 1.0 against its verifier. Failing that closed
                    // would report a correct run as broken, and no revision can
                    // clear it — that workspace never becomes observable.
                    let mut evidence = unverifiable_evidence(&inputs);
                    evidence.ladder = Some(Box::new(snapshot.clone()));
                    self.unverifiable(&evidence.summary);
                    self.emit(AgentEvent::Verdict {
                        passed: true,
                        evidence: evidence.clone(),
                    });
                    return state.into_verified(
                        true,
                        &evidence,
                        score_from_verification(false, None),
                    );
                }
                LadderDecision::Revise => {
                    // Deterministic failure (touched tests red) — no verifier.
                    //
                    let (mut evidence, brief) =
                        Self::deterministic_disclosure(&mut state, &sealed, &test_tail);
                    evidence.ladder = Some(Box::new(snapshot.clone()));
                    self.emit(AgentEvent::Verdict {
                        passed: false,
                        evidence: evidence.clone(),
                    });
                    if !self.affords_repair(&state, &meter, *spend.total, spend.budget) {
                        return state.into_verified(
                            false,
                            &evidence,
                            score_from_verification(false, Some(false)),
                        );
                    }
                    // Distress trigger: a SECOND deterministic failure of this
                    // candidate — the ledger is cumulative, so the two need not
                    // be consecutive (#868) — means the evidence alone didn't
                    // steer the worker: spend one verifier call on course-correction
                    // (event-triggered, never a fixed midpoint checkpoint).
                    // Counted from that ledger (`deterministic_disclosure` just
                    // recorded this round's fingerprint), not from `revisions`:
                    // a prior model-verifier FAIL also increments `revisions`,
                    // and gating on it paid a guidance call on the FIRST deterministic
                    // red while telling the verifier the agent had "failed twice in a row".
                    let mut reason = brief.message();
                    if self.config.distress_guidance && state.failures.len() >= 2 {
                        // Same witness exclusion as the verdict call (#1433):
                        // guidance reads the change under correction, and the
                        // verifier's own test is not part of it.
                        let stripped =
                            crate::verify::strip_witness_hunks(&state.diff_text, &witness_paths);
                        let prompt = guidance_prompt(
                            frame.goal,
                            &stripped.diff,
                            &evidence.summary,
                            // Guidance never carries a delta baseline: its
                            // render is already evidence-scoped (#1432), and
                            // "unchanged since a verdict round" is a
                            // verdict-shaped claim.
                            &DiffContext {
                                witness_paths: &witness_paths,
                                previous: None,
                            },
                        );
                        match self
                            .verifier_guidance(prompt, spend.budget, spend.total)
                            .await
                        {
                            Ok(Some(guidance)) => {
                                if let Some(text) =
                                    self.airlock_forward(&guidance, "distress_guidance", &sealed)
                                {
                                    reason
                                        .push_str("\n\nIndependent reviewer course-correction:\n");
                                    reason.push_str(&text);
                                }
                            }
                            Ok(None) => {}
                            Err(abort) => {
                                return CandidateResult::aborted(
                                    state.messages,
                                    abort.reason,
                                    AbortKind::DeliberateStop,
                                );
                            }
                        }
                    }
                    if let Err(abort) = self
                        .revise_candidate(engine, surface, &reason, spend, &mut state)
                        .await
                    {
                        return CandidateResult::turn_aborted(state.messages, abort);
                    }
                }
                // Triage judged this result not worth a separate reviewer,
                // and the warrant AGREES from the change itself
                // (`verifier_waiver_stands`) — the guard is load-bearing, because
                // this arm is only reached when the ladder came back
                // inconclusive, which falsifies the waiver's own premise. A
                // prompt-time `VERIFIER: no` must not strip the last reviewer
                // from a behavioral change nothing proved (§7.1:
                // predict-then-commit is the bug).
                LadderDecision::ModelVerdict
                    if !frame.assessment.wants_verifier()
                        && Self::verifier_waiver_stands(&state) =>
                {
                    let evidence = self.waived_completion(&snapshot);
                    return state.into_verified(
                        true,
                        &evidence,
                        score_from_verification(false, None),
                    );
                }
                LadderDecision::ModelVerdict => {
                    // Escalate on evidence, not on prediction. "Inconclusive"
                    // here can mean two very different things: a real change
                    // nothing proved, or a change with nothing to prove. The
                    // diff tells them apart, and the prompt never could — so
                    // ask it before buying a verifier call to confirm the absence
                    // of a test that was never warranted
                    // (docs/spec/witness-protocol.md §7).
                    if let Some(evidence) = self.warranted_completion(&state, &snapshot) {
                        return state.into_verified(
                            true,
                            &evidence,
                            score_from_verification(false, None),
                        );
                    }
                    // Inconclusive — escalate to the model verifier (verifier ≠
                    // worker; a verifier-call failure falls back to a heuristic).
                    // The summary and the witness-stripped diff are assembled
                    // together (`pipeline/evidence.rs`) so the reuse digest
                    // below hashes exactly what the prompt carries.
                    let (evidence_summary, stripped) = Self::verifier_evidence_summary(
                        &state,
                        &inputs,
                        &snapshot,
                        test_infra,
                        &lint_sample,
                        &witness_paths,
                    );
                    // Reuse before re-buying (#1431): a revision that changed
                    // nothing the verdict depends on — same goal, same diff,
                    // same evidence, byte for byte — would re-ask the same
                    // question and pay full price for sampling noise. The
                    // cached verdict is the same opinion at zero cost, and the
                    // appended note steers the worker better than a fresh
                    // reading of an unchanged tree ever did.
                    let inputs_digest =
                        verdict_inputs_digest(frame.goal, &stripped.diff, &evidence_summary);
                    let cached = state
                        .last_verdict
                        .as_ref()
                        .filter(|(digest, _)| *digest == inputs_digest)
                        .map(|(_, verdict)| verdict.clone());
                    let verdict = match cached {
                        Some(mut verdict) => {
                            verdict.reasoning.push_str(
                                "\n(verdict reused: the goal, diff, and evidence are unchanged \
                                 since the previous review round — no new model call was made)",
                            );
                            verdict
                        }
                        None => {
                            // Delta framing (#1431) rides the render context:
                            // file sections byte-identical to what the
                            // previous verdict read arrive as stat lines.
                            // `previous` holds that round's STRIPPED diff —
                            // the text a verdict actually read — so both
                            // sides of the comparison are the same shape.
                            let prompt = verifier_prompt(
                                frame.goal,
                                &stripped.diff,
                                &evidence_summary,
                                &DiffContext {
                                    witness_paths: &witness_paths,
                                    previous: state.last_verdict_diff.as_deref(),
                                },
                            );
                            match self
                                .verifier(prompt, &inputs, spend.budget, spend.total)
                                .await
                            {
                                Ok(verdict) => {
                                    // Only pin a real model verdict for reuse. A
                                    // heuristic fallback is a transient-outage
                                    // stand-in (unresolvable provider, unparseable
                                    // response, failed/timed-out call), not the
                                    // opinion this candidate bought: caching it
                                    // would suppress recovery on the next round
                                    // (the verifier may have come back) and graft
                                    // the "no new model call was made" reuse note
                                    // onto a fallback that never made one.
                                    if !verdict.heuristic {
                                        state.last_verdict = Some((inputs_digest, verdict.clone()));
                                    }
                                    verdict
                                }
                                Err(abort) => {
                                    return CandidateResult::aborted(
                                        state.messages,
                                        abort.reason,
                                        AbortKind::DeliberateStop,
                                    );
                                }
                            }
                        }
                    };
                    if !verdict.heuristic {
                        // The delta-framing baseline (#1431) advances only on
                        // a verdict a model actually answered: a heuristic
                        // fallback read nothing, and must not let the next
                        // round stat-line text no model ever saw. It records
                        // the STRIPPED diff — what the verdict actually read —
                        // so the next round's per-section comparison holds
                        // stripped text against stripped text.
                        state.last_verdict_diff = Some(stripped.diff.clone());
                    }
                    let mut evidence = model_verdict_evidence(&verdict);
                    evidence.ladder = Some(Box::new(
                        snapshot
                            .with_rung(verdict.rung())
                            .with_verifier_independence(verdict.verifier_independent),
                    ));
                    self.emit(AgentEvent::Verdict {
                        passed: verdict.passed,
                        evidence: evidence.clone(),
                    });
                    if verdict.passed {
                        // Asymmetric trust (#871). "Not yet" from a verifier is
                        // cheap to be wrong about — it buys one more revision.
                        // "Done" ends the run, so it has to be corroborated by
                        // something that is not another model's opinion.
                        //
                        // Unsupported, the pass is recorded as UNVERIFIED, not
                        // as a failure. The distinction is load-bearing: a
                        // Terminal-Bench trial that solved its task through
                        // shell redirects and scored 1.0 against its own
                        // verifier reaches exactly this state, and failing it
                        // closed would report a correct run as broken. The
                        // `Unverified` score keeps it from tying a genuinely
                        // verified sibling in best-of-N.
                        if inputs.verifier_pass_stands_alone() {
                            // Ask for the missing evidence once, if asking can
                            // be answered at all (#1295 — the predicate states
                            // why `effective_cmd` decides that).
                            if evidence_demand_is_worth_a_turn(
                                &self.config,
                                state.evidence_demands,
                                state.revisions,
                                effective_cmd.map(|cmd| cmd.command),
                            ) && let Some(cmd) = effective_cmd
                            {
                                state.evidence_demands += 1;
                                let ask = crate::verify::evidence_demand_prompt(cmd.command);
                                if let Err(abort) = self
                                    .revise_candidate(engine, surface, &ask, spend, &mut state)
                                    .await
                                {
                                    return CandidateResult::turn_aborted(state.messages, abort);
                                }
                                // Re-observe from the top: the revised tree
                                // gets a fresh test run and the ladder
                                // re-decides over it. A turn that produced the
                                // evidence now takes a deterministic rung; one
                                // that did not lands back here with the ask
                                // spent, and is relabelled below.
                                continue;
                            }
                            // No answerable ask left — record the pass as
                            // unverified, per the comment above, with the rung
                            // moved to match (see `uncorroborated_pass_evidence`
                            // for why leaving `ModelVerdict` would train reward
                            // on a verdict the ladder declined to believe).
                            let abstained = uncorroborated_pass_evidence(&evidence, &snapshot);
                            self.unverifiable(&abstained.summary);
                            return state.into_verified(
                                true,
                                &abstained,
                                score_from_verification(false, None),
                            );
                        }
                        return state.into_verified(
                            true,
                            &evidence,
                            score_from_verification(false, Some(true)),
                        );
                    }
                    if !self.affords_repair(&state, &meter, *spend.total, spend.budget) {
                        return state.into_verified(
                            false,
                            &evidence,
                            score_from_verification(false, Some(false)),
                        );
                    }
                    // The verifier read the deterministic evidence summary, so
                    // its prose can carry the tail back. A reasoning that
                    // quotes sealed material degrades to the symptom rather
                    // than being forwarded (§4.3).
                    let feedback = self
                        .airlock_forward(&verdict.reasoning, "verifier_reasoning", &sealed)
                        .map(|text| crate::verify::bound_forwarded_reasoning(&text))
                        .unwrap_or_else(|| redact(&sealed, DisclosureGrain::Symptom).message());
                    if let Err(abort) = self
                        .revise_candidate(engine, surface, &feedback, spend, &mut state)
                        .await
                    {
                        return CandidateResult::turn_aborted(state.messages, abort);
                    }
                }
            }
        }
    }

    fn effective_test_command<'c>(
        &'c self,
        witness: Option<&'c Witness>,
    ) -> Option<EffectiveTestCommand<'c>> {
        if let Ok(Some(invocation)) = &self.configured_test {
            return self
                .config
                .test_command
                .as_deref()
                .map(|command| EffectiveTestCommand {
                    command,
                    invocation,
                });
        }
        witness.map(|w| EffectiveTestCommand {
            command: &w.command,
            invocation: &w.invocation,
        })
    }

    /// Spend one revision: run [`Pipeline::revise_turn`] with the failure
    /// evidence and fold the fresh diff back into `state`. `Err` is the typed
    /// abort of a turn that died mid-revision (budget/loop).
    async fn revise_candidate(
        &self,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        reason: &str,
        spend: &mut Spend<'_>,
        state: &mut CandidateState,
    ) -> Result<(), TurnAbort> {
        let probe = self
            .revise_turn(engine, surface, reason, spend, state)
            .await?;
        self.absorb_probe(state, probe);
        state.revisions += 1;
        Ok(())
    }

    /// Run one revision turn: append an evidence-carrying instruction, execute,
    /// and re-gather the diff. Emits the `Execute`/`Verify` stage bookends so
    /// the stream shows the revise loop. Returns the fresh `(diff_lines,
    /// diff_text)` on success, or the typed abort on a budget/loop abort.
    async fn revise_turn(
        &self,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        reason: &str,
        spend: &mut Spend<'_>,
        state: &mut CandidateState,
    ) -> Result<DiffProbe, TurnAbort> {
        state
            .messages
            .push(CompletionMessage::user(revision_prompt(reason)));
        self.emit(AgentEvent::Stage {
            name: StageKind::Execute,
        });
        match self
            .run_engine_turn(
                engine,
                &mut state.messages,
                spend.budget,
                &mut state.signals,
                state.flip_halt.as_ref().and_then(FlipHalt::unfired),
            )
            .await
        {
            TurnOutcome::Completed { text, cost_usd } => {
                state.final_text = text;
                *spend.total += cost_usd;
            }
            TurnOutcome::Aborted {
                reason,
                kind,
                cost_usd,
            } => {
                *spend.total += cost_usd;
                return Err(TurnAbort { reason, kind });
            }
        }
        let probe = self.gather_diff(surface, &state.untracked_before).await;
        self.emit(AgentEvent::Stage {
            name: StageKind::Verify,
        });
        Ok(probe)
    }

    // Shared helpers

    /// Resolve a role to a concrete provider via the router + provider
    /// resolver. Reads `self.providers` as a copy of its `&'a` reference so the
    /// returned provider borrow carries the full `'a` lifetime (long enough to
    /// build a per-candidate `Engine`).
    fn resolve_provider(&self, role: Role) -> Result<ResolvedRole<'a>, RoleResolveError> {
        let decision = self
            .router
            .resolve(role)
            .map_err(RoleResolveError::Router)?;
        let providers: &'a dyn ProviderResolver = self.providers;
        let provider = providers
            .provider_for(&decision.model_ref)
            .ok_or_else(|| RoleResolveError::NoProvider(decision.model_ref.clone()))?;
        Ok(ResolvedRole {
            model_ref: decision.model_ref,
            provider,
            fallback: decision.fallback,
            caveat: decision.caveat,
        })
    }

    /// Run one engine turn, forwarding every event to the consumer **live**
    /// (a concurrent drain task, not a post-hoc flush — an execute turn can
    /// run tool loops for minutes, and buffering froze the renderer for the
    /// whole turn) **except** the engine's `Stage`/`Complete` (the pipeline
    /// owns those), tallying `FileChange`s into `signals.file_changes` for
    /// the zero-diff guard and mutating-capable `ToolStart`s into
    /// `signals.mutating_actions` for the ladder's no-op rung.
    ///
    /// The tallies are deliberately independent. `file_changes` answers
    /// "did the recorder see the tree change", which a shell redirect defeats;
    /// `mutating_actions` answers "was anything even asked to change", which
    /// nothing can defeat, because it is counted off the calls this pipeline
    /// dispatched rather than off any look at the world.
    async fn run_engine_turn(
        &self,
        engine: &Engine<'_>,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        signals: &mut ChangeSignals,
        flip_halt: Option<Arc<FlipHalt>>,
    ) -> TurnOutcome {
        // The filtered sender is SYNCHRONOUS on purpose: when the outer
        // sender carries a durability boundary, a paid StepUsage cannot
        // return to the engine before append+flush completes. Draining a
        // channel from a spawned forwarder instead would let the engine make
        // another paid call before the previous one's metering row is durable.
        let seen_file_changes = Arc::new(AtomicU32::new(0));
        let count = seen_file_changes.clone();
        let seen_mutating = Arc::new(AtomicU32::new(0));
        let mutating = seen_mutating.clone();
        let seen_opaque = Arc::new(AtomicU32::new(0));
        let opaque = seen_opaque.clone();
        let read_only = self.read_only_tool_names();
        let consumer = self.events.clone();
        // Correlate a shell call's command line (carried on `ToolStart`) with
        // its exit status (carried in the `ToolResult` content), because
        // neither event has both. Keyed by `call_id` rather than a
        // last-command slot: a step dispatches up to eight calls
        // concurrently, so "the most recent command" is genuinely ambiguous.
        let pending_commands: Arc<Mutex<HashMap<String, String>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let halt_for_events = flip_halt.clone();
        let commands = pending_commands.clone();
        // Read once per turn, not per event: a turn belongs to one candidate,
        // and the fan-out sets this before dispatching any of them.
        let shared_lane = self.shared_event_lane.load(Ordering::Relaxed);
        let filtered = EventSender::from_fn(move |event| {
            match &event {
                // The pipeline is the sole authority for stage boundaries and
                // the terminal event of an outcome-producing run — drop the
                // engine's per-turn copies.
                AgentEvent::Stage { .. } | AgentEvent::Complete { .. } => Ok(()),
                // Concurrent candidates share this stream, and these two are
                // the only events whose meaning depends on arriving
                // uninterrupted: `TextDelta` is a preview its own `Text` event
                // supersedes, and `Reasoning` is accumulated by the consumer.
                // Three models' fragments spliced together is not a preview of
                // anything. Everything durable still goes out live — see
                // `Pipeline::shared_event_lane`.
                AgentEvent::TextDelta { .. } | AgentEvent::Reasoning { .. } if shared_lane => {
                    Ok(())
                }
                AgentEvent::FileChange { kind, .. } => {
                    // Reads ride the same event for the files panel but are
                    // not changes — counting them would defeat the zero-diff
                    // guard on read-only turns.
                    if kind.is_mutation() {
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                    consumer.send(event)
                }
                AgentEvent::ToolStart { call } => {
                    // Counted at dispatch, not at result: a call that errored
                    // or timed out still means the turn *tried* to act, and
                    // the no-op rung is about the attempt. Only a name the
                    // registry positively advertises as read-only is excluded.
                    if !read_only.contains(&call.name) {
                        mutating.fetch_add(1, Ordering::Relaxed);
                        // The warrant's premise check: a mutating call whose
                        // effects the diff cannot fully account for (the
                        // shell, processes, MCP, anything unrecognized)
                        // forfeits every path-classified waiver (#1701).
                        if !crate::witness::warrant::diff_accountable_mutator(&call.name) {
                            opaque.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    // Remember the command line so its result can be scored
                    // against the tracked test. Only when a halt is armed —
                    // otherwise this map is pure overhead on every turn.
                    if halt_for_events.is_some()
                        && let Some(command) = command_of(&call.input)
                        && let Ok(mut pending) = commands.lock()
                    {
                        pending.insert(call.call_id.clone(), command.to_string());
                    }
                    consumer.send(event)
                }
                AgentEvent::ToolResult {
                    call_id, output, ..
                } => {
                    // The agent running the tracked test itself is the
                    // earliest moment anyone can know the goal is met — and
                    // before this, it was the one observation the oracle
                    // never saw (it watched only a pre-execute baseline and
                    // post-execute verification). Feeding it here is what
                    // lets the engine stop at the next step boundary instead
                    // of running until a limit fires.
                    if let Some(halt) = halt_for_events.as_ref() {
                        let command = commands
                            .lock()
                            .ok()
                            .and_then(|mut pending| pending.remove(call_id));
                        if let Some(command) = command
                            && let ToolOutput::Ok { content } = output
                        {
                            // Nothing is emitted on the transition: this is a
                            // success, and the only event available in this
                            // closure is `Error`, which a TUI renders as a
                            // failure. The reason reaches the transcript as
                            // the halted turn's own text (`TurnHalt`).
                            halt.observe(&command, content);
                        }
                    }
                    consumer.send(event)
                }
                _ => consumer.send(event),
            }
        });
        // A halted engine only when there is something to watch, so a turn
        // with no armed flip runs on exactly the engine it always did.
        let halted;
        let engine = match flip_halt {
            Some(halt) => {
                halted = engine.with_turn_halt(halt as Arc<dyn TurnHalt>);
                &halted
            }
            None => engine,
        };
        let outcome = engine
            .run_turn_with_sender(messages, budget, &filtered)
            .await;
        signals.file_changes += seen_file_changes.load(Ordering::Relaxed);
        signals.mutating_actions += seen_mutating.load(Ordering::Relaxed);
        signals.opaque_actions += seen_opaque.load(Ordering::Relaxed);
        outcome
    }

    /// Tool names the registry advertises as `read_only` — the calls that
    /// structurally cannot have changed the workspace.
    ///
    /// Membership is the *only* thing that lets a call be discounted, so the
    /// direction of every uncertainty is fixed: a name this set has never
    /// heard of (an MCP server attached mid-run, a host's own extension, a
    /// tool added since) counts as mutating, and the ladder declines to call
    /// the turn a no-op. Getting that backwards would let an unrecognized
    /// tool's real work be reported as nothing attempted.
    fn read_only_tool_names(&self) -> HashSet<String> {
        self.tools
            .schemas()
            .into_iter()
            .filter(|schema| schema.read_only)
            .map(|schema| schema.name)
            .collect()
    }

    /// Emit this recall's telemetry. The projection itself lives on
    /// [`Recall::telemetry_event`] — Phase 2 (#713) moved it there because the
    /// pipeline was the only surface that emitted it, and four other recall
    /// paths now need the same event built the same way.
    ///
    /// `frames` is separate because the caller bounds them first (#616) and
    /// the event must report what the turn actually pays for. Everything else
    /// rides off the original rather than being rebuilt from parts:
    /// reconstructing a `Recall` field by field here is exactly how
    /// `latency_ms` (#875) would have been dropped the moment it was added.
    fn emit_context_recall(&self, frames: &[RecalledFrame], recall: &Recall) {
        let bounded = Recall {
            frames: frames.to_vec(),
            ..recall.clone()
        };
        if let Some(event) = bounded.telemetry_event() {
            self.emit(event);
        }
    }

    /// Whether the wiring can supply a witness author independent of the
    /// worker, and — when it cannot — why.
    ///
    /// Split from [`Self::can_author_independent_witness`] because the same
    /// verdict is read twice with different consequences: the ladder degrades
    /// on it, `require_independent_witness` refuses on it (#1147). Pure — it
    /// emits nothing, so asking twice costs nothing and says nothing twice.
    fn witness_author_independence(&self) -> WitnessAuthorIndependence {
        let Ok(worker) = self.resolve_provider(Role::Worker) else {
            return WitnessAuthorIndependence::WorkerUnresolvable;
        };
        match self.resolve_provider(Role::Verifier) {
            Ok(verifier) if verifier.model_ref != worker.model_ref => {
                WitnessAuthorIndependence::Independent
            }
            // Role-neutral wording on purpose (#1795): the same finding is
            // framed by two different refusals (witness author, verdict
            // grader) and one degradation notice, and each supplies its own
            // role — a reason that named one would misname the others.
            Ok(_) => WitnessAuthorIndependence::Unavailable(format!(
                "no model independent of the worker resolves (verifier and worker both \
                 resolved to `{}`)",
                worker.model_ref
            )),
            Err(_) => WitnessAuthorIndependence::Unavailable(
                "no model independent of the worker resolves (the verifier role is \
                 unresolvable)"
                    .to_string(),
            ),
        }
    }

    /// Whether a witness author independent of the worker can be resolved.
    ///
    /// Losing the author costs the run its authored witness, never the task:
    /// a `false` here routes to the ordinary single-shot path and the
    /// deterministic/verifier verify ladder. Announced once, at the one point
    /// that decides it, so the run never pays for isolation it cannot use.
    ///
    /// A host that cannot afford that degradation sets
    /// [`PipelineConfig::require_independent_witness`], which refuses the run
    /// up front rather than reaching this call at all.
    fn can_author_independent_witness(&self) -> bool {
        match self.witness_author_independence() {
            WitnessAuthorIndependence::Independent => true,
            // Reported through `unproven`, not a bare `warn`. A witness
            // triage asked for and the wiring cannot supply is precisely
            // `WitnessUnavailable` — routing it to the warning channel alone
            // left the rail's witness row with no statement, so it fell
            // through to the backstop's "not reported" when the real answer
            // was known all along and worth naming.
            //
            // The message names the degradation AND the way out: a same-model
            // posture is legitimate (an auto-routing gateway can serve
            // distinct upstream models behind one id, and this run keeps
            // going either way), but the operator must hear that the flip
            // oracle cannot arm and what config change restores it.
            WitnessAuthorIndependence::Unavailable(reason) => {
                self.unproven(format!(
                    "{reason}; verification is degraded — the deterministic \
                     flip oracle cannot arm, so the verdict is a model review \
                     with no independent test. Set `pipeline_verifier_model` \
                     (or `agents.verifier.model`) to a model distinct from \
                     the worker to restore independent verification"
                ));
                false
            }
            // A worker that won't resolve fails later, on its own terms —
            // not here, disguised as a witness-independence verdict.
            WitnessAuthorIndependence::WorkerUnresolvable => false,
        }
    }

    /// Whether this run does its work in a throwaway worktree, per
    /// [`PipelineConfig::create_worktrees`].
    ///
    /// Asked at triage and nowhere else, because triage is the first moment the
    /// question is answerable *and* worth asking. Earlier — at launch, say —
    /// every `stella "what does this do"` would raise a prompt about relocating
    /// work it is never going to do. Later, the run has already started
    /// changing the checkout, and the answer could not be honoured.
    ///
    /// Three conditions come before the policy, in order of how little they owe
    /// the user an explanation:
    ///
    /// - A class that does not change files is never isolated. There is nothing
    ///   to protect, and the prompt would be pure noise on the commonest path.
    /// - Without a [`crate::ports::CandidateWorkspacePort`] there is no
    ///   isolation to offer — a plain directory, or a caller that wired none.
    ///   Under `Always` that IS worth saying, because the operator configured
    ///   something they are not getting.
    /// - `Never` and `Always` answer without asking. Only `Ask` reaches the
    ///   gate, and a gate with nobody behind it declines (see
    ///   [`crate::ports::ApprovalGate::confirm`]).
    async fn isolate_in_worktree(&self, task_class: TaskClass) -> bool {
        let isolation_available = self.candidate_workspaces.is_some();
        match worktree_decision_without_asking(
            self.config.create_worktrees,
            task_class.verifies_unconditionally(),
            isolation_available,
        ) {
            Decided::Yes => true,
            Decided::No => false,
            Decided::NoAndSayWhy(reason) => {
                self.warn(reason.to_string());
                false
            }
            Decided::MustAsk => self.approvals.confirm(WORKTREE_QUESTION).await,
        }
    }

    fn emit_fallback(&self, fb: &FallbackInfo) {
        self.emit(AgentEvent::ProviderFallback {
            from: fb.from.clone(),
            to: fb.to.clone(),
            reason: fb.reason.clone(),
        });
    }

    /// Emit a narration line when there is one — see `candidate_narration`,
    /// which returns `None` for a single-candidate run.
    fn emit_text(&self, notice: Option<String>) {
        if let Some(text) = notice {
            self.emit(AgentEvent::Text { text });
        }
    }

    /// A non-fatal degradation the user should see (witness discarded,
    /// guidance unavailable): a `retryable: true` error event — the deck
    /// folds it as a warning, never a failed turn.
    fn warn(&self, message: String) {
        self.emit(AgentEvent::Error {
            message,
            retryable: true,
        });
    }

    fn aborted_before_execute(
        &self,
        task_class: TaskClass,
        total_cost: f64,
        reason: &str,
        kind: AbortKind,
    ) -> PipelineOutcome {
        let (event, outcome) =
            stage_budget::aborted_before_execute(task_class, total_cost, reason, kind);
        self.emit(event);
        outcome
    }

    fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    /// Publish one step of the proof this turn is building for itself. Pure
    /// observability — nothing downstream reads these back, and dropping them
    /// changes no decision — but a run that proves its work used to be
    /// indistinguishable on the stream from one that did not.
    fn emit_proof(&self, step: ProofStep) {
        self.emit(AgentEvent::Proof { step });
    }
}

/// How many of a fan-out's candidates actually reached the worker — the honest
/// value for [`PipelineOutcome::candidates_run`], as opposed to the configured
/// `n`, which overstates both the work done and the spend it implies whenever
/// isolation setup fails for some of the slots.
fn executed_count(candidates: &[CandidateResult]) -> u32 {
    candidates.iter().filter(|c| c.executed()).count() as u32
}

/// The winning candidate's index: strongest verification evidence first,
/// smallest diff as the tiebreak, earliest index for reproducibility
/// ([`select_best_candidate`]).
fn best_index(candidates: &[CandidateResult]) -> usize {
    let summaries: Vec<CandidateSummary> = candidates
        .iter()
        .map(|c| CandidateSummary {
            score: c.score,
            // Read off the verdict's own provenance snapshot (#865) — the
            // audit's warning count needs no separate plumbing. Absent
            // snapshot (no verdict, pre-audit path) reads 0, the pre-#869
            // behavior.
            mutation_survived: c
                .verdict
                .as_ref()
                .and_then(|v| v.ladder.as_deref())
                .and_then(|s| s.witness_mutation)
                == Some(true),
            new_diag_warnings: c
                .verdict
                .as_ref()
                .and_then(|v| v.ladder.as_deref())
                .map_or(0, |s| s.new_diag_warnings),
            diff_lines: c.diff_lines,
        })
        .collect();
    select_best_candidate(&summaries).unwrap_or(0)
}

/// The flip oracle's command for this run: an explicit `--test-command`
/// always wins; otherwise the witness author's (when one was validated).
#[derive(Clone, Copy)]
struct EffectiveTestCommand<'c> {
    command: &'c str,
    invocation: &'c TestInvocation,
}

/// Assemble the volatile recall+goal user message that rides *after* the
/// stable system prefix (L-E8 cache discipline). Keeping the system prefix
/// untouched preserves prompt-cache hits on it across turns; the recall and
/// goal — both volatile per turn — go here in one message.
/// Per-frame content ceiling, in chars: one frame may not monopolize the
/// recall budget (#616). Head-kept — a frame's contract is stated up front.
const RECALL_FRAME_CHARS: usize = 4_000;
/// Total recalled-content budget per turn, in chars (~5k tokens). Recall is
/// advisory grounding; past this it displaces the work itself.
const RECALL_PROMPT_BUDGET_CHARS: usize = 20_000;

/// Clamp recalled frames to the prompt budget (#616): each frame's content is
/// truncated to [`RECALL_FRAME_CHARS`] and frames past
/// [`RECALL_PROMPT_BUDGET_CHARS`] of cumulative content are dropped. Both
/// interventions are visible — truncation leaves an in-content marker, and a
/// dropped tail is summarized in a final marker frame — so neither the model
/// nor an operator reading the transcript can mistake a clamped recall for
/// the full one.
fn bound_recalled_frames(frames: Vec<RecalledFrame>) -> Vec<RecalledFrame> {
    let mut out: Vec<RecalledFrame> = Vec::with_capacity(frames.len());
    let mut spent = 0usize;
    let total = frames.len();
    for mut frame in frames {
        if spent >= RECALL_PROMPT_BUDGET_CHARS {
            let dropped = total - out.len();
            out.push(RecalledFrame {
                citation_label: "recall-budget".into(),
                provider: "pipeline".into(),
                source: "pipeline".into(),
                kind: "note".into(),
                uri: None,
                method: None,
                content: format!(
                    "[{dropped} recalled frame(s) dropped — recall exceeded the \
                     {RECALL_PROMPT_BUDGET_CHARS}-char prompt budget]"
                ),
                token_cost: 0,
                id: None,
                content_digest: None,
            });
            break;
        }
        if frame.content.chars().count() > RECALL_FRAME_CHARS {
            let kept: String = frame.content.chars().take(RECALL_FRAME_CHARS).collect();
            frame.content = format!("{kept}\n[… frame truncated during recall budgeting …]");
        }
        // Chars, matching the budget's unit — `len()` (bytes) over-charged
        // multi-byte content and shrank the effective budget below the
        // documented one.
        spent += frame.content.chars().count();
        out.push(frame);
    }
    out
}

/// What the worker's user message says about how this run will be verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerificationContract<'a> {
    /// An operator-configured oracle: disclose the command.
    Oracle(&'a str),
    /// No oracle and no independent witness author: the worker's own failing
    /// test, written first, is the only deterministic evidence the run will
    /// carry — say so on the channel the worker plans from.
    WorkerTestFirst,
    /// Nothing to add: a conversational turn, a class that never verifies, or
    /// an authored witness that will supply the oracle post-execution (its
    /// disclosure stays governed by the airlock).
    None,
}

fn assemble_user_message(
    goal: &str,
    frames: &[RecalledFrame],
    contract: VerificationContract<'_>,
) -> String {
    if frames.is_empty() && contract == VerificationContract::None {
        return goal.to_string();
    }
    let mut s = String::new();
    if !frames.is_empty() {
        s.push_str("## Recalled context\n");
        for f in frames {
            // Cite by human label (L-C4); include content as grounding.
            s.push_str("- [");
            s.push_str(&f.citation_label);
            s.push_str("] (");
            s.push_str(&f.source);
            s.push_str(")\n");
            if !f.content.trim().is_empty() {
                s.push_str("  ");
                s.push_str(f.content.trim());
                s.push('\n');
            }
        }
        s.push('\n');
    }
    s.push_str("## Task\n");
    s.push_str(goal.trim());
    // The verification contract, when the operator configured one. The
    // methodology prompt tells the worker to "run the target test" without
    // ever saying which — the command that actually gates the run was
    // withheld until the first failure disclosed it (the airlock's L1 brief
    // names it anyway). Saying it up front moves that information one failed
    // revision earlier, on the exact channel the worker plans from. Only the
    // operator-CONFIGURED command is ever disclosed here: an authored
    // witness's command does not exist yet at assembly time, and its
    // disclosure stays governed by the airlock (`crate::witness::airlock`).
    match contract {
        VerificationContract::Oracle(command) => {
            s.push_str("\n\n## Verification\n");
            s.push_str(&format!(
                "This run's primary verification is `{command}`: the accepted deterministic \
                 evidence is this command failing before your change and passing after it. \
                 Reproduce the failure with it before editing; make it pass before finishing. \
                 Do not modify the tests it runs."
            ));
        }
        VerificationContract::WorkerTestFirst => {
            s.push_str("\n\n## Verification\n");
            s.push_str(
                "No test command is configured for this run and no independent test author \
                 is available: nothing outside your own work will check this change. Before \
                 implementing, write the failing test that captures this task and run it to \
                 watch it fail; make it pass before finishing. That test is the only \
                 deterministic evidence this run will carry.",
            );
        }
        VerificationContract::None => {}
    }
    s
}

/// One digest over everything a model verdict depends on — goal, the (witness
/// -stripped) diff, and the evidence summary. Byte-identical inputs are the
/// same question; the ModelVerdict arm reuses its previous answer rather than
/// paying for sampling noise (#1431). Within-run only, so the std hasher's
/// stability across versions is irrelevant.
fn verdict_inputs_digest(goal: &str, diff: &str, evidence_summary: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    goal.hash(&mut hasher);
    diff.hash(&mut hasher);
    evidence_summary.hash(&mut hasher);
    hasher.finish()
}

/// The instruction appended to a revision turn, carrying the failing
/// verification evidence so the worker can fix it.
fn revision_prompt(reason: &str) -> String {
    format!(
        "Verification did not pass. Evidence:\n{}\n\nFix the issue and complete the task.",
        reason.trim()
    )
}

/// Count changed lines in a unified diff: lines beginning with `+`/`-` but not
/// the `+++`/`---` file headers. A coarse but deterministic size proxy for the
/// ladder's diff budget.
fn count_diff_lines(diff: &str) -> u32 {
    diff.lines()
        .filter(|l| {
            (l.starts_with('+') && !l.starts_with("+++"))
                || (l.starts_with('-') && !l.starts_with("---"))
        })
        .count() as u32
}

#[cfg(test)]
mod test_doubles;
#[cfg(test)]
mod tests;
