// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Mouse dispatch for the deck: a tab-row click switches tabs, the wheel
//! scrolls the body or list under the reader — a tab's own, or a modal
//! dialog's — and a click skips the splash. Capture is opt-in
//! ([`crate::deck_shell::DeckOptions::mouse_capture`], L-T2), so a default
//! session keeps the terminal's own text selection.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::WorkspaceModel;
use crate::deck::DeckTab;
use crate::deck_ui::{DeckAction, DeckUi, handle_deck_key, queue_issues_first_load};

/// Lines (or list items) one wheel notch moves. Three is the
/// terminal-emulator convention (xterm and its descendants), so the deck
/// scrolls at the speed the rest of the user's terminal does.
const WHEEL_LINES: usize = 3;

/// Route one mouse event to the deck.
///
/// `width` is the frame's, from the same terminal the last draw measured —
/// the tab hit test needs it because the tab row narrows with the frame
/// (`views::frame::tab_row_hit`).
///
/// The outcome goes through the same `apply_deck_action` a key's does
/// (`deck_shell`): a wheel notch over a modal dialog re-enters the key
/// dispatch, so this can return anything [`handle_deck_key`] can.
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
    // click must not reach the surface behind it, and a wheel notch drives
    // the dialog's own body. The AGENTS page replaces the bands outright —
    // the tab row a click would hit is not even drawn — and its own key
    // handler takes the same route.
    if ui.agents_page.open || crate::deck_render::overlay_owns_keyboard(model, ui) {
        return match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                wheel_as_arrows(ev.kind == MouseEventKind::ScrollUp, model, ui)
            }
            _ => DeckAction::Ignored,
        };
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
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            wheel_on_tab(ev.kind == MouseEventKind::ScrollUp, model, ui)
        }
        _ => DeckAction::Ignored,
    }
}

/// A wheel notch while a dialog owns the keyboard, re-entered as arrow keys
/// so the dialog's own handler moves its own body — `list_nav`'s vocabulary,
/// which every overlay that scrolls or selects already speaks, so there is
/// no list of overlays here to fall out of date.
///
/// Synthesis is safe here and only here: on a bare tab, `↑`/`↓` carry
/// affordances a wheel must not trigger — an empty-composer `↑` on SESSION
/// opens the queue editor — which is why [`wheel_on_tab`] moves scroll state
/// directly instead. An action other than Handled/Ignored is returned
/// immediately, so nothing a dialog decides is dropped.
fn wheel_as_arrows(up: bool, model: &WorkspaceModel, ui: &mut DeckUi) -> DeckAction {
    let code = if up { KeyCode::Up } else { KeyCode::Down };
    let key = KeyEvent::new(code, KeyModifiers::NONE);
    let mut last = DeckAction::Ignored;
    for _ in 0..WHEEL_LINES {
        match handle_deck_key(key, model, ui) {
            DeckAction::Handled => last = DeckAction::Handled,
            DeckAction::Ignored => {}
            other => return other,
        }
    }
    last
}

