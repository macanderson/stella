//! SETTINGS tab — the home of all config in stella. Today it hosts two
//! sections side by side: the `agent_engine_config` editor (the per-role model
//! / prompt / sampling overrides plus the global routing toggles) and the
//! `tools` editor (which of this session's tools are switched off). As more
//! config surfaces move here they become further sections of this tab.
//!
//! The editors themselves live in [`crate::views::engine`] and
//! [`crate::views::tools`] — this module is the thin tab shell that places
//! their `render_panel`s. Each is **modal while focused** (`e` focuses the
//! engine editor, `t` the tools editor; Esc hands the keyboard back), so the
//! always-on composer stays live until you enter one, and only ever one of
//! them holds the keyboard.
//!
//! Layout is width-driven rather than fixed: two columns need enough room for
//! the engine panel's label column and the tools panel's name + state + reason
//! columns, so below [`SPLIT_MIN_WIDTH`] the tab shows one panel full-width —
//! whichever is focused, defaulting to the engine editor. A cramped two-column
//! split that truncated every reason to nothing would be worse than one honest
//! panel and a keystroke.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;

/// Below this the tab renders one panel full-width instead of splitting.
const SPLIT_MIN_WIDTH: u16 = 96;

/// Draw the SETTINGS tab. `model` is unused (both editors work over
/// driver-owned snapshots held on `ui`), kept in the signature to match every
/// other tab view.
pub fn render(_model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    if area.width < SPLIT_MIN_WIDTH {
        if ui.tools.focused {
            crate::views::tools::render_panel(ui, area, buf);
        } else {
            crate::views::engine::render_panel(ui, area, buf);
        }
        return;
    }
    let columns =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
    crate::views::engine::render_panel(ui, columns[0], buf);
    crate::views::tools::render_panel(ui, columns[1], buf);
}
