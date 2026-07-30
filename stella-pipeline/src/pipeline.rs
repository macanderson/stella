//! The orchestrator: the staged turn flow that sits
//! *above* `stella-core::Engine`. It sequences evaluate → enhance → route →
//! execute → witness → verify → judge → revise over the injected ports,
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

use std::collections::HashMap;
use std::time::Duration;

use std::sync::Arc;

use futures_util::StreamExt as _;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use stella_core::hooks::{HookRunner, Hooks};
use stella_core::receipts::RECEIPT_SEQ_ALLOCATED_BASE;
use stella_core::retry::{RetryPolicy, Sleeper};
use stella_core::router::FallbackInfo;
use stella_core::{BudgetGuard, Engine, EngineConfig, EventSender, Router, TurnOutcome};
use stella_protocol::{
    AgentEvent, CompletionMessage, JudgeEvidence, MessageRole, ModelCallRole, ModelRef, ProofStep,
    ProofTree, Provider, Role, StageKind,
};

use crate::candidate::{
    CandidateScore, CandidateSummary, score_from_verification, select_best_candidate,
};
use crate::candidate_narration::{candidate_start_notice, candidate_winner_notice};
use crate::candidate_steering::SteeringFanOut;
use crate::plan::{PlanStep, build_planner_prompt, parse_plan, plan_repair_prompt};
use crate::ports::{
    ApprovalGate, CandidateWorkspace, CandidateWorkspacePort, ContextRecallPort,
    DiagnosticInvocation, DiagnosticRunner, FileTouchPort, McpPrefetchPort, PipelinePorts,
    ProviderResolver, Recall, RecalledFrame, RepoStatusPort, RepoStructurePort, ScopeDecision,
    TestInvocation, TestRunner, WorkspaceError,
};
use crate::scope::{
    MAX_SCOPE_REVISIONS, PlannedScope, ScopeEstimate, ScopeVerdict, apply_trim, build_proposal,
    needs_scope_review,
};
use crate::triage::{
    TaskAssessment, TaskClass, parse_triage_response, resolve_conversational, resolve_task_class,
    resolve_witness, triage_prompt,
};
use crate::verify::{
    FlipOracle, JudgeVerdict as ModelJudgeVerdict, LadderDecision, LadderInputs,
    deterministic_fail_evidence, deterministic_pass_evidence, guidance_prompt, heuristic_fallback,
    judge_prompt, ladder_decision, model_verdict_evidence, parse_judge_response,
    unverifiable_evidence,
};
use crate::witness::airlock::{
    DisclosureGrain, FailureFingerprint, SealedFailure, grain_for_repeats, redact, scrub,
};
use crate::witness::warrant::warrant;
use crate::witness::{
    Witness, parse_test_invocation, parse_witness_command, validate_witness_artifact,
    validate_witness_identity, validate_witness_invocation, witness_identity_matches,
    witness_prompt, witness_repair_prompt,
};
mod disclosure;
mod raw_usage;
mod run_error;
mod scope_stage;
mod stage_budget;
mod witness_stage;
use raw_usage::{RawCall, RawCallError};
use run_error::RoleResolveError;
pub use run_error::{PipelineError, PipelineRunError};
use stage_budget::{PipelineBudgetAbort, budget_abort};
use witness_stage::{BoundHookRunner, WitnessAuthoring};
/// Make a diff that verification hands downstream *incapable of lying*.
///
/// An empty diff is ambiguous: it can mean the agent genuinely changed
/// nothing, OR that the diff machinery is blind to changes that DID happen
/// (work that was committed, a baseline mismatch, an uncaptured file). Handed
/// a bare empty string, a judge reads the second case as the first and
/// concludes "no changes were made" — the archetypal verification lie that
/// once drove an agent to reinitialize git to beat the check.
///
/// The pipeline already has the independent signal that resolves the
/// ambiguity: `file_changes`, the count of `FileChange` events the turn
/// emitted. When that is positive but the diff is empty, the honest report is
/// "the tree changed but the diff could not be captured" — a *couldn't
/// verify*, never a *verified nothing*. Every consumer (judge, guidance,
/// evidence) then sees the truth.
fn verification_honest_diff(diff_text: String, file_changes: u32) -> String {
    if file_changes > 0 && diff_text.trim().is_empty() {
        format!(
            "[{file_changes} file-change event(s) were observed this turn, but the diff could \
             not be captured — the change is real; the diff is blind to it. This is NOT evidence \
             that nothing changed; verify the result on its own merits.]"
        )
    } else {
        diff_text
    }
}

/// What [`Pipeline::gather_diff`] reports when the diff probe *failed* rather
/// than found nothing.
///
/// `verification_honest_diff` resolves the same ambiguity from `file_changes`,
/// but that signal is structurally unavailable to an isolated candidate: the
/// engine emits no `FileChange` events (the deck's `FileChangeTap` synthesizes
/// them, and it wraps only the SESSION tool stack), so inside a best-of-N or
/// witness candidate the count is always zero. A candidate whose probe went
/// blind therefore handed an empty string to [`crate::witness::warrant`], which
/// read it as [`crate::NoWitnessReason::NothingChanged`] and completed the run
/// with a PASSING verdict stating no behavior had changed — the exact
/// verification lie the honest-diff guard exists to prevent, reached with no
/// witness stage and no warning. Non-empty text keeps the warrant fail-closed
/// on the one path its own guard could not see.
const DIFF_PROBE_FAILED: &str = "[the diff probe failed; the working tree could not be read. This \
     is NOT evidence that nothing changed — verify the result on its own merits.]";

/// The same report, for the one cause worth naming separately: there is no git
/// repository here, so `git diff` can never answer.
///
/// Terminal-Bench task images are plain directories. `DIFF_PROBE_FAILED` reads
/// as a transient fault a reader might expect to clear on a retry; this one
/// says the probe is structurally inapplicable to this workspace, which is the
/// difference between "try again" and "use another channel" (#973).
const DIFF_PROBE_NOT_A_REPO: &str = "[the working tree is not a git repository, so the diff probe cannot read it at all. This is \
     a permanent property of this workspace, NOT evidence that nothing changed — verify the \
     result on its own merits.]";

/// One observation of the working tree by [`Pipeline::gather_diff`].
///
/// `available` is the field that did not exist and had to: without it, "the
/// probe read the tree and it was unchanged" and "the probe could not read the
/// tree" both arrived as `(0, ...)`, and every consumer downstream was free to
/// read the second as the first. The text has said the right thing for a while;
/// nothing could act on it, because prose is not a signal a ladder can branch
/// on.
struct DiffProbe {
    lines: u32,
    text: String,
    /// Whether the probe could read the working tree AT ALL. Never `false`
    /// merely because the diff came back empty.
    available: bool,
}

/// How many `git diff --no-index --numstat` probes [`Pipeline::gather_diff`]
/// keeps in flight at once. High enough that the usual handful of new files
/// costs roughly one round-trip instead of N, low enough that a turn which
/// creates hundreds cannot fork an unbounded burst of git processes.
const UNTRACKED_NUMSTAT_CONCURRENCY: usize = 16;

