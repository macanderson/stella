//! The focused-agent gates: the `ask_user` question and the hunk-review card,
//! and specifically who owns the reviewer's *submission* while one is pending.
//!
//! The rule the gates follow: a pending, unanswered card claims the submit
//! chord. Without that, typing is a way to make a gate unanswerable — the deck
//! reads a mid-turn submission as a new request and spawns a sidecar
//! sub-session for it, leaving the gate parked with nobody answering it.
//!
//! The scope-review dialog was the third gate here and its tests lived at the
//! top of this file. It was removed in #3861: nothing has raised that card
//! since the staged pipeline was deleted (#3865), so the keys answered a
//! question no door asked.

use super::*;

fn type_str(s: &str, model: &WorkspaceModel, ui: &mut DeckUi) {
    for c in s.chars() {
        handle_deck_key(ch(c), model, ui);
    }
}

/// With no gate pending, a submission is still a prompt — the sidecar path
/// is correct behavior and must not be collateral damage.
#[test]
fn without_a_pending_gate_a_submission_is_still_an_ordinary_prompt() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();

    type_str("go look at the graph", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "go look at the graph".into()
        })
    );
}

// ---------------------------------------------------------------------------
// The STATE overlay (ctrl+s)
// ---------------------------------------------------------------------------

/// The complaint this overlay answers: an approved scope had no way back.
/// `⌃S` is the way back, and it toggles from anywhere.
#[test]
fn ctrl_s_opens_and_closes_the_plan_card() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    assert!(!ui.cards.is_open());
    assert_eq!(
        handle_deck_key(ctrl('s'), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(
        ui.cards.open,
        Some(crate::deck_ui::cards::Card::Plan),
        "ctrl+s raises the plan"
    );
    assert_eq!(
        handle_deck_key(ctrl('s'), &model, &mut ui),
        DeckAction::Handled
    );
    assert!(!ui.cards.is_open(), "a second ctrl+s lowers it");
}

/// Every way in has a way out — Esc closes it like every other card.
#[test]
fn esc_closes_the_plan_card() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    handle_deck_key(ctrl('s'), &model, &mut ui);
    handle_deck_key(KeyEvent::from(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.cards.is_open());
}

/// Modal while open: a keystroke must not reach the composer behind it, or
/// opening the overlay to check a plan would quietly edit the prompt.
#[test]
fn the_state_overlay_owns_the_keyboard_while_it_is_up() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    handle_deck_key(ctrl('s'), &model, &mut ui);
    type_str("hello", &model, &mut ui);
    assert!(
        ui.composer.is_empty(),
        "typing leaked into the composer behind the overlay: {:?}",
        ui.composer.buffer()
    );
}

/// The collision this binding has to avoid: `ctrl+s` is the *save* chord in the
/// SKILLS and SETTINGS editors, and those are modal. The card is dispatched
/// after them on purpose — a save must never be stolen by a detail surface.
#[test]
fn ctrl_s_still_saves_inside_a_modal_editor() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.tab = crate::deck_ui::DeckTab::Settings;
    ui.engine.focused = true;
    handle_deck_key(ctrl('s'), &model, &mut ui);
    assert!(
        !ui.cards.is_open(),
        "the plan card stole the engine panel's save chord"
    );
}

// ── per-hunk approval (#1265) ───────────────────────────────────────────────

/// A two-hunk approval card, raised on the lead.
fn hunk_card() -> Inbound {
    Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::HunkReview {
            proposal: stella_protocol::HunkProposal {
                id: "hunk-review-1".into(),
                tool: "apply_edits".into(),
                hunks: vec![
                    stella_protocol::ProposedHunk {
                        path: "a.rs".into(),
                        diff: "@@ -1,1 +1,1 @@\n-a\n+A\n".into(),
                        lines_added: 1,
                        lines_removed: 1,
                    },
                    stella_protocol::ProposedHunk {
                        path: "a.rs".into(),
                        diff: "@@ -9,1 +9,1 @@\n-b\n+B\n".into(),
                        lines_added: 1,
                        lines_removed: 1,
                    },
                ],
            },
        },
    }
}

fn decision(accepted: Vec<usize>) -> DeckAction {
    DeckAction::Send(WorkspaceInput::ToAgent {
        agent: "lead".into(),
        input: UserInput::HunkDecision {
            id: "hunk-review-1".into(),
            accepted,
        },
    })
}

/// A fresh card arrives pre-marked accepted, so a bare ⏎ applies what the model
/// proposed — the same thing that would have happened with no gate at all.
#[test]
fn a_bare_enter_at_a_hunk_card_applies_every_hunk() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![0, 1])
    );
}

/// The issue's shape at the key layer: move to the second hunk, unmark it, and
/// the decision that goes out keeps exactly the first.
#[test]
fn space_unmarks_the_hunk_under_the_cursor() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Down), &model, &mut ui),
        DeckAction::Ignored
    );
    assert_eq!(
        handle_deck_key(ch(' '), &model, &mut ui),
        DeckAction::Ignored
    );
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![0])
    );
}

/// Esc declines everything — a real answer (apply nothing), not a fall-through
/// to the turn-stop chain, which for a pipeline turn is a hard cancel.
#[test]
fn esc_at_a_hunk_card_declines_every_hunk() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        decision(vec![])
    );
}

/// A typed selection is the other way to answer, for a reviewer who would
/// rather name the hunks than walk them.
#[test]
fn a_typed_selection_answers_the_hunk_card() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    type_str("2", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![1])
    );
}

/// Prose is NOT an answer here. There is no revise channel to absorb it, and a
/// generous reading at this gate writes files — so the card stays up and the
/// submission must never escape to the driver as a new request.
#[test]
fn prose_at_a_hunk_card_is_ignored_rather_than_guessed_at() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    type_str("keep the first one please", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Ignored
    );
}

/// The latch: the pending card clears only on the host's follow-on event, so a
/// second ⏎ during that round-trip must not re-send the decision.
#[test]
fn a_second_enter_does_not_re_answer_a_hunk_card() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![0, 1])
    );
    assert!(!matches!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::ToAgent {
            input: UserInput::HunkDecision { .. },
            ..
        })
    ));
}

/// While the composer holds a line, the mark keys belong to the text — the
/// space bar types a space. This is the rule the scope card learned the hard
/// way, applied before it could bite twice.
#[test]
fn space_types_a_space_when_the_composer_is_not_empty() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    type_str("1", &model, &mut ui);
    handle_deck_key(ch(' '), &model, &mut ui);
    type_str("2", &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "1 2");
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![0, 1])
    );
}

/// Scrollback must survive a pending card: `↑`/`↓` drive the hunk cursor, but
/// `PgUp`/`PgDn` stay pure transcript scroll, so a reviewer can still read back
/// for context before deciding what to keep.
#[test]
fn page_keys_still_scroll_the_transcript_while_a_hunk_card_is_up() {
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    assert!(
        !matches!(
            handle_deck_key(key(KeyCode::PageUp), &model, &mut ui),
            DeckAction::Ignored
        ),
        "PgUp must not be swallowed by the card"
    );
    // …and the card is still answerable afterwards.
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![0, 1])
    );
}
