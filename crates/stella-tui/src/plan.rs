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
//! style. No ratatui types, so the fold is unit-testable without a terminal.

use std::collections::BTreeMap;

use stella_protocol::{ScopeProposal, TaskContract, TaskItem, TaskStatus};

/// What a plan-step row says about itself — SPEC 7.2's six states, one per
/// glyph cell.
///
/// This is the *render* vocabulary, not the wire one. Five of the six are
/// lifecycle positions and fold straight from [`TaskStatus`]; the sixth,
/// [`Self::DriftInserted`], is a fact about the plan graph rather than about
/// the step's progress, and it takes the glyph cell because a row has one.
/// See [`stella_protocol::TaskStatus`] for why the wire refuses to carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanStepState {
    /// Agreed and not yet begun — SPEC 7.2's `○ queued`. The default for every
    /// step of a fresh plan.
    #[default]
    Planned,
    /// Being worked right now — `◐ running`.
    Started,
    /// Finished successfully — `✓ done`. Terminal.
    Complete,
    /// Stopped short — `✗ blocked`.
    ///
    /// A cancelled step lands here too: SPEC 4 spends one `✗` on every way a
    /// step can end badly, so [`PlanStep::note`] carries the distinction in
    /// words and the row never reads as a crash when it was a decision. The
    /// wire tells the two apart now ([`TaskStatus::Blocked`] against
    /// [`TaskStatus::Cancelled`]) even though the glyph cannot.
    Blocked,
    /// Its checks are running or awaiting a verdict — `◇ verify`, a gate task
    /// that blocks the merge.
    Verify,
    /// A step the approved plan did not contain — `⌥ drift-inserted`.
    ///
    /// Reached from [`PlanLanes`] and from nowhere else. No [`TaskStatus`]
    /// asserts drift and none ever should: a board status is a claim its own
    /// producer makes, and the producer with the strongest reason not to
    /// report drift is the one that drifted. [`Plan::steps`] therefore reads
    /// it off the two lanes disagreeing (`mark_drift`), which is the same
    /// discipline `stella_core::plan_graph::PlanGraph::divergences` applies on
    /// the engine side.
    ///
    /// [`Plan::lanes`] has no producer today, so no live session reaches this
    /// state — #5270 is that wiring. What is built here is the fold that will
    /// consume it.
    DriftInserted,
}

impl PlanStepState {
    /// Whether the step can still change state.
    ///
    /// [`Self::Blocked`] is open: SPEC 8.1 unblocks a red gate on green, and
    /// the card offers `x` on an open row, which is the dismiss that section
    /// asks for. An exhaustive match, so a seventh state has to answer this
    /// rather than inherit an answer.
    pub fn is_open(self) -> bool {
        match self {
            PlanStepState::Planned
            | PlanStepState::Started
            | PlanStepState::Blocked
            | PlanStepState::Verify
            | PlanStepState::DriftInserted => true,
            PlanStepState::Complete => false,
        }
    }

    /// Whether work has begun on this step — what makes the plan as a whole
    /// `started` rather than `approved`.
    ///
    /// A drift-inserted step is queued work that arrived late, so its presence
    /// is not progress; it answers this the same way [`Self::Planned`] does.
    fn has_begun(self) -> bool {
        match self {
            PlanStepState::Planned | PlanStepState::DriftInserted => false,
            PlanStepState::Started
            | PlanStepState::Complete
            | PlanStepState::Blocked
            | PlanStepState::Verify => true,
        }
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
    /// What this step means by done, when the board carried a contract
    /// (SPEC 7.1). `None` is *no contract stated*, which is a different fact
    /// from [`TaskContract::ReadOnly`] — a step that produces no diff — and
    /// the zoom prints the two differently rather than collapsing them.
    pub contract: Option<TaskContract>,
}

/// What kind of work one evidence row records.
///
/// Three, because SPEC 7.1 names three: *edits, runs, graph writes*. A
/// mechanism outside those is not silently retyped as one of them — it does
/// not reach this fold at all until the vocabulary grows to hold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    /// A file the step changed.
    Edit,
    /// A command the step ran.
    Run,
    /// A write the step made to the code graph.
    GraphWrite,
}

