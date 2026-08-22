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
