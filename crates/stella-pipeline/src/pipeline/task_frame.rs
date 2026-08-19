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
use crate::schedule::Schedule;
use crate::triage::TaskAssessment;

/// The immutable per-turn inputs of the execute/verify plane. Constructed
/// once in `Pipeline::run` when the candidate stages begin; every stage from
/// `run_best_of_n` down to `verify_candidate` reads the parts it needs.
///
/// `Clone`, not `Copy` (#3408): [`Self::schedule`] carries a
/// [`stella_plugin::ProgressiveResolver`] underneath, which owns two small
/// `Vec`s. A call site that needs the frame more than once — a fan-out loop,
/// a shared/isolated split with a degrade-to-bare fallback — clones it
/// explicitly at the point the frames diverge, the same discipline
/// [`Schedule`] itself asks for and for the same reason: each candidate needs
/// its own independent walk from there.
#[derive(Clone)]
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
    /// This turn's `[wrapper]` schedule, positioned right after the shared
    /// pre-execute prefix (`triage` through `scope` — see
    /// `Pipeline::begin_turn_schedule` and `crate::schedule`'s module docs).
    /// `execute`, `witness`, and `verify` are NOT yet decided: each candidate
    /// clones this and decides them independently, against its own real
    /// outcome, in `Pipeline::run_candidate` (#3408).
    pub(super) schedule: Schedule<'a>,
    /// The manifest's bare `witness` decision, already made once against the
    /// shared prefix's facts (`Pipeline::begin_turn_schedule`, needed there
    /// to shape the worker's test-first contract before any candidate
    /// exists). Carried so each candidate's own witness re-decision —
    /// unavoidable to reach `verify` in [`Schedule`]'s declared-order walk —
    /// can assert it agrees, rather than silently trusting two computations
    /// of the same thing to stay in sync (#3408).
    pub(super) witness_scheduled: bool,
}
