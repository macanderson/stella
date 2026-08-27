//! SETTINGS tab — the home of all config in stella, behind a one-line
//! secondary nav (AGENTS | TOOLS | SEATS, then a plugin's own, switched with
//! ←/→) exactly like the
//! AGENTS tab:
//!
//! - **AGENTS** ([`crate::views::engine_panel`]): the `agent_engine_config` editor —
//!   the per-role model / prompt / sampling overrides plus the global routing
//!   toggles.
//! - **TOOLS** ([`crate::views::tools`]): which of this session's tools are
//!   switched off.
//! - **SEATS** ([`crate::views::seats`]): which model each **plugin-declared**
//!   role runs on. Read-only for now; the editor arrives with the AGENTS
//!   pane's persona tabs leaving (`doc:roleless-core` slice 5b).
//! - **one pane per installed plugin** that declares
//!   `PanelSurface::Settings` (SPEC 12.2), drawn from the last frame that
//!   plugin sent and labelled with the name its installer consented to.
//!
//! The nav, the ←/→ cycle and the key handler all read
//! [`SettingsPane::panes`], so a fourth built-in is one entry in
//! [`SettingsPane::BUILTIN`] and an installed plugin's pane is no edit at all.
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
///
/// # Three built-in panes plus `Plugin(index)`, not a list of names
///
/// A plugin that declares [`stella_plugin::PanelSurface::Settings`] gets a
/// pane of its own (SPEC 12.2), so the nav is as long as the install roster
/// and a `const ALL: [SettingsPane; 3]` cannot express it. Two other shapes
/// were available and are worse:
///
/// - **One `SettingsPane` case per plugin, carrying its name**, makes the enum
///   open-ended and costs the compiler's exhaustiveness check on
///   [`SettingsPane::label`] — the check that is the whole reason a fourth
///   built-in pane cannot arrive unlabelled.
/// - **An index into one flat runtime list**, with no enum at all, makes
///   `AGENTS` and a plugin's pane the same kind of thing. They are not: the
///   three are the product's, they are compile-time total, and they must stay
///   first however many plugins are installed. A bare index says none of that
///   and lets a reordering put a plugin's rectangle where `AGENTS` was.
///
/// So the built-ins stay closed cases and every plugin pane is `Plugin(index)`
/// carrying its seat. The **order is derived**, by [`SettingsPane::panes`],
/// from live state — which is what the pane list actually is — and
/// [`SettingsPane::prev`]/[`next`](SettingsPane::next) take that list rather
/// than reading a const, so neither can answer from a nav that no longer
/// exists.
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
    /// One installed plugin's own pane, by its index in
    /// [`crate::panel_deck::PanelDeck::slots`].
    ///
    /// The index and not the name, so the pane cannot outlive the seat it
    /// draws: a plugin retracted mid-session loses its slot, and a pane
    /// pointing past the end resolves to nothing rather than to a stale
    /// rectangle under somebody else's label.
    Plugin(usize),
}

impl SettingsPane {
    /// The panes the product ships, always first and always in this order.
    ///
    /// Still a const, and still the thing the nav is built from: a fourth
    /// built-in is one entry here, exactly as before. What changed is that it
    /// is no longer the *whole* list — see [`SettingsPane::panes`].
    pub const BUILTIN: [SettingsPane; 3] = [
        SettingsPane::Agents,
        SettingsPane::Tools,
        SettingsPane::Seats,
    ];

    /// The tab's live nav order: the built-ins, then one pane per installed
    /// plugin that declares the `settings` surface, in seating order.
    #[must_use]
    pub fn panes(panels: &crate::panel_deck::PanelDeck) -> Vec<SettingsPane> {
        let mut panes = Vec::from(Self::BUILTIN);
        panes.extend(
            panels
                .on(stella_plugin::PanelSurface::Settings)
                .into_iter()
                .map(SettingsPane::Plugin),
        );
        panes
    }

