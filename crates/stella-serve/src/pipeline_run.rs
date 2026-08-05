//! The verification pipeline over the wire (#1288): `pipeline` on
//! `POST /v1/turns` (and `POST /v1/sessions/{id}/turns`) drives a turn through
//! `stella_pipeline::Pipeline::run` — plan, scope review, execute, witness,
//! verify, verifier — instead of a bare engine step loop.
//!
//! # Why a mode on a turn, not a `/v1/runs` resource
//!
//! Everything a pipeline run needs from the transport already exists on a
//! turn and is already witnessed, exactly as `goal` (#1297) found before it:
//! `GET /v1/turns/{id}/events` streams its progress (the pipeline already
//! emits `AgentEvent::Stage`, `AgentEvent::ScopeReview`, `AgentEvent::
//! Verdict`, and the rest of its own vocabulary — this module adds no
//! new event types), `POST /v1/turns/{id}/cancel` stops it, and the
//! settlement hook writes the transcript back. A `/v1/runs` resource would
//! restate every one of those with its own id space and its own bugs — the
//! same argument `crate::goal`'s module docs make, word for word. The ONE
//! genuinely new interaction a pipeline run introduces is the scope-review
//! gate, which is why that (and only that) gets a new wire primitive:
//! [`crate::frame::ServerFrame::ScopeReviewRequest`] and
//! `POST /v1/turns/{id}/approve` — see `crate::remote::RemoteApprovalGate`.
//!
//! # The three questions #1288 asked, answered
//!
//! 1. **Verification can take a long time.** Same answer as every other
//!    turn: the caller subscribes to the SSE stream (resumable via
//!    `?after=`), and `POST /v1/turns/{id}/cancel` stops the run at its next
//!    reverse-RPC boundary. No new streaming model — the pipeline already
//!    reports its stage transitions as ordinary events.
//! 2. **The approval step needs a human.** The human is whoever the host's
//!    bearer token belongs to — the same party that already answers
//!    `tool-result` / `provider-result`. `POST /v1/turns/{id}/approve`
//!    resolves it, fail-closed ([`ScopeDecision::Abort`]) on cancellation,
//!    disconnection, or the same reverse-request deadline every other call
//!    obeys. A caller that wants to run unattended sets `auto_approve` and
//!    gets a local gate that says yes without a round trip — an explicit
//!    request, never a silent default.
//! 3. **Verification runs commands from the repository.** Nothing runs on
//!    this server. `TestRunner`/`DiagnosticRunner` — the pipeline's only
//!    process-launching ports — are remoted through the exact same
//!    `ToolRequest`/`ToolResultIn` frames every tool call already crosses
//!    (`crate::remote::RemoteVerificationRunner`), under two reserved names
//!    the host recognizes. This is not a new containment decision: it is
//!    `stella-parity`'s existing `tools.local_execution: NotApplicable`
//!    posture ("the sidecar never executes tools itself") applied to two
//!    more typed callers. A host that has not implemented the two reserved
//!    tool names answers as it would any unknown tool, which the ladder
//!    already reads as "no toolchain" — a first-class, honestly-degrading
//!    outcome, not a crash.
//!
//! # What is deliberately not wired yet
//!
//! Declared, not silent — the same discipline `stella-parity` enforces on
//! every other capability row:
//!
//! - **Context recall, repo structure, lint, mutation, coverage, candidate
//!   isolation** ([`NoContextRecall`], [`NoRepoStructure`], [`NoRepoStatus`],
//!   [`NoFileTouches`], and `lint`/`mutation`/`coverage`/
//!   `candidate_workspaces` left `None`). Every one of these ports is
//!   designed to degrade open when absent (their own doc comments say so:
//!   "a caller with nothing to offer supplies `NoContextRecall`"), so a
//!   served run still verifies — it just cannot isolate best-of-N candidates
//!   or measure diff coverage yet. Wiring them is additive follow-up work,
//!   not a redesign.
//! - **Per-role provider routing.** Worker, triage, and verifier each get their
//!   own [`crate::remote::RemoteProvider`] (so the host can route a verifier to
//!   a different model, mirroring `goal.verifier_provider_id`), but a served
//!   run's `witness_writer` role rides the worker's — an independent witness
//!   author needs its own provider id the same way verifier does, and is a
//!   small, additive follow-up.
//! - **Mid-run steering and sub-agents.** `goal`/`sub_agents` blocks are not
//!   accepted together with `pipeline` — the pipeline already loops
//!   internally (revisions, best-of-N) and drives its own verifier, so layering
//!   the turn-level verifier/delegation modes on top would be two competing
//!   control loops. Refused with a named 400, not silently ignored.
//! - **Cancellation is reverse-RPC-boundary, not step-boundary.** Exactly the
//!   limitation `docs/design/serve-surface.md` already states for the plain
//!   engine driver ("does not interrupt CPU-bound work between reverse
//!   requests") — a pipeline run has no CPU-bound stretch longer than the
//!   engine's own, so this is the same bound, not a new one.

