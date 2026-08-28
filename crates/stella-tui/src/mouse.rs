// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Mouse dispatch for the deck: a click on the tab row switches tabs, the
//! wheel scrolls the Session transcript, and a click while the splash is up
//! skips it — each routed to the state change its key equivalent makes.
//! Capture is opt-in ([`crate::deck_shell::DeckOptions::mouse_capture`],
//! L-T2), so a default session never reaches this module and keeps the
//! terminal's own text selection.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::WorkspaceModel;
use crate::deck::DeckTab;
use crate::deck_ui::{DeckAction, DeckUi, queue_issues_first_load};

/// Transcript lines one wheel notch moves. Three is the terminal-emulator
/// convention (xterm and its descendants), so the deck scrolls at the speed
/// the rest of the user's terminal does.
const WHEEL_LINES: usize = 3;

/// Route one mouse event to the deck.
///
/// `width` is the frame's, from the same terminal the last draw measured —
/// the tab hit test needs it because the tab row narrows with the frame
/// (`views::frame::tab_row_hit`).
///
/// Returns [`DeckAction::Handled`] or [`DeckAction::Ignored`] and nothing
/// else, by construction: no mouse verb submits, quits, or runs a shell
/// command, which is what lets `deck_shell`'s mouse arm stay free of the key
/// arm's queue and quit plumbing. A future verb that sends must grow that
/// arm to match the key arm's dispatch.
///
/// Not wrapped in `accessible::announce`: accessible mode forces mouse
/// capture off (`accessible::mouse_capture_enabled`), so no event reaches
/// here on a session with a reader to announce to.
pub fn handle_deck_mouse(
    ev: MouseEvent,
    width: u16,
    model: &WorkspaceModel,
    ui: &mut DeckUi,
) -> DeckAction {
    // While the splash is up, a click dismisses it — the same impatience rule
    // as "any key, Esc included, dismisses it" (`handle_key_inner`).
    if !ui.splash.is_done() {
        if matches!(ev.kind, MouseEventKind::Down(_)) {
            ui.splash.skip();
            return DeckAction::Handled;
        }
        return DeckAction::Ignored;
    }
    // A modal dialog owns the mouse on the terms it owns the keyboard: a
    // click or a wheel notch must not reach the surface behind it. The
    // AGENTS page replaces the bands outright, so the tab row a click would
    // hit is not even drawn.
    if ui.agents_page.open || crate::deck_render::overlay_owns_keyboard(model, ui) {
        return DeckAction::Ignored;
    }
    match ev.kind {
        // The tab row is the frame's top row (`deck_render`'s first band).
        MouseEventKind::Down(MouseButton::Left) if ev.row == 0 => {
            match crate::views::frame::tab_row_hit(model, ui, width, ev.column) {
                Some(tab) => {
                    ui.set_tab(tab);
                    // The same follow-up the Tab key runs: landing on ISSUES
                    // for the first time starts its load.
                    queue_issues_first_load(ui);
                    DeckAction::Handled
                }
                None => DeckAction::Ignored,
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if ui.tab == DeckTab::Session => {
            let (total, height) = (ui.metrics.session_total, ui.metrics.session_height);
            if ev.kind == MouseEventKind::ScrollUp {
                ui.session_scroll.scroll_up(WHEEL_LINES, total, height);
            } else {
                ui.session_scroll.scroll_down(WHEEL_LINES, total, height);
            }
            DeckAction::Handled
        }
        _ => DeckAction::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};

    fn down(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }
    }

    fn wheel(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 10,
            row: 10,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }
    }

    /// A model with one registered agent and a `DeckUi` past the splash — the
    /// state a deck is in when a mouse event can first mean anything.
    fn fixture() -> (WorkspaceModel, DeckUi) {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        let mut ui = DeckUi::default();
        ui.splash.skip();
        (model, ui)
    }

    /// The column a title occupies on the rendered tab row, so the click the
    /// test sends is the click a user aims at what they see.
    fn column_of(model: &WorkspaceModel, ui: &DeckUi, title: &str, width: u16) -> u16 {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        crate::views::frame::render_tab_row(model, ui, area, &mut buf);
        let row: String = (0..width)
            .map(|x| {
                buf.cell((x, 0))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect();
        row.find(title).expect("title on the rendered row") as u16
    }

    /// The witness for clickable tabs: a left click on a tab's title on the
    /// top row moves the deck to that tab, exactly as Tab-cycling to it would.
    #[test]
    fn a_click_on_a_tab_title_switches_to_that_tab() {
        let (model, mut ui) = fixture();
        assert_eq!(ui.tab, DeckTab::Session);
        let col = column_of(&model, &ui, DeckTab::Graph.title(), 100);
        let action = handle_deck_mouse(down(col, 0), 100, &model, &mut ui);
        assert_eq!(action, DeckAction::Handled);
        assert_eq!(ui.tab, DeckTab::Graph);
    }

    /// A click on the ISSUES title runs the Tab key's first-load follow-up,
    /// so the tab a click opens is never emptier than the tab a key opens.
    #[test]
    fn a_click_on_issues_queues_its_first_load() {
        let (model, mut ui) = fixture();
        let col = column_of(&model, &ui, DeckTab::Issues.title(), 100);
        handle_deck_mouse(down(col, 0), 100, &model, &mut ui);
        assert_eq!(ui.tab, DeckTab::Issues);
        assert!(ui.issues.busy, "first load queued");
    }

    /// A click below the tab row, or on the air between titles, changes
    /// nothing.
    #[test]
    fn a_click_off_the_titles_is_ignored() {
        let (model, mut ui) = fixture();
        let col = column_of(&model, &ui, DeckTab::Graph.title(), 100);
        assert_eq!(
            handle_deck_mouse(down(col, 5), 100, &model, &mut ui),
            DeckAction::Ignored,
            "below the tab row"
        );
        assert_eq!(
            handle_deck_mouse(down(0, 0), 100, &model, &mut ui),
            DeckAction::Ignored,
            "the leading column of air"
        );
        assert_eq!(ui.tab, DeckTab::Session);
    }

    /// While an overlay owns the keyboard it owns the mouse: a click on a tab
    /// title under the help overlay must not switch the surface behind it.
    #[test]
    fn a_modal_overlay_claims_the_click() {
        let (model, mut ui) = fixture();
        ui.help_open = true;
        let col = column_of(&model, &ui, DeckTab::Graph.title(), 100);
        assert_eq!(
            handle_deck_mouse(down(col, 0), 100, &model, &mut ui),
            DeckAction::Ignored
        );
        assert_eq!(ui.tab, DeckTab::Session);
    }

    /// The wheel scrolls the Session transcript by [`WHEEL_LINES`], against
    /// the totals the last draw recorded — the same route the arrow keys take.
    #[test]
    fn the_wheel_scrolls_the_session_transcript() {
        let (model, mut ui) = fixture();
        ui.metrics.session_total = 100;
        ui.metrics.session_height = 10;
        handle_deck_mouse(wheel(MouseEventKind::ScrollUp), 100, &model, &mut ui);
        assert!(!ui.session_scroll.follow, "scrolled out of follow");
        assert_eq!(ui.session_scroll.top, 100 - 10 - WHEEL_LINES);
        handle_deck_mouse(wheel(MouseEventKind::ScrollDown), 100, &model, &mut ui);
        assert_eq!(ui.session_scroll.top, 100 - 10, "one notch back down");
    }

    /// A click while the splash is up skips it, exactly as any key does.
    #[test]
    fn a_click_skips_the_splash() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        let mut ui = DeckUi::default();
        assert!(!ui.splash.is_done());
        let action = handle_deck_mouse(down(0, 0), 100, &model, &mut ui);
        assert_eq!(action, DeckAction::Handled);
        assert!(ui.splash.is_done());
    }
}
