// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plan: what stella said it would do, and how far through it is.
//!
//! # Why this module exists
//!
//! The deck used to carry two unrelated systems for the same idea.
//!
//! - **Scope steps** — `ScopeProposal::steps`, a `Vec<String>`. This is what
//!   the approval gate shows and what the user actually consents to. Bare
//!   strings: no identity, no state, so nothing about them could ever move.
//! - **The task board** — `Vec<TaskItem>` with a real lifecycle, driven by the
//!   `task_*` tools. All of the state, and reliably empty, because nothing
//!   ever seeded it from the steps the user had just approved.
//!
//! So the surface with the states had no content and the surface with the
//! content had no states, and they rendered as two panels under two headings.
//!
//! This module is the single fold both event streams land in. The proposal
//! supplies the *steps* — the titles the user agreed to, in order. The board
//! supplies the *states*, joined by ordinal id. Neither is authoritative
//! alone and neither is discarded: a plan whose board never arrives still
//! renders every approved step (as `Planned`), and a board that runs past the
//! approved steps still renders the extra ones.
//!
//! # Vocabulary (D6)
//!
//! Stella makes and executes **plans**. A plan has **plan steps**. `task`,
//! `scope`, and `issue` are other tools' words — GitHub's, Jira's — and none
//! of them reach a rendered string here. The `task_*` tool names and the
//! `TaskUpdate` wire event keep their identifiers; this is the layer where
//! they become one vocabulary.
//!
//! Pure: this module folds and formats, and returns rows for a renderer to
//! style. No ratatui types, so the fold is unit-testable without a terminal —
//! the same discipline as [`crate::proof`].

use stella_protocol::{ScopeProposal, TaskItem, TaskStatus};

/// Where one plan step is in its lifecycle.
///
/// Four states, deliberately. A step is *going to happen*, *happening*,
/// *happened*, or *went wrong*; anything finer is scheduler detail the reader
/// of a rail does not need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanStepState {
    /// Agreed and not yet begun. The default for every step of a fresh plan.
    #[default]
    Planned,
    /// Being worked right now.
    Started,
    /// Finished successfully. Terminal.
    Complete,
    /// Failed, or was abandoned before finishing. Terminal.
    ///
    /// A cancelled step lands here too: the four-state vocabulary has one slot
    /// for "terminal and not done", and a dropped step is not a completed one.
    /// [`PlanStep::note`] carries the distinction in words so the row never
    /// reads as a crash when it was a decision.
    Error,
}

impl PlanStepState {
    /// Whether the step can still change state.
    pub fn is_open(self) -> bool {
        matches!(self, PlanStepState::Planned | PlanStepState::Started)
    }
}

/// Where the plan as a whole stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanState {
    /// Being drafted — no steps have been proposed yet.
    #[default]
    Draft,
    /// Proposed, waiting on the user's decision at the gate.
    PendingApproval,
    /// The user approved it; no step has started yet.
    Approved,
    /// At least one step is under way.
    Started,
    /// Every step reached a terminal state and none of them failed.
    Completed,
    /// Declined at the gate, or abandoned before any step ran.
    Cancelled,
    /// At least one step failed.
    Error,
}

impl PlanState {
    /// The word the rail prints for this state.
    pub fn label(self) -> &'static str {
        match self {
            PlanState::Draft => "draft",
            PlanState::PendingApproval => "pending approval",
            PlanState::Approved => "approved",
            PlanState::Started => "working",
            PlanState::Completed => "completed",
            PlanState::Cancelled => "cancelled",
            PlanState::Error => "error",
        }
    }
}

/// One step of the plan: what it is, and where it got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    /// Ordinal id ("1", "2", …) — the join key between the approved steps and
    /// the board snapshots, and what the `task_*` tools address.
    pub id: String,
    /// The imperative title the user approved.
    pub title: String,
    /// The elaboration, when the planner or the worker wrote one. This is what
    /// makes a proposed step *readable* rather than a headline.
    pub detail: Option<String>,
    /// Lifecycle position.
    pub state: PlanStepState,
    /// Which lane owns it, once claimed.
    pub owner: Option<String>,
    /// Why a terminal step ended the way it did, when that is not obvious from
    /// the state alone (a cancelled step says so here).
    pub note: Option<String>,
}