use std::time::Duration;

use async_trait::async_trait;
use stella_core::router::{CircuitBreaker, ProviderProfile};
use stella_core::{BudgetGuard, EngineConfig, RoleTable, Router, TurnOutcome};
use stella_pipeline::ports::{
    ApprovalGate, NoContextRecall, NoFileTouches, NoRepoStatus, NoRepoStructure, PipelinePorts,
    ProviderResolver, ScopeDecision,
};
use stella_pipeline::{Pipeline, PipelineConfig};
use stella_protocol::{AgentEvent, CompletionMessage, ModelRef, Provider, ScopeProposal};
use tokio::sync::mpsc;

use crate::pending::Pending;
use crate::remote::{RemoteApprovalGate, RemoteProvider, RemoteVerificationRunner, TokioSleeper};
use crate::session::DrivenTurn;

/// A pipeline-driven turn, as the host asked for it — the `pipeline` block on
/// `POST /v1/turns`.
pub struct PipelineRun {
    /// The task, exactly as `stella run "<goal>"` takes it on the CLI. Not the
    /// turn's `messages`: those carry only the stable system prefix (if any),
    /// and the pipeline appends its own recall+goal message from this field —
    /// the same split `stella-cli` already keeps.
    pub goal: String,
    /// Skip the scope-review gate entirely rather than asking the host,
    /// using a local gate that approves every plan without a round trip.
    /// `false` — the default — asks the host over `POST
    /// /v1/turns/{id}/approve` for any plan that crosses the operator's
    /// scope thresholds; this is an explicit opt into unattended execution,
    /// never an implicit one.
    pub auto_approve: bool,
    /// The test command the flip oracle tracks. `None` (the default) hands
    /// the flip oracle to the witness author when `witness_writer` is on.
    pub test_command: Option<String>,
    /// Witness authoring (L-E11): an independent model authors a failing
    /// test to arm the flip oracle when no `test_command` is configured.
    /// Defaults to [`PipelineConfig::default`]'s `true`.
    pub witness_writer: bool,
    /// Maximum revision turns per candidate when verification fails.
    /// `None` keeps [`PipelineConfig::default`]'s value.
    pub max_revisions: Option<u32>,
    /// Best-of-N candidate count. `None`/`Some(1)` is single-shot.
    ///
    /// Isolated (worktree-snapshotted) fan-out needs
    /// [`stella_pipeline::ports::CandidateWorkspacePort`], which this served
    /// posture does not wire yet (see the module docs) — a served run with
    /// `candidates > 1` therefore executes candidates into the shared
    /// remoted workspace sequentially, exactly as the pipeline's own
    /// documented degradation says a candidate count with no isolation port
    /// does. Left as the caller's choice rather than silently clamped to 1,
    /// so the behavior is legible from the pipeline's own contract instead
    /// of a serve-specific carve-out.
    pub candidates: Option<u32>,
    /// The provider id the triage call announces, when the caller wants it
    /// on a different account/model than the worker. `None` runs it on the
    /// turn's own provider id.
    pub triage_provider_id: Option<String>,
    /// The provider id the verifier call announces — the same knob
    /// `goal.verifier_provider_id` already exposes, generalized to the
    /// pipeline's own internal verifier.
    pub verifier_provider_id: Option<String>,
}

/// A time source for the single-profile [`CircuitBreaker`] a served pipeline
/// run constructs. The breaker only matters when a role fails over between
/// multiple profiles; a served run has exactly one, so nothing here ever
/// trips it — but `CircuitBreaker::new` still wants a real clock rather than
/// a stub that could misreport `now_ms`.
struct WallClock;

impl stella_core::ports::Clock for WallClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }
}

/// Tags the dummy [`ModelRef`]s a served run's single-provider [`Router`]
/// resolves roles to — never a real model slug, just a key
/// [`ServedProviderResolver`] switches on to pick which remoted provider
/// instance serves a role.
mod role_tag {
    pub(super) const WORKER: &str = "worker";
    pub(super) const TRIAGE: &str = "triage";
    pub(super) const VERIFIER: &str = "verifier";
}

