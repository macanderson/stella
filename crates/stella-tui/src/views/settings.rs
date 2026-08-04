//! SETTINGS tab — the home of all config in stella, behind a one-line
//! secondary nav (AGENTS | TOOLS, switched with ←/→) exactly like the AGENTS
//! tab:
//!
//! - **AGENTS** ([`crate::views::engine`]): the `agent_engine_config` editor —
//!   the per-role model / prompt / sampling overrides plus the global routing
//!   toggles.
//! - **TOOLS** ([`crate::views::tools`]): which of this session's tools are
//!   switched off.
//!
//! One pane is on screen at a time and it fills the tab, so neither editor is
//! ever squeezed into half a terminal — the old side-by-side split truncated
//! the engine panel's values and the tools panel's reasons on any ordinary
//! width, and made the tab read as two things at once.
//!
//! As more config surfaces move here they become further panes of this nav.
//!
//! The editors themselves are unchanged and each is **modal while focused**:
//! `e` hands the keyboard to the pane you are looking at (`t` still jumps
//! straight to the tools editor from either pane), and its own Esc hands the
//! keyboard back. The always-on composer stays live until you enter one, and
//! only ever one of them holds the keyboard. Because a focused editor claims
//! every key ahead of the tab handler, ←/→ can only move the nav from browse
//! state — never out from under an open edit.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::theme;

/// Which pane this tab shows — its secondary nav, switched with ←/→ exactly
/// like [`crate::deck_ui::AgentsPane`]. Each pane is one config editor
/// rendered full-width; `e` hands it the keyboard.
///
/// It lives here rather than beside `AgentsPane` for the same reason
/// `ToolsOverlay`/`EngineOverlay` do: the view that draws a piece of state
/// owns it, and `deck_ui.rs` is the tree's most oversized module.
///
/// This is BROWSE-level state only: while an editor is focused it claims
/// every key ahead of the tab handler, so ←/→ can never switch the pane out
/// from under an open edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsPane {
    /// The `agent_engine_config` editor: per-role model / prompt / sampling
    /// overrides plus the global routing toggles.
    #[default]
    Agents,
    /// The `tools` editor: which of this session's tools are switched off.
    Tools,
}

impl SettingsPane {
    /// The nav label, UPPERCASE like every other secondary-nav label.
    pub fn label(self) -> &'static str {
        match self {
            SettingsPane::Agents => "AGENTS",
            SettingsPane::Tools => "TOOLS",
        }
    }
}

/// Draw the SETTINGS tab. `model` is unused (both editors work over
/// driver-owned snapshots held on `ui`), kept in the signature to match every
/// other tab view.
pub fn render(_model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    // The one-line secondary nav, then the active pane below it — the pane
    // fills the whole tab.
    let bands = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    render_pane_nav(ui.settings_pane, bands[0], buf);

    let body = bands[1];
    match ui.settings_pane {
        SettingsPane::Agents => crate::views::engine::render_panel(ui, body, buf),
        SettingsPane::Tools => crate::views::tools::render_panel(ui, body, buf),
    }
}

