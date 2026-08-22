//! The tab bar and the per-tab controls that only fire on an empty composer:
//! ⌃C from anywhere, the AGENTS tab's focus/stop keys, and the TRACES filter.

use super::*;

#[test]
fn ctrl_c_quits_from_any_tab() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Graph;
    assert_eq!(
        handle_deck_key(ctrl('c'), &model, &mut ui),
        DeckAction::Quit
    );
}

#[test]
fn agents_tab_controls_fire_only_when_composer_empty() {
    let model = model_with(&["lead", "sub"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Agents;
    ui.focused = 1;
    // 's' with empty composer → Stop control for the focused agent.
    assert_eq!(
        handle_deck_key(ch('s'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Control {
            agent: "sub".into(),
            control: AgentControl::Stop,
        })
    );
    // With text typed, 's' types instead.
    handle_deck_key(ch('h'), &model, &mut ui);
    handle_deck_key(ch('s'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "hs");
}

#[test]
fn agents_updown_moves_focus_and_enter_opens_session() {
    let model = model_with(&["a", "b", "c"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Agents;
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.focused, 2);
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert_eq!(ui.focused, 1);
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Session);
}

#[test]
fn tab_and_backtab_walk_the_tab_bar() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    assert_eq!(ui.tab, DeckTab::Session);

    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Agents);
    handle_deck_key(key(KeyCode::BackTab), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Session);

    // Re-selecting the active tab is a no-op, not an error.
    ui.set_tab(DeckTab::Session);
    assert_eq!(ui.tab, DeckTab::Session);
}

#[test]
fn traces_filter_cycles_through_agents_and_back() {
    let model = model_with(&["a", "b"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Traces;
    assert_eq!(ui.trace_filter, None);
    handle_deck_key(ch('f'), &model, &mut ui);
    assert_eq!(ui.trace_filter.as_deref(), Some("a"));
    handle_deck_key(ch('f'), &model, &mut ui);
    assert_eq!(ui.trace_filter.as_deref(), Some("b"));
    handle_deck_key(ch('f'), &model, &mut ui);
    assert_eq!(ui.trace_filter, None);
}