/// The whole plan for one turn: the fold of `ScopeReview` (the steps and their
/// approval) and `TaskUpdate` (their states).
///
/// `PartialEq` but not `Eq` — the cost estimate is an `f64`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// One line describing the work — the gate's headline.
    pub summary: String,
    /// Where the plan stands. Derived from the steps once any of them moves;
    /// set directly by the gate before that.
    pub state: PlanState,
    /// The steps the user approved, in order. Empty until a plan is proposed.
    proposed: Vec<String>,
    /// The latest board snapshot, if one has arrived.
    board: Vec<TaskItem>,
    /// How many files the plan expects to touch.
    pub estimated_files: u32,
    /// Projected spend, when the planner could estimate one.
    pub estimated_cost_usd: Option<f64>,
    /// Set once the user has decided, so a later empty board cannot silently
    /// walk the plan back to `PendingApproval`.
    decided: bool,
}

impl Plan {
    /// Whether there is nothing at all to show.
    pub fn is_empty(&self) -> bool {
        self.proposed.is_empty() && self.board.is_empty() && self.summary.is_empty()
    }

    /// Fold a proposal arriving at the gate: this is the plan, pending the
    /// user's decision.
    pub fn propose(&mut self, proposal: &ScopeProposal) {
        self.summary = proposal.summary.clone();
        self.proposed = proposal.steps.clone();
        self.estimated_files = proposal.estimated_files;
        self.estimated_cost_usd = proposal.estimated_cost_usd;
        self.state = PlanState::PendingApproval;
        self.decided = false;
        // A proposal at the gate is a *new* plan, so the previous one's
        // progress does not carry over. Without this, a re-plan mid-turn read
        // as `pending approval 5/7` — five steps of a plan nobody has agreed
        // to yet, already done.
        self.board.clear();
    }

    /// The user approved the plan at the gate.
    pub fn approve(&mut self) {
        self.decided = true;
        self.state = PlanState::Approved;
        self.resettle();
    }

    /// The user declined it.
    pub fn cancel(&mut self) {
        self.decided = true;
        self.state = PlanState::Cancelled;
    }

    /// Fold a board snapshot: the states of the steps.
    ///
    /// A board that arrives without a preceding gate is still a plan — the
    /// worker planned as it went — so this seeds `summary`-less plans too
    /// rather than dropping the rows on the floor.
    pub fn apply_board(&mut self, board: &[TaskItem]) {
        self.board = board.to_vec();
        if !board.is_empty() {
            self.decided = true;
        }
        self.resettle();
    }

    /// Recompute the plan's own state from its steps. Called after anything
    /// that can move a step.
    ///
    /// The plan-level state is *derived*, not stored, for the reason the proof
    /// rail's invariant exists: a state that has to be remembered at every exit
    /// is one refactor away from being wrong, and wrong invisibly.
    fn resettle(&mut self) {
        if matches!(self.state, PlanState::Cancelled) {
            return;
        }
        let steps = self.steps();
        if steps.is_empty() {
            if !self.decided {
                self.state = PlanState::Draft;
            }
            return;
        }
        self.state = if steps.iter().any(|s| s.state == PlanStepState::Error) {
            PlanState::Error
        } else if steps.iter().all(|s| s.state == PlanStepState::Complete) {
            PlanState::Completed
        } else if steps.iter().any(|s| s.state != PlanStepState::Planned) {
            PlanState::Started
        } else if self.decided {
            PlanState::Approved
        } else {
            PlanState::PendingApproval
        };
    }

