//! The top-level deck frame: the comfy-tabs bar + the active view + an
//! always-on status bar + the splash overlay. The tab dispatcher.
//!
//! STUB: the comfy-tabs `TabNav` bar and the status bar are filled in by the
//! deck builder; this places the bands and dispatches to the active view so the
//! skeleton renders end-to-end.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};

use crate::deck::{DeckTab, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::{splash, views};

pub fn render_deck(model: &WorkspaceModel, ui: &mut DeckUi, frame: &mut Frame) {
    let area = frame.area();
    let buf = frame.buffer_mut();

    // Splash owns the whole frame until it's done.
    if !ui.splash.is_done() {
        splash::render(&ui.splash, area, buf);
        return;
    }

    // Bands: tab bar (3 rows, comfy-tabs needs exactly 3) | content | status.
    let bands = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);
    let content = bands[1];

    // TODO(deck builder): comfy-tabs TabNav in bands[0]; status bar in bands[2].

    let tab = ui.tab;
    match tab {
        DeckTab::Session => views::session::render(model, ui, content, buf),
        DeckTab::Agents => views::agents::render(model, ui, content, buf),
        DeckTab::Traces => views::traces::render(model, ui, content, buf),
        DeckTab::Graph => views::graph::render(model, ui, content, buf),
        DeckTab::Files => views::files::render(model, ui, content, buf),
    }
}
