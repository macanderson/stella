//! The orchestrator: the staged turn flow that sits
//! *above* `stella-core::Engine`. It sequences evaluate → enhance → route →
//! execute → deterministic verify → bounded revise over the injected ports,
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
use stella_core::{
    AbortKind, BudgetGuard, CalibrationMap, Engine, EngineConfig, EventSender, Router, TurnOutcome,
};
use stella_protocol::{
    AgentEvent, CompletionMessage, LadderSnapshot, MessageRole, ModelCallRole, ModelRef,
    OracleObservation, ProofStep, ProofTree, Provider, Role, StageKind, VerdictEvidence,
};

use self::revision::{RevisionCause, TestFailureReceipt, revision_prompt};
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
    DiagnosticInvocation, DiagnosticRunner, FileTouchPort, McpPrefetchPort, PipelinePorts,
    ProviderResolver, Recall, RecalledFrame, RepoStatusPort, RepoStructurePort, ScopeDecision,
    TestInvocation, TestRunner, WorkspaceError,
};
use crate::research::ResearchFinding;
use crate::roster::Roster;
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
use crate::verify::coverage::DiffCoverage;
use crate::verify::{
    FlipOracle, LadderDecision, LadderInputs, deterministic_fail_evidence,
    deterministic_pass_evidence, ladder_decision, normalize_command, nothing_attempted_evidence,
    unverifiable_evidence, unverified_evidence,
};
use crate::witness::airlock::{FailureFingerprint, SealedFailure, grain_for_repeats};
use crate::witness::parse_test_invocation;
use crate::witness::warrant::{ChangeSignals, warrant};
pub use resume_stage::{FrameProgress, PipelineResume, RecordedBaseline};
mod attachments;
// `pub(crate)` for one constant: `verify`'s instruction block names the
// authored section's header so the prompt and the header cannot drift apart.
pub(crate) mod authored;
mod candidate_result;
mod disclosure;
mod evidence;
mod execute_stage;
mod fanout_stage;
mod plan_steps;
mod raw_usage;
mod repair_gate;
mod research_stage;
mod role_overrides;
pub use role_overrides::{PipelineRoleOverrides, RoleCallOverrides};
mod resume_stage;
mod revision;
mod role_pace;
mod roster_wiring;
use roster_wiring::Assigned;
mod run_error;
mod scope_stage;
mod stage_budget;
mod task_frame;
mod triage_stage;
/// The worker's opening user message (recall, research findings, goal,
/// verification contract) — pure text assembly, in its own module because
/// `pipeline.rs` is closed to growth.
mod user_message;
use user_message::{VerificationContract, assemble_user_message};
mod verify_probes;
use candidate_result::{CandidateAbort, CandidateResult, TurnAbort, escape_abort_reason};
use fanout_stage::SerialCreates;
use raw_usage::{RawCall, RawCallError};
use run_error::RoleResolveError;
pub use run_error::{PipelineError, PipelineRunError};
use stage_budget::{PipelineStageAbort, Spend};
use task_frame::TaskFrame;
use verify_probes::DiffProbe;
use verify_probes::ObservedTest;

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
    /// and refused repairs a long run could still afford (#1507).
    ///
    /// **This field is an estimate, not a ceiling.** Nothing is cancelled when
    /// it elapses and no call is refused because of it; with the deadline below
    /// it sizes the repair gate's headroom and the witness-repair bound, and
    /// both `None` means nobody is measuring — the axis abstains, never zeroes.
    ///
    /// The enforcing wall clock is a different field elsewhere:
    /// `BudgetGuard::set_task_deadline`, an absolute instant on the guard
    /// threaded into [`Pipeline::run`]. That one IS honoured — by the engine's
    /// step loop (`stella_core::driver::settlement`) and, since #2238, before
    /// every raw stage dispatch. Both feed `Pipeline::remaining_wall_clock`,
    /// which since #2433 reports whichever binds first, so a surface arming
    /// only one of them is still measured.
    pub run_budget: Option<Duration>,
    /// Per-role request overrides (`agent_engine_config`) for raw management
    /// calls. Verification does not consume an override or call a model.
    pub role_overrides: PipelineRoleOverrides,
    /// Who performs each responsibility, and whether it runs at all (#2381).
    /// Live rows cover triage, research, planning, and worker execution.
    /// Historical verifier-owned rows (`witness_author`,
    /// `distress_guidance`, and `verdict`) are hard-retired: configuration may
    /// preserve an explicit disabled row for compatibility, but cannot enable
    /// or reassign one.
    pub roster: Roster,
    /// Decision latency ceiling on the triage classification call (L-M4): if
    /// it doesn't answer within this, the in-flight call is dropped and
    /// triage falls through to the full path. The expiry is not silent in
    /// accounting: `run_accounted_call` records a content-free
    /// `UsageIncomplete` envelope for the abandoned attempt (its provider-side
    /// spend is unknowable once the response never lands), and — since
    /// #2414 — the stage emits a [`stella_protocol::ProofStep::TriageDegraded`]
    /// naming the ceiling it hit.
    ///
    /// The default was 10s, and that number was measurably below the
    /// distribution it was bounding rather than above it. Across three
    /// Terminal-Bench arm runs, 27 of 34 triage calls burned the full 10,000ms
    /// and returned nothing — while the 7 that *did* answer took
    /// 4,684-8,587ms, i.e. even a successful triage was landing within a
    /// couple of seconds of the wall. A ceiling set inside the answering
    /// distribution does not bound a pathology, it converts slow-but-correct
    /// answers into no answer at all, and pays the full ceiling for the
    /// privilege.
    ///
    /// The never-wedge contract is what this exists for and it is unchanged: a
    /// wedged provider still costs exactly one bounded wait and the run still
    /// proceeds on the deterministic floor. What changed is which side of the
    /// observed distribution the bound sits on. It remains an order of
    /// magnitude under a run's own budget, and the honest reading of the new
    /// number is that it is sized from 7 answering samples — small, and now at
    /// least reported rather than assumed, which is what the degradation
    /// record above is for.
    pub triage_latency_ceiling: Duration,
    /// Latency ceiling on the context-recall port. Recall runs concurrently
    /// with triage and is advisory (L-C6), never a gate — but nothing bounded
    /// it, so a wedged embedding call or an unresponsive CGP host hung the
    /// whole turn before the first stage completed, with no event after
    /// `Stage { ContextRecall }` to say why. Past this, recall degrades to
    /// [`crate::ports::Recall::default`] (no frames) and the turn proceeds.
    pub recall_latency_ceiling: Duration,
    /// Latency ceiling on each pre-plan research sub-agent (#1778) — the same
    /// never-wedge contract as [`Self::triage_latency_ceiling`], sized for a
    /// bounded multi-step read rather than one completion. Children run
    /// concurrently, so the stage's wall-clock is bounded by one ceiling, not
    /// one per question; a child past it is cancelled and degrades to a
    /// missing finding.
    pub research_latency_ceiling: Duration,
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
    /// When absent, completion requires a concrete built-in `verify_done`
    /// receipt; otherwise verification abstains.
    pub test_command: Option<String>,
    /// Legacy authored-witness setting retained for configuration decoding.
    /// Model-authored witnesses are retired, so this flag has no effect.
    pub keep_witness: bool,
    /// The closed diagnostic that reports what the turn changed. `None`
    /// disables diff-size and zero-diff inspection.
    pub diff_diagnostic: Option<DiagnosticInvocation>,
    /// Legacy diff-size telemetry threshold. It does not gate deterministic
    /// authority: an oracle flip verifies regardless of diff size.
    pub diff_budget_lines: u32,
    /// Legacy diagnostics-audit setting retained for configuration decoding.
    /// Diagnostics no longer override an oracle flip or invoke a reviewer.
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
    /// Legacy coverage-audit setting retained for configuration decoding.
    /// Coverage no longer overrides or substitutes for the fail-to-pass oracle.
    pub require_diff_coverage: bool,
    /// Legacy model-verifier setting retained for configuration decoding. It
    /// has no effect because no model verdict can request corroboration.
    pub verifier_evidence_demand: bool,
    /// Legacy independence setting retained for configuration decoding. Model
    /// witness authors are retired, so this flag no longer refuses a run.
    pub require_independent_witness: bool,
    /// Legacy independence setting retained for configuration decoding. Model
    /// verdicts are retired, so this flag no longer refuses a run.
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
            roster: Roster::default(),
            triage_latency_ceiling: Duration::from_secs(30),
            // Sized from the recall port's own round trip, NOT as a fraction
            // of the triage ceiling above (it was written as "half of it",
            // which stopped being true when triage's moved). Recall runs
            // concurrently with triage, so this can never extend the critical
            // path; it only stops recall from becoming it. A remote CGP
            // embedding round trip is 100-500ms and the local path is
            // single-digit ms, so this is an order of magnitude above the
            // realistic worst case.
            recall_latency_ceiling: Duration::from_secs(5),
            // Wider than triage's because a research child is a bounded
            // multi-step read (up to RESEARCH_MAX_STEPS tool round-trips),
            // not one completion — but still a hard wall: research past it
            // degrades to a missing finding, never a wedged turn.
            research_latency_ceiling: Duration::from_secs(45),
            scope_thresholds: crate::scope::ScopeThresholds::default(),
            headless: false,
            headless_bypass_scope_review: false,
            plan_mode: false,
            test_command: None,
            keep_witness: false,
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
/// ran. `deterministic` is true only for a fail-to-pass oracle receipt or a
/// completed candidate test failure; false denotes an unverified abstention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub passed: bool,
    pub deterministic: bool,
    pub summary: String,
    /// The ladder input snapshot this verdict was decided from (#865), when
    /// verification ran far enough to take one. `replay` answers "why did
    /// this run pass / revise / abstain?" from here without
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
    /// Latest policy-final built-in `verify_done` request confirmed during
    /// execution. It is never authority by itself: every verification round
    /// replays it against the current sealed candidate state.
    verify_done_request: Option<serde_json::Value>,
    /// Ends the execute turn as soon as the tracked test goes fail→pass.
    ///
    /// `None` while there is no failing configured-command baseline to watch.
    /// See [`crate::flip_halt`] for why stopping is separate from crediting.
    flip_halt: Option<Arc<FlipHalt>>,
    oracle: FlipOracle,
    /// The oracle's observations in the order they were made (#864) —
    /// configured-command baseline, per-iteration candidate runs, and the
    /// pre-submit confirmation.
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
    revisions: u32,
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
        }
    }
}