impl EvidenceKind {
    /// The word the evidence block prints in its left column.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            EvidenceKind::Edit => "edit",
            EvidenceKind::Run => "run",
            EvidenceKind::GraphWrite => "graph",
        }
    }
}

/// One line of a step's evidence ledger: an event tagged with that step's id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRow {
    /// Which of the three kinds of work this was.
    pub kind: EvidenceKind,
    /// What it acted on — a path, a command, a node count.
    pub subject: String,
    /// The measured outcome beside it: `+41 -6`, `2/4`, `wr`.
    pub outcome: String,
}

/// What one step spent (SPEC 7.1's third part).
///
/// `PartialEq` but not `Eq` — the dollar figures are `f64`, the same reason
/// [`Plan`] is.
#[derive(Debug, Clone, PartialEq)]
pub struct StepSpend {
    /// Dollars this step has cost so far.
    pub usd: f64,
    /// Tokens it has spent.
    pub tokens: u64,
    /// What share of its input tokens were served from cache, 0–100.
    pub cache_read_pct: u8,
    /// How many model calls it made. A step with none cost `$0.00`, and the
    /// strip says so without needing the dollar figure to prove it.
    pub model_calls: u32,
    /// Projected remaining spend, when there is a basis for one.
    pub est_remaining_usd: Option<f64>,
}

/// A step's evidence and spend together — everything the ledger knows about
/// one step, keyed by [`PlanStep::id`] in [`Plan::ledger`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepLedger {
    /// Every event tagged with this step's id, in the order they happened.
    pub evidence: Vec<EvidenceRow>,
    /// What the step spent, when spend has been attributed to it.
    pub spend: Option<StepSpend>,
}

/// One node of the actual path: what ran, and whether it was in the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualStep {
    /// The step's title as it ran.
    pub title: String,
    /// Why the plan changed here, for a step the plan did not contain. `None`
    /// for a step that was planned — SPEC 7.4 requires a cause on every
    /// divergence, so an unexplained insertion has no representation.
    pub cause: Option<String>,
}

impl ActualStep {
    /// Whether this step diverges from the plan.
    #[must_use]
    pub const fn is_drift(&self) -> bool {
        self.cause.is_some()
    }
}

/// The two paths through a plan (SPEC 7.4): what was planned, and what
/// happened.
///
/// Planned comes from the plan graph's `[:NEXT]` edges and actual from
/// `[:THEN]`. Neither is derivable from the board — the board holds only the
/// path that survived — so a plan with no graph behind it has `None` here and
/// the zoom says the lanes are not recorded rather than drawing the board
/// twice under two headings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLanes {
    /// The planned path, in `[:NEXT]` order.
    pub planned: Vec<String>,
    /// The path actually taken, in `[:THEN]` order.
    pub actual: Vec<ActualStep>,
}

