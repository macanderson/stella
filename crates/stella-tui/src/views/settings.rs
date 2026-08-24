//! SETTINGS tab — the home of all config in stella, behind a one-line
//! secondary nav (AGENTS | TOOLS | SEATS, switched with ←/→) exactly like the
//! AGENTS tab:
//!
//! - **AGENTS** ([`crate::v2::engine_panel`]): the `agent_engine_config` editor —
//!   the per-role model / prompt / sampling overrides plus the global routing
//!   toggles.
//! - **TOOLS** ([`crate::v2::tools`]): which of this session's tools are
//!   switched off.
//! - **SEATS** ([`crate::views::seats`]): which model each **plugin-declared**
//!   role runs on. Read-only for now; the editor arrives with the AGENTS
//!   pane's persona tabs leaving (`doc:roleless-core` slice 5b).
//!
//! The nav, the ←/→ cycle and the key handler all read [`SettingsPane::ALL`],
//! so a fourth pane is one entry there rather than four edits that can drift
//! apart.
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
use crate::deck_ui::{DeckAction, DeckUi};
use crate::theme;

/// Which pane this tab shows — its secondary nav, switched with ←/→ exactly
/// like the SKILLS tab's panes. Each pane is one config editor
/// rendered full-width; `e` hands it the keyboard.
///
/// It lives here rather than in `deck_ui` for the same reason
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
    /// The `seats` view: which model each plugin-declared role runs on.
    Seats,
}

impl SettingsPane {
    /// Left-to-right nav order, which is also the ←/→ cycle order.
    ///
    /// A slice rather than three hand-written comparisons, so adding a pane is
    /// one entry here instead of an edit in the nav, an edit in the key
    /// handler, and an edit in the dispatch — the shape that let
    /// `EngineTab::ALL` drift from `EngineRole::ALL` before it was derived.
    pub const ALL: [SettingsPane; 3] = [
        SettingsPane::Agents,
        SettingsPane::Tools,
        SettingsPane::Seats,
    ];

    /// The nav label, UPPERCASE like every other secondary-nav label.
    pub fn label(self) -> &'static str {
        match self {
            SettingsPane::Agents => "AGENTS",
            SettingsPane::Tools => "TOOLS",
            SettingsPane::Seats => "SEATS",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    /// The pane to the left, or `None` at the first.
    ///
    /// **Does not wrap**, and the `None` is required rather than tidy: the
    /// key handler returns it unhandled so ← at the left edge keeps falling
    /// through to whatever handled it before, exactly as the two hard-coded
    /// guards it replaced did.
    pub fn prev(self) -> Option<Self> {
        self.index().checked_sub(1).map(|i| Self::ALL[i])
    }

    /// The pane to the right, or `None` at the last.
    pub fn next(self) -> Option<Self> {
        Self::ALL.get(self.index() + 1).copied()
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
        SettingsPane::Agents => crate::v2::engine_panel::render(ui, body, buf),
        SettingsPane::Tools => crate::v2::tools::render_panel(ui, body, buf),
        SettingsPane::Seats => crate::views::seats::render_panel(ui, body, buf),
    }
}

/// The SETTINGS tab's browse-level keys: ←/→ between panes, `e` to edit what
/// you are looking at, `t` for the tools editor from anywhere.
///
/// Lives here rather than in `deck_ui.rs` — which is a grandfathered god file
/// closed to growth (AGENTS.md § "God files") — because it is pane vocabulary,
/// and the pane owns its vocabulary the same way each view owns its own state.
/// Moving it also made room for a third pane without touching the ratchet.
///
/// Returns `None` for a key this tab does not claim, including ← at the first
/// pane and → at the last: those must keep falling through to whatever handled
/// them before, which is what the two hard-coded pane guards used to do.
pub fn handle_key(
    key: crossterm::event::KeyEvent,
    ui: &mut DeckUi,
    composer_empty: bool,
) -> Option<DeckAction> {
    use crossterm::event::KeyCode;

    if !composer_empty || !key.modifiers.is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Left => ui.settings_pane.prev().map(|pane| {
            ui.settings_pane = pane;
            DeckAction::Handled
        }),
        KeyCode::Right => ui.settings_pane.next().map(|pane| {
            ui.settings_pane = pane;
            DeckAction::Handled
        }),
        // `e` edits what you are looking at — the one key every pane shares,
        // rather than a per-pane letter to remember. SEATS has no editor yet
        // (`crate::views::seats`, slice 5b), so it claims the key and does
        // nothing rather than silently focusing a different pane's editor.
        KeyCode::Char('e') => Some(match ui.settings_pane {
            SettingsPane::Agents => crate::v2::engine_panel::focus_panel(ui),
            SettingsPane::Tools => crate::v2::tools::focus_panel(ui),
            SettingsPane::Seats => DeckAction::Handled,
        }),
        KeyCode::Char('t') => Some(crate::v2::tools::focus_panel(ui)),
        _ => None,
    }
}

