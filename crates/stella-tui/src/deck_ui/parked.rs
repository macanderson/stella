// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Key routing for the **parked asks** — the two overlays that each hold a
//! live tool call open while they are up (#4220, #4240).
//!
//! They are claimed ahead of every other modal on the deck, the routing card
//! included, for a reason none of the others share: a turn stopped waiting
//! for an answer cannot make progress on any key that is not that answer.
//! Everything else the deck can be modal about — the queue editor, a config
//! panel, help — is something the user opened while the session was free to
//! keep running.
//!
//! Its own module rather than lines in `deck_ui.rs`, matching
//! [`super::dispatch`] right beside it in the routing order, and because that
//! file is a god file closed to growth (`scripts/file-size-baseline.txt`).

use crossterm::event::KeyEvent;

use crate::envelope::WorkspaceInput;
use crate::views::approval::ApprovalAction;
use crate::views::question::QuestionAction;

use super::{DeckAction, DeckUi};

/// Route `key` to whichever parked ask owns the keyboard.
///
/// `None` means neither is up (or the key was Ctrl-C, which both decline so
/// the deck's quit branch still fires — [`super::dispatch::handle_key`]
/// takes the same line for the same reason).
///
/// **Approval is asked first.** It gates a call that is about to execute,
/// where a question is a decision still being deliberated; and it carries
/// the shorter of the two deadlines, so it is also the one that expires out
/// from under the driver if made to wait its turn. Rendering
/// mirrors this — `deck_render` draws the approval card last, so it lands on
/// top of the overlay whose keys it is already taking.
pub(super) fn handle_key(key: KeyEvent, ui: &mut DeckUi) -> Option<DeckAction> {
    match ui.approval.key(key) {
        ApprovalAction::Ignored => {}
        ApprovalAction::Handled => return Some(DeckAction::Handled),
        ApprovalAction::Resolve(response) => {
            ui.approval.close();
            return Some(DeckAction::Send(WorkspaceInput::ApprovalAnswered(
                Box::new(response),
            )));
        }
    }
    match ui.question.key(key) {
        QuestionAction::Ignored => None,
        QuestionAction::Handled => Some(DeckAction::Handled),
        QuestionAction::Resolve(outcome) => {
            ui.question.close();
            Some(DeckAction::Send(WorkspaceInput::QuestionAnswered(
                Box::new(outcome),
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};
    use stella_tools::registry::approval::{ApprovalRequest, ApprovalResponse};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn approval() -> ApprovalRequest {
        ApprovalRequest {
            parked: stella_tools::registry::approval::ApprovalSubject::Tool {
                name: "bash".into(),
                read_only: false,
            },
            reason: "matched rule no-destructive-shell".into(),
            gate: "command.started".into(),
            subject: Some("rm -rf build/".into()),
        }
    }

    fn question() -> stella_protocol::QuestionRequest {
        stella_protocol::QuestionRequest {
            asker: None,
            questions: vec![stella_protocol::Question {
                header: "Scope".into(),
                question: "How far should this go?".into(),
                options: vec![stella_protocol::QuestionOption {
                    label: "Just the parser".into(),
                    description: String::new(),
                }],
                multi_select: false,
            }],
        }
    }

    /// Nothing parked means nothing claimed, so the caller can route every
    /// key here unconditionally and ahead of everything else.
    #[test]
    fn nothing_parked_claims_nothing() {
        let mut ui = DeckUi::default();
        for code in [KeyCode::Esc, KeyCode::Enter, KeyCode::Char('n')] {
            assert!(handle_key(key(code), &mut ui).is_none());
        }
    }

    /// **Approval wins the keyboard when both are up.** It carries the
    /// shorter of the two deadlines, so a card made to wait its turn is a
    /// card that expires under the driver — and the call it gates is about
    /// to run either way.
    #[test]
    fn an_approval_takes_the_keyboard_from_a_parked_question() {
        let mut ui = DeckUi::default();
        ui.question.open(question());
        ui.approval.open(approval());

        // Esc denies the approval and leaves the question standing.
        let Some(DeckAction::Send(WorkspaceInput::ApprovalAnswered(response))) =
            handle_key(key(KeyCode::Esc), &mut ui)
        else {
            panic!("the approval must claim the key");
        };
        assert!(matches!(*response, ApprovalResponse::Deny { .. }));
        assert!(!ui.approval.is_open(), "the approval card came down");
        assert!(
            ui.question.is_open(),
            "the question underneath is untouched — it is a separate parked call"
        );

        // With the approval gone the question gets the keyboard back.
        assert!(matches!(
            handle_key(key(KeyCode::Esc), &mut ui),
            Some(DeckAction::Send(WorkspaceInput::QuestionAnswered(_)))
        ));
    }

    /// Ctrl-C is declined by both, so the deck's quit branch still fires from
    /// inside a parked ask. Otherwise a card would be the one state a user
    /// cannot get out of.
    #[test]
    fn ctrl_c_is_declined_by_both() {
        let mut ui = DeckUi::default();
        ui.question.open(question());
        ui.approval.open(approval());
        assert!(
            handle_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &mut ui
            )
            .is_none()
        );
        assert!(ui.approval.is_open() && ui.question.is_open());
    }
}