/// Resolves the pipeline's role-tagged [`ModelRef`]s to one of three
/// [`RemoteProvider`]s, each announcing its own `provider_id` and
/// [`stella_protocol::ModelCallRole`] on the wire — so a host can route a
/// served run's verifier to a different model than its worker, the same
/// property `goal.verifier_provider_id` already buys a turn-level verifier.
struct ServedProviderResolver {
    worker: RemoteProvider,
    triage: RemoteProvider,
    verifier: RemoteProvider,
}

impl ProviderResolver for ServedProviderResolver {
    fn provider_for(&self, model: &ModelRef) -> Option<&dyn Provider> {
        match model.model_id.as_str() {
            role_tag::WORKER => Some(&self.worker),
            role_tag::TRIAGE => Some(&self.triage),
            role_tag::VERIFIER => Some(&self.verifier),
            _ => None,
        }
    }
}

/// An [`ApprovalGate`] that approves every plan without a round trip — the
/// explicit `auto_approve` opt-in, never the default. Distinct from
/// [`stella_pipeline::ports::AlwaysAbortGate`], which exists for exactly the
/// opposite posture (a headless run with nobody to ask, conservative by
/// refusing); this one is for a caller who has already decided to run
/// unattended and said so.
struct AutoApproveGate;

#[async_trait]
impl ApprovalGate for AutoApproveGate {
    async fn review(&self, _proposal: &ScopeProposal) -> ScopeDecision {
        ScopeDecision::Approve
    }
}

/// Lower a [`PipelineRun`] plus the turn's own engine config into a full
/// [`PipelineConfig`]. `headless` is always `false`: a served run always has
/// *some* [`ApprovalGate`] behind it — [`AutoApproveGate`] or
/// [`RemoteApprovalGate`] — never [`stella_pipeline::ports::AlwaysAbortGate`],
/// so the CLI's "headless means nobody to ask" branch never applies here.
/// `headless_bypass_scope_review` is therefore inert (only read inside the
/// `headless` arm) and left at its default.
fn pipeline_config(run: &PipelineRun, engine: EngineConfig) -> PipelineConfig {
    let mut config = PipelineConfig {
        engine,
        headless: false,
        test_command: run.test_command.clone(),
        witness_writer: run.witness_writer,
        candidates: run.candidates,
        ..PipelineConfig::default()
    };
    if let Some(max_revisions) = run.max_revisions {
        config.max_revisions = max_revisions;
    }
    config
}

