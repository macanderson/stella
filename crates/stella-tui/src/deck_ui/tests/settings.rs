//! **The witness (#4368).** The SETTINGS editor's `r` — "reload from disk" —
//! pressed through [`super::handle_deck_key`].
//!
//! The tab's other rows are witnessed where their vocabulary lives:
//! `views::settings` for the browse-level `← →`, `e` and `t`, and
//! `views::engine` for the editor's `tab`, `⏎`, `space`, `x`, `s / S` and
//! `esc`. Both of those files are closed to growth (`views/engine.rs` is a
//! god file), so the one row neither covered is witnessed here.

use super::*;

/// `r` inside the editor asks the driver for the on-disk config again and
/// says so, rather than quietly leaving the stale working copy on screen.
#[test]
fn settings_r_reloads_the_engine_config_from_disk() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Settings);
    handle_deck_key(ch('e'), &model, &mut ui);
    assert!(ui.engine.focused, "e focused the editor");
    ui.pending_inputs.clear();

    handle_deck_key(ch('r'), &model, &mut ui);
    assert_eq!(
        ui.pending_inputs,
        vec![WorkspaceInput::EngineConfigRefresh],
        "r asks the driver to re-read the config"
    );
    assert!(ui.engine.busy, "and the panel says it is waiting");
}
