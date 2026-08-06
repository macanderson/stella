//! The task frame: the read-only view of the task that every candidate stage
//! receives (#1809).
//!
//! Between `run` and `run_engine_turn` the same four values — the goal, the
//! staged conversation prefix, the plan, and triage's assessment — used to be
//! threaded positionally through every layer, and each new stage input meant
//! widening half a dozen signatures (and re-justifying a
//! `clippy::too_many_arguments` allow at most of them). They travel as one
//! borrowed `Copy` frame now. The frame is transport, not semantics: each
//! field keeps its own meaning, documented where the field's type lives.
//!
//! What deliberately does NOT ride here: anything mutable (the budget and
//! running total travel as [`super::stage_budget::Spend`]; per-candidate
//! tallies as [`crate::witness::warrant::ChangeSignals`] inside
//! `CandidateState`), and anything per-candidate (the engine, the surface, a
//! workspace) — a frame outliving the candidates that read it is what makes
//! it safe to copy freely into each one.

use stella_protocol::CompletionMessage;

use crate::plan::PlanStep;
use crate::triage::TaskAssessment;

/// The immutable per-turn inputs of the execute/verify plane. Constructed
/// once in `Pipeline::run` when the candidate stages begin; every stage from
/// `run_best_of_n` down to `verify_candidate` reads the parts it needs.
#[derive(Clone, Copy)]
pub(super) struct TaskFrame<'a> {
    /// The user's goal, verbatim — what triage classified and what the
    /// verifier is ultimately asked to judge the work against.
    pub(super) goal: &'a str,
    /// The staged conversation prefix every candidate starts from (recall,
    /// skills, scope — everything before the worker's first turn).
    pub(super) base_messages: &'a [CompletionMessage],
    /// The scope stage's plan, when one was produced. `None` runs a single
    /// unplanned turn.
    pub(super) plan: Option<&'a [PlanStep]>,
    /// Triage's classification — which stages run at all, and how strictly
    /// the ladder verifies.
    pub(super) assessment: TaskAssessment,
}
