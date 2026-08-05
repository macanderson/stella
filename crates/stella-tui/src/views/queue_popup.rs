// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The queue editor popup (`ctrl+t`, or `↑` from an empty composer): every
//! waiting prompt as a navigable list, newest last, with the
//! edit/delete/clear legend. Split out of `deck_render.rs` beside the other
//! popup renderers (the god-file rule: new surface, sibling module).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::theme;

/// Draw the popup centered over `area`, with the armed clear-all warning when
/// `ui.queue_confirm_clear` is set.
pub(crate) fn render(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let pending = model.queue.pending();
    let w = area.width.min(64);
    let h = ((pending + 4).min(14) as u16).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    let selected = ui.queue_sel.min(pending.saturating_sub(1));
    let mut lines: Vec<Line<'static>> = Vec::new();
    if pending == 0 {
        lines.push(Line::from(Span::styled("queue is empty", theme::muted())));
    }
    // Keep the selected row in view on long queues.
    let visible_rows = (h as usize).saturating_sub(4).max(1);
    let start = selected
        .saturating_sub(visible_rows.saturating_sub(1) / 2)
        .min(pending.saturating_sub(visible_rows));
    for (i, item) in model
        .queue
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(visible_rows)
    {
        let is_sel = i == selected;
        let marker = if is_sel { "▸ " } else { "  " };
        let mut style = theme::body();
        if is_sel {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let text: String = item
            .text
            .chars()
            .take((w as usize).saturating_sub(6))
            .collect();
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{}. ", i + 1), style.fg(theme::ACCENT)),
            Span::styled(text, style),
        ]));
    }
    lines.push(Line::default());
    lines.push(if ui.queue_confirm_clear {
        Line::from(Span::styled(
            " press ctrl+d again to clear ALL queued prompts",
            theme::body().fg(theme::WARN).add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(
            " ↑/↓ select · enter edit · ctrl+x delete · ctrl+d ctrl+d clear · esc close",
            theme::muted(),
        ))
    });
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(format!(" queue · {pending} pending "));
    Paragraph::new(lines).block(block).render(popup, buf);
}