impl PlanLanes {
    /// How many steps of the actual path the plan did not contain.
    #[must_use]
    pub fn divergences(&self) -> usize {
        self.actual.iter().filter(|s| s.is_drift()).count()
    }
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
    /// Which revision of the plan this is, when the proposal stated one
    /// (#4333). `None` for a board-only plan and for any recording written
    /// before `ScopeProposal::revision` existed — the rail then says nothing
    /// rather than claiming a first revision it cannot see.
    pub revision: Option<u32>,
    /// The plan graph's two lanes, once something has recorded them (SPEC
    /// 7.4). `None` until #5037 authors `[:NEXT]` and `[:THEN]` edges; the
    /// task zoom elides its lane block on `None` rather than drawing the
    /// board twice and calling one copy the plan.
    pub lanes: Option<PlanLanes>,
    /// Per-step evidence and spend, keyed by [`PlanStep::id`] (SPEC 7.1).
    ///
    /// Empty until #5039 stamps a step id on the events that carry work. The
    /// session's untagged edits and runs are **not** borrowed to fill it: an
    /// edit this step did not make is not this step's evidence, and a ledger
    /// that cannot tell the difference is worse than an absent one.
    pub ledger: BTreeMap<String, StepLedger>,
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
        self.revision = proposal.revision;
        // A proposal at the gate is a *new* plan, so the previous one's
        // progress does not carry over. Without this, a re-plan mid-turn read
        // as `pending approval 5/7` — five steps of a plan nobody has agreed
        // to yet, already done.
        self.board.clear();
        // The lanes and the ledger belong to the plan that was replaced. A
        // revision's actual path starts empty, and carrying the previous
        // one's evidence forward would attribute work to steps that have not
        // run.
        self.lanes = None;
        self.ledger.clear();
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
    ///
    /// `Cancelled` is the exception, and it is terminal until [`Self::propose`]
    /// replaces the plan. It is the only state here that records a *person's*
    /// decision rather than a fold of the board, and the board cannot answer
    /// for it: with every step still `Planned` and `decided` set, the
    /// derivation below lands on `Approved`, which is the refusal read back as
    /// its opposite. So the verdict outlives the snapshot, and the revised
    /// proposal — not a `TaskUpdate` — is what clears it (#4667).
    ///
    /// This costs the rail nothing, because the state word is not how the rail
    /// shows movement. [`Self::steps`] and [`Self::progress`] read `board`
    /// directly, so a board arriving after a refusal moves the glyphs, the
    /// active step and the fraction while the word stays `cancelled` —
    /// `a_declined_plan_still_shows_the_board_moving` pins exactly that.
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
        self.state = if steps.iter().any(|s| s.state == PlanStepState::Blocked) {
            PlanState::Error
        } else if steps.iter().all(|s| s.state == PlanStepState::Complete) {
            PlanState::Completed
        } else if steps.iter().any(|s| s.state.has_begun()) {
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
    /// A surface that only resolves on the happy path is silence wearing the
    /// costume of progress, so this is called from every terminal exit — the
    /// run ending, and a non-retryable error ending it early.
    pub fn finish(&mut self) {
        for item in &mut self.board {
            // `Verify` counts as in flight: a gate that never returned a
            // verdict is not a pass, and leaving the row on `◇` would imply
            // one is still coming.
            if matches!(item.status, TaskStatus::InProgress | TaskStatus::Verify) {
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
                // A proposal states titles, never contracts: the board is the
                // only source for what a step means by done.
                contract: None,
            })
            .collect();
        for item in &self.board {
            // No arm reaches `DriftInserted`: see its doc comment — the wire
            // deliberately carries no status that asserts drift.
            let (state, note) = match item.status {
                TaskStatus::Pending => (PlanStepState::Planned, None),
                TaskStatus::InProgress => (PlanStepState::Started, None),
                TaskStatus::Verify => (PlanStepState::Verify, None),
                TaskStatus::Completed => (PlanStepState::Complete, None),
                TaskStatus::Blocked => (PlanStepState::Blocked, None),
                // Both draw `✗`; the note is the only thing that tells a
                // reader a person dropped this step rather than a gate
                // stopping it.
                TaskStatus::Cancelled => (PlanStepState::Blocked, Some("cancelled".to_string())),
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
                    step.contract.clone_from(&item.contract);
                }
                None => steps.push(PlanStep {
                    id: item.id.clone(),
                    title: item.subject.clone(),
                    detail: item.description.clone(),
                    state,
                    owner: item.owner.clone(),
                    note,
                    contract: item.contract.clone(),
                }),
            }
        }
        mark_drift(&mut steps, self.lanes.as_ref());
        steps
    }