    /// The nav label, UPPERCASE like every other secondary-nav label.
    ///
    /// A plugin's label is the manifest name the person consented to at
    /// install and nothing the plugin chose — SPEC 12.3's rule for the panel's
    /// own chrome, which holds for the nav that opens it too. Borrowed from
    /// `panels` rather than owned, so there is no second copy of that name to
    /// go stale.
    #[must_use]
    pub fn label(self, panels: &crate::panel_deck::PanelDeck) -> &str {
        match self {
            SettingsPane::Agents => "AGENTS",
            SettingsPane::Tools => "TOOLS",
            SettingsPane::Seats => "SEATS",
            SettingsPane::Plugin(index) => panels
                .slots()
                .get(index)
                .map(crate::panel_deck::PanelSlot::plugin)
                .unwrap_or(""),
        }
    }

    fn index(self, panes: &[SettingsPane]) -> usize {
        panes.iter().position(|p| *p == self).unwrap_or(0)
    }

    /// The pane to the left, or `None` at the first.
    ///
    /// **Does not wrap**, and the `None` is required rather than tidy: the
    /// key handler returns it unhandled so ← at the left edge keeps falling
    /// through to whatever handled it before, exactly as the two hard-coded
    /// guards it replaced did.
    #[must_use]
    pub fn prev(self, panes: &[SettingsPane]) -> Option<Self> {
        self.index(panes).checked_sub(1).map(|i| panes[i])
    }

