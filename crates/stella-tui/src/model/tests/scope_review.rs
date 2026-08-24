// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The scope-review gate (L-E5): what opens it, what closes it, and what
//! survives it.
//!
//! Split out of `model/tests.rs` so that file stays under the ungrandfathered
//! 1500-line ceiling rather than taking a baseline entry (#4217, #3441). Pure
//! relocation: no test was changed, added, or removed in the move.
//!
//! The gate and the plan it carried are deliberately tested together, because
//! the defect they guard is the seam between them: `pending_scope_review` is
//! the gate and correctly clears the moment the engine moves past it, which
//! used to destroy the only copy of the approved steps — the scrollback record
//! keeps a summary and two counts, never `ScopeProposal::steps`. So "the gate
//! closed" and "the plan outlived it" are one property, and a test file that
//! held only the first half would go green on the regression.

use super::*;

#[test]
fn scope_review_sets_then_clears_on_next_stage() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ScopeReview {
        proposal: ScopeProposal {
            summary: "big refactor".into(),
            steps: vec!["s1".into(), "s2".into()],
            estimated_files: 12,
            estimated_cost_usd: Some(1.0),
            ..Default::default()
        },
    });
    assert!(model.pending_scope_review.is_some());
    // The scope-review stage marker itself must NOT clear it.
    model.apply(&AgentEvent::Stage {
        name: StageKind::ScopeReview.into(),
        scope: stella_protocol::StageScope::Run,
    });
    assert!(model.pending_scope_review.is_some());
    // The engine moving on to execute clears it.
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute.into(),
        scope: stella_protocol::StageScope::Run,
    });
    assert!(model.pending_scope_review.is_none());
    // …but the plan itself survives: the gate closing is the approval, and the
    // approved steps have to stay recallable for the rest of the turn.
    let approved = model.approved_scope.as_ref().expect("the plan is kept");
    assert_eq!(approved.steps, vec!["s1".to_string(), "s2".to_string()]);
}

/// The reported defect, isolated: the steps a user consented to were the one
/// thing the session could no longer show them one minute later. The scrollback
/// record keeps a summary and two counts — never the steps — so dropping the
/// proposal destroyed the only copy.
#[test]
fn an_approved_plan_outlives_the_gate_that_carried_it() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ScopeReview {
        proposal: ScopeProposal {
            summary: "collapse the panels".into(),
            steps: vec!["gate on relevance".into(), "pin the scope".into()],
            estimated_files: 9,
            estimated_cost_usd: Some(1.4),
            ..Default::default()
        },
    });
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute.into(),
        scope: stella_protocol::StageScope::Run,
    });
    model.apply(&AgentEvent::RunComplete {
        model: "glm".into(),
        cost_usd: 0.5,
    });
    let approved = model
        .approved_scope
        .as_ref()
        .expect("the plan survives the whole turn, not just the gate");
    assert_eq!(approved.summary, "collapse the panels");
    assert_eq!(approved.steps.len(), 2);

    // And it belongs to *that* turn: the next one starts unapproved rather
    // than inheriting consent it was never given.
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute.into(),
        scope: stella_protocol::StageScope::Run,
    });
    assert!(
        model.approved_scope.is_none(),
        "a new turn inherited the previous turn's approval"
    );
}

/// A turn that died at the gate was never approved, and must not be recorded
/// as though it were — an abandoned proposal is not a plan.
#[test]
fn a_turn_that_dies_at_the_gate_records_no_approval() {
    for terminal in [
        AgentEvent::Error {
            message: "aborted".into(),
            retryable: false,
        },
        AgentEvent::RunComplete {
            model: "glm".into(),
            cost_usd: 0.01,
        },
    ] {
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::ScopeReview {
            proposal: ScopeProposal {
                summary: "never approved".into(),
                steps: vec!["s1".into()],
                estimated_files: 1,
                estimated_cost_usd: None,
                ..Default::default()
            },
        });
        model.apply(&terminal);
        assert!(model.pending_scope_review.is_none());
        assert!(
            model.approved_scope.is_none(),
            "an abandoned proposal was promoted to an approved plan"
        );
    }
}

