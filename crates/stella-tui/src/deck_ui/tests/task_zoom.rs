//! The task zoom's keys (SPEC 7.5): how it opens, how it gets out of the way,
//! and the five action verbs that are drawn but not yet wired.

use super::*;
use crate::deck_ui::cards::Card;
use stella_protocol::{TaskItem, TaskStatus};

/// A lead with a three-row board, so a selection can be moved and zoomed.
fn model_with_board() -> WorkspaceModel {
    let mut m = model_with(&["lead"]);
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::TaskUpdate {
            tasks: ["one", "two", "three"]
                .iter()
                .enumerate()
                .map(|(i, subject)| TaskItem {
                    id: (i + 1).to_string(),
                    subject: (*subject).to_string(),
                    description: None,
                    status: TaskStatus::Pending,
                    owner: None,
                    contract: None,
                })
                .collect(),
        },
    });
    m
}

/// **The defect #5041 names.** `⏎` on the plan card used to flip a
/// `plan_expanded` flag no renderer read, so the key did nothing at all. It
/// now raises the zoom on the selected step.
#[test]
fn enter_on_the_plan_card_zooms_the_selected_step() {
    let model = model_with_board();
    let mut ui = ready_ui();
    ui.cards.raise(Card::Plan);
    assert_eq!(
        handle_deck_key(key(KeyCode::Down), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.cards.plan_sel, 1);

    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Handled,
        "the zoom claims ⏎ rather than letting it queue a prompt"
    );
    assert_eq!(ui.cards.open, Some(Card::TaskZoom));
    assert_eq!(ui.cards.plan_sel, 1, "it zooms the step that was selected");
}

/// `esc` on the zoom is a step back, not a way out: it returns to the plan
/// card **on the step that was zoomed**, which is why opening the zoom does
/// not go through `CardState::raise` (that one resets the selection).
#[test]
fn esc_leaves_the_zoom_for_the_plan_card_on_the_same_step() {
    let model = model_with_board();
    let mut ui = ready_ui();
    ui.cards.raise(Card::Plan);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.cards.open, Some(Card::TaskZoom));

    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.cards.open, Some(Card::Plan), "esc backs out one level");
    assert_eq!(ui.cards.plan_sel, 2, "onto the step that was zoomed");

    // A second esc leaves the plan card the way it always did, so the pair is
    // a way out and not a loop between two cards.
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.cards.is_open());
}

/// `⌃S` raised the plan card, so it closes the whole surface from inside the
/// zoom too. A chord that only opens is a trap.
#[test]
fn ctrl_s_closes_the_whole_plan_surface_from_inside_the_zoom() {
    let model = model_with_board();
    let mut ui = ready_ui();
    ui.cards.raise(Card::Plan);
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.cards.open, Some(Card::TaskZoom));
    assert_eq!(
        handle_deck_key(ctrl('s'), &model, &mut ui),
        DeckAction::Handled
    );
    assert!(!ui.cards.is_open());
}

/// The five action verbs are drawn and inert (#5149–#5153). Inert has to mean
/// *swallowed*: a bare `s` reaching the composer while the reader is looking
/// at the zoom would type into the prompt behind it, which is worse than a key
/// that does nothing.
#[test]
fn the_zoom_action_verbs_are_drawn_and_inert_and_never_reach_the_composer() {
    let model = model_with_board();
    let mut ui = ready_ui();
    ui.cards.raise(Card::Plan);
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);

    for verb in ['r', 's', 'b', 'i', stella_tui_theme::glyph::DRIFT] {
        assert_eq!(
            handle_deck_key(ch(verb), &model, &mut ui),
            DeckAction::Handled,
            "`{verb}` must be swallowed by the zoom"
        );
        assert_eq!(
            ui.cards.open,
            Some(Card::TaskZoom),
            "`{verb}` leaves the zoom up"
        );
        assert!(
            ui.composer.buffer().is_empty(),
            "`{verb}` reached the composer behind the zoom"
        );
    }
}

/// Every verb the action row draws is a verb the key handler knows about. The
/// row is built from `views::task_zoom::ACTION_VERBS`, so this is the join
/// that stops the two from drifting into a row advertising a sixth key.
#[test]
fn the_action_row_and_the_key_handler_agree_on_the_verbs() {
    let drawn: Vec<&str> = crate::views::task_zoom::ACTION_VERBS
        .iter()
        .map(|(key, _)| *key)
        .collect();
    assert_eq!(drawn, ["r", "s", "b", "i"]);
}
