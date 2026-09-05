// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `/models` card, with a plugin seat beside the session's own role.
//!
//! A child of `deck_render_snapshots`, like [`super::voice`]. The parent
//! file is near its size cap.
//!
//! **The witness.** `EngineConfigState::roles` holds one row per role. This
//! fixture gives it two rows: the session's own role, and a plugin's seat.
//! Its model is its own. The golden checks that the card draws both rows.
//!
//! The fixture is its own, not `super::base_ui`'s. The demo config still
//! names five old roles. A golden built on it would use words the product
//! does not have.

use stella_tui::DeckTab;
use stella_tui::deck_ui::cards::Card;
use stella_tui::envelope::{EngineConfigState, RoleWiringRow};

use super::{H, W, assert_golden, fixture_model, render_frame, ui_for};

/// The session's own role, then one plugin seat with its own model. This is
/// the least a fixture needs to show the card naming both.
fn fixture_roles() -> EngineConfigState {
    EngineConfigState {
        roles: vec![
            RoleWiringRow {
                role: "default".to_string(),
                model: "zai/glm-5.2-air".to_string(),
                effort: "medium".to_string(),
                thinking: "thinking on".to_string(),
                source: "default_model".to_string(),
                next_session: None,
            },
            RoleWiringRow {
                role: "acme/reviewer".to_string(),
                model: "anthropic/claude-opus-5".to_string(),
                effort: "provider default".to_string(),
                thinking: "thinking default".to_string(),
                source: "seat_models.acme/reviewer".to_string(),
                next_session: None,
            },
        ],
        ..Default::default()
    }
}

#[test]
fn deck_render_snapshots_pin_the_models_card_with_a_seat() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Session);
    // Through the fold a driver snapshot takes, never by hand — see
    // `settings_seats`'s own test for why.
    stella_tui::views::engine_panel::ingest_config(&mut ui, &fixture_roles(), &None);
    ui.cards.raise(Card::Models);
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "card_models_seats",
        "the /models card: the session's role, then a plugin's seat",
        W,
        H,
        &frame,
    );
}