#[test]
fn scope_review_clears_on_error_and_complete() {
    for terminal in [
        AgentEvent::Error {
            message: "aborted".into(),
            retryable: false,
        },
        AgentEvent::RunComplete {
            model: "glm".into(),
            cost_usd: 0.01,
        },
    ] {
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::ScopeReview {
            proposal: ScopeProposal {
                summary: "x".into(),
                steps: vec![],
                estimated_files: 1,
                estimated_cost_usd: None,
                ..Default::default()
            },
        });
        assert!(model.pending_scope_review.is_some());
        model.apply(&terminal);
        assert!(model.pending_scope_review.is_none());
    }
}

/// **The #4612 witness.** Approval has a wire signal — the run-scoped stage
/// the gate emits — and a refusal deliberately has none: the gate hands the
/// driver's change request back as the parked `task_start`'s own error and
/// emits nothing (#3861, #4594). So the latch stayed set for the whole window
/// the model spent re-planning, and three surfaces read a decision as pending
/// while nobody was waiting on anything: `deck::classify` said
/// `WaitingInput`, `fleet_dashboard` held the lane `Blocked`, and the rail
/// said `pending approval`.
///
/// It also gives [`crate::plan::Plan::cancel`] its first caller reachable from
/// an event; it existed for exactly this state and nothing could reach it.
#[test]
fn a_refused_plan_stops_claiming_a_decision_is_pending() {
    let mut model = SessionModel::new();
    start_a_plan(&mut model, "1", &["migrate", "backfill", "cut over"]);
    assert!(model.pending_scope_review.is_some());
    assert_eq!(model.plan.state, crate::plan::PlanState::PendingApproval);

    // "Change it first": the gate refuses the call it was parked inside, and
    // that error is the whole of the refusal on the wire.
    model.apply(&AgentEvent::ToolResult {
        call_id: "1".into(),
        output: ToolOutput::error("the plan was not approved — they said: smaller"),
        duration_ms: 400,
        speculated: false,
    });
    assert!(
        model.pending_scope_review.is_none(),
        "nobody is being asked while the model re-plans"
    );
    assert!(
        model.approved_scope.is_none(),
        "a refusal is not an approval"
    );
    assert_eq!(
        model.plan.state,
        crate::plan::PlanState::Cancelled,
        "the rail must stop saying `pending approval` for a plan that was sent back"
    );

    // The revised plan reopens the gate: a cancelled rail is this plan's
    // verdict, not a latch that outlives it.
    start_a_plan(&mut model, "2", &["cut over"]);
    assert!(model.pending_scope_review.is_some());
    assert_eq!(model.plan.state, crate::plan::PlanState::PendingApproval);
}

/// A `task_start` that fails for its own reasons — after the driver approved,
/// so no gate is open — is an ordinary tool error and must not be read as a
/// refusal of a plan nobody re-proposed.
#[test]
fn a_failing_step_after_approval_is_not_a_refusal() {
    let mut model = SessionModel::new();
    start_a_plan(&mut model, "1", &["migrate", "backfill", "cut over"]);
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute.into(),
        scope: stella_protocol::StageScope::Run,
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: "1".into(),
        output: ToolOutput::error("no task with id 1"),
        duration_ms: 3,
        speculated: false,
    });
    assert_eq!(
        model.plan.state,
        crate::plan::PlanState::Approved,
        "the plan was approved and stays approved"
    );
    assert!(model.approved_scope.is_some());
}

/// The gate as the engine emits it: `task_start` announced, then the proposal
/// raised from inside that call, which is the ordering the park creates.
fn start_a_plan(model: &mut SessionModel, call_id: &str, steps: &[&str]) {
    model.apply(&AgentEvent::ToolStart {
        call: stella_protocol::ToolCall {
            call_id: call_id.into(),
            name: stella_tools::tasks::START.into(),
            input: serde_json::json!({ "id": "1" }),
        },
    });
    model.apply(&AgentEvent::ScopeReview {
        proposal: ScopeProposal {
            summary: "ship the migration".into(),
            steps: steps.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        },
    });
}