/// Minimal fallback when the caller supplies no stable system prefix.
const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a precise, careful software engineering agent. Make the smallest correct change.";

/// The system prompt for the conversational fast path. Swapped in for
/// [`DEFAULT_SYSTEM_PROMPT`] when triage classified the input as chat so the
/// reply reads as a normal, brief conversational turn rather than a work plan.
const CONVERSATIONAL_SYSTEM_PROMPT: &str = "You are Stella, a careful software engineering agent. The user's latest \
     message is a greeting, small talk, or a question about you — not a coding \
     task. Reply briefly and warmly in plain prose: no tools, no code, no plan, \
     no test. Do not invent a task. If it fits, add one short line inviting \
     them to describe a change, bug, or question about their codebase.";

/// Small fixed system prompt for the independent witness author.
const WITNESS_SYSTEM_PROMPT: &str = "You are a precise test author. You write minimal failing tests that pin down intended \
     behavior. You never modify production code and never fix the problem yourself.";

/// Per-role request overrides for the pipeline's raw completion calls
/// (triage / judge / guidance), resolved by the caller from
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

/// The pipeline's per-role override set. Worker (and plan/witness, which
/// ride the worker's tier) is configured through
/// [`PipelineConfig::engine`] directly; only the two roles with their own
/// models get their own request shaping.
#[derive(Debug, Clone, Default)]
pub struct PipelineRoleOverrides {
    pub triage: RoleCallOverrides,
    pub judge: RoleCallOverrides,
}

/// Tuning for the whole staged flow.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Passed to `stella-core::Engine` for every execute turn.
    pub engine: EngineConfig,
    /// Per-role request overrides (`agent_engine_config`) for the raw
    /// triage/judge completion calls.
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
    /// Distress-triggered course-correction: on the *second consecutive*
    /// deterministic verification failure, spend one judge call for guidance
    /// that rides with the next revision prompt ([`crate::verify::guidance_prompt`]).
    /// Event-triggered by design — never a fixed mid-run checkpoint. Bounded
    /// by `max_revisions` (at most `max_revisions - 1` guidance calls per
    /// candidate).
    pub distress_guidance: bool,
    /// The closed diagnostic that reports what the turn changed. `None`
    /// disables diff-size and zero-diff inspection.
    pub diff_diagnostic: Option<DiagnosticInvocation>,
    /// The diff-size budget in changed lines: a diff at or under this is
    /// "small enough" to trust deterministic evidence without a judge (L-E11).
    pub diff_budget_lines: u32,
    /// Maximum revision turns per candidate when verification fails.
    pub max_revisions: u32,
    /// Best-of-N (L-E7). `None` or `Some(1)` is single-shot (the default);
    /// `Some(n)` generates n candidate executions — each in an isolated
    /// snapshot of the current tree state when a
    /// [`crate::ports::CandidateWorkspacePort`] is wired — and selects the
    /// best, adopting only the winner's changes into the real tree. Paid for
    /// with n× the execution cost — opt-in only.
    pub candidates: Option<u32>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            engine: EngineConfig::default(),
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
            test_command: None,
            witness_writer: true,
            keep_witness: false,
            distress_guidance: true,
            diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
            diff_budget_lines: 400,
            max_revisions: 2,
            candidates: None,
        }
    }
}

impl PipelineConfig {
    /// The effective candidate count (`candidates`, floored at 1).
    fn candidate_count(&self) -> u32 {
        self.candidates.unwrap_or(1).max(1)
    }
}

/// The final verification verdict a pipeline run produced, if verification
/// ran. `deterministic` distinguishes a flip-oracle/ladder verdict from a
/// model/heuristic judge's opinion (never conflated, L-E11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub passed: bool,
    pub deterministic: bool,
    pub summary: String,
}