    /// How many steps the plan was approved with — SPEC 7.3's `planned 6`.
    ///
    /// The *approved* count, not [`Self::steps`]'s length: the board grows as
    /// work is inserted, and a `planned` that grew with it could never differ
    /// from `actual`, which is the whole point of stating both.
    ///
    /// `None` for a board-only plan (the worker planned as it went), which has
    /// no approved list to count. That renders as an elision rather than as a
    /// zero somebody could read as "nothing was planned".
    #[must_use]
    pub fn planned_count(&self) -> Option<usize> {
        (!self.proposed.is_empty()).then_some(self.proposed.len())
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

/// Repaint every step the approved plan did not contain as
/// [`PlanStepState::DriftInserted`] — SPEC 7.3's `⌥` in gold-bright with an
/// `inserted` tag.
///
/// The lanes are the only thing that can say so, and that is why no arm of
/// [`Plan::steps`]'s board match reaches this state: a board `status` is a
/// claim its own producer makes, and drift is exactly the claim a producer
/// that drifted has an incentive not to make. `PlanLanes::actual` is derived
/// from `[:THEN]` edges against `[:NEXT]` ones, so a step is drift because the
/// two lanes disagree, never because something said it was.
///
/// The drift mark replaces the lifecycle glyph rather than sitting beside it,
/// which is what SPEC 7.3 asks for: a reader scanning the panel is looking for
/// what the plan did not contain, and a `✓` on an inserted step buries that
/// under the news that it finished. An existing note is kept and the tag is
/// appended to it, so a step that was both cancelled and inserted says both.
///
/// Matched on title because that is the only key the two lanes share —
/// `PlanLanes::actual` is `ActualStep { title, cause }` and carries no board
/// id. A plan with two steps of the same title marks both; giving the lanes
/// their own ids is #5270's business, not this fold's.
fn mark_drift(steps: &mut [PlanStep], lanes: Option<&PlanLanes>) {
    let Some(lanes) = lanes else {
        return;
    };
    for step in steps.iter_mut() {
        if !lanes
            .actual
            .iter()
            .any(|actual| actual.is_drift() && actual.title == step.title)
        {
            continue;
        }
        step.state = PlanStepState::DriftInserted;
        step.note = Some(match step.note.take() {
            Some(note) => format!("{note} · inserted"),
            None => "inserted".to_owned(),
        });
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
            contract: None,
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

    /// The verdict is the user's, so nothing on the board may overwrite it —
    /// see [`Plan::resettle`] for why a derivation would land on `Approved`.
    #[test]
    fn a_declined_plan_stays_cancelled_whatever_arrives_next() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one"]));
        plan.cancel();
        plan.apply_board(&[item("1", "one", TaskStatus::Completed)]);
        assert_eq!(plan.state, PlanState::Cancelled);
    }

    /// The other half of the same rule, and the half nothing pinned (#4667): a
    /// terminal verdict freezes the *word*, never the board under it. A
    /// `TaskUpdate` landing between a refusal and the re-proposal moves the
    /// glyphs, the active step and the fraction, because [`Plan::steps`] and
    /// [`Plan::progress`] read the snapshot rather than the state.
    ///
    /// Without this, an early return in [`Plan::apply_board`] for a cancelled
    /// plan — the obvious reading of "cancelled is terminal" — would blank the
    /// rail for the whole window the model spends revising, and no test would
    /// have said so.
    #[test]
    fn a_declined_plan_still_shows_the_board_moving() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two", "three"]));
        plan.cancel();
        assert_eq!(plan.progress(), (0, 3));
        assert!(plan.active().is_none());

        plan.apply_board(&[
            item("1", "one", TaskStatus::Completed),
            item("2", "two", TaskStatus::InProgress),
            item("3", "three", TaskStatus::Pending),
        ]);
        assert_eq!(
            plan.state,
            PlanState::Cancelled,
            "the verdict is the user's"
        );
        assert_eq!(plan.progress(), (1, 3), "the fraction follows the board");
        assert_eq!(
            plan.active().map(|s| s.title).as_deref(),
            Some("two"),
            "the running step is named while the model revises"
        );
        assert_eq!(
            plan.steps().iter().map(|s| s.state).collect::<Vec<_>>(),
            vec![
                PlanStepState::Complete,
                PlanStepState::Started,
                PlanStepState::Planned
            ]
        );
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

    /// The board is the only source for what a step means by done, so the
    /// fold has to carry its contract — the task zoom's contract block reads
    /// this and nothing else (SPEC 7.1, #5041).
    #[test]
    fn the_board_carries_each_steps_contract_onto_its_step() {
        use stella_protocol::{Check, CheckKind, CheckMechanism, DefinitionOfDone};
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two"]));
        plan.approve();
        assert!(
            plan.steps().iter().all(|s| s.contract.is_none()),
            "a proposal states titles, never contracts"
        );

        let mut second = item("2", "two", TaskStatus::InProgress);
        second.contract = Some(TaskContract::DefinitionOfDone(DefinitionOfDone::new(
            Check::new("the suite is green", CheckMechanism::Known(CheckKind::Unit)),
            Vec::new(),
        )));
        plan.apply_board(&[item("1", "one", TaskStatus::Completed), second]);

        let steps = plan.steps();
        assert!(steps[0].contract.is_none(), "no contract stated");
        let contract = steps[1].contract.as_ref().expect("the board stated one");
        assert_eq!(contract.checks().count(), 1);
    }

    /// A proposal at the gate is a *new* plan, so the previous one's lanes and
    /// evidence do not carry over — they would attribute work to steps that
    /// have not run.
    #[test]
    fn a_new_proposal_drops_the_previous_plans_lanes_and_ledger() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one"]));
        plan.approve();
        plan.lanes = Some(PlanLanes {
            planned: vec!["one".into()],
            actual: vec![ActualStep {
                title: "one".into(),
                cause: None,
            }],
        });
        plan.ledger.insert("1".to_string(), StepLedger::default());

        plan.propose(&proposal(&["one", "two"]));
        assert!(plan.lanes.is_none(), "the lanes belonged to the old plan");
        assert!(plan.ledger.is_empty(), "so did the evidence");
    }

    /// SPEC 7.4 requires a cause on every divergence, so an actual step with
    /// one is drift and a step without is not — there is no third state to
    /// get wrong.
    #[test]
    fn only_an_actual_step_with_a_cause_counts_as_drift() {
        let lanes = PlanLanes {
            planned: vec!["a".into(), "b".into()],
            actual: vec![
                ActualStep {
                    title: "a".into(),
                    cause: None,
                },
                ActualStep {
                    title: "fix borrow err".into(),
                    cause: Some("E0502 borrow".into()),
                },
                ActualStep {
                    title: "b".into(),
                    cause: None,
                },
            ],
        };
        assert_eq!(lanes.divergences(), 1);
        assert!(lanes.actual[1].is_drift());
        assert!(!lanes.actual[0].is_drift());
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

    /// **The de-conflation.** `✗` is the only red mark SPEC 4 has, so a
    /// blocked step and an abandoned one draw the same glyph — but they are no
    /// longer the same fact, and the row says which. Before this the board had
    /// one word for both and the note always read `cancelled`, including for
    /// work a red gate had stopped.
    #[test]
    fn a_blocked_step_and_a_cancelled_one_are_told_apart_by_their_note() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two"]));
        plan.approve();
        plan.apply_board(&[
            item("1", "one", TaskStatus::Blocked),
            item("2", "two", TaskStatus::Cancelled),
        ]);
        let steps = plan.steps();
        assert_eq!(steps[0].state, PlanStepState::Blocked);
        assert_eq!(steps[1].state, PlanStepState::Blocked);
        assert_eq!(steps[0].note, None, "a gate stopped it; nobody dropped it");
        assert_eq!(steps[1].note.as_deref(), Some("cancelled"));
        assert_eq!(plan.state, PlanState::Error);
    }

    /// A task whose checks are running is neither working nor done: it folds
    /// to its own state, and the plan reads as started rather than complete.
    #[test]
    fn a_verifying_step_folds_to_its_own_state_and_is_not_yet_complete() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two"]));
        plan.approve();
        plan.apply_board(&[
            item("1", "one", TaskStatus::Completed),
            item("2", "two", TaskStatus::Verify),
        ]);
        let steps = plan.steps();
        assert_eq!(steps[1].state, PlanStepState::Verify);
        assert_eq!(plan.progress(), (1, 2), "a gate in flight is not a pass");
        assert_eq!(plan.state, PlanState::Started);
        assert!(steps[1].state.is_open());
    }

