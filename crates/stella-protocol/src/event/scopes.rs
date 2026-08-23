// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The small closed-set enums several [`super::AgentEvent`] variants carry as
//! fields, moved out of `event.rs` because that file sits close to the
//! 1500-line ratchet (AGENTS.md § "God files"). A pure move: re-exported from
//! `event` so `crate::event::StageKind` and friends keep resolving, and every
//! doc comment is unchanged.

use serde::{Deserialize, Serialize};

/// Whose stage boundary an [`super::AgentEvent::Stage`] reports (#3398).
///
/// Deliberately **not** `#[serde(default)]`. A default would silently claim
/// one scope for every historical recording, and half of them are the other
/// one — a decode ambiguity that would live in the fixtures forever. A
/// recording written before this field existed decodes through
/// [`super::AgentEvent::Unknown`] instead, which says "I do not know what this is"
/// rather than guessing wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StageScope {
    /// One engine turn's own phases. Several per run when a wrapper drives.
    Turn,
    /// A wrapper's stages over the whole run: triage, plan, witness, verify.
    Run,
}

/// A named point in the turn's data flow — **the boundaries this host emits**,
/// and only those. Exactly one such vocabulary exists in this workspace, never
/// duplicated per-crate (the TS-era `StageKind` duplication this structurally
/// forbids, L-E1).
///
/// This is deliberately no longer the same thing as "every stage a turn can
/// have". [`crate::StageName`] is what [`super::AgentEvent::Stage`] carries, and it is
/// open: a stage a plugin contributed has a name here that this enum does not
/// and should not know (`doc:roleless-core`). Staying closed is what keeps this
/// type useful to the consumers that genuinely need a fixed set — the
/// diagnostics bridge, whose field values cannot hold a runtime string at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    /// Prompt classification and routing: how hard is this turn, and which
    /// tier should serve it.
    Triage,
    /// Context recall: the frames the context plane put in front of the model
    /// before it planned anything.
    ContextRecall,
    /// Pre-plan research: triage named questions, and parallel read-only
    /// sub-agents answer them so the planner names files it has evidence for
    /// rather than guesses (#1778). Skipped whenever triage named none.
    Research,
    /// Planning: the ordered steps the worker is about to attempt.
    Plan,
    /// The interactive approval gate a large plan passes through (L-E5).
    ScopeReview,
    /// Witness authoring: after the worker executes — once the warrant has
    /// read the diff and found something worth proving — an independent
    /// model (the verifier's resolution, never the worker's transcript)
    /// writes the witness test in a pristine snapshot of the pre-execution
    /// tree: a test that FAILS there and will pass once the goal is met,
    /// arming the deterministic flip oracle (L-E11). The witness is visible
    /// to the worker's revise turns (iterating against a failing test is
    /// where convergence comes from); integrity comes from tamper exclusion
    /// at verify time, not from hiding the test.
    Witness,
    /// The worker's own tool-calling loop — the steps that actually change
    /// the workspace.
    Execute,
    /// The deterministic verification ladder: the flip oracle, the touched
    /// tests, the diff budget.
    Verify,
    /// The verifier's verdict, reached only when the deterministic ladder came
    /// back inconclusive (L-E11). Named for the output rather than the model:
    /// a stage called `Verifier` sitting next to `Verify` hid which of the two
    /// was proof and which was opinion.
    ///
    /// Aliased for the same reason as the other renames in this pass: the
    /// stage shipped on the wire as `judge`, so every recorded session names
    /// it that way. Reading them is not optional — replay, the observatory and
    /// the golden fixtures all parse stored streams. `verifier` is aliased too
    /// because it was this stage's name for the length of one commit on this
    /// branch, and a stream recorded against that build must still read.
    #[serde(alias = "judge", alias = "verifier")]
    Verdict,
    /// Post-turn self-reflection: the agent reviews its own performance on
    /// the completed turn and records improvement memories into the context
    /// plane, tagged with the workspace's inferred domains, for recall on
    /// future relevant turns.
    Reflect,
    /// Context write-back: episode summaries and fact upserts landing in the
    /// context plane (close-not-delete, L-C3).
    ContextWrite,
    /// The turn is done. The last stage boundary a turn emits.
    Complete,
}

/// Budget enforcement mode: `off` (no metering),
/// `observed` (meter + warn), `enforced` (hard stop with a clean turn
/// abort — never a mid-tool kill).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BudgetMode {
    /// No metering at all — spend is neither tracked nor reported.
    Off,
    /// Spend is metered and a breach warns, but nothing is ever denied.
    Observed,
    /// A breach aborts the turn at the next clean boundary.
    Enforced,
}

/// Which budget limit a [`super::AgentEvent::BudgetDenied`] tripped — mirrors
/// `stella-core::budget::BudgetAxis` (kept separate so `stella-protocol`
/// never depends on `stella-core`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    /// The per-turn limit — reset at every `run_turn`.
    Turn,
    /// The per-session limit — accumulated across every turn of the session.
    Session,
}

/// What kind of policy-plane decision a [`super::AgentEvent::PolicyDecision`]
/// records (receipts spec §6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PolicyKind {
    /// A blocking policy chain evaluated a tool call or side effect.
    Evaluated,
    /// A policy denied the call/side effect.
    Blocked,
    /// A policy deferred the call to human approval.
    ApprovalRequested,
    /// A payload-hygiene detector flagged secret-shaped content.
    SecretDetected,
}

/// Which authority held a workspace's steering back
/// ([`super::AgentEvent::SteeringWithheld`], #2302/#3616).
///
/// Two causes resolve one refusal, and they are not interchangeable: they have
/// different remedies, and one of them the user cannot lift at all. A harness
/// that folded them together would tell an operator to set a flag they have
/// already set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Withholder {
    /// This process was not told to trust the repository. The remedy is the
    /// operator's own — trusting the checkout lifts it.
    ProjectUntrusted,
    /// The org-managed scope pins project prompts off. A **ceiling**: it holds
    /// whether or not the checkout is trusted, and no environment variable
    /// lifts it.
    ManagedCeiling,
}

/// Content-free reason a provider attempt cannot contribute a truthful usage
/// envelope. Error bodies and prompts are deliberately unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum UsageIncompleteReason {
    /// The provider returned a failure after dispatch, so the request was
    /// received and may have been billed even though no usage frame arrived.
    ProviderError,
    /// The client-side deadline elapsed with the call still in flight. The
    /// server may have completed the work regardless.
    Timeout,
    /// The caller dropped the turn (hard cancel) while a paid provider
    /// attempt was still in flight — the call may have real server-side
    /// cost whose usage is unknowable. Emitted by the engine's drop guard,
    /// which is armed only for exactly that window (a call that settles
    /// normally reports through its ordinary `StepUsage` envelope instead).
    Cancelled,
}