/// The working-tree surface one candidate executes and verifies against: the
/// session ports (the real tree) on single-shot and shared-tree runs, an
/// isolated snapshot's ports under best-of-N isolation. Grouped and `Copy`
/// so the candidate phases thread one value instead of two borrows.
#[derive(Clone, Copy)]
struct CandidateSurface<'c> {
    tools: &'c dyn stella_core::ToolExecutor,
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
    /// Boundary pause gate ([`Pipeline::with_turn_gate`]): attached to every
    /// engine this pipeline builds and consulted before every management
    /// call, so a paused pipeline-driven worker parks instead of spending.
    turn_gate: Option<&'a dyn stella_core::ports::TurnGate>,
    /// Caller-owned token-drift model ([`Pipeline::with_calibration`]), lent to
    /// every engine this pipeline builds. `None` leaves estimation uncorrected.
    calibration: Option<&'a CalibrationMap>,
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
    /// Measured per-role wall clock — the anticipatory rung's basis (#2432).
    role_pace: role_pace::RolePace,
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
    /// Where resume-relevant progress facts go as stages settle (#1671), or
    /// `None` for a host with no durable frame to update. Owned (`Arc`, not
    /// `&'a`) because the sink is built beside the pipeline by
    /// `resume_frame::pipeline`, after the ports struct is already assembled.
    frame_sink: Option<Arc<dyn crate::ports::ResumeFrameSink>>,
    /// The accumulated progress record behind [`Pipeline::record_progress`] —
    /// kept whole so every push to the sink carries every fact so far, never
    /// a delta the reader would have to merge.
    progress: Mutex<FrameProgress>,
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
            turn_gate: None,
            calibration: None,
            events: events.into(),
            config,
            configured_test,
            raw_call_seq: AtomicU64::new(RECEIPT_SEQ_ALLOCATED_BASE),
            role_pace: role_pace::RolePace::default(),
            shared_event_lane: AtomicBool::new(false),
            started: std::time::Instant::now(),
            frame_sink: None,
            progress: Mutex::new(FrameProgress::default()),
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
        // Before triage, before recall, before a single paid call: a roster
        // that tries to restore retired model-verification authority is
        // refused here. The legacy independence switches are intentionally
        // inert because no verifier model remains to compare with the worker.
        if let Err(error) = self.roster_refusal() {
            return Err(PipelineRunError::new(error, total_cost));
        }
        self.report_roster_posture();
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
        let (triaged, mut recalled) =
            tokio::join!(self.triage(goal, budget, &mut total_cost), recall_future);
        // Bounded at the source (#616), so every consumer — the user message,
        // the planner prompt, the witness prompt — inherits one budget, and
        // the ContextRecall event reports the frames the turn actually pays
        // for. A mis-tuned recall port must not silently inflate every
        // subsequent turn (N candidates × every revision) past the window.
        let frames = bound_recalled_frames(std::mem::take(&mut recalled.frames));
        self.emit_context_recall(&frames, &recalled);
        let (assessment, research_questions) = match triaged {
            Ok(triaged) => triaged,
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
        // The resume frame's first facts (#1671): a kill after this point can
        // restore the class — and the goal the verifier judges against —
        // without re-triaging.
        self.record_progress(|p| {
            p.task_class = Some(task_class);
            p.goal = Some(goal.to_string());
        });
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
        let contract = match verified_by {
            Some(command) => VerificationContract::Oracle(command),
            None if !assessment.conversational && task_class.verifies_unconditionally() => {
                VerificationContract::WorkerTestFirst
            }
            None => VerificationContract::None,
        };
        // --- 2b. Pre-plan research (#1778): triage named questions, so
        // parallel read-only sub-agents answer them before anything is
        // prompted. Empty questions skip the stage byte-for-byte (L-E2),
        // which is what lets it sit ahead of the conversational branch below:
        // triage never names questions for a chat turn, so a greeting reaches
        // `research_stage` and returns from its first line with no events and
        // no spend.
        //
        // It runs BEFORE the user message is assembled because the worker's
        // message is now one of its sinks (#2415) — findings used to reach
        // the planner alone, so a fact a read-only sub-agent verified against
        // this workspace survived to the worker only as whatever residue of
        // it the planner encoded into a step string.
        let research = self
            .research_stage(goal, &research_questions, budget, &mut total_cost)
            .await;
        messages.push(CompletionMessage::user(assemble_user_message(
            goal, &frames, &research, contract,
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
        // A withheld `plan` takes the branch a non-planning class takes — no
        // frame, no call ([`Pipeline::responsibility_enabled`], #2381).
        let plan: Option<Vec<PlanStep>> =
            if task_class.plans() && self.responsibility_enabled(ModelCallRole::Plan) {
                match self
                    .plan_with_review(goal, &frames, &research, budget, &mut total_cost)
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
        if let Some(steps) = &plan {
            // The frame needs the whole plan, not the transcript's echo of it:
            // step prompts are pushed one turn at a time, so at a mid-plan
            // kill the unreached steps exist nowhere else (#1671).
            self.record_progress(|p| p.plan = Some(steps.clone()));
        }

        // --- 5. Witness + execute + verify (single-shot or best-of-N). ------
        let n = self.config.candidate_count();
        let base_messages = messages.clone();
        // The one frame every candidate stage below reads (#1809). Built here
        // because this is where its last field settles: nothing after this
        // point changes the goal, the staged prefix, the plan, or the class.
        let frame = TaskFrame {
            base_messages: &base_messages,
            plan: plan.as_deref(),
            assessment,
        };
        // A single candidate uses a disposable worktree only when the
        // operator explicitly asks for isolation. Best-of-N always isolates.
        let isolate = if n == 1 {
            self.isolate_in_worktree(task_class).await
        } else {
            false
        };
        // Single-shot (the default) runs directly over the session ports.
        // Best-of-N runs every candidate in an isolated snapshot of the
        // current tree state and adopts only the winner's changes (L-E7).
        let (best, worker_model_label, candidates_run) = if n == 1 && !isolate {
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

        // Adopt the winning candidate's trajectory, then settle — the §6
        // Complete/abort projection is shared with the resumed path
        // (`resume_stage`), so the two cannot drift.
        let mut best = best;
        *messages = std::mem::take(&mut best.messages);
        Ok(self.settle_outcome(
            best,
            task_class,
            total_cost,
            worker_model_label,
            candidates_run,
        ))
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

        // Deliberately NOT `role_overrides.worker`, unlike the plan stage
        // (#2416). Both halves of that row would undo what this path exists to
        // do. `agents.worker.prompt` is the operator's *engineering* persona —
        // it replaces the base instruction set on worker turns
        // (`build_pipeline_system_prompt`), and prepending it here would re-arm
        // exactly the behaviour `CONVERSATIONAL_SYSTEM_PROMPT` suppresses two
        // lines above ("no tools, no code, no plan, no test") on a turn that
        // has no task. And the worker's `effort` would displace the
        // `ReasoningEffort::Low` this role is pinned to in `management_bounds`,
        // buying deliberation for a greeting.
        //
        // The tuning this path does want — temperature, params — still applies:
        // `metered_raw_call` falls back to `config.engine`, which is already
        // built from the worker's own settings.
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
            // The conversational path IS the whole run — there is no executed
            // work either ceiling could settle with, so both stop the same way.
            Err(RawCallError::Budget(abort) | RawCallError::Deadline(abort)) => {
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
    /// for a turn's first plan. `spend` bundles budget + total as downstream
    /// does: #1778's `research` param took the pair one over clippy's cap.
    async fn plan_stage(
        &self,
        goal: &str,
        recall: &[RecalledFrame],
        research: &[ResearchFinding],
        repo_structure: &str,
        revision: Option<&str>,
        spend: &mut Spend<'_>,
    ) -> Result<Vec<PlanStep>, PipelineStageAbort> {
        self.emit(AgentEvent::Stage {
            name: StageKind::Plan,
        });
        let fallback_plan = || vec![PlanStep::new(goal)];

        let resolved = match self.assigned(ModelCallRole::Plan) {
            Assigned::To(r) => r,
            Assigned::Withheld | Assigned::Unresolvable => return Ok(fallback_plan()),
        };
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }

        let prompt = build_planner_prompt(goal, recall, research, repo_structure, revision);
        // The planner's own row, which the caller resolves as `agents.plan`
        // over `agents.worker` field by field — so plan still rides the
        // worker's settings whenever nobody has said otherwise (#2416), and
        // an operator who *has* said otherwise is obeyed (#2374). The
        // non-prompt knobs also arrive via `config.engine` (built from the
        // worker's tuning); `prompt` has no seat there and reaches the planner
        // only here, prepended as a system message ahead of
        // `PLANNER_INSTRUCTIONS` so the JSON-array contract `parse_plan` reads
        // is never replaced.
        let plan_overrides = &self.config.role_overrides.plan;
        let result = match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::Plan,
                    resolved: &resolved,
                    messages: prompt.into_messages(),
                    policy: RetryPolicy::standard(),
                    overrides: plan_overrides,
                    timeout: self.config.engine.model_timeout,
                },
                spend.budget,
                spend.total,
            )
            .await
        {
            Ok(r) => r,
            // Still before execute: a fallback plan here would only buy the
            // worker turns the run has no clock left to run.
            Err(RawCallError::Budget(abort) | RawCallError::Deadline(abort)) => return Err(abort),
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
                    messages: plan_repair_prompt(&result.text).into_messages(),
                    policy: RetryPolicy::deterministic(),
                    overrides: plan_overrides,
                    timeout: self.config.engine.model_timeout,
                },
                spend.budget,
                spend.total,
            )
            .await
        {
            Ok(repair) => {
                if let Some(steps) = parse_plan(&repair.text) {
                    return Ok(steps);
                }
            }
            Err(RawCallError::Budget(abort) | RawCallError::Deadline(abort)) => return Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => {}
        }

        // Degrade to a single-step plan rather than failing — a planner that
        // won't produce a parseable plan must still let the work proceed.
        Ok(fallback_plan())
    }

    // Stage: execute + verify — candidate generation and selection

    /// Last-resort execution when candidate setup failed before the worker
    /// ever ran. Runs exactly one worker turn on the session tree — no
    /// isolation, the simplest path that still does the work. Returns `None`
    /// only when there is no resolvable worker provider
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
            tools: self.tools,
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
            engine = self.attach(engine);
            let view = fan.as_ref().map(|fan| fan.candidate());
            if let Some(view) = view.as_ref() {
                engine = engine.with_steering(view);
            }
            // Authoring is `None`: a shared-tree run has no workspace to graft
            // into and no pristine snapshot to author blind in, so it never
            // buys a witness. The ordinal is 1-based, like the start notice.
            results.push(
                self.run_candidate(i + 1, frame, &engine, surface, spend)
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
        spend: &mut Spend<'_>,
    ) -> Result<(CandidateResult, Option<String>, u32), PipelineError> {
        // Orchestrator pre-fetch (issue #248) — see `crate::mcp_prefetch::fold`.
        let prefetched = crate::mcp_prefetch::fold(self.mcp_prefetch, n, frame.base_messages).await;
        let frame = TaskFrame {
            base_messages: prefetched.as_deref().unwrap_or(frame.base_messages),
            ..frame
        };
        let Some(port) = self.candidate_workspaces else {
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

        let worker = self
            .resolve_provider(Role::Worker)
            .map_err(RoleResolveError::into_pipeline_error)?;
        if let Some(fallback) = &worker.fallback {
            self.emit_fallback(fallback);
        }
        let worker_label = worker.model_ref.to_string();
        // Reaching the isolated path with one candidate means the operator
        // requested worktree isolation; larger runs are ordinary best-of-N.
        let single_shot_isolation = n == 1;

        // Isolation is created in index order even when candidates run
        // together. See `fanout_stage` for why git creation is serialized.
        let serialized = SerialCreates::new(port);
        let port: &dyn CandidateWorkspacePort = &serialized;
        let width = fan_out_width(n, self.config.candidate_concurrency);
        self.emit_text(candidate_fanout_notice(n, width));
        // Index-aligned with the results below, so adoption can pair a
        // workspace with the result that produced it (and with the witness
        // paths that result carries). `Err` marks a candidate that never got a
        // workspace, and carries the reason it will be scored with.
        let workspaces = self.create_candidate_workspaces(port, n, frame.plan).await;
        let candidates = self
            .dispatch_isolated_candidates(frame, &worker, &workspaces, width, spend)
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
                match ws.adopt(&[]).await {
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
        _candidate: u32,
        frame: TaskFrame<'_>,
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
            && let Some(cmd) = self.effective_test_command()
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
                // The baseline is an observation of the pre-execution tree,
                // which no post-crash process can ever repeat — record it so
                // a resumed run's oracle keeps its `Failing` precondition
                // instead of forfeiting the flip (#1671).
                self.record_progress(|p| {
                    p.baseline = Some(RecordedBaseline {
                        command: normalize_command(cmd.command),
                        passed,
                        output_tail: output.clone(),
                    });
                });
            }
        }

        // Snapshot untracked files (with content fingerprints) BEFORE
        // executing so `gather_diff` can tell files this turn created OR
        // modified from pre-existing dirty state — a stale untracked file with
        // an unchanged fingerprint is not this turn's work, but one the turn
        // edited (fingerprint changed) is.
        let untracked_before = surface.repo_status.untracked_fingerprints().await;
        // Execute is beginning: stamp the facts a resume-into-execute needs
        // and cannot re-derive (#1671). `isolated` decides restorability — a
        // candidate worktree dies with the process, so a resume of an
        // isolated run must decline rather than write into the wrong tree.
        self.record_progress(|p| {
            p.executing = true;
            p.isolated = surface.workspace.is_some();
            p.untracked_before = untracked_before.clone();
        });

        let mut state = CandidateState {
            messages: candidate_narration::messages_rooted_at(
                frame.base_messages,
                surface.workspace,
            ),
            final_text: String::new(),
            signals: ChangeSignals::default(),
            verify_done_request: None,
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
            revisions: 0,
            failures: Vec::new(),
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

        // Publish the warrant observation from the one place every verifying
        // candidate reaches. It remains useful telemetry, but grants no
        // completion authority.
        let warrant = warrant(&state.diff_text, state.signals);
        self.emit_proof(ProofStep::Warrant {
            required: warrant.is_required(),
            reason: warrant.reason().map(|r| r.sentence().to_string()),
            diff_lines: state.diff_lines,
        });

        self.verify_candidate(frame, engine, surface, spend, state)
            .await
    }

    /// Verify + bounded revise loop over an executed candidate: observe the
    /// tests, take the deterministic ladder decision (L-E11), and either
    /// finish verified, abstain, or spend one of `max_revisions` on a revise
    /// pass after a completed test failure. Owns `state` because
    /// every exit moves it into the returned [`CandidateResult`].
    async fn verify_candidate(
        &self,
        _frame: TaskFrame<'_>,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        spend: &mut Spend<'_>,
        mut state: CandidateState,
    ) -> CandidateResult {
        self.emit(AgentEvent::Stage {
            name: StageKind::Verify,
        });
        let effective_cmd = self.effective_test_command();
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
            let observed_test = self
                .observe_touched_tests(surface, effective_cmd, &mut state)
                .await;
            let mut observed_test = observed_test;
            let mut failure_command = effective_cmd.map(|cmd| cmd.command.to_string());
            let mut verify_done_flip = false;

            // Only the concrete built-in executor can deposit one of these
            // policy-final requests. Replay it now, after sealing, so a
            // confirmation before a later mutation or revision can never
            // verify stale bytes. The latest request persists across revision
            // rounds and is replayed against every new final state.
            if effective_cmd.is_none() {
                if let Some(latest) = surface.tools.drain_verification_requests().pop() {
                    state.verify_done_request = Some(latest);
                }
                if let Some(request) = state.verify_done_request.as_ref()
                    && let Some(result) = surface.tools.replay_verification_request(request).await
                {
                    match result {
                        stella_core::VerificationOracleResult::Confirmed { .. } => {
                            verify_done_flip = true;
                        }
                        stella_core::VerificationOracleResult::CandidateFailed {
                            command,
                            exit_code,
                            stdout,
                            stderr,
                        } => {
                            failure_command = Some(command);
                            observed_test = ObservedTest {
                                assertion: Some(false),
                                exit_code: Some(exit_code),
                                stdout_tail: stdout,
                                stderr_tail: stderr,
                                infra: None,
                            };
                        }
                        stella_core::VerificationOracleResult::Unverifiable { .. } => {}
                    }
                }
            }
            let touched_tests_passed = observed_test.assertion;
            let test_tail = observed_test.combined_output();
            let test_infra = observed_test.infra;
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
            let mut inputs = self.ladder_inputs(
                &state,
                touched_tests_passed,
                verify_done_flip,
                effective_cmd.is_none() && state.verify_done_request.is_none(),
            );

            // Pre-submit audit (#859): a deterministic pass is about to be
            // credited, so spend the one cheap check that can refute it —
            // gated on the DECISION rather than on the flip transition, so a
            // turn already headed for an abstention pays nothing.
            //
            // Re-run the tracked command once more against the same sealed
            // tree. A pass that does not reproduce moves the oracle to
            // `Unstable`, which `is_flipped` does not credit, so the decision
            // re-derived below withholds the credit instead of shipping a
            // flake. This is the guard that a single lucky pass on a test that
            // failed the baseline for an unrelated reason would otherwise walk
            // straight through.
            if matches!(ladder_decision(&inputs), LadderDecision::SubmitFast)
                && let Some(cmd) = effective_cmd
            {
                // Through the retrying runner (#1294) for a reason worth
                // stating: an OOM'd confirmation demotes the oracle, which is a
                // real cost paid for a run that observed nothing. Retrying
                // first is what keeps a memory kill from silently withdrawing a
                // deterministic pass the candidate had earned.
                let confirmation = self.run_test_observed(surface.tests, cmd.invocation).await;
                if let Some(passed) = confirmation.assertion_result() {
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
                inputs.flip_achieved = state.oracle.is_flipped();
            }

            // The verdict's provenance (#865): the ladder inputs frozen at
            // decision time, attached to every evidence value emitted below.
            // `witness_intact` states that the tamper exclusion above RAN
            // and passed — a tampered witness never reaches a verdict.
            let snapshot = Self::ladder_snapshot(&inputs, &state, test_infra, None);

            // Everything the verification side knows about this round's
            // failure. Both failing arms disclose from it, and nothing reaches
            // the worker except through `Pipeline::airlock_forward` or a
            // `redact` of this value.
            let sealed = SealedFailure {
                command: failure_command.as_deref().unwrap_or(""),
                invocation: effective_cmd.map(|cmd| cmd.invocation),
                output: &test_tail,
                witness_paths: &[],
            };

            match ladder_decision(&inputs) {
                LadderDecision::NothingAttempted => {
                    // The turn ended without dispatching one call that could
                    // write anything. Unlike the abstention below, no probe
                    // failed here and no evidence is missing — the pipeline is
                    // reporting its own dispatch record — so this is a plain
                    // `passed: false`.
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
                    return state.into_verified(
                        false,
                        &evidence,
                        score_from_verification(false, Some(false)),
                    );
                }
                LadderDecision::Unverified => {
                    // Observed, and unproven. Terminal: nothing deterministic
                    // settled this turn and no model is asked, because the
                    // evidence that reaches here is by construction the
                    // evidence no oracle could settle.
                    //
                    // `passed: true` for the same reason the abstention arm
                    // uses it — a run is not failed by the absence of a way to
                    // check it — and what keeps it from reading as a pass is
                    // the pair beside it: the summary says UNVERIFIED in its
                    // first word and the score is `Unverified`, so this
                    // candidate can never tie a genuinely verified sibling in
                    // best-of-N and then win the smaller-diff tiebreak.
                    let mut evidence = unverified_evidence(&inputs, state.oracle.tracked_command());
                    evidence.ladder = Some(Box::new(snapshot.clone()));
                    self.unproven_verdict(&evidence.summary);
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
                LadderDecision::SubmitFast => {
                    // Deterministic pass: completion authority is the oracle
                    // receipt itself; no model participates (L-E11).
                    let mut evidence = deterministic_pass_evidence(
                        state.oracle.tracked_command(),
                        inputs.verify_done_flip,
                        state.diff_lines,
                        inputs.diff_coverage,
                    );
                    evidence.ladder = Some(Box::new(snapshot.clone()));
                    self.emit(AgentEvent::Verdict {
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
                    let (mut evidence, _brief) =
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
                    let (Some(command), Some(exit_code)) =
                        (failure_command.as_deref(), observed_test.exit_code)
                    else {
                        // `Revise` is reachable only from a completed failed
                        // test. Keep the boundary total if a future caller
                        // violates that invariant: abstain rather than invent
                        // feedback or panic on runtime data.
                        let mut abstained = unverifiable_evidence(&inputs);
                        abstained.ladder = Some(Box::new(snapshot.clone()));
                        self.unverifiable(&abstained.summary);
                        return state.into_verified(
                            true,
                            &abstained,
                            score_from_verification(false, None),
                        );
                    };
                    let cause = RevisionCause::TestFailure(TestFailureReceipt {
                        command,
                        exit_code,
                        stdout: &observed_test.stdout_tail,
                        stderr: &observed_test.stderr_tail,
                    });
                    if let Err(abort) = self
                        .revise_candidate(engine, surface, cause, spend, &mut state)
                        .await
                    {
                        return CandidateResult::turn_aborted(state.messages, abort);
                    }
                }
            }
        }
    }

    fn effective_test_command(&self) -> Option<EffectiveTestCommand<'_>> {
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
        None
    }

    /// Spend one revision: run [`Pipeline::revise_turn`] with the failure
    /// evidence and fold the fresh diff back into `state`. `Err` is the typed
    /// abort of a turn that died mid-revision (budget/loop).
    async fn revise_candidate(
        &self,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        cause: RevisionCause<'_>,
        spend: &mut Spend<'_>,
        state: &mut CandidateState,
    ) -> Result<(), TurnAbort> {
        let probe = self
            .revise_turn(engine, surface, cause, spend, state)
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
        cause: RevisionCause<'_>,
        spend: &mut Spend<'_>,
        state: &mut CandidateState,
    ) -> Result<DiffProbe, TurnAbort> {
        state
            .messages
            .push(CompletionMessage::user(revision_prompt(cause)));
        self.emit(AgentEvent::Stage {
            name: StageKind::Execute,
        });
        match self
            .run_engine_turn(
                engine,
                &mut state.messages,
                spend.budget,
                &mut state.signals,
                crate::flip_halt::for_revision(&state.flip_halt, state.oracle.state()),
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
        })
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
