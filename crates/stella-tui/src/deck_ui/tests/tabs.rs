//! The tab bar itself: `tab` / `⇧tab` walking it, and `⌃C` quitting from
//! wherever it has stopped.
//!
//! Each tab's own keys are witnessed in the file named for that tab —
//! `agents.rs`, `traces.rs`, `graph.rs`, `skills.rs`, `issues.rs` (#4429).

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
