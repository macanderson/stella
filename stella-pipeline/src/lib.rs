// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-pipeline` — the orchestration plane that sits *above*
//! `stella-core::Engine`. It drives one prompt
//! through the staged turn flow — **evaluate → enhance → route → execute →
//! witness → verify → judge → revise** (the witness is authored on demand,
//! after execution, once the warrant has read the diff) — over injected
//! ports, emitting an `AgentEvent` at every stage boundary.
//!
//! # What lives here vs. the engine
//!
//! `stella-core::Engine::run_turn` drives *one* model-call/tool loop. This
//! crate composes turns into a governed pipeline: it classifies the prompt
//! (triage), recalls context, plans multi-step work, gates large plans behind
//! interactive scope review, executes each step through the engine, and
//! verifies the result with a deterministic-first ladder before ever spending
//! a model-judge call.
//!
//! # The design lessons this crate encodes
//!
//! - **L-E2** — triage fast paths: simple lookups skip planner + judge (with a
//!   self-revoking zero-diff guard); single-task goals skip DAG planning.
//!   [`triage`], and the fast-path wiring in [`pipeline`].
//! - **L-E5** — the scope-review gate. [`scope`].
//! - **L-E6** — the planner's split context (goal + recall + structure, never
//!   the transcript). [`plan::build_planner_prompt`].
//! - **L-E7** — single-shot default; best-of-N opt-in. [`candidate`].
//! - **L-E8** — recall rides as a volatile message after the stable system
//!   prefix (cache discipline). [`pipeline`].
//! - **L-E11** — the deterministic verification ladder: the flip-oracle state
//!   machine, the evidence ladder that skips the judge on strong evidence, and
//!   a model judge (judge ≠ worker) only on inconclusive evidence with a
//!   heuristic fallback. [`verify`]. Its front half is **witness authoring**:
//!   when the user armed no `--test-command`, an independent model (the
//!   judge's resolution) writes the failing witness test that the flip oracle
//!   tracks — visible to the worker, integrity-checked by tamper exclusion,
//!   never hidden. [`witness`].
//! - **The feedback airlock** — the one channel from verification back to the
//!   worker. A deterministic failure no longer replays the raw test-runner
//!   output into the revision prompt: it is disclosed at a grain (`L0`–`L3`)
//!   that tightens when the same failure repeats, and every model-authored
//!   text crossing inbound (distress guidance, judge reasoning) is scrubbed
//!   against the sealed material first. The operator still sees the real
//!   output; only the worker's prompt is redacted.
//!   [`witness::airlock`]. Design: `docs/design/witness-protocol.md` §4.
//! - **Proportionate verification** — escalate on evidence, never on a
//!   prediction made before any work exists. Deterministic routes resolve
//!   before the paid triage call rather than after it (a greeting no longer
//!   buys a classification that cannot change its own answer), and
//!   [`witness::warrant`] reads the *diff* to decide whether a change needed a
//!   test at all — recording a stated reason when it did not, the pipeline's
//!   half of the contract contributors are held to. Fails closed: anything
//!   mixed or unreadable buys the test. Design:
//!   `docs/design/witness-protocol.md` §7.
//! - **L-M4** — triage runs with `max_retries = 0` under a latency ceiling.
//!   [`pipeline::Pipeline::run`].
//! - **Distress guidance** — on the second consecutive deterministic
//!   verification failure, one judge call steers the next revision
//!   (event-triggered course-correction, never a fixed mid-run checkpoint).
//!   [`verify::guidance_prompt`], wired in [`pipeline`].
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
//! subsystem is wired; the two *optional* ones — candidate isolation and MCP
//! pre-fetch — are `Option` fields on [`PipelinePorts`] instead, because
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
pub(crate) mod candidate_narration;
pub(crate) mod candidate_steering;
pub(crate) mod mcp_prefetch;
pub mod pipeline;
pub mod plan;
pub mod ports;
pub mod replay;
pub mod scope;
pub mod triage;
pub mod verify;
pub mod witness;

pub use pipeline::{
    Pipeline, PipelineConfig, PipelineError, PipelineOutcome, PipelineRoleOverrides,
    PipelineRunError, PipelineStatus, RoleCallOverrides, Verdict,
};
pub use ports::{
    AdoptedChange, AlwaysAbortGate, ApprovalGate, ArtifactIdentity, ArtifactKind,
    CandidateWorkspace, CandidateWorkspacePort, CmdKind, CmdOutcome, ContextRecallPort,
    DiagnosticInvocation, DiagnosticRunner, FileTouchPort, LintProbe, LintRecord, McpPrefetchPort,
    NoContextRecall, NoFileTouches, NoRepoStatus, NoRepoStructure, PipelinePorts, ProviderResolver,
    Recall, RecalledFrame, RepoStatusPort, RepoStructurePort, ScopeDecision, StdioApprovalGate,
    TestInvocation, TestRunner, WorkspaceError,
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