/// A wheel notch on a bare tab: the tab's primary body or list, moved
/// [`WHEEL_LINES`] at a time through `list_nav`'s wheel entries.
///
/// Each arm names the same state and count the tab's own key handler hands
/// `list_nav::scroll` / `list_nav::select`, so the wheel and the arrows move
/// one thing. A tab whose list moves elsewhere must move here in the same
/// change.
fn wheel_on_tab(up: bool, model: &WorkspaceModel, ui: &mut DeckUi) -> DeckAction {
    use crate::deck_ui::list_nav::{scroll_wheel, select_wheel};
    let n = WHEEL_LINES;
    let m = ui.metrics;
    match ui.tab {
        DeckTab::Session => {
            scroll_wheel(
                up,
                n,
                &mut ui.session_scroll,
                m.session_total,
                m.session_height,
            );
        }
        DeckTab::Traces => {
            scroll_wheel(up, n, &mut ui.trace_scroll, m.trace_total, m.trace_height);
        }
        // The FILES tab is a list until a diff is open, then a body — the
        // same split `handle_files_key` makes.
        DeckTab::Files if ui.files_diff_open => {
            scroll_wheel(
                up,
                n,
                &mut ui.files_diff_scroll,
                m.files_diff_total,
                m.files_diff_height,
            );
        }
        DeckTab::Files => select_wheel(up, n, &mut ui.files_sel, model.ledger.records.len()),
        DeckTab::Agents => select_wheel(up, n, &mut ui.installed.sel, ui.installed.entries.len()),
        DeckTab::Graph => {
            let count = ui.graph.as_ref().map(|g| g.nodes.len()).unwrap_or(0);
            select_wheel(up, n, &mut ui.graph_cursor, count);
        }
        DeckTab::Skills => select_wheel(up, n, &mut ui.skills.sel, ui.skills.view.rows.len()),
        DeckTab::Mcp => select_wheel(up, n, &mut ui.mcp.selected, ui.mcp.servers.len()),
        DeckTab::Issues => select_wheel(up, n, &mut ui.issues.sel, ui.issues.rows.len()),
        // SETTINGS is two focus-claimed editors (modal while focused, so
        // they take the synthesis path above); unfocused, the tab has no
        // list for the wheel to move.
        DeckTab::Settings => return DeckAction::Ignored,
    }
    DeckAction::Handled
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

    /// The wheel scrolls whichever tab is up, through the same state its
    /// arrows move: a body tab (TRACES) by scroll state, a list tab with
    /// nothing in it without panicking, and FILES as a body once a diff is
    /// open.
    #[test]
    fn the_wheel_moves_every_tabs_own_surface() {
        let (model, mut ui) = fixture();
        ui.set_tab(DeckTab::Traces);
        ui.metrics.trace_total = 40;
        ui.metrics.trace_height = 8;
        let action = handle_deck_mouse(wheel(MouseEventKind::ScrollUp), 100, &model, &mut ui);
        assert_eq!(action, DeckAction::Handled);
        assert_eq!(ui.trace_scroll.top, 40 - 8 - WHEEL_LINES);

        ui.set_tab(DeckTab::Agents);
        assert_eq!(
            handle_deck_mouse(wheel(MouseEventKind::ScrollDown), 100, &model, &mut ui),
            DeckAction::Handled
        );
        assert_eq!(ui.installed.sel, 0, "an empty list stays put");

        ui.set_tab(DeckTab::Files);
        ui.files_diff_open = true;
        ui.metrics.files_diff_total = 30;
        ui.metrics.files_diff_height = 6;
        handle_deck_mouse(wheel(MouseEventKind::ScrollUp), 100, &model, &mut ui);
        assert_eq!(ui.files_diff_scroll.top, 30 - 6 - WHEEL_LINES);
    }

    /// A wheel notch while a modal dialog is up drives the dialog's body —
    /// the help overlay here — and leaves the transcript behind it alone.
    #[test]
    fn a_wheel_notch_over_a_modal_scrolls_the_dialog_not_the_tab() {
        let (model, mut ui) = fixture();
        ui.help_open = true;
        ui.metrics.help_total = 100;
        ui.metrics.help_height = 10;
        ui.metrics.session_total = 100;
        ui.metrics.session_height = 10;
        // Down, not up: help opens pinned to its top, where an upward notch
        // is already a no-op.
        let action = handle_deck_mouse(wheel(MouseEventKind::ScrollDown), 100, &model, &mut ui);
        assert_eq!(action, DeckAction::Handled);
        assert_eq!(ui.help_scroll.window(100, 10).start, WHEEL_LINES);
        assert!(
            ui.session_scroll.follow,
            "the transcript behind it did not move"
        );
        assert!(ui.help_open, "scrolling does not close the overlay");
    }

    /// The wheel is not an arrow key on a bare tab: an empty-composer `↑` on
    /// SESSION with prompts queued opens the queue editor, and a wheel notch
    /// in the same state must scroll instead.
    #[test]
    fn a_wheel_notch_on_session_never_opens_the_queue_editor() {
        let (mut model, mut ui) = fixture();
        model.queue.enqueue("queued prompt".into(), 0);
        ui.metrics.session_total = 100;
        ui.metrics.session_height = 10;
        handle_deck_mouse(wheel(MouseEventKind::ScrollUp), 100, &model, &mut ui);
        assert!(!ui.queue_open, "the queue editor stayed closed");
        assert_eq!(ui.session_scroll.top, 100 - 10 - WHEEL_LINES);
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
