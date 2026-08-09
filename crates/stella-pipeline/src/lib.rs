// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-pipeline` — the orchestration plane that sits *above*
//! `stella-core::Engine`. It drives one prompt
//! through the staged turn flow — **evaluate → enhance → route → execute →
//! deterministic verify → bounded revise** — over injected ports, emitting an
//! `AgentEvent` at every stage boundary.
//!
//! # What lives here vs. the engine
//!
//! `stella-core::Engine::run_turn` drives *one* model-call/tool loop. This
//! crate composes turns into a governed pipeline: it classifies the prompt
//! (triage), recalls context, plans multi-step work, gates large plans behind
//! interactive scope review, executes each step through the engine, and
//! verifies the result without granting a model completion authority.
//!
//! # The design lessons this crate encodes
//!
//! - **L-E2** — triage fast paths: simple lookups skip planner + verifier (with a
//!   self-revoking zero-diff guard); single-task goals skip DAG planning.
//!   [`triage`], and the fast-path wiring in [`pipeline`].
//! - **L-E5** — the scope-review gate. [`scope`].
//! - **L-E6** — the planner's split context (goal + recall + structure, never
//!   the transcript). [`plan::build_planner_prompt`].
//! - **L-E7** — single-shot default; best-of-N opt-in. [`candidate`].
//! - **L-E8** — recall rides as a volatile message after the stable system
//!   prefix (cache discipline). [`pipeline`].
//! - **L-E11** — deterministic completion authority: either the same normalized
//!   configured command fails on the baseline and passes on the candidate, or
//!   the concrete built-in `verify_done` oracle is replayed against the final
//!   candidate state. Everything else abstains. [`verify`].
//! - **The feedback boundary** — when a candidate test completes unsuccessfully,
//!   the worker receives only a bounded structured receipt containing command,
//!   exit code, stdout, and stderr. No reviewer prose or model-authored test can
//!   enter the revision loop (`pipeline::revision::revision_prompt`).
//! - **Proportionate verification** — escalate on evidence, never on a
//!   prediction made before any work exists. Deterministic routes resolve
//!   before the paid triage call rather than after it (a greeting no longer
//!   buys a classification that cannot change its own answer), and
//!   [`witness::warrant`] reads the *diff* to decide whether a change needed a
//!   test at all — recording a stated reason when it did not, the pipeline's
//!   half of the contract contributors are held to. Fails closed: anything
//!   mixed or unreadable buys the test. Design:
//!   `docs/spec/witness-protocol.md` §7.
//! - **L-M4** — triage runs with `max_retries = 0` under a latency ceiling.
//!   [`pipeline::Pipeline::run`].
//! Historical verifier/witness wire tokens and pure parsers remain readable for
//! stored traces and configuration compatibility, but live orchestration never
//! dispatches those model roles.
//!
//! # The port surface the CLI glue implements
//!
//! The pipeline does no I/O itself; it orchestrates over the traits in
//! [`ports`]: [`ProviderResolver`], [`ContextRecallPort`], [`RepoStructurePort`],
//! [`RepoStatusPort`], [`TestRunner`], [`DiagnosticRunner`], [`ApprovalGate`],
//! [`CandidateWorkspacePort`] (best-of-N candidate isolation), and
//! [`McpPrefetchPort`] — plus `stella-core`'s `Router`, `ToolExecutor`, and
//! `Sleeper`. The `stella-cli` glue supplies the real implementations.
//!
//! The always-present ports each have a no-op default here
//! ([`NoContextRecall`], [`NoRepoStructure`], [`NoRepoStatus`],
//! [`AlwaysAbortGate`]) so the pipeline runs before every
//! subsystem is wired; the *optional* ones — lint, mutation, hooks, candidate
//! isolation, MCP pre-fetch, and steering — are `Option` fields on
//! [`PipelinePorts`] instead, because
//! "unavailable" changes what the run does (it degrades) rather than being a
//! port that answers with nothing.
//!
//! [`ProviderResolver`]: ports::ProviderResolver
//! [`ContextRecallPort`]: ports::ContextRecallPort
//! [`RepoStructurePort`]: ports::RepoStructurePort
//! [`RepoStatusPort`]: ports::RepoStatusPort
//! [`DiagnosticRunner`]: ports::DiagnosticRunner
//! [`TestRunner`]: ports::TestRunner
//! [`ApprovalGate`]: ports::ApprovalGate
//! [`CandidateWorkspacePort`]: ports::CandidateWorkspacePort
//! [`McpPrefetchPort`]: ports::McpPrefetchPort
//! [`NoContextRecall`]: ports::NoContextRecall
//! [`NoRepoStructure`]: ports::NoRepoStructure
//! [`NoRepoStatus`]: ports::NoRepoStatus
//! [`AlwaysAbortGate`]: ports::AlwaysAbortGate
//! [`PipelinePorts`]: ports::PipelinePorts

pub mod candidate;
pub(crate) mod candidate_fanout;
pub(crate) mod candidate_narration;
pub(crate) mod candidate_steering;
pub mod flip_halt;
pub mod management_prompt;
pub(crate) mod mcp_prefetch;
pub mod oom;
pub mod pipeline;
pub mod plan;
pub mod ports;
pub mod replay;
pub mod research;
pub mod reward;
pub mod roster;
pub mod scope;
pub mod triage;
pub mod verify;
pub mod witness;

pub use oom::{ExitFacts, killed_by_oom};
pub use pipeline::{
    FrameProgress, Pipeline, PipelineConfig, PipelineError, PipelineOutcome, PipelineResume,
    PipelineRoleOverrides, PipelineRunError, PipelineStatus, RecordedBaseline, RoleCallOverrides,
    Verdict,
};
pub use ports::{
    AdoptedChange, AlwaysAbortGate, ApprovalGate, ArtifactIdentity, ArtifactKind, AuthoredChange,
    CITE_MEMORY_REQUEST, CandidateWorkspace, CandidateWorkspacePort, CmdKind, CmdOutcome,
    ContextRecallPort, CoverageProbe, DiagnosticInvocation, DiagnosticRunner, FileTouchPort,
    LineMutation, LintProbe, LintRecord, McpPrefetchPort, MutantOutcome, MutationProbe,
    NoContextRecall, NoFileTouches, NoRepoStatus, NoRepoStructure, PipelinePorts, ProviderResolver,
    Recall, RecalledFrame, RepoStatusPort, RepoStructurePort, ResumeFrameSink, ScopeDecision,
    StdioApprovalGate, TestInvocation, TestRunner, WorkspaceError, decision_from_line,
};
pub use reward::{
    DiscardReason, OutcomeWeights, RewardLabel, RewardPolicy, RewardShaping, Settlement,
    TrajectoryCost, WeightError, label,
};
pub use roster::{
    AgentId, Assignment, AssignmentOverride, IndependenceLoss, Roster, RosterError, default_agent,
};
pub use triage::TaskClass;
pub use verify::{FlipOracle, FlipState, LadderDecision, LadderInputs};
pub use witness::airlock::{
    DisclosureGrain, FailureBrief, FailureFingerprint, LeakKind, SealedFailure, SymptomClass,
    grain_for_repeats, redact, scrub,
};
pub use witness::warrant::{NoWitnessReason, WitnessWarrant, warrant};
pub use witness::{
    TestInvocationError, Witness, WitnessArtifactError, parse_test_invocation,
    parse_witness_command, validate_witness_artifact, validate_witness_identity,
    validate_witness_invocation, witness_identity_matches,
};
