// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A plan change can wait on a person. While it waits, this gate stops the
//! tool calls of a turn that is already running (SPEC 8.1 item 3).
//!
//! # The half that was missing
//!
//! `RevisionGate::admits` says whether work may go on. The deck's
//! `gates::revision_hold` holds back new prompts. Neither one can stop a tool
//! call the model has already asked for. This gate can. It runs before the
//! step does. It hands back a [`ToolOutput`] the tool returns in place of its
//! work.
//!
//! # One gate, two tasks
//!
//! A [`SharedRevisions`] is the lane's plan-change gate. The lane's forwarder
//! writes to it, because a gate board reaches this host on that stream and
//! nowhere else. The plan gate reads it, because the turn's plan graph lives
//! here and nowhere else. Two gates would each hold half the answer and
//! disagree about the rest.
//!
//! # A yes goes through the plan graph
//!
//! The person driving says yes on the deck, and that verb puts the repair on
//! the task board (`driver_support::service_approve_revision`). So a board
//! that carries the waiting subject **is** their yes, and the next
//! [`PlanGate::review`] writes it: [`PlanGate::approve_revision`] hands the
//! graph to `RevisionGate::approve`. The `[:NEXT]` edge and the cause come
//! from `PlanGraph::revise`. One writer makes them, so no second path can
//! disagree.

use std::sync::{Arc, Mutex};

use stella_core::{RevisionError, RevisionGate};
use stella_protocol::{ErrorClass, PlanRevision, RevisionProposal, TaskItem, ToolOutput};

use super::PlanGate;

/// One lane's plan-change gate, shared by the two tasks that need it.
///
/// Cheap to clone, and per lane and per turn like the forwarder's own
/// ledgers. A lane with no plan gate still gets one, so its proposals reach
/// the deck the way they always did.
pub(crate) type SharedRevisions = Arc<Mutex<RevisionGate>>;

/// What the model is told while a plan change waits on the person driving.
///
/// It says to stop, not to fix. The model has nothing to fix here: the answer
/// is a person's to give. A retry loop on a hold would burn the whole turn on
/// one refusal.
const REVISION_HELD: &str = "the plan has a change waiting on the person driving, and nothing runs \
     until they answer it. Wait for their answer; do not retry this call, and do not work around \
     it.";

/// What the model is told when the change the person agreed to cannot be
/// written, because the plan moved while it was on the table.
const REVISION_STALE: &str = "the plan change could not be written, so nothing runs until the \
     person driving is asked again.";

impl PlanGate {
    /// Settle the waiting plan change against `board`, and return the refusal
    /// a guarded tool call gets while it is still waiting.
    ///
    /// `None` in three cases: nothing is waiting; there is no plan to write a
    /// yes into; or the board carries the change, so it is written and work
    /// goes on.
    pub(super) fn settle_revision(&self, board: &[TaskItem]) -> Option<ToolOutput> {
        let waiting = self.pending_revision()?;
        let planned = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .graph
            .is_some();
        if !planned {
            // No plan, no hold. A yes is written into this turn's plan graph,
            // so a hold raised before one exists could never be lifted.
            return None;
        }
        if !board.iter().any(|item| item.subject == waiting.subject) {
            return Some(ToolOutput::classified_error(
                ErrorClass::RefusedByPolicy,
                hold_message(REVISION_HELD, &waiting),
            ));
        }
        // The plan can move while a change sits on the table, and the graph
        // then refuses it. The person is asked again rather than having some
        // other change written under their answer, and the hold stands until
        // they are.
        match self.approve_revision() {
            Ok(_) => None,
            Err(_) => Some(ToolOutput::classified_error(
                ErrorClass::RefusedByPolicy,
                hold_message(REVISION_STALE, &waiting),
            )),
        }
    }

    /// The plan change on the table, if there is one.
    pub(crate) fn pending_revision(&self) -> Option<RevisionProposal> {
        self.revisions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pending()
            .cloned()
    }

