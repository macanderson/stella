//! Composer editing and submission: what a bare ⏎ does versus a modified one,
//! cursor movement inside a multi-line draft, and where the first submission
//! after a stop-and-hold lands in the queue.

use super::*;

/// The newline chord — `⌘⏎` as the kitty keyboard protocol reports it
/// (a modified Enter inserts a line break; a bare Enter submits).
fn cmd_enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::SUPER)
}
fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

#[test]
fn only_tab_switches_tabs_and_digits_always_type() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    assert_eq!(ui.tab, DeckTab::Session);
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Agents);
    handle_deck_key(key(KeyCode::BackTab), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Session);
    // A digit with an empty composer starts the prompt — it never jumps
    // to a tab, so prompts can begin with 1–5.
    handle_deck_key(ch('3'), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Session, "digit typed, tab unchanged");
    handle_deck_key(ch('h'), &model, &mut ui);
    handle_deck_key(ch('2'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "3h2");
}

#[test]
fn bare_enter_always_enqueues_a_prompt_without_blocking() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "do the thing".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "do the thing".into()
        })
    );
    assert!(
        ui.composer.buffer().is_empty(),
        "composer clears after submit"
    );
}

#[test]
fn a_modified_enter_inserts_a_line_break_preserved_through_submit() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "line one".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(cmd_enter(), &model, &mut ui),
        DeckAction::Handled,
        "⌘⏎ is a line break, not a submit"
    );
    for c in "line two".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "line one\nline two".into()
        }),
        "the typed line break survives into the submitted prompt"
    );
}

#[test]
fn plain_enter_on_a_blank_composer_inserts_nothing() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Graph; // a tab with no Enter binding of its own
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Ignored
    );
    assert!(ui.composer.buffer().is_empty(), "no stray leading newline");
}

#[test]
fn alt_brackets_jump_the_cursor_to_start_and_end() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "abc".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(ui.composer.cursor(), 3);
    assert_eq!(
        handle_deck_key(alt('['), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.composer.cursor(), 0, "⌥[ → before the first character");
    assert_eq!(
        handle_deck_key(alt(']'), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.composer.cursor(), 3, "⌥] → one past the last character");
}

#[test]
fn bare_enter_queues_and_a_modified_enter_inserts_a_break() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "hi".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
    assert_eq!(
        handle_deck_key(alt_enter, &model, &mut ui),
        DeckAction::Handled,
        "⌥⏎ inserts a line break"
    );
    assert_eq!(ui.composer.buffer(), "hi\n");
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "hi\n".into()
        }),
        "bare ⏎ queues (never blocks)"
    );
}

#[test]
fn arrow_keys_edit_a_multiline_prompt_instead_of_scrolling() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "ab".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(cmd_enter(), &model, &mut ui); // ⌘⏎ inserts a line break
    for c in "cd".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    // ↑ moves the cursor into the first line (not the session scroll,
    // and NOT the queue editor — the composer is not empty).
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert_eq!(ui.composer.cursor(), 2, "column kept on the line above");
    handle_deck_key(key(KeyCode::Left), &model, &mut ui);
    handle_deck_key(ch('X'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "aXb\ncd", "typed at the cursor");
}

#[test]
fn the_first_submission_after_a_hold_enqueues_at_the_front() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.dispatch_held = true;
    for c in "urgent fix".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::EnqueueFront {
            text: "urgent fix".into()
        }),
        "the held submission jumps the queue"
    );
    assert!(!ui.dispatch_held, "the submission releases the hold");
    for c in "later".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "later".into()
        }),
        "after the hold clears, submissions append as usual"
    );
}