/// Drive one pipeline-driven turn to its outcome (#1288).
///
/// Structurally the pipeline's counterpart to [`crate::session::drive_turn`]
/// / [`crate::goal::drive_goal`]: it builds the pipeline's own port set over
/// the same reverse-RPC primitives every turn already uses, runs
/// [`Pipeline::run`], and maps the result back to the shared [`DrivenTurn`]
/// shape so `run_session`'s three modes (plain turn, goal, pipeline) settle
/// identically.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_pipeline(
    run: &PipelineRun,
    provider_id: String,
    // The turn's already-constructed, already-hook-gated tool executor
    // (`crate::session::run_session`'s `tools`, or its sub-agent-delegating
    // view) — reused rather than rebuilt, so a pipeline-driven turn's
    // execute-stage tool calls go through the same operator
    // `tool.call.requested` policy as an ordinary turn's. The verification
    // ladder's own process launches (`RemoteVerificationRunner`) are
    // deliberately NOT gated the same way: they are the pipeline's own
    // deterministic diagnostics, not model-initiated tool calls, exactly as
    // `verify_done`'s local runner bypasses `ToolRegistry` in the CLI.
    tools: &dyn stella_core::ToolExecutor,
    engine_config: EngineConfig,
    messages: Vec<CompletionMessage>,
    mut budget: BudgetGuard,
    frame_tx: crate::backlog::FrameSink,
    pending: Pending,
    reverse_request_timeout: Duration,
    events: &mpsc::UnboundedSender<AgentEvent>,
    gate: &dyn stella_core::ports::TurnGate,
) -> DrivenTurn {
    let worker_id = provider_id;
    let triage_id = run
        .triage_provider_id
        .clone()
        .unwrap_or_else(|| worker_id.clone());
    let verifier_id = run
        .verifier_provider_id
        .clone()
        .unwrap_or_else(|| worker_id.clone());

    let resolver = ServedProviderResolver {
        worker: RemoteProvider::new(
            worker_id.clone(),
            frame_tx.clone(),
            pending.clone(),
            reverse_request_timeout,
        ),
        triage: RemoteProvider::new(
            triage_id,
            frame_tx.clone(),
            pending.clone(),
            reverse_request_timeout,
        )
        .with_role(stella_protocol::ModelCallRole::Triage),
        verifier: RemoteProvider::new(
            verifier_id,
            frame_tx.clone(),
            pending.clone(),
            reverse_request_timeout,
        )
        .with_role(stella_protocol::ModelCallRole::Verdict),
    };

    let router = Router::new(
        RoleTable::new(),
        vec![ProviderProfile::new(
            worker_id,
            ModelRef::new(role_tag::WORKER, role_tag::WORKER),
            ModelRef::new(role_tag::TRIAGE, role_tag::TRIAGE),
            ModelRef::new(role_tag::VERIFIER, role_tag::VERIFIER),
        )],
        CircuitBreaker::new(Box::new(WallClock)),
    );

    let verify =
        RemoteVerificationRunner::new(frame_tx.clone(), pending.clone(), reverse_request_timeout);
    let auto_approve_gate = AutoApproveGate;
    let remote_approval_gate = RemoteApprovalGate::new(frame_tx, pending, reverse_request_timeout);
    let sleeper = TokioSleeper;

    let ports = PipelinePorts {
        router: &router,
        providers: &resolver,
        tools,
        recall: &NoContextRecall,
        repo: &NoRepoStructure,
        repo_status: &NoRepoStatus,
        touches: &NoFileTouches,
        diagnostics: &verify,
        tests: &verify,
        lint: None,
        mutation: None,
        coverage: None,
        approvals: if run.auto_approve {
            &auto_approve_gate as &dyn ApprovalGate
        } else {
            &remote_approval_gate as &dyn ApprovalGate
        },
        sleeper: &sleeper,
        hooks: None,
        candidate_workspaces: None,
        mcp_prefetch: None,
        steering: None,
    };

    let config = pipeline_config(run, engine_config);
    let pipeline = Pipeline::new(ports, events.clone(), config).with_turn_gate(gate);

    let mut messages = messages;
    let outcome = match pipeline.run(&run.goal, &mut messages, &mut budget).await {
        Ok(outcome) => {
            use stella_pipeline::PipelineStatus;
            match outcome.status {
                PipelineStatus::Completed | PipelineStatus::VerificationFailed { .. } => {
                    TurnOutcome::Completed {
                        text: outcome.final_text,
                        cost_usd: outcome.total_cost_usd,
                    }
                }
                PipelineStatus::Aborted { reason } => TurnOutcome::Aborted {
                    reason,
                    cost_usd: outcome.total_cost_usd,
                },
            }
        }
        Err(err) => TurnOutcome::Aborted {
            reason: err.cause.to_string(),
            cost_usd: err.total_cost_usd,
        },
    };

    DrivenTurn {
        outcome,
        messages,
        budget,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_run() -> PipelineRun {
        PipelineRun {
            goal: "add a widget".to_string(),
            auto_approve: false,
            test_command: None,
            witness_writer: true,
            max_revisions: None,
            candidates: None,
            triage_provider_id: None,
            verifier_provider_id: None,
        }
    }

    /// A served run is never the CLI's "nobody to ask" headless posture — it
    /// always has a real `ApprovalGate` behind it, so the pipeline's own
    /// headless-without-bypass refusal must never fire for one.
    #[test]
    fn a_served_pipeline_run_is_never_headless() {
        let config = pipeline_config(&base_run(), EngineConfig::default());
        assert!(!config.headless);
    }

    /// The caller's knobs land on the right `PipelineConfig` fields, and
    /// anything left unset keeps the pipeline's own default rather than a
    /// serve-specific one.
    #[test]
    fn caller_knobs_lower_onto_pipeline_config_field_by_field() {
        let mut run = base_run();
        run.test_command = Some("cargo test -p widget".to_string());
        run.witness_writer = false;
        run.max_revisions = Some(5);
        run.candidates = Some(3);
        let config = pipeline_config(&run, EngineConfig::default());
        assert_eq!(config.test_command.as_deref(), Some("cargo test -p widget"));
        assert!(!config.witness_writer);
        assert_eq!(config.max_revisions, 5);
        assert_eq!(config.candidates, Some(3));
        // Left unset by `PipelineRun` — the pipeline's own default survives.
        assert_eq!(
            config.diff_budget_lines,
            PipelineConfig::default().diff_budget_lines
        );
    }

    /// `max_revisions: None` must not zero out the pipeline's own default —
    /// only an explicit caller value may move it.
    #[test]
    fn an_unset_max_revisions_keeps_the_pipelines_own_default() {
        let config = pipeline_config(&base_run(), EngineConfig::default());
        assert_eq!(
            config.max_revisions,
            PipelineConfig::default().max_revisions
        );
    }
}