    /// Close the plan at the end of a turn: a step still `Started` when the
    /// turn ends never finished, and saying so is the honest report.
    ///
    /// The same half-invariant [`crate::proof::ProofState::finish`] holds — a
    /// surface that only resolves on the happy path is silence wearing the
    /// costume of progress.
    pub fn finish(&mut self) {
        for item in &mut self.board {
            if item.status == TaskStatus::InProgress {
                item.status = TaskStatus::Cancelled;
            }
        }
        self.resettle();
    }

    /// The steps, as the rail renders them: the approved titles carrying the
    /// board's states, joined by ordinal id.
    ///
    /// Neither source wins outright. The proposal supplies titles the board's
    /// snapshot may not carry; the board supplies states the proposal cannot
    /// have. A board row past the end of the proposal is a step the worker
    /// added, and it is shown — hiding it would make the rail lie about how
    /// much work is left.
    pub fn steps(&self) -> Vec<PlanStep> {
        let mut steps: Vec<PlanStep> = self
            .proposed
            .iter()
            .enumerate()
            .map(|(i, title)| PlanStep {
                id: (i + 1).to_string(),
                title: title.clone(),
                detail: None,
                state: PlanStepState::Planned,
                owner: None,
                note: None,
            })
            .collect();
        for item in &self.board {
            let (state, note) = match item.status {
                TaskStatus::Pending => (PlanStepState::Planned, None),
                TaskStatus::InProgress => (PlanStepState::Started, None),
                TaskStatus::Completed => (PlanStepState::Complete, None),
                TaskStatus::Cancelled => (PlanStepState::Error, Some("cancelled".to_string())),
            };
            match steps.iter_mut().find(|s| s.id == item.id) {
                Some(step) => {
                    step.state = state;
                    step.note = note;
                    step.owner.clone_from(&item.owner);
                    // The board's subject wins when it differs: the worker may
                    // have restated the step, and what it is doing beats what
                    // it said it would do.
                    step.title.clone_from(&item.subject);
                    if item.description.is_some() {
                        step.detail.clone_from(&item.description);
                    }
                }
                None => steps.push(PlanStep {
                    id: item.id.clone(),
                    title: item.subject.clone(),
                    detail: item.description.clone(),
                    state,
                    owner: item.owner.clone(),
                    note,
                }),
            }
        }
        steps
    }

    /// `(complete, total)` — the rail's fraction.
    pub fn progress(&self) -> (usize, usize) {
        let steps = self.steps();
        (
            steps
                .iter()
                .filter(|s| s.state == PlanStepState::Complete)
                .count(),
            steps.len(),
        )
    }

