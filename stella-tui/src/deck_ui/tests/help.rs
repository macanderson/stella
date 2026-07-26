//! Help overlay and the inspect/context overlays.

use super::*;

// ── Help overlay ───────────────────────────────────────────────────────

#[test]
fn question_mark_opens_help_overlay() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    assert!(!ui.help_open);
    handle_deck_key(ch('?'), &model, &mut ui);
    assert!(ui.help_open, "? opens the help overlay");
}

#[test]
fn show_help_inbound_opens_the_overlay_at_the_top() {
    let mut model = WorkspaceModel::new();
    let mut ui = ready_ui();
    // Simulate a prior scroll so we can prove ShowHelp resets it.
    ui.help_scroll.top = 42;
    ui.help_scroll.follow = false;
    ingest_inbound(&Inbound::ShowHelp, &mut model, &mut ui);
    assert!(ui.help_open, "ShowHelp opens the overlay");
    assert_eq!(ui.help_scroll.top, 0, "ShowHelp resets scroll to the top");
    assert!(!ui.help_scroll.follow);
}

#[test]
fn help_overlay_scrolls_with_arrow_keys() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    ui.help_open = true;
    ui.help_scroll.follow = false;
    // Fake a tall document so scrolling is meaningful.
    ui.metrics.help_total = 100;
    ui.metrics.help_height = 10;
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.help_scroll.top, 1, "↓ scrolls down one line");
    handle_deck_key(key(KeyCode::PageDown), &model, &mut ui);
    assert!(ui.help_scroll.top > 1, "PageDown scrolls by a page");
}

#[test]
fn help_overlay_closes_with_esc_or_q() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    ui.help_open = true;
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.help_open, "Esc closes the help overlay");
    // Re-open and close with q.
    ui.help_open = true;
    handle_deck_key(ch('q'), &model, &mut ui);
    assert!(!ui.help_open, "q closes the help overlay");
}

#[test]
fn help_overlay_does_not_close_on_random_key() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    ui.help_open = true;
    // A letter that isn't q or ? must NOT close the overlay (it used to —
    // "any key closes" made the long content unreadable).
    handle_deck_key(ch('x'), &model, &mut ui);
    assert!(ui.help_open, "a random key does not close the overlay");
}

fn call(step: u64, call_seq: u64, role: &str) -> crate::envelope::RecordedCallInfo {
    crate::envelope::RecordedCallInfo {
        turn_instance: 0,
        step,
        call_seq,
        call_role: role.into(),
        provider: "anthropic".into(),
        model: "opus".into(),
        estimated_input_tokens: 100,
    }
}

#[test]
fn ctrl_g_opens_inspect_and_asks_the_driver_for_the_call_index() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    assert!(!ui.inspect_open);
    let action = handle_deck_key(ctrl('g'), &model, &mut ui);
    assert!(ui.inspect_open, "⌃g opens the INSPECT overlay");
    assert!(
        matches!(action, DeckAction::Send(WorkspaceInput::InspectRefresh)),
        "opening asks for a fresh index: {action:?}"
    );
}

#[test]
fn enter_on_a_call_requests_that_calls_coordinate_not_the_first() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    handle_deck_key(ctrl('g'), &model, &mut ui);
    ingest_inbound(
        &Inbound::RecordedCalls(vec![
            call(0, 0, "worker"),
            call(3, 1, "summarization"),
            call(3, 0, "worker"),
        ]),
        &mut WorkspaceModel::new(),
        &mut ui,
    );
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert!(
        matches!(
            action,
            DeckAction::Send(WorkspaceInput::InspectCall {
                turn_instance: 0,
                step: 3,
                call_seq: 1,
            })
        ),
        "⏎ reconstructs the SELECTED call — the summarizer here: {action:?}"
    );
    assert!(ui.inspect_pending, "the detail shows a pending state");
}

#[test]
fn enter_on_an_empty_index_is_a_no_op_not_a_request_for_call_zero() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    handle_deck_key(ctrl('g'), &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert!(
        matches!(action, DeckAction::Handled),
        "no calls recorded — ⏎ must not fabricate a coordinate: {action:?}"
    );
    assert!(!ui.inspect_pending);
}

#[test]
fn esc_steps_back_from_the_detail_to_the_list_before_closing() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    handle_deck_key(ctrl('g'), &model, &mut ui);
    ingest_inbound(
        &Inbound::InspectedCall(Box::new(crate::envelope::InspectView {
            call: call(0, 0, "worker"),
            messages: vec![crate::envelope::InspectMessage {
                role: "system".into(),
                content: "You are Stella.".into(),
            }],
            verified: true,
            unresolved: 0,
            digest_mismatches: 0,
        })),
        &mut WorkspaceModel::new(),
        &mut ui,
    );
    assert!(ui.inspect_view.is_some());
    assert!(!ui.inspect_pending, "the answer clears the pending flag");

    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(ui.inspect_view.is_none(), "esc returns to the call list");
    assert!(ui.inspect_open, "…and does NOT close the overlay");

    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.inspect_open, "a second esc closes it");
}

#[test]
fn inspect_selection_cannot_run_past_the_last_call() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    handle_deck_key(ctrl('g'), &model, &mut ui);
    ingest_inbound(
        &Inbound::RecordedCalls(vec![call(0, 0, "worker"), call(1, 0, "worker")]),
        &mut WorkspaceModel::new(),
        &mut ui,
    );
    for _ in 0..10 {
        handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    }
    assert_eq!(
        ui.inspect_sel, 1,
        "the selection indexes a real row — render must be able to trust it"
    );
}

#[test]
fn inspect_is_modal_and_does_not_leak_keys_to_the_composer() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    handle_deck_key(ctrl('g'), &model, &mut ui);
    handle_deck_key(ch('x'), &model, &mut ui);
    assert!(
        ui.composer.buffer().is_empty(),
        "a letter typed over the overlay must not reach the composer"
    );
    assert!(ui.inspect_open, "…and must not close it either");
}
