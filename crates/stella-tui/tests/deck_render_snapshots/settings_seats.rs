// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SEATS pane of the SETTINGS tab, with one plugin seat on screen.
//!
//! A child of `deck_render_snapshots`, like [`super::voice`]. The parent file
//! is near its size cap.
//!
//! **The witness.** The pane lists the role this session resolved. Then it
//! lists each role a plugin declares. So one plugin makes two rows. Fed the
//! plugin seats alone, the pane draws one row. It then says nothing at all
//! about the model the rest of the turn runs on.
//!
//! The fixture is its own, not `super::base_ui`'s. The demo config still
//! names five roles the config collapse took away. A golden of this pane
//! would teach words the product does not have.

use stella_tui::envelope::{EngineConfigState, RoleWiringRow};
use stella_tui::{DeckTab, SeatRow, SettingsPane};

use super::{H, W, assert_golden, fixture_model, render_frame, ui_for};

/// One session role and one plugin. That is the least a fixture needs to tell
/// the two halves of the list apart.
fn fixture_seats() -> EngineConfigState {
    EngineConfigState {
        roles: vec![RoleWiringRow {
            role: "default".to_string(),
            model: "zai/glm-5.2-air".to_string(),
            effort: "medium".to_string(),
            thinking: "thinking on".to_string(),
            source: "default_model".to_string(),
            next_session: None,
        }],
        seats: vec![SeatRow {
            key: "acme/reviewer".to_string(),
            // No model, so the frame pins what such a row says: the word
            // `default`, not a blank cell.
            model: None,
            from: "acme".to_string(),
        }],
        ..Default::default()
    }
}

#[test]
fn deck_render_snapshots_pin_the_settings_seats_pane() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Settings);
    ui.settings_pane = SettingsPane::Seats;
    // Through the fold a driver snapshot takes, never by hand. That call
    // writes both copies. A fixture that sets one claims edits the panel
    // does not hold.
    stella_tui::views::engine_panel::ingest_config(&mut ui, &fixture_seats(), &None);
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "pane_settings_seats",
        "the SETTINGS tab's SEATS pane: the session's role, then a plugin's",
        W,
        H,
        &frame,
    );
}