impl Verdict {
    fn from_evidence(passed: bool, evidence: &JudgeEvidence) -> Self {
        Self {
            passed,
            deterministic: evidence.deterministic,
            summary: evidence.summary.clone(),
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
    /// aborted at scope review.
    Aborted {
        reason: String,
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
    /// How many revision turns the selected candidate took.
    pub revisions: u32,
    /// How many candidates actually reached the worker (1 for single-shot).
    ///
    /// This is what RAN, not what was configured: a candidate that aborted in
    /// setup — no isolation port, a tree that could not be snapshotted, no
    /// independent witness author — never dispatched a model call, and is not
    /// counted. A `--candidates 4` run where three failed isolation reports 1.
    pub candidates_run: u32,
}

/// A role resolved to a concrete provider.
struct ResolvedRole<'a> {
    model_ref: ModelRef,
    provider: &'a dyn Provider,
    fallback: Option<FallbackInfo>,
}

/// The outcome of running one candidate (execute + verify + bounded revise).
struct CandidateResult {
    messages: Vec<CompletionMessage>,
    final_text: String,
    /// `Some(reason)` if a turn aborted (budget/loop/step-cap).
    aborted: Option<String>,
    /// Whether this abort is a degradable **infrastructure** setup failure —
    /// no isolation port, or a workspace that could not be snapshotted. `run`
    /// degrades these to a bare worker run rather than ending the turn with
    /// zero work (the "never choose nothing" rule). Deliberately does NOT
    /// cover a witness-integrity rejection (symlink artifact, language
    /// mismatch, a witness author touching production code): those are
    /// fail-closed security decisions and keep their abort. `false` on every
    /// executed and every verified/unverified result.
    degradable: bool,
    /// The verification verdict, if verification ran.
    verdict: Option<Verdict>,
    /// This candidate's verification score, for best-of-N selection.
    score: CandidateScore,
    diff_lines: u32,
    revisions: u32,
    /// Workspace-relative paths of the witness artifact this candidate had
    /// grafted into it, withheld from adoption unless
    /// [`PipelineConfig::keep_witness`]. Empty when no witness was warranted,
    /// which under demand-driven authoring is the common case.
    ///
    /// These ride on the result rather than beside the workspace because
    /// authoring now happens *inside* the candidate, after execution: the
    /// paths are an output of the run, and the caller indexes them by the same
    /// index it already uses to find the workspace.
    witness_paths: Vec<String>,
}

impl CandidateResult {
    /// Whether this candidate got as far as dispatching worker work.
    ///
    /// Only a degradable **setup** abort answers `false`: by construction it is
    /// the arm taken before any model call, so it is exactly the "generated
    /// nothing" case. An execution abort (budget, loop, step-cap) and a
    /// fail-closed witness rejection both ran real turns and count.
    fn executed(&self) -> bool {
        !(self.aborted.is_some() && self.degradable)
    }

    /// A candidate that aborted for a reason that is NOT a degradable
    /// infrastructure setup failure: an execution abort (budget, loop,
    /// step-cap — the worker ran) or a fail-closed security rejection (a
    /// poisoned witness). These keep their stop.
    fn aborted(messages: Vec<CompletionMessage>, reason: String) -> Self {
        Self {
            messages,
            final_text: String::new(),
            aborted: Some(reason),
            degradable: false,
            verdict: None,
            score: CandidateScore::Failed,
            diff_lines: 0,
            revisions: 0,
            witness_paths: Vec::new(),
        }
    }

    /// A candidate that aborted on a degradable **infrastructure** setup
    /// failure — no isolation port, or a tree that could not be snapshotted.
    /// Flagged so `run` degrades it to a bare worker run rather than ending
    /// the turn having done nothing.
    fn setup_aborted(messages: Vec<CompletionMessage>, reason: String) -> Self {
        Self {
            degradable: true,
            ..Self::aborted(messages, reason)
        }
    }
}

/// The candidate-local mutable state one execute+verify+revise pass threads
/// through its phases — grouped so [`Pipeline::run_candidate`]'s sub-methods
/// take one argument instead of seven. Every exit path moves it into the
/// returned [`CandidateResult`].
struct CandidateState {
    messages: Vec<CompletionMessage>,
    final_text: String,
    /// `FileChange` events observed across this candidate's engine turns —
    /// one half of the zero-diff guard's "touched files" signal (L-E2).
    file_changes: u32,
    oracle: FlipOracle,
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
    revisions: u32,
    /// Paths of the witness artifact grafted into this candidate, if the
    /// warrant bought one after execution. Empty until then, and empty forever
    /// when the change had nothing to prove.
    witness_paths: Vec<String>,
    /// Every deterministic failure this candidate has produced, in order.
    /// The airlock reads it to tell "stuck on the same thing" from "made
    /// progress and hit something new" — the signal that decides how much the
    /// next revision is told (`witness::airlock`).
    failures: Vec<FailureFingerprint>,
}

impl CandidateState {
    /// Finish this candidate with a verification verdict — every verified
    /// exit (deterministic or judged, passed or failed) funnels through here.
    fn into_verified(
        self,
        passed: bool,
        evidence: &JudgeEvidence,
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
    approvals: &'a dyn ApprovalGate,
    sleeper: &'a dyn Sleeper,
    hooks: Option<(&'a Hooks, &'a dyn HookRunner)>,
    candidate_workspaces: Option<&'a dyn CandidateWorkspacePort>,
    mcp_prefetch: Option<&'a dyn McpPrefetchPort>,
    steering: Option<&'a dyn stella_core::ports::TurnSteering>,
    events: EventSender,
    config: PipelineConfig,
    configured_test: Result<Option<TestInvocation>, crate::witness::TestInvocationError>,
    /// Monotonic `StepManifest::call_seq` for the management roles that call
    /// providers directly (`metered_raw_call`). They sit outside any step loop,
    /// so nothing else keys their receipts apart — several judge calls in one
    /// run would otherwise all land on the same primary key and overwrite each
    /// other. Starts at [`RECEIPT_SEQ_ALLOCATED_BASE`], above the seats the
    /// engine's worker and summarizer reserve.
    raw_call_seq: AtomicU64,
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
            approvals: ports.approvals,
            sleeper: ports.sleeper,
            hooks: ports.hooks,
            candidate_workspaces: ports.candidate_workspaces,
            mcp_prefetch: ports.mcp_prefetch,
            steering: ports.steering,
            events: events.into(),
            config,
            configured_test,
            raw_call_seq: AtomicU64::new(RECEIPT_SEQ_ALLOCATED_BASE),
        }
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
                ));
            }
        };
        let task_class = assessment.class;
        // The volatile recall+goal message rides AFTER the stable system
        // prefix (L-E8) — see assemble_user_message.
        messages.push(CompletionMessage::user(assemble_user_message(
            goal, &frames,
        )));

        // --- Conversational fast path. -------------------------------------
        // Triage classified this as chat, not a software task (a greeting,
        // small talk, a question about the agent), and the deterministic floor
        // saw no task signal to overrule it (triage::resolve_conversational).
        // Answer in one plain, tool-less completion and skip plan → witness →
        // execute → verify entirely. This is the fix for "typing `hi` authored
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
                    return Ok(self.aborted_before_execute(task_class, total_cost, &reason));
                }
                Err(cause) => return Err(PipelineRunError::new(cause, total_cost)),
            }
        } else {
            None
        };

        // --- 5. Witness + execute + verify (single-shot or best-of-N). ------
        let n = self.config.candidate_count();
        let base_messages = messages.clone();
        // Decided here, before the single-shot/best-of-N split, because an
        // authored witness is the *only* reason a single candidate needs
        // disposable isolation. Resolving independence later would commit the
        // run to snapshot machinery it then discovers it cannot use — and
        // candidate isolation requires a git working tree, so on a plain
        // directory that is a hard failure rather than an unused cost.
        let authored_witness = self.config.test_command.is_none()
            && self.config.witness_writer
            && assessment.wants_witness()
            && task_class.verifies_unconditionally()
            && self.can_author_independent_witness();
        // Single-shot (the default) runs directly over the session ports —
        // zero snapshot/adoption machinery only when the user supplied the
        // test invocation (or witness authoring is otherwise disabled).
        // Authored witnesses always require a disposable candidate, even at
        // N=1, so authoring can never mutate the session tree.
        // Best-of-N runs every candidate in an isolated snapshot of the
        // current tree state and adopts only the winner's changes (L-E7).
        let (best, worker_model_label, candidates_run) = if n == 1 && !authored_witness {
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
                    goal,
                    &base_messages,
                    plan.as_deref(),
                    assessment,
                    &worker,
                    1,
                    budget,
                    &mut total_cost,
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
                    goal,
                    &base_messages,
                    plan.as_deref(),
                    assessment,
                    n,
                    &frames,
                    authored_witness,
                    budget,
                    &mut total_cost,
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
                    goal,
                    &base_messages,
                    plan.as_deref(),
                    assessment,
                    budget,
                    &mut total_cost,
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
        if let Some(reason) = best.aborted {
            self.emit(AgentEvent::Error {
                message: reason.clone(),
                retryable: false,
            });
            return Ok(PipelineOutcome {
                status: PipelineStatus::Aborted { reason },
                task_class,
                final_text: best.final_text,
                total_cost_usd: total_cost,
                verdict: best.verdict,
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
                    messages: vec![CompletionMessage::user(triage_prompt(goal))],
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
        let model_says_chat = assessment.map(|a| a.conversational).unwrap_or(false);
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
                judge: resolved.wants_judge(),
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
        let mut convo = messages.clone();
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
                    timeout: None,
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
                ));
            }
            // Even chat needs the model; if it is down, abort cleanly rather
            // than fabricate a reply.
            Err(RawCallError::Provider | RawCallError::Timeout) => {
                return Ok(self.aborted_before_execute(
                    TaskClass::SimpleLookup,
                    *total_cost,
                    "conversational reply unavailable",
                ));
            }
        };

        // Adopt the assistant turn into the running trajectory (the same thing
        // the worked path does via `*messages = best.messages`) so a follow-up
        // keeps context.
        messages.push(CompletionMessage::assistant(reply.clone()));
        self.emit(AgentEvent::Text {
            delta: reply.clone(),
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
                    timeout: None,
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
                    timeout: None,
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
    #[allow(clippy::too_many_arguments)]
    async fn degrade_to_bare_execution(
        &self,
        goal: &str,
        base_messages: &[CompletionMessage],
        plan: Option<&[PlanStep]>,
        assessment: TaskAssessment,
        budget: &mut BudgetGuard,
        total: &mut f64,
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
        self.run_shared_candidates(
            goal,
            base_messages,
            plan,
            assessment,
            &worker,
            1,
            budget,
            total,
        )
        .await
        .pop()
    }

    /// Run `n` candidates sequentially over the session ports (the real
    /// working tree): the single-shot path, and the shared-tree degradation
    /// of best-of-N when no [`CandidateWorkspacePort`] is wired.
    #[allow(clippy::too_many_arguments)]
    async fn run_shared_candidates(
        &self,
        goal: &str,
        base_messages: &[CompletionMessage],
        plan: Option<&[PlanStep]>,
        assessment: TaskAssessment,
        worker: &ResolvedRole<'a>,
        n: u32,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Vec<CandidateResult> {
        let surface = CandidateSurface {
            diagnostics: self.diagnostics,
            tests: self.tests,
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
            let view = fan.as_ref().map(|fan| fan.candidate());
            if let Some(view) = view.as_ref() {
                engine = engine.with_steering(view);
            }
            results.push(
                self.run_candidate(
                    goal,
                    base_messages,
                    plan,
                    assessment,
                    // A shared-tree run has no workspace to graft into and no
                    // pristine snapshot to author blind in, so it never buys a
                    // witness — exactly as before, when it was passed `None`.
                    None,
                    &engine,
                    surface,
                    budget,
                    total,
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
    #[allow(clippy::too_many_arguments)]
    async fn run_best_of_n(
        &self,
        goal: &str,
        base_messages: &[CompletionMessage],
        plan: Option<&[PlanStep]>,
        assessment: TaskAssessment,
        n: u32,
        frames: &[RecalledFrame],
        author_witness: bool,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<(CandidateResult, Option<String>, u32), PipelineError> {
        // Orchestrator pre-fetch (issue #248) — see `crate::mcp_prefetch::fold`.
        let prefetched = crate::mcp_prefetch::fold(self.mcp_prefetch, goal, n, base_messages).await;
        let base_messages: &[CompletionMessage] = prefetched.as_deref().unwrap_or(base_messages);
        let Some(port) = self.candidate_workspaces else {
            if author_witness {
                return Ok((
                    CandidateResult::setup_aborted(
                        base_messages.to_vec(),
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
            let candidates = self
                .run_shared_candidates(
                    goal,
                    base_messages,
                    plan,
                    assessment,
                    &worker,
                    n,
                    budget,
                    total,
                )
                .await;
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
        // `can_author_independent_witness` already gated `author_witness` and
        // announced any degradation, so this is the invariant guard for that
        // decision — silent on purpose, never a second warning.
        let mut author_witness = author_witness;
        let witness_author = match author_witness
            .then(|| self.resolve_provider(Role::Judge))
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

        let mut candidates: Vec<CandidateResult> = Vec::with_capacity(n as usize);
        // Index-aligned with `candidates` — every path below pushes to both, so
        // adoption can pair a workspace with the result that produced it (and
        // with the witness paths that result carries). `None` marks a candidate
        // that never got a workspace.
        let mut workspaces: Vec<Option<Box<dyn CandidateWorkspace>>> =
            Vec::with_capacity(n as usize);
        // One mirror, one cursor per candidate — see `candidate_steering`.
        let fan = self.steering.map(SteeringFanOut::new);
        for i in 0..n {
            self.emit_text(candidate_start_notice(i, n, true));
            let ws = match port.create().await {
                Ok(ws) => ws,
                Err(e) => {
                    // Isolation was promised, so a candidate that cannot be
                    // isolated is never run in the shared tree instead: it
                    // scores as aborted and the remaining candidates go on.
                    self.warn(format!("candidate {}/{n} skipped: {e}", i + 1));
                    candidates.push(CandidateResult::setup_aborted(
                        base_messages.to_vec(),
                        format!("candidate isolation failed: {e}"),
                    ));
                    workspaces.push(None);
                    continue;
                }
            };
            let result = {
                let bound_hook_runner = self.hooks.map(|(_, runner)| BoundHookRunner {
                    inner: runner,
                    cwd: ws.root(),
                });
                let surface = CandidateSurface {
                    diagnostics: ws.diagnostics(),
                    tests: ws.tests(),
                    repo_status: ws.repo_status(),
                    cwd: Some(ws.root()),
                    hook_runner: bound_hook_runner
                        .as_ref()
                        .map(|runner| runner as &dyn HookRunner),
                    workspace: Some(ws.as_ref()),
                };
                // Handed to the candidate rather than spent here: whether this
                // run buys a witness is not knowable until the candidate has
                // executed and its diff can be read.
                let authoring = author_witness.then(|| WitnessAuthoring {
                    port,
                    author: witness_author
                        .as_ref()
                        .expect("authored witness identity is resolved before dispatch"),
                    frames,
                });
                let mut engine = Engine::with_sleeper(
                    worker.provider,
                    ws.tools(),
                    self.engine_config_for(surface),
                    self.sleeper,
                );
                if let Some((hooks, runner)) = self.hooks {
                    engine = engine.with_hooks(hooks, surface.hook_runner.unwrap_or(runner));
                }
                let view = fan.as_ref().map(|fan| fan.candidate());
                if let Some(view) = view.as_ref() {
                    engine = engine.with_steering(view);
                }
                self.run_candidate(
                    goal,
                    base_messages,
                    plan,
                    assessment,
                    authoring,
                    &engine,
                    surface,
                    budget,
                    total,
                )
                .await
            };
            // Pushed together, always: adoption pairs `candidates[i]` with
            // `workspaces[i]`, and the witness paths now ride on the result,
            // so a candidate can never be matched to another one's artifact.
            candidates.push(result);
            workspaces.push(Some(ws));
        }

        let best_idx = best_index(&candidates);
        // Counted before the winner is moved out — the report is about the
        // whole fan-out, not the one result that survives it.
        let ran = executed_count(&candidates);
        self.emit_text(candidate_winner_notice(best_idx, n, ran));
        // Winner adoption + cleanup. An aborted winner adopts nothing — an
        // aborted best-of-N run leaves the real tree untouched.
        let mut adopt_failure: Option<WorkspaceError> = None;
        for (i, slot) in workspaces.into_iter().enumerate() {
            let Some(ws) = slot else {
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
            } else {
                ws.remove().await;
            }
        }
        let mut best = candidates
            .into_iter()
            .nth(best_idx)
            .expect("best_index returns an in-range index");
        if let Some(e) = adopt_failure {
            best.aborted = Some(e.to_string());
        }
        Ok((best, Some(worker_label), ran))
    }

    // Stages: execute + verify + revise (one candidate)

    #[allow(clippy::too_many_arguments)]
    async fn run_candidate(
        &self,
        goal: &str,
        base_messages: &[CompletionMessage],
        plan: Option<&[PlanStep]>,
        assessment: TaskAssessment,
        authoring: Option<WitnessAuthoring<'_>>,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        budget: &mut BudgetGuard,
        total: &mut f64,
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
        if assessment.class.verifies_unconditionally()
            && let Some(cmd) = self.effective_test_command(None)
        {
            let pre = surface.tests.run_test(cmd.invocation).await;
            oracle.observe(cmd.command, pre.passed());
            self.emit_proof(ProofStep::Oracle {
                command: cmd.command.to_string(),
                passed: pre.passed(),
                tree: ProofTree::Baseline,
            });
        }

        // Snapshot untracked files (with content fingerprints) BEFORE
        // executing so `gather_diff` can tell files this turn created OR
        // modified from pre-existing dirty state — a stale untracked file with
        // an unchanged fingerprint is not this turn's work, but one the turn
        // edited (fingerprint changed) is.
        let untracked_before = surface.repo_status.untracked_fingerprints().await;

        let mut state = CandidateState {
            messages: base_messages.to_vec(),
            final_text: String::new(),
            file_changes: 0,
            oracle,
            untracked_before,
            diff_lines: 0,
            diff_text: String::new(),
            diff_available: true,
            touch_baseline: self.touches.mutations_recorded(),
            revisions: 0,
            witness_paths: Vec::new(),
            failures: Vec::new(),
        };

        if let Err(reason) = self
            .execute_plan(plan, engine, budget, total, &mut state)
            .await
        {
            return CandidateResult::aborted(state.messages, reason);
        }

        // Decide whether to verify: unconditional for single/multi; for a
        // simple lookup, only if the turn unexpectedly touched files (the
        // zero-diff guard, L-E2). "Touched files" = FileChange events observed
        // OR a non-empty diff.
        let probe = self.gather_diff(surface, &state.untracked_before).await;
        self.absorb_probe(&mut state, probe);
        let files_touched = state.file_changes > 0 || !state.diff_text.trim().is_empty();
        let should_verify = assessment.class.verifies_unconditionally()
            || (assessment.class == TaskClass::SimpleLookup && files_touched);
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
        let warrant = warrant(&state.diff_text, state.file_changes);
        self.emit_proof(ProofStep::Warrant {
            required: warrant.is_required(),
            reason: warrant.reason().map(|r| r.sentence().to_string()),
            diff_lines: state.diff_lines,
        });

        // Buy the witness now, or not at all. Everything above this line has
        // already happened, so the diff is evidence rather than a prediction —
        // which is the whole reason authoring waits until here.
        let witness = match self
            .witness_on_demand(goal, authoring, surface, &mut state, budget, total)
            .await
        {
            Ok(witness) => witness,
            Err(reason) => return CandidateResult::aborted(state.messages, reason),
        };

        self.verify_candidate(
            goal,
            assessment,
            witness.as_ref(),
            engine,
            surface,
            budget,
            total,
            state,
        )
        .await
    }

    /// Execute stage: one turn for simple/single-task; one turn per plan step
    /// for multi-step (each step guides a fresh engine turn). The last turn's
    /// text lands in `state.final_text`; `Err` is the first aborted turn's
    /// reason.
    async fn execute_plan(
        &self,
        plan: Option<&[PlanStep]>,
        engine: &Engine<'_>,
        budget: &mut BudgetGuard,
        total: &mut f64,
        state: &mut CandidateState,
    ) -> Result<(), String> {
        self.emit(AgentEvent::Stage {
            name: StageKind::Execute,
        });
        // Borrowed, not collected: the steps are only read, so materializing a
        // `Vec<&PlanStep>` per candidate bought nothing.
        let steps: &[PlanStep] = plan.unwrap_or_default();
        if steps.is_empty() {
            match self
                .run_engine_turn(engine, &mut state.messages, budget, &mut state.file_changes)
                .await
            {
                TurnOutcome::Completed { text, cost_usd } => {
                    state.final_text = text;
                    *total += cost_usd;
                }
                TurnOutcome::Aborted { reason, cost_usd } => {
                    *total += cost_usd;
                    return Err(reason);
                }
            }
        } else {
            let n = steps.len();
            for (i, step) in steps.iter().enumerate() {
                state.messages.push(CompletionMessage::user(format!(
                    "Step {}/{}: {}",
                    i + 1,
                    n,
                    step.description
                )));
                match self
                    .run_engine_turn(engine, &mut state.messages, budget, &mut state.file_changes)
                    .await
                {
                    TurnOutcome::Completed { text, cost_usd } => {
                        state.final_text = text;
                        *total += cost_usd;
                    }
                    TurnOutcome::Aborted { reason, cost_usd } => {
                        *total += cost_usd;
                        return Err(reason);
                    }
                }
            }
        }
        Ok(())
    }

    /// Verify + bounded revise loop over an executed candidate: observe the
    /// tests, take the deterministic ladder decision (L-E11), and either
    /// finish with a verdict, escalate to the model judge, or spend one of
    /// `max_revisions` on a revise pass and re-observe. Owns `state` because
    /// every exit moves it into the returned [`CandidateResult`].
    #[allow(clippy::too_many_arguments)]
    async fn verify_candidate(
        &self,
        goal: &str,
        assessment: TaskAssessment,
        witness: Option<&Witness>,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        budget: &mut BudgetGuard,
        total: &mut f64,
        mut state: CandidateState,
    ) -> CandidateResult {
        self.emit(AgentEvent::Stage {
            name: StageKind::Verify,
        });
        let effective_cmd = self.effective_test_command(witness);
        let witness_paths = Self::witness_paths(witness);
        loop {
            if let Some(workspace) = surface.workspace
                && let Err(error) = workspace.seal().await
            {
                return CandidateResult::aborted(
                    state.messages,
                    format!("candidate could not be sealed for verification: {error}"),
                );
            }
            let (touched_tests_passed, test_tail) = self
                .observe_touched_tests(surface, effective_cmd, &mut state.oracle)
                .await;
            // Tamper exclusion is an authority boundary, not evidence for a
            // model to weigh. Any post-baseline witness mutation hard-fails
            // the candidate before a judge can override it.
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
                );
            }
            if let Some(workspace) = surface.workspace {
                match workspace.sealed_is_unchanged().await {
                    Ok(true) => {}
                    Ok(false) => {
                        return CandidateResult::aborted(
                            state.messages,
                            "candidate worktree changed after verification".to_string(),
                        );
                    }
                    Err(error) => {
                        return CandidateResult::aborted(
                            state.messages,
                            format!("could not validate the verified candidate seal: {error}"),
                        );
                    }
                }
            }
            let inputs = LadderInputs {
                flip_achieved: state.oracle.is_flipped(),
                touched_tests_passed,
                diff_lines: state.diff_lines,
                diff_budget: self.config.diff_budget_lines,
                diff_available: state.diff_available,
                file_change_events: state.file_changes,
            };

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
                    // Deterministic pass — judge SKIPPED (L-E11).
                    let evidence = deterministic_pass_evidence(
                        state.oracle.tracked_command(),
                        state.diff_lines,
                    );
                    self.emit(AgentEvent::JudgeVerdict {
                        passed: true,
                        evidence: evidence.clone(),
                    });
                    return state.into_verified(
                        true,
                        &evidence,
                        score_from_verification(true, None),
                    );
                }
                LadderDecision::Unverifiable => {
                    // Every channel was blind. The judge is not asked, because
                    // the only thing it could do is guess from an empty record
                    // — which in the wild it did, returning `FAIL … the file
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
                    let evidence = unverifiable_evidence(&inputs);
                    self.unverifiable(&evidence.summary);
                    self.emit(AgentEvent::JudgeVerdict {
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
                    // Deterministic failure (touched tests red) — no judge.
                    //
                    let (evidence, brief) =
                        Self::deterministic_disclosure(&mut state, &sealed, &test_tail);
                    self.emit(AgentEvent::JudgeVerdict {
                        passed: false,
                        evidence: evidence.clone(),
                    });
                    if state.revisions >= self.config.max_revisions {
                        return state.into_verified(
                            false,
                            &evidence,
                            score_from_verification(false, Some(false)),
                        );
                    }
                    // Distress trigger: a SECOND consecutive deterministic
                    // failure means the evidence alone didn't steer the
                    // worker — spend one judge call on course-correction
                    // (event-triggered, never a fixed midpoint checkpoint).
                    let mut reason = brief.message();
                    if self.config.distress_guidance && state.revisions >= 1 {
                        match self
                            .judge_guidance(
                                goal,
                                &state.diff_text,
                                &evidence.summary,
                                budget,
                                total,
                            )
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
                                return CandidateResult::aborted(state.messages, abort.reason);
                            }
                        }
                    }
                    if let Err(reason) = self
                        .revise_candidate(engine, surface, budget, &reason, total, &mut state)
                        .await
                    {
                        return CandidateResult::aborted(state.messages, reason);
                    }
                }
                // Triage judged this result not worth a separate reviewer.
                // Record exactly that: a pass carrying no independent
                // evidence. Falling through to `heuristic_fallback` would
                // report "judge unavailable", which describes a judge that
                // broke — not one that was deliberately waived — and would
                // turn a task triage called simple into a verification
                // failure. The summary states plainly what was not done.
                LadderDecision::ModelJudge if !assessment.wants_judge() => {
                    let evidence = JudgeEvidence {
                        summary: "model review waived by triage; no independent \
                                  verification was performed"
                            .to_string(),
                        deterministic: false,
                        evidence_refs: vec![],
                    };
                    self.emit(AgentEvent::JudgeVerdict {
                        passed: true,
                        evidence: evidence.clone(),
                    });
                    // Scored `Unverified`, NOT `DeterministicPass`: the run
                    // passes, but no evidence was gathered for it. Claiming
                    // the ladder's strongest score here would let a
                    // review-waived candidate tie a genuinely flip-verified
                    // sibling in best-of-N and then win the smaller-diff
                    // tiebreak — selection would prefer the candidate that
                    // proved the least.
                    return state.into_verified(
                        true,
                        &evidence,
                        score_from_verification(false, None),
                    );
                }
                LadderDecision::ModelJudge => {
                    // Escalate on evidence, not on prediction. "Inconclusive"
                    // here can mean two very different things: a real change
                    // nothing proved, or a change with nothing to prove. The
                    // diff tells them apart, and the prompt never could — so
                    // ask it before buying a judge call to confirm the absence
                    // of a test that was never warranted
                    // (docs/design/witness-protocol.md §7).
                    if let Some(evidence) = self.warranted_completion(&state) {
                        return state.into_verified(
                            true,
                            &evidence,
                            score_from_verification(false, None),
                        );
                    }
                    // Inconclusive — escalate to the model judge (judge ≠
                    // worker; a judge-call failure falls back to a heuristic).
                    let evidence_summary = format!(
                        "flip_achieved={}; touched_tests={:?}; diff_lines={} (budget {}); \
                         file_change_events={}",
                        inputs.flip_achieved,
                        inputs.touched_tests_passed,
                        state.diff_lines,
                        self.config.diff_budget_lines,
                        state.file_changes,
                    );
                    let verdict = match self
                        .judge(
                            goal,
                            &state.diff_text,
                            &evidence_summary,
                            &inputs,
                            budget,
                            total,
                        )
                        .await
                    {
                        Ok(verdict) => verdict,
                        Err(abort) => {
                            return CandidateResult::aborted(state.messages, abort.reason);
                        }
                    };
                    let evidence = model_verdict_evidence(&verdict);
                    self.emit(AgentEvent::JudgeVerdict {
                        passed: verdict.passed,
                        evidence: evidence.clone(),
                    });
                    if verdict.passed {
                        return state.into_verified(
                            true,
                            &evidence,
                            score_from_verification(false, Some(true)),
                        );
                    }
                    if state.revisions >= self.config.max_revisions {
                        return state.into_verified(
                            false,
                            &evidence,
                            score_from_verification(false, Some(false)),
                        );
                    }
                    // The judge read the deterministic evidence summary, so
                    // its prose can carry the tail back. A reasoning that
                    // quotes sealed material degrades to the symptom rather
                    // than being forwarded (§4.3).
                    let feedback = self
                        .airlock_forward(&verdict.reasoning, "judge_reasoning", &sealed)
                        .unwrap_or_else(|| redact(&sealed, DisclosureGrain::Symptom).message());
                    if let Err(reason) = self
                        .revise_candidate(engine, surface, budget, &feedback, total, &mut state)
                        .await
                    {
                        return CandidateResult::aborted(state.messages, reason);
                    }
                }
            }
        }
    }

    /// Post-execute test observation for the flip oracle + the touched-tests
    /// signal: `(Some(passed), stderr tail)` when a test command is available
    /// (configured or witness-authored), `(None, "")` when there is nothing
    /// to run.
    async fn observe_touched_tests(
        &self,
        surface: CandidateSurface<'_>,
        cmd: Option<EffectiveTestCommand<'_>>,
        oracle: &mut FlipOracle,
    ) -> (Option<bool>, String) {
        match cmd {
            Some(cmd) => {
                let post = surface.tests.run_test(cmd.invocation).await;
                let passed = post.passed();
                oracle.observe(cmd.command, passed);
                self.emit_proof(ProofStep::Oracle {
                    command: cmd.command.to_string(),
                    passed,
                    tree: ProofTree::Candidate,
                });
                (Some(passed), post.stderr_tail)
            }
            None => (None, String::new()),
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
    /// evidence and fold the fresh diff back into `state`. `Err` is the abort
    /// reason of a turn that died mid-revision (budget/loop).
    async fn revise_candidate(
        &self,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        budget: &mut BudgetGuard,
        reason: &str,
        total: &mut f64,
        state: &mut CandidateState,
    ) -> Result<(), String> {
        let probe = self
            .revise_turn(
                engine,
                surface,
                &mut state.messages,
                budget,
                reason,
                &mut state.file_changes,
                &mut state.final_text,
                total,
                &state.untracked_before,
            )
            .await?;
        self.absorb_probe(state, probe);
        state.revisions += 1;
        Ok(())
    }

    /// Run one revision turn: append an evidence-carrying instruction, execute,
    /// and re-gather the diff. Emits the `Execute`/`Verify` stage bookends so
    /// the stream shows the revise loop. Returns the fresh `(diff_lines,
    /// diff_text)` on success, or the abort reason on a budget/loop abort.
    #[allow(clippy::too_many_arguments)]
    async fn revise_turn(
        &self,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        reason: &str,
        file_changes: &mut u32,
        final_text: &mut String,
        total: &mut f64,
        untracked_before: &HashMap<String, String>,
    ) -> Result<DiffProbe, String> {
        messages.push(CompletionMessage::user(revision_prompt(reason)));
        self.emit(AgentEvent::Stage {
            name: StageKind::Execute,
        });
        match self
            .run_engine_turn(engine, messages, budget, file_changes)
            .await
        {
            TurnOutcome::Completed { text, cost_usd } => {
                *final_text = text;
                *total += cost_usd;
            }
            TurnOutcome::Aborted { reason, cost_usd } => {
                *total += cost_usd;
                return Err(reason);
            }
        }
        let probe = self.gather_diff(surface, untracked_before).await;
        self.emit(AgentEvent::Stage {
            name: StageKind::Verify,
        });
        Ok(probe)
    }

    // Stage: judge

    /// One distress-guidance call ([`guidance_prompt`]): best-effort and
    /// never a verdict — the failure it reacts to is already deterministic,
    /// so the judge's job here is *steering*, not re-judging. A failed call
    /// (or an unresolvable judge) degrades to evidence-only revision.
    async fn judge_guidance(
        &self,
        goal: &str,
        diff: &str,
        evidence_summary: &str,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<Option<String>, PipelineBudgetAbort> {
        let resolved = match self.resolve_provider(Role::Judge) {
            Ok(resolved) => resolved,
            Err(_) => return Ok(None),
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }
        self.emit(AgentEvent::Stage {
            name: StageKind::Judge,
        });
        let prompt = guidance_prompt(goal, diff, evidence_summary);
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::DistressGuidance,
                    resolved: &resolved,
                    messages: vec![CompletionMessage::user(prompt)],
                    policy: RetryPolicy::deterministic(),
                    overrides: &self.config.role_overrides.judge,
                    timeout: None,
                },
                budget,
                total,
            )
            .await
        {
            Ok(result) => {
                let text = result.text.trim().to_string();
                if text.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(text))
                }
            }
            Err(RawCallError::Budget(abort)) => Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => Ok(None),
        }
    }

    async fn judge(
        &self,
        goal: &str,
        diff: &str,
        evidence_summary: &str,
        inputs: &LadderInputs,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<ModelJudgeVerdict, PipelineBudgetAbort> {
        self.emit(AgentEvent::Stage {
            name: StageKind::Judge,
        });
        let resolved = match self.resolve_provider(Role::Judge) {
            Ok(r) => r,
            // Judge unresolvable → conservative heuristic verdict (L-E11).
            Err(_) => return Ok(heuristic_fallback(inputs)),
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }

        let prompt = judge_prompt(goal, diff, evidence_summary);
        // Deterministic policy: a judge call that fails must not hang; it falls
        // back to the heuristic verdict rather than retrying.
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::Judge,
                    resolved: &resolved,
                    messages: vec![CompletionMessage::user(prompt)],
                    policy: RetryPolicy::deterministic(),
                    overrides: &self.config.role_overrides.judge,
                    timeout: None,
                },
                budget,
                total,
            )
            .await
        {
            Ok(result) => {
                let verdict = parse_judge_response(&result.text)
                    .unwrap_or_else(|| heuristic_fallback(inputs));
                Ok(verdict)
            }
            Err(RawCallError::Budget(abort)) => Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => Ok(heuristic_fallback(inputs)),
        }
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
        })
    }

    /// Run one engine turn, forwarding every event to the consumer **live**
    /// (a concurrent drain task, not a post-hoc flush — an execute turn can
    /// run tool loops for minutes, and buffering froze the renderer for the
    /// whole turn) **except** the engine's `Stage`/`Complete` (the pipeline
    /// owns those), tallying `FileChange`s into `file_changes` for the
    /// zero-diff guard.
    async fn run_engine_turn(
        &self,
        engine: &Engine<'_>,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        file_changes: &mut u32,
    ) -> TurnOutcome {
        // The filtered sender is SYNCHRONOUS on purpose: when the outer
        // sender carries a durability boundary, a paid StepUsage cannot
        // return to the engine before append+flush completes. Draining a
        // channel from a spawned forwarder instead would let the engine make
        // another paid call before the previous one's metering row is durable.
        let seen_file_changes = Arc::new(AtomicU32::new(0));
        let count = seen_file_changes.clone();
        let consumer = self.events.clone();
        let filtered = EventSender::from_fn(move |event| {
            match &event {
                // The pipeline is the sole authority for stage boundaries and
                // the terminal event of an outcome-producing run — drop the
                // engine's per-turn copies.
                AgentEvent::Stage { .. } | AgentEvent::Complete { .. } => Ok(()),
                AgentEvent::FileChange { kind, .. } => {
                    // Reads ride the same event for the files panel but are
                    // not changes — counting them would defeat the zero-diff
                    // guard on read-only turns.
                    if kind.is_mutation() {
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                    consumer.send(event)
                }
                _ => consumer.send(event),
            }
        });
        let outcome = engine
            .run_turn_with_sender(messages, budget, &filtered)
            .await;
        *file_changes += seen_file_changes.load(Ordering::Relaxed);
        outcome
    }

    /// The real added-line count of an untracked file, via a no-index diff
    /// numstat (`<added>\t<deleted>\t<path>`). A binary file numstats as `-`
    /// and counts as one changed line (a change the ladder must see, but
    /// unmeasurable in lines). Counting real lines — not a flat 1 per file —
    /// is what keeps a large untracked file from slipping under the diff budget
    /// and taking `SubmitFast`. A single file's numstat is one line, so this is
    /// safe against the diagnostic runner's output truncation.
    async fn untracked_added_lines(&self, surface: CandidateSurface<'_>, path: &str) -> u32 {
        let out = surface
            .diagnostics
            .run_diagnostic(&DiagnosticInvocation::UntrackedNumstat {
                path: path.to_string(),
            })
            .await;
        out.stdout_tail
            .lines()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| l.split('\t').next())
            .and_then(|added| added.trim().parse::<u32>().ok())
            .unwrap_or(1)
    }

    /// Run the diff command and return `(changed_line_count, raw_diff)`.
    ///
    /// `git diff` cannot see untracked files, so a turn whose entire change is
    /// a NEW (or edited-untracked) file would read as "no diff" — skipping
    /// verification via the zero-diff guard and showing the judge an empty
    /// diff. When the configured command is git's, untracked files that were
    /// **created or modified this turn** — present now with either no
    /// `untracked_before` entry or a changed fingerprint — are appended with
    /// their real added-line counts. Untouched dirty files (same fingerprint)
    /// are excluded, so pre-existing state is never attributed to the turn, and
    /// a large untracked file cannot slip under the diff-size budget.
    async fn gather_diff(
        &self,
        surface: CandidateSurface<'_>,
        untracked_before: &HashMap<String, String>,
    ) -> DiffProbe {
        let Some(diagnostic) = &self.config.diff_diagnostic else {
            // No probe configured is not "nothing changed" either: this host
            // simply has no diff channel, so it reports one fewer way to see.
            return DiffProbe {
                lines: 0,
                text: String::new(),
                available: false,
            };
        };
        let out = surface.diagnostics.run_diagnostic(diagnostic).await;
        // `git diff` exits 0 on a clean tree, so a non-zero exit with nothing on
        // stdout is the machinery failing to READ the tree, never a report that
        // the tree is unchanged. Observed in the wild as a candidate shadow
        // whose worktree registration was gone by the time it was probed
        // (`git add -A` → "fatal: not a git repository"). See
        // [`DIFF_PROBE_FAILED`] for why the empty string could not be left to
        // `verification_honest_diff` to catch. Scoped to `GitDiff` because
        // `--no-index --numstat` exits 1 whenever the files differ, which is
        // that probe's ordinary success.
        if matches!(diagnostic, DiagnosticInvocation::GitDiff)
            && !out.passed()
            && out.stdout_tail.trim().is_empty()
        {
            // git's own words, matched case-insensitively because the phrasing
            // is stable across versions but its capitalization is not.
            let not_a_repo = out
                .stderr_tail
                .to_ascii_lowercase()
                .contains("not a git repository");
            return DiffProbe {
                lines: 0,
                text: if not_a_repo {
                    DIFF_PROBE_NOT_A_REPO.to_string()
                } else {
                    DIFF_PROBE_FAILED.to_string()
                },
                available: false,
            };
        }
        let mut lines = count_diff_lines(&out.stdout_tail);
        let mut text = out.stdout_tail;
        if matches!(diagnostic, DiagnosticInvocation::GitDiff) {
            let after = surface.repo_status.untracked_fingerprints().await;
            // Created (absent before) OR modified (fingerprint changed) this
            // turn — never an untouched dirty file.
            let mut fresh: Vec<&str> = after
                .iter()
                .filter(|(path, fp)| untracked_before.get(*path) != Some(*fp))
                .map(|(path, _)| path.as_str())
                .collect();
            fresh.sort(); // deterministic order for the appended evidence
            // Each of these is a `git diff --no-index --numstat` subprocess.
            // Run sequentially, a turn that creates many untracked files paid
            // one full process round-trip per file — on every verification
            // observation and again after every revision, so the cost is
            // repaid per revision per candidate.
            //
            // Bounded concurrency rather than a truncating cap: this count
            // feeds the zero-diff guard and the diff-size budget, so dropping
            // the tail would let a large untracked change slip under a budget
            // it should have tripped. `buffered` preserves input order, so
            // the appended evidence stays deterministic.
            let counted: Vec<(&str, u32)> =
                futures_util::stream::iter(fresh.into_iter().map(|path| async move {
                    (path, self.untracked_added_lines(surface, path).await)
                }))
                .buffered(UNTRACKED_NUMSTAT_CONCURRENCY)
                .collect()
                .await;
            for (path, added) in counted {
                lines += added;
                text.push_str(&format!("\n+ untracked change: {path} (+{added} lines)"));
            }
        }
        DiffProbe {
            lines,
            text,
            available: true,
        }
    }

    /// Mutating file touches this candidate has made, read from the recorder
    /// that emitted the `FileChange` events rather than from a wrapper around
    /// the engine's sender.
    ///
    /// This is the fix for the counter that read `0` while six `file_change`
    /// events sat in the very stream the judge's run produced (#973). The two
    /// numbers came from different wires: `ToolRegistry::record_touch` sends to
    /// the channel the *host* attached, and the tally lived on a sender the
    /// pipeline handed the *engine*, which no file tool ever uses.
    ///
    /// `max` of the two, because they are independent lower bounds rather than
    /// a sum: the recorder's delta is authoritative wherever a `FileTouchPort`
    /// is wired, and the event tally is the only signal for a host with no
    /// recorder at all. Neither can double-count the other — the sender the
    /// tally wraps is built per-turn inside this pipeline and never handed out.
    fn observed_mutations(&self, state: &CandidateState) -> u32 {
        let delta = self
            .touches
            .mutations_recorded()
            .saturating_sub(state.touch_baseline);
        u32::try_from(delta)
            .unwrap_or(u32::MAX)
            .max(state.file_changes)
    }

    /// Fold one working-tree observation into `state`, refreshing every
    /// evidence channel from its own source at the same instant.
    ///
    /// One function, so the channels cannot be updated apart and disagree about
    /// which round they describe — the honest-diff text in particular is built
    /// from the touch count, and reading a stale one is how a real change gets
    /// rendered as an empty diff.
    fn absorb_probe(&self, state: &mut CandidateState, probe: DiffProbe) {
        state.file_changes = self.observed_mutations(state);
        state.diff_lines = probe.lines;
        state.diff_available = probe.available;
        state.diff_text = verification_honest_diff(probe.text, state.file_changes);
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

    /// Whether a witness author independent of the worker can be resolved.
    ///
    /// Losing the author costs the run its authored witness, never the task:
    /// a `false` here routes to the ordinary single-shot path and the
    /// deterministic/judge verify ladder. Announced once, at the one point
    /// that decides it, so the run never pays for isolation it cannot use.
    fn can_author_independent_witness(&self) -> bool {
        let Ok(worker) = self.resolve_provider(Role::Worker) else {
            // A worker that won't resolve fails later, on its own terms —
            // not here, disguised as a witness-independence verdict.
            return false;
        };
        match self.resolve_provider(Role::Judge) {
            Ok(judge) if judge.model_ref != worker.model_ref => true,
            // Both arms report through `unproven`, not a bare `warn`. A
            // witness triage asked for and the wiring cannot supply is
            // precisely `WitnessUnavailable` — routing it to the warning
            // channel alone left the rail's witness row with no statement, so
            // it fell through to the backstop's "not reported" when the real
            // answer was known all along and worth naming.
            Ok(_) => {
                self.unproven(format!(
                    "no author independent of the worker (judge and worker both resolved to `{}`)",
                    worker.model_ref
                ));
                false
            }
            Err(_) => {
                self.unproven(
                    "no author independent of the worker (the judge role is unresolvable)"
                        .to_string(),
                );
                false
            }
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
        if let Some(delta) = notice {
            self.emit(AgentEvent::Text { delta });
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
    ) -> PipelineOutcome {
        let (event, outcome) = stage_budget::aborted_before_execute(task_class, total_cost, reason);
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

fn assemble_user_message(goal: &str, frames: &[RecalledFrame]) -> String {
    if frames.is_empty() {
        return goal.to_string();
    }
    let mut s = String::from("## Recalled context\n");
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
    s.push_str("\n## Task\n");
    s.push_str(goal.trim());
    s
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
mod tests;
