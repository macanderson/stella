// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The deck's half of SPEC 8.1 item 3: the proposal a failing gate puts up,
//! the three keys that answer it, and the submissions withheld until one of
//! them is pressed.

use super::*;
use stella_protocol::{DivergenceCause, RevisionProposal, plan_graph::PlanRevision};

fn proposal() -> RevisionProposal {
    RevisionProposal {
        revision: PlanRevision::new(2).expect("r2"),
        subject: "repair a_short_cycle_is_detected".into(),
        gate: "tests".into(),
        cause: DivergenceCause::new("assertion `left == right` failed").expect("a cause"),
        issue: None,
    }
}

/// A lane with one ordinary row and then a standing proposal, highlighted.
///
/// The row above matters: it is what the "letters fall through elsewhere" test
/// moves the highlight onto, and a transcript whose only entry is the proposal
/// cannot express "the highlight is somewhere else".
fn proposed(model: &mut WorkspaceModel, ui: &mut DeckUi) {
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Text {
            text: "the tests gate went red".into(),
        },
    });
    ingest_inbound(
        &Inbound::RevisionProposed {
            agent: "lead".into(),
            proposal: Box::new(proposal()),
        },
        model,
        ui,
    );
    handle_deck_key(key(KeyCode::Up), model, ui);
}

/// **The acceptance criterion.** With a proposal standing, a submitted prompt
/// does not dispatch — no `Send`, nothing enqueued — and the composer keeps
/// the text so nobody loses what they typed. Pressing `a` releases it, and the
/// same prompt then dispatches.
///
/// The withholding is what this pins. A test that only checked the proposal
/// renders would witness a picture and call it a gate.
#[test]
fn nothing_dispatches_until_the_proposal_is_answered() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    proposed(&mut model, &mut ui);

    for c in "fix it".chars() {
        handle_deck_key(key(KeyCode::Char(c)), &model, &mut ui);
    }
    let held = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        held,
        DeckAction::Handled,
        "the submission must be withheld, not dispatched"
    );
    assert_eq!(
        ui.composer.buffer(),
        "fix it",
        "a held prompt keeps its text — holding a prompt is not eating one"
    );

    // Answer it. Typing moved no highlight, so `a` lands on the proposal row
    // the setup left it on.
    ui.composer.clear();
    let approved = handle_deck_key(key(KeyCode::Char('a')), &model, &mut ui);
    assert_eq!(
        approved,
        DeckAction::Send(WorkspaceInput::ApproveRevision {
            agent: "lead".into(),
            proposal: Box::new(proposal()),
        }),
        "a sends the approval, carrying the proposal the driver re-checks"
    );
    assert!(
        ui.pending_revisions.is_empty(),
        "and releases the withholding"
    );

    // The hold's own notice is still on screen; clearing it is the reader
    // pressing Esc, and is not part of what this test is about.
    ui.notice.dismiss();
    for c in "fix it".chars() {
        handle_deck_key(key(KeyCode::Char(c)), &model, &mut ui);
    }
    let sent = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert!(
        matches!(
            sent,
            DeckAction::Send(
                WorkspaceInput::Enqueue { .. }
                    | WorkspaceInput::EnqueueNext { .. }
                    | WorkspaceInput::ToAgent { .. }
            )
        ),
        "once answered, the same prompt dispatches: {sent:?}"
    );
}

/// `x dismiss`: the proposal goes, the withholding goes with it, and nothing
/// is sent — a dismissal writes no revision, so there is nothing for a driver
/// to do about it.
#[test]
fn x_dismisses_the_proposal_and_sends_nothing() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    proposed(&mut model, &mut ui);

    let action = handle_deck_key(key(KeyCode::Char('x')), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "x claimed the keystroke");
    assert!(ui.pending_revisions.is_empty());
    assert!(
        ui.notice.entries().iter().any(|n| n.contains("r2")),
        "the dismissal says what it dropped"
    );
}

/// `e edit` hands the proposed subject to the composer and stands the proposal
/// down — see `row_keys`'s `'e'` arm and #5289 for why in-place editing is not
/// this key yet.
#[test]
fn e_hands_the_subject_to_the_composer() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    proposed(&mut model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Char('e')), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.composer.buffer(), "repair a_short_cycle_is_detected");
    assert!(ui.pending_revisions.is_empty());
}

/// The three letters belong to the row, not to the keyboard: with the
/// highlight anywhere else they type, which is what keeps a prompt containing
/// the word `already` from losing its `a`.
#[test]
fn the_letters_type_when_the_highlight_is_not_a_proposal() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    proposed(&mut model, &mut ui);
    // Off the proposal row and onto the one above it.
    ui.session_selected = Some(0);

    for c in "aex".chars() {
        handle_deck_key(key(KeyCode::Char(c)), &model, &mut ui);
    }
    assert_eq!(ui.composer.buffer(), "aex");
    assert_eq!(
        ui.pending_revisions.len(),
        1,
        "and the proposal is still standing"
    );
}

/// A second press on an already-answered row types rather than re-answering:
/// the row stays in the scrollback because it is what the reader saw, and it
/// is no longer a question.
#[test]
fn an_answered_proposal_row_lends_no_more_letters() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    proposed(&mut model, &mut ui);
    handle_deck_key(key(KeyCode::Char('x')), &model, &mut ui);

    handle_deck_key(key(KeyCode::Char('a')), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "a");
}

/// The row is in the scrollback and reads as SPEC 8.1 writes it.
#[test]
fn the_proposal_lands_in_the_transcript() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    proposed(&mut model, &mut ui);

    let lane = model.agents.first().expect("the lead lane");
    assert!(
        lane.model.transcript.iter().any(|entry| matches!(
            entry,
            crate::model::TranscriptEntry::RevisionProposal { proposal } if proposal.gate == "tests"
        )),
        "the proposal is filed where the reader is looking"
    );
}