    /// Say yes to the waiting change: write it into this turn's plan.
    ///
    /// The insert goes through `RevisionGate::approve`. That adds the task
    /// and calls `PlanGraph::revise`. So the new revision, its `[:NEXT]`
    /// chain and the cause it carries all come from the code that writes
    /// every other revision.
    ///
    /// The yes is kept as this turn's approved revision too. Without that,
    /// the next [`PlanGate::review`] reads the longer plan as a plan that
    /// changed. It would put it back to the person who just agreed to it.
    ///
    /// [`RevisionError::NothingPending`] when no plan was ever put up. A
    /// change cannot be written into a plan that does not exist.
    pub(crate) fn approve_revision(&self) -> Result<PlanRevision, RevisionError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(graph) = state.graph.as_mut() else {
            return Err(RevisionError::NothingPending);
        };
        let revision = self
            .revisions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .approve(graph)?;
        state.approved = Some(revision);
        Ok(revision)
    }
}

/// The refusal text: what waits, what it would add, and why.
///
/// It names the gate and the cause, so the model can tell one hold from the
/// next. A bare "wait" would read the same on every board.
fn hold_message(frame: &str, waiting: &RevisionProposal) -> String {
    format!(
        "{frame} {} would add \"{}\", because the {} gate reported: {}",
        waiting.revision,
        waiting.subject,
        waiting.gate,
        waiting.cause.as_str()
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use stella_protocol::{
        Answer, GateBoard, GateRow, GateState, QuestionOutcome, QuestionRequest, TaskStatus,
    };
    use stella_tools::ToolRegistry;
    use stella_tools::registry::question::QuestionResponder;

    use super::super::{APPROVE_LABEL, PlanSetup};
    use super::*;
    use crate::settings::PlanReviewPolicy;

    /// A person who says yes to every plan they are shown.
    struct Approving;

    #[async_trait]
    impl QuestionResponder for Approving {
        async fn respond(&self, _request: &QuestionRequest) -> QuestionOutcome {
            QuestionOutcome::Answered {
                answers: vec![Answer {
                    header: "Plan".into(),
                    question: String::new(),
                    chosen: vec![APPROVE_LABEL.to_string()],
                    note: None,
                }],
            }
        }
    }

    fn row(id: &str, subject: &str) -> TaskItem {
        TaskItem {
            id: id.to_string(),
            subject: subject.to_string(),
            description: None,
            status: TaskStatus::Pending,
            owner: None,
            contract: None,
        }
    }

    fn board(n: usize) -> Vec<TaskItem> {
        (0..n)
            .map(|i| row(&(i + 1).to_string(), &format!("step {}", i + 1)))
            .collect()
    }

    /// What a plugin reported: one gate green, one gate failed.
    fn failing_board() -> GateBoard {
        GateBoard {
            patch: Some("patch-7".into()),
            gates: vec![
                GateRow {
                    name: "fmt".into(),
                    state: GateState::Green,
                    deterministic: true,
                },
                GateRow {
                    name: "tests".into(),
                    state: GateState::Failed {
                        case: "stella_core::loop_detect::a_short_cycle_is_detected".into(),
                        log: "assertion `left == right` failed\n  left: 3\n right: 2\n".into(),
                    },
                    deterministic: true,
                },
            ],
        }
    }

    /// A gate with a person to answer it, the plan-change gate its forwarder
    /// would write to, and the stream it puts its card on. The test holds the
    /// receiver open.
    fn gate(
        registry: &ToolRegistry,
    ) -> (
        PlanGate,
        SharedRevisions,
        tokio::sync::mpsc::UnboundedReceiver<stella_protocol::AgentEvent>,
    ) {
        registry.attach_question_responder(Arc::new(Approving), Duration::from_secs(30));
        let (events, rx) = tokio::sync::mpsc::unbounded_channel();
        let revisions = SharedRevisions::default();
        let gate = PlanGate::install(
            registry.question_broker(),
            events,
            PlanSetup {
                goal: "fix the router".into(),
                policy: PlanReviewPolicy::default(),
                revisions: Arc::clone(&revisions),
            },
        )
        .expect("a person is attached, so the gate is installed");
        (gate, revisions, rx)
    }

    /// What the lane's forwarder does with a failing board: put a plan change
    /// on the shared gate at the number the reader will be shown.
    fn observe(revisions: &SharedRevisions, gate: &PlanGate) -> RevisionProposal {
        let graph = gate.plan_graph().expect("a plan was put up this turn");
        let mut held = revisions.lock().expect("the gate");
        held.observe(
            graph.revision().next(),
            &graph.planned(graph.revision()),
            &failing_board(),
        )
        .expect("a plain gate failure puts a change up")
        .clone()
    }

    /// **The witness.** A gate failed while the turn ran, so a plan change
    /// waits. The next tool call is held back until somebody answers it. It
    /// runs once they say yes, and the plan carries the gate's own cause.
    ///
    /// Nothing here can pass without the plan-change gate this file hangs on
    /// the plan gate. With no gate there is nothing to read, nothing for
    /// `review` to hold back, and no way to write the answer into the plan.
    #[tokio::test]
    async fn a_waiting_plan_change_stops_the_next_tool_call_until_it_is_answered() {
        let registry = ToolRegistry::new(std::path::PathBuf::from("."));
        let (gate, revisions, _events) = gate(&registry);
        let mut plan = board(3);
        assert!(
            gate.review(&plan).await.is_none(),
            "the person driving said start, so r1 runs"
        );

        let waiting = observe(&revisions, &gate);
        assert_eq!(waiting.revision, PlanRevision::new(2).expect("r2"));
        assert_eq!(waiting.gate, "tests");

        let held = gate
            .review(&plan)
            .await
            .expect("nothing runs while a change waits");
        let ToolOutput::Error { message, class } = &held else {
            panic!("a hold is a refusal, not a success: {held:?}");
        };
        assert_eq!(
            *class,
            Some(ErrorClass::RefusedByPolicy),
            "a plane above the call held it back"
        );
        assert!(
            message.contains("r2"),
            "the hold names the change: {message}"
        );
        assert!(
            message.contains(&waiting.subject),
            "the hold says what it would add: {message}"
        );
        assert!(
            message.contains("tests"),
            "the hold names the gate that failed: {message}"
        );

        // The deck's approve verb puts the repair on the board. That row is
        // the person's yes, and the next call reads it.
        plan.push(row("4", &waiting.subject));
        assert!(
            gate.review(&plan).await.is_none(),
            "work goes on once the change is answered"
        );
        assert!(
            gate.pending_revision().is_none(),
            "a written change stops waiting"
        );

        let graph = gate.plan_graph().expect("a plan was put up this turn");
        assert_eq!(graph.revision(), waiting.revision, "the yes wrote r2");
        assert!(
            graph
                .planned(waiting.revision)
                .iter()
                .any(|task| task.subject == waiting.subject),
            "a yes writes the repair into the plan"
        );
        let drift = graph.divergences();
        assert_eq!(drift.len(), 1, "one insert, from PlanGraph::revise");
        assert_eq!(
            drift[0].cause, waiting.cause,
            "the revision carries the gate's own cause"
        );
    }

    /// A change put up before any plan cannot be written, since a plan graph
    /// is what it would be written into. It must not wedge the turn either.
    #[tokio::test]
    async fn a_change_with_no_plan_to_write_it_into_holds_nothing() {
        let registry = ToolRegistry::new(std::path::PathBuf::from("."));
        let (gate, revisions, _events) = gate(&registry);
        revisions
            .lock()
            .expect("the gate")
            .observe(PlanRevision::FIRST, &[], &failing_board())
            .expect("a plain gate failure puts a change up");

        assert_eq!(
            gate.approve_revision(),
            Err(RevisionError::NothingPending),
            "there is no plan, so there is nothing to write into"
        );
        assert!(
            gate.settle_revision(&board(2)).is_none(),
            "a change nobody can write must not wedge the turn"
        );
    }
}