    /// `finish` reaches the gate too: a verdict that never arrived is not a
    /// green one, so the row must stop implying one is coming.
    #[test]
    fn a_turn_ending_mid_gate_does_not_leave_the_step_verifying() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one"]));
        plan.approve();
        plan.apply_board(&[item("1", "one", TaskStatus::Verify)]);
        plan.finish();
        let steps = plan.steps();
        assert_eq!(steps[0].state, PlanStepState::Blocked);
        assert_eq!(steps[0].note.as_deref(), Some("cancelled"));
    }

    /// SPEC 8.1 unblocks a red gate on green, so a blocked step is still
    /// moveable — and the card's `x` is offered on exactly the open rows.
    #[test]
    fn only_a_complete_step_is_settled() {
        for state in [
            PlanStepState::Planned,
            PlanStepState::Started,
            PlanStepState::Verify,
            PlanStepState::Blocked,
            PlanStepState::DriftInserted,
        ] {
            assert!(state.is_open(), "{state:?} can still move");
        }
        assert!(!PlanStepState::Complete.is_open());
    }

    /// A step the plan did not contain is queued work that arrived late, so
    /// its presence alone must not promote an approved plan to `working` —
    /// nothing has been done yet.
    #[test]
    fn a_drift_inserted_step_is_not_progress() {
        assert!(!PlanStepState::DriftInserted.has_begun());
        assert!(!PlanStepState::Planned.has_begun());
        for state in [
            PlanStepState::Started,
            PlanStepState::Verify,
            PlanStepState::Complete,
            PlanStepState::Blocked,
        ] {
            assert!(state.has_begun(), "{state:?} means work happened");
        }
    }

    /// The wire carries no status that asserts drift, so no board snapshot can
    /// fold into it — see [`PlanStepState::DriftInserted`]. Pinned because the
    /// obvious "add a `TaskStatus::DriftInserted`" would let a producer claim
    /// drift the plan graph does not show (#5037).
    #[test]
    fn no_board_snapshot_can_fold_a_step_into_drift_inserted() {
        for status in [
            TaskStatus::Pending,
            TaskStatus::InProgress,
            TaskStatus::Completed,
            TaskStatus::Cancelled,
            TaskStatus::Verify,
            TaskStatus::Blocked,
        ] {
            let mut plan = Plan::default();
            plan.apply_board(&[item("1", "one", status)]);
            assert_ne!(plan.steps()[0].state, PlanStepState::DriftInserted);
        }
    }

    /// The lanes are the one thing that *can* reach it — SPEC 7.3's drift row,
    /// derived from the two lanes disagreeing rather than from a status
    /// somebody set.
    #[test]
    fn a_step_the_plan_did_not_contain_renders_as_drift_with_its_tag() {
        let mut plan = Plan::default();
        plan.apply_board(&[
            item("1", "read the routes", TaskStatus::Completed),
            item("2", "repair the tests gate", TaskStatus::Completed),
        ]);
        plan.lanes = Some(PlanLanes {
            planned: vec!["read the routes".into()],
            actual: vec![
                ActualStep {
                    title: "read the routes".into(),
                    cause: None,
                },
                ActualStep {
                    title: "repair the tests gate".into(),
                    cause: Some("E0432: unresolved import".into()),
                },
            ],
        });

        let steps = plan.steps();
        assert_eq!(
            steps[0].state,
            PlanStepState::Complete,
            "planned work is not drift"
        );
        assert_eq!(steps[1].state, PlanStepState::DriftInserted);
        assert_eq!(steps[1].note.as_deref(), Some("inserted"));
    }

    /// A step that was both cancelled and inserted says both — the tag is
    /// appended rather than written over the note that was already there.
    #[test]
    fn a_drift_step_keeps_the_note_it_already_had() {
        let mut plan = Plan::default();
        plan.apply_board(&[item("1", "drop it", TaskStatus::Cancelled)]);
        plan.lanes = Some(PlanLanes {
            planned: Vec::new(),
            actual: vec![ActualStep {
                title: "drop it".into(),
                cause: Some("the driver asked for it".into()),
            }],
        });
        assert_eq!(
            plan.steps()[0].note.as_deref(),
            Some("cancelled · inserted")
        );
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
