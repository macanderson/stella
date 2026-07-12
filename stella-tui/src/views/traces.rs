//! Traces tab — unified cross-agent timeline. STUB filled by builder.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Widget};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;

pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let _ = (model, ui);
    Block::default()
        .borders(Borders::ALL)
        .title(" Traces ")
        .render(area, buf);
}
