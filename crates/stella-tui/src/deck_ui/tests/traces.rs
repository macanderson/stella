//! **The witness (#4429).** The TRACES tab's one row of [`crate::keymap`] —
//! `f`, "cycle the per-agent filter" — pressed through
//! [`super::handle_deck_key`].
//!
//! The cycle test moved here from `tabs.rs`, which is about the tab bar: a row
//! whose witness lives under another tab's file is a row #4368's guard cannot
//! find. The composer gate below is new — `f` is a letter, and a letter verb
//! that fires while a prompt is being typed is the defect the gate exists to
//! stop.

use super::*;

/// `f` walks the registered agents in order and then returns to unfiltered,
/// so the whole timeline is always one more press away.
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

/// With text in the prompt, `f` is the next character of it.
#[test]
fn traces_filter_types_when_the_composer_has_text() {
    let model = model_with(&["a", "b"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Traces;
    handle_deck_key(ch('o'), &model, &mut ui);
    handle_deck_key(ch('f'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "of");
    assert_eq!(ui.trace_filter, None, "the filter did not cycle");
}

/// Nothing to filter by is not a filter on nothing: with no agents registered
/// the press leaves the timeline unfiltered rather than selecting an absent
/// agent and rendering an empty tab.
#[test]
fn traces_filter_stays_unfiltered_with_no_agents() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    ui.tab = DeckTab::Traces;
    handle_deck_key(ch('f'), &model, &mut ui);
    assert_eq!(ui.trace_filter, None);
}