/// The secondary nav line: the two pane labels (UPPERCASE, like the deck's tab
/// labels), active in the accent cyan, plus the switch hint — the same line
/// [`crate::views::agents`] draws, so one nav is learned, not two.
fn render_pane_nav(pane: SettingsPane, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let style_for = |active: bool| {
        if active {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            theme::muted()
        }
    };
    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            SettingsPane::Agents.label(),
            style_for(pane == SettingsPane::Agents),
        ),
        Span::styled("  │  ", theme::muted()),
        Span::styled(
            SettingsPane::Tools.label(),
            style_for(pane == SettingsPane::Tools),
        ),
        Span::styled("   ←/→", theme::muted()),
    ]);
    Paragraph::new(line).render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck::DeckTab;
    use crate::deck_ui::{DeckAction, handle_deck_key};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Flatten a `Buffer` to one `String` per row (styling stripped — content
    /// is what we assert on).
    fn buffer_text(buf: &Buffer) -> String {
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn draw(ui: &mut DeckUi, width: u16) -> String {
        let model = WorkspaceModel::new();
        let area = Rect::new(0, 0, width, 24);
        let mut buf = Buffer::empty(area);
        render(&model, ui, area, &mut buf);
        buffer_text(&buf)
    }

    fn open_ui() -> (WorkspaceModel, DeckUi) {
        let mut ui = DeckUi::default();
        ui.splash.skip();
        ui.set_tab(DeckTab::Settings);
        (WorkspaceModel::new(), ui)
    }

    /// The core of the change: one pane on screen, never both — even on a
    /// terminal wide enough for the old two-column split.
    #[test]
    fn only_the_selected_pane_is_on_screen() {
        let (_model, mut ui) = open_ui();

        let text = draw(&mut ui, 160);
        assert!(text.contains(" agents "), "agents panel drawn:\n{text}");
        assert!(
            !text.contains("tools · "),
            "the tools panel is NOT also drawn:\n{text}"
        );

        ui.settings_pane = SettingsPane::Tools;
        let text = draw(&mut ui, 160);
        assert!(text.contains("tools · "), "tools panel drawn:\n{text}");
        assert!(
            !text.contains(" agents "),
            "the agents panel is NOT also drawn:\n{text}"
        );
    }

    #[test]
    fn the_nav_line_names_both_panes_and_the_switch_key() {
        let (_model, mut ui) = open_ui();
        let text = draw(&mut ui, 120);
        let nav = text.lines().next().unwrap_or_default();
        assert!(nav.contains("AGENTS"), "nav names the agents pane: {nav:?}");
        assert!(nav.contains("TOOLS"), "nav names the tools pane: {nav:?}");
        assert!(nav.contains("←/→"), "nav teaches the switch key: {nav:?}");
    }

    /// ←/→ walk the nav from browse state, and stop at each end rather than
    /// wrapping — the AGENTS tab's contract, key-for-key.
    #[test]
    fn left_right_walk_the_panes_from_a_blank_composer() {
        let (model, mut ui) = open_ui();
        assert_eq!(ui.settings_pane, SettingsPane::Agents, "agents first");

        handle_deck_key(key(KeyCode::Right), &model, &mut ui);
        assert_eq!(ui.settings_pane, SettingsPane::Tools);
        handle_deck_key(key(KeyCode::Right), &model, &mut ui);
        assert_eq!(ui.settings_pane, SettingsPane::Tools, "no wrap at the end");

        handle_deck_key(key(KeyCode::Left), &model, &mut ui);
        assert_eq!(ui.settings_pane, SettingsPane::Agents);
        handle_deck_key(key(KeyCode::Left), &model, &mut ui);
        assert_eq!(
            ui.settings_pane,
            SettingsPane::Agents,
            "no wrap at the start"
        );
    }

    /// `e` edits whichever pane is showing — one key, not a letter per pane.
    #[test]
    fn e_focuses_the_editor_of_the_highlighted_pane() {
        let (model, mut ui) = open_ui();

        handle_deck_key(key(KeyCode::Char('e')), &model, &mut ui);
        assert!(ui.engine.focused, "e on the AGENTS pane edits the engine");
        assert!(!ui.tools.focused);

        handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
        handle_deck_key(key(KeyCode::Right), &model, &mut ui);
        handle_deck_key(key(KeyCode::Char('e')), &model, &mut ui);
        assert!(ui.tools.focused, "e on the TOOLS pane edits the tools");
        assert!(!ui.engine.focused);
    }

    /// A focused editor owns the keyboard, so ←/→ drive the editor (which
    /// swallows them) rather than sliding the nav out from under it.
    #[test]
    fn a_focused_editor_pins_the_nav() {
        let (model, mut ui) = open_ui();
        handle_deck_key(key(KeyCode::Char('e')), &model, &mut ui);
        assert!(ui.engine.focused);

        handle_deck_key(key(KeyCode::Right), &model, &mut ui);
        assert_eq!(
            ui.settings_pane,
            SettingsPane::Agents,
            "the nav did not move while an editor held the keyboard"
        );
        assert!(ui.engine.focused, "and the editor still has it");
    }

    /// `t` remains the direct route to the tools editor from either pane —
    /// and brings the nav with it, so the focused editor is the visible one.
    #[test]
    fn t_jumps_to_the_tools_pane_and_focuses_it() {
        let (model, mut ui) = open_ui();
        let action = handle_deck_key(key(KeyCode::Char('t')), &model, &mut ui);
        assert!(
            matches!(action, DeckAction::Send(_)),
            "focusing asks the driver for the live tool list"
        );
        assert_eq!(ui.settings_pane, SettingsPane::Tools, "the nav followed");
        assert!(ui.tools.focused);
    }
}