/// The secondary nav line: the two pane labels (UPPERCASE, like the deck's tab
/// labels), active in the accent cyan, plus the switch hint.
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
    // Built from `SettingsPane::ALL` rather than written out, so a pane added
    // there appears here with no edit — the same derivation `EngineTab::ALL`
    // uses over `EngineRole::ALL`, and for the same reason: a hand-typed copy
    // costs nothing until the day it silently renders one label fewer than the
    // tab actually has.
    let mut spans = vec![Span::raw("  ")];
    for (i, entry) in SettingsPane::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  │  ", theme::muted()));
        }
        spans.push(Span::styled(entry.label(), style_for(pane == *entry)));
    }
    spans.push(Span::styled("   ←/→", theme::muted()));
    Paragraph::new(Line::from(spans)).render(area, buf);
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

    /// ←/→ walk the nav from browse state; at either end the key **rises to
    /// the tab strip** and moves to the neighbouring tab instead of wrapping
    /// the panes — the focus tree's bubbling (`deck_ui::focus`), witnessed
    /// on the one tab with horizontal siblings at both levels.
    ///
    /// Walks all of [`SettingsPane::ALL`] rather than naming two panes, so
    /// adding a fourth extends the walk instead of silently leaving it
    /// asserting an interior stretch of the nav.
    #[test]
    fn left_right_walk_the_panes_then_rise_to_the_tab_strip() {
        let (model, mut ui) = open_ui();
        assert_eq!(ui.settings_pane, SettingsPane::ALL[0], "agents first");

        for expected in &SettingsPane::ALL[1..] {
            handle_deck_key(key(KeyCode::Right), &model, &mut ui);
            assert_eq!(ui.tab, DeckTab::Settings, "a pane step stays on the tab");
            assert_eq!(ui.settings_pane, *expected);
        }
        let last = *SettingsPane::ALL.last().expect("panes exist");
        handle_deck_key(key(KeyCode::Right), &model, &mut ui);
        assert_eq!(ui.settings_pane, last, "the panes do not wrap");
        assert_eq!(
            ui.tab,
            DeckTab::Settings.next(),
            "→ past the last pane is the next tab"
        );

        ui.set_tab(DeckTab::Settings);
        for expected in SettingsPane::ALL.iter().rev().skip(1) {
            handle_deck_key(key(KeyCode::Left), &model, &mut ui);
            assert_eq!(ui.settings_pane, *expected);
        }
        handle_deck_key(key(KeyCode::Left), &model, &mut ui);
        assert_eq!(ui.settings_pane, SettingsPane::ALL[0]);
        assert_eq!(
            ui.tab,
            DeckTab::Settings.prev(),
            "← past the first pane is the previous tab"
        );
    }

    /// The nav is built from [`SettingsPane::ALL`], so every pane is reachable
    /// by name. A pane that exists but is not on the nav is one a user can only
    /// find by pressing → and noticing the body changed.
    #[test]
    fn the_nav_names_every_pane() {
        let (_model, mut ui) = open_ui();
        let nav = draw(&mut ui, 160);
        let nav = nav.lines().next().unwrap_or_default().to_string();
        for pane in SettingsPane::ALL {
            assert!(
                nav.contains(pane.label()),
                "nav names {}: {nav:?}",
                pane.label()
            );
        }
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