    /// The step being worked right now, if any.
    pub fn active(&self) -> Option<PlanStep> {
        self.steps()
            .into_iter()
            .find(|s| s.state == PlanStepState::Started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(steps: &[&str]) -> ScopeProposal {
        ScopeProposal {
            summary: "unify the plan surfaces".into(),
            steps: steps.iter().map(|s| (*s).to_string()).collect(),
            estimated_files: 9,
            estimated_cost_usd: Some(1.40),
            ..Default::default()
        }
    }

    fn item(id: &str, subject: &str, status: TaskStatus) -> TaskItem {
        TaskItem {
            id: id.into(),
            subject: subject.into(),
            description: None,
            status,
            owner: None,
        }
    }

    /// **The defect this module exists for.** An approved plan whose board
    /// never arrives must still render every step the user agreed to — the old
    /// TASKS panel showed an empty list in exactly this, the most common, case.
    #[test]
    fn an_approved_plan_with_no_board_still_has_all_its_steps() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["read the layout", "fold the rail", "test it"]));
        plan.approve();
        let steps = plan.steps();
        assert_eq!(steps.len(), 3, "{steps:?}");
        assert!(steps.iter().all(|s| s.state == PlanStepState::Planned));
        assert_eq!(plan.state, PlanState::Approved);
        assert_eq!(plan.progress(), (0, 3));
    }

    /// The join: the proposal's titles carry the board's states.
    #[test]
    fn the_board_supplies_states_for_the_approved_steps() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two", "three"]));
        plan.approve();
        plan.apply_board(&[
            item("1", "one", TaskStatus::Completed),
            item("2", "two", TaskStatus::InProgress),
        ]);
        let steps = plan.steps();
        assert_eq!(steps[0].state, PlanStepState::Complete);
        assert_eq!(steps[1].state, PlanStepState::Started);
        assert_eq!(
            steps[2].state,
            PlanStepState::Planned,
            "unreported = planned"
        );
        assert_eq!(plan.state, PlanState::Started);
        assert_eq!(plan.active().unwrap().title, "two");
    }

    /// A board that runs past the approved steps is showing work the worker
    /// added. Hiding it would make the fraction lie.
    #[test]
    fn a_board_longer_than_the_plan_keeps_its_extra_steps() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one"]));
        plan.approve();
        plan.apply_board(&[
            item("1", "one", TaskStatus::Completed),
            item("2", "a step nobody proposed", TaskStatus::Pending),
        ]);
        assert_eq!(plan.progress(), (1, 2));
        assert_eq!(plan.steps()[1].title, "a step nobody proposed");
    }

    /// A worker that plans as it goes, with no gate, is still running a plan.
    #[test]
    fn a_board_with_no_gate_is_still_a_plan() {
        let mut plan = Plan::default();
        plan.apply_board(&[item("1", "improvised", TaskStatus::InProgress)]);
        assert_eq!(plan.state, PlanState::Started);
        assert_eq!(plan.steps().len(), 1);
    }

    #[test]
    fn a_declined_plan_stays_cancelled_whatever_arrives_next() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one"]));
        plan.cancel();
        plan.apply_board(&[item("1", "one", TaskStatus::Completed)]);
        assert_eq!(plan.state, PlanState::Cancelled);
    }

    #[test]
    fn a_failed_step_puts_the_whole_plan_in_error() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two"]));
        plan.approve();
        plan.apply_board(&[
            item("1", "one", TaskStatus::Completed),
            item("2", "two", TaskStatus::Cancelled),
        ]);
        assert_eq!(plan.state, PlanState::Error);
        assert_eq!(plan.steps()[1].note.as_deref(), Some("cancelled"));
    }

    #[test]
    fn every_step_complete_completes_the_plan() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two"]));
        plan.approve();
        plan.apply_board(&[
            item("1", "one", TaskStatus::Completed),
            item("2", "two", TaskStatus::Completed),
        ]);
        assert_eq!(plan.state, PlanState::Completed);
        assert_eq!(plan.progress(), (2, 2));
    }

    /// The `finish` half of the resolve-on-every-path invariant: a step still
    /// `Started` when the turn ends never finished, and the rail must not keep
    /// implying it is in flight.
    #[test]
    fn no_step_is_left_working_after_a_turn_ends() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one"]));
        plan.approve();
        plan.apply_board(&[item("1", "one", TaskStatus::InProgress)]);
        plan.finish();
        assert!(
            !plan
                .steps()
                .iter()
                .any(|s| s.state == PlanStepState::Started),
            "a finished turn may not leave a step working"
        );
        assert_eq!(plan.state, PlanState::Error);
    }

    /// The board's wording wins over the proposal's: what the worker is doing
    /// beats what it said it would do.
    #[test]
    fn a_restated_step_shows_the_workers_wording() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["vague original"]));
        plan.approve();
        plan.apply_board(&[item(
            "1",
            "the concrete restatement",
            TaskStatus::InProgress,
        )]);
        assert_eq!(plan.steps()[0].title, "the concrete restatement");
    }

    #[test]
    fn a_fresh_plan_is_empty_and_draft() {
        let plan = Plan::default();
        assert!(plan.is_empty());
        assert_eq!(plan.state, PlanState::Draft);
        assert_eq!(plan.progress(), (0, 0));
        assert!(plan.active().is_none());
    }
}