    /// The pane to the right, or `None` at the last.
    #[must_use]
    pub fn next(self, panes: &[SettingsPane]) -> Option<Self> {
        panes.get(self.index(panes) + 1).copied()
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
    let panes = SettingsPane::panes(&ui.panels);
    // A pane whose plugin was retracted mid-session no longer exists; land on
    // the first rather than drawing a rectangle for a seat nobody renewed.
    if !panes.contains(&ui.settings_pane) {
        ui.settings_pane = SettingsPane::default();
    }
    let bands = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    render_pane_nav(ui.settings_pane, &panes, &ui.panels, bands[0], buf);

    let body = bands[1];
    match ui.settings_pane {
        SettingsPane::Agents => crate::views::engine_panel::render(ui, body, buf),
        SettingsPane::Tools => crate::views::tools::render_panel(ui, body, buf),
        SettingsPane::Seats => crate::views::seats::render(
            ui.engine.state.as_ref().map(|state| &state.seats[..]),
            body,
            buf,
        ),
        // The plugin's own rectangle, drawn from the last frame it sent. The
        // chrome round it is the host's (`crate::plugin_panel::chrome`), so a
        // pane a plugin fills is still labelled by the name its installer read.
        SettingsPane::Plugin(index) => {
            if let Some(slot) = ui.panels.slot_mut(index) {
                slot.render(body, buf);
            }
        }
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
    let panes = SettingsPane::panes(&ui.panels);
    match key.code {
        KeyCode::Left => ui.settings_pane.prev(&panes).map(|pane| {
            ui.settings_pane = pane;
            DeckAction::Handled
        }),
        KeyCode::Right => ui.settings_pane.next(&panes).map(|pane| {
            ui.settings_pane = pane;
            DeckAction::Handled
        }),
        // `e` edits what you are looking at — the one key every pane shares,
        // rather than a per-pane letter to remember. SEATS has no editor yet
        // (`crate::views::seats`, slice 5b), so it claims the key and does
        // nothing rather than silently focusing a different pane's editor.
        KeyCode::Char('e') => Some(match ui.settings_pane {
            SettingsPane::Agents => crate::views::engine_panel::focus_panel(ui),
            SettingsPane::Tools => crate::views::tools::focus_panel(ui),
            // A plugin's pane has no editor of Stella's to focus, and the
            // keyboard is not the deck's to hand over: SPEC 12 leases a panel
            // a rectangle, never a keystroke. It claims the key so `e` never
            // opens a different pane's editor from here.
            SettingsPane::Seats | SettingsPane::Plugin(_) => DeckAction::Handled,
        }),
        KeyCode::Char('t') => Some(crate::views::tools::focus_panel(ui)),
        _ => None,
    }
}

/// The secondary nav line: the two pane labels (UPPERCASE, like the deck's tab
/// labels), active in the accent cyan, plus the switch hint.
fn render_pane_nav(
    pane: SettingsPane,
    panes: &[SettingsPane],
    panels: &crate::panel_deck::PanelDeck,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.height == 0 {
        return;
    }
    let style_for = |active: bool| {
        if active {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            theme::text_secondary()
        }
    };
    // Built from the live pane list rather than written out, so a pane added
    // to `SettingsPane::BUILTIN` — or seated by an install — appears here with
    // no edit. The same derivation `EngineTab::ALL` uses over `EngineRole::ALL`,
    // and for the same reason: a hand-typed copy costs nothing until the day it
    // silently renders one label fewer than the tab actually has.
    //
    // A plugin's label is prefixed with the panel glyph, so the nav says which
    // panes are Stella's and which a third party's before the pane is opened —
    // the same reading SPEC 12.3's chrome gives once it is.
    let mut spans = vec![Span::raw("  ")];
    for (i, entry) in panes.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  │  ", theme::text_secondary()));
        }
        let label = match entry {
            SettingsPane::Plugin(_) => {
                format!(
                    "{} {}",
                    crate::plugin_panel::PANEL_GLYPH,
                    entry.label(panels)
                )
            }
            _ => entry.label(panels).to_string(),
        };
        spans.push(Span::styled(label, style_for(pane == *entry)));
    }
    spans.push(Span::styled("   ←/→", theme::text_secondary()));
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
    /// Walks the whole live pane list rather than naming two panes, so
    /// adding a fourth extends the walk instead of silently leaving it
    /// asserting an interior stretch of the nav.
    #[test]
    fn left_right_walk_the_panes_then_rise_to_the_tab_strip() {
        let (model, mut ui) = open_ui();
        let panes = SettingsPane::panes(&ui.panels);
        assert_eq!(ui.settings_pane, panes[0], "agents first");

        for expected in &panes[1..] {
            handle_deck_key(key(KeyCode::Right), &model, &mut ui);
            assert_eq!(ui.tab, DeckTab::Settings, "a pane step stays on the tab");
            assert_eq!(ui.settings_pane, *expected);
        }
        let last = *panes.last().expect("panes exist");
        handle_deck_key(key(KeyCode::Right), &model, &mut ui);
        assert_eq!(ui.settings_pane, last, "the panes do not wrap");
        assert_eq!(
            ui.tab,
            DeckTab::Settings.next(),
            "→ past the last pane is the next tab"
        );

        ui.set_tab(DeckTab::Settings);
        for expected in panes.iter().rev().skip(1) {
            handle_deck_key(key(KeyCode::Left), &model, &mut ui);
            assert_eq!(ui.settings_pane, *expected);
        }
        handle_deck_key(key(KeyCode::Left), &model, &mut ui);
        assert_eq!(ui.settings_pane, panes[0]);
        assert_eq!(
            ui.tab,
            DeckTab::Settings.prev(),
            "← past the first pane is the previous tab"
        );
    }

    /// The nav is built from [`SettingsPane::panes`], so every pane is reachable
    /// by name. A pane that exists but is not on the nav is one a user can only
    /// find by pressing → and noticing the body changed.
    #[test]
    fn the_nav_names_every_pane() {
        let (_model, mut ui) = open_ui();
        let nav = draw(&mut ui, 160);
        let nav = nav.lines().next().unwrap_or_default().to_string();
        let panels = ui.panels.clone();
        for pane in SettingsPane::panes(&panels) {
            assert!(
                nav.contains(pane.label(&panels)),
                "nav names {}: {nav:?}",
                pane.label(&panels)
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
