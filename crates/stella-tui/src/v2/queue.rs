// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The queue editor overlay (`ctrl+t`, or `↑` from an empty composer):
//!
//! ```text
//! ╭ queue · 2 pending ───────────────────────────────────────────────╮
//! │ ▸ 1. write the tests                                             │
//! │   2. open a pr                                                   │
//! ╰ ↑↓ select · ↵ edit · ^X delete · ^D ^D clear · esc ──────────────╯
//! ```
//!
//! One row per waiting prompt, oldest first — the order
//! [`crate::deck::PromptQueue::take_next`] dispatches them in, so
//! the row a reader sees at the top is the prompt the next free turn takes.
//! The ordinal is the row's position in that order rather than an identifier:
//! deleting row 1 renumbers the rest, because what the number answers is "how
//! many turns until this one runs".
//!
//! The armed clear-all warning takes the footer rather than a body row, the
//! same place [`crate::v2::subagents`] puts an armed verb: a warning that
//! pushes the list up by a row moves the thing the reader is aiming at, and
//! the second `ctrl+d` lands on whatever slid under the cursor.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use stella_tui_theme::token;

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::views::cards;

/// Tallest the overlay grows, in rows. Past it the list windows on the
/// selection instead: a queue deep enough to fill the frame would leave the
/// transcript it is queued against invisible.
const MAX_H: u16 = 14;

/// Widest the overlay grows, in columns. A queued prompt is a sentence, and a
/// sentence set across a 200-column terminal is a line the eye loses.
const MAX_W: u16 = 72;

/// Body rows the empty state spends: one of air, one of prose.
const EMPTY_ROWS: usize = 2;

/// Draw the overlay centered over `area`, with the armed clear-all warning in
/// the footer when `ui.queue_confirm_clear` is set.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let pending = model.queue.pending();
    let w = area.width.saturating_sub(6).min(MAX_W);
    let body_rows = if pending == 0 { EMPTY_ROWS } else { pending };
    // Clamped to the frame, so a terminal too small for the overlay renders
    // none of it rather than a rect reaching past the buffer.
    let h = u16::try_from(body_rows)
        .unwrap_or(MAX_H)
        .saturating_add(2)
        .min(MAX_H)
        .min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    // Two border cells and a cell of air either side of the row.
    let inner_w = (w as usize).saturating_sub(4);
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if pending == 0 {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  the queue is empty — ⏎ at the composer adds a prompt",
            muted,
        )));
    }

    // Window on the selection so a long queue keeps the cursor in view.
    let selected = ui.queue_sel.min(pending.saturating_sub(1));
    let visible = (h as usize).saturating_sub(2).max(1);
    let start = selected
        .saturating_sub(visible.saturating_sub(1) / 2)
        .min(pending.saturating_sub(visible));
    for (i, item) in model
        .queue
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
    {
        let is_sel = i == selected;
        let ordinal = format!("{}. ", i + 1);
        let mut row_style = text;
        let mut ordinal_style = muted;
        if is_sel {
            row_style = row_style.bg(token::HL).add_modifier(Modifier::BOLD);
            ordinal_style = ordinal_style.bg(token::HL);
        }
        let room = inner_w.saturating_sub(ordinal.chars().count() + 2);
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                if is_sel { "▸ " } else { "  " }.to_string(),
                Style::new().fg(token::GOLD),
            ),
            Span::styled(ordinal, ordinal_style),
            Span::styled(cards::truncate_cols(&item.text, room), row_style),
        ]));
    }

    let footer = if ui.queue_confirm_clear {
        Span::styled(
            " press ctrl+d again to clear ALL queued prompts ",
            Style::new().fg(token::RED).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" ↑↓ select · ↵ edit · ^X delete · ^D ^D clear · esc ", dim)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(Line::from(vec![
            Span::styled(" queue", text),
            Span::styled(format!(" · {pending} pending "), muted),
        ]))
        .title_bottom(Line::from(footer).right_aligned());
    Paragraph::new(lines).block(block).render(popup, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(buf: &Buffer) -> String {
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

    fn queued(texts: &[&str]) -> WorkspaceModel {
        let mut model = WorkspaceModel::new();
        for (i, t) in texts.iter().enumerate() {
            model.queue.enqueue((*t).to_string(), i as u64);
        }
        model
    }

    /// The list is the dispatch order, numbered from the front, and the
    /// selected row carries the `▸` marker as well as the tint — the golden
    /// suite strips style, so a tint-only selection would be invisible to it.
    #[test]
    fn every_pending_prompt_is_listed_with_the_selection_marked() {
        let model = queued(&["write the tests", "open a pr"]);
        let ui = DeckUi {
            queue_open: true,
            queue_sel: 1,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&model, &ui, area, &mut buf);
        let frame = text(&buf);
        assert!(frame.contains("queue · 2 pending"), "{frame}");
        assert!(frame.contains("1. write the tests"), "{frame}");
        assert!(frame.contains("▸ 2. open a pr"), "{frame}");
        assert!(frame.contains("^X delete"), "{frame}");
    }

    /// The armed clear-all takes the footer, so the list underneath does not
    /// move between the first `ctrl+d` and the second.
    #[test]
    fn the_armed_clear_warns_in_the_footer_without_moving_the_list() {
        let model = queued(&["write the tests", "open a pr"]);
        let mut ui = DeckUi {
            queue_open: true,
            queue_sel: 1,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 80, 20);
        let mut calm = Buffer::empty(area);
        render(&model, &ui, area, &mut calm);
        ui.queue_confirm_clear = true;
        let mut armed = Buffer::empty(area);
        render(&model, &ui, area, &mut armed);

        let warned = text(&armed);
        assert!(warned.contains("press ctrl+d again"), "{warned}");
        assert!(!warned.contains("^X delete"), "{warned}");
        let row_of = |frame: &str, needle: &str| {
            frame
                .lines()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("no row reads {needle:?}:\n{frame}"))
        };
        assert_eq!(
            row_of(&text(&calm), "▸ 2. open a pr"),
            row_of(&warned, "▸ 2. open a pr"),
            "arming the clear moved the row the second press lands on"
        );
    }

    /// An empty queue says so and says what fills it, rather than drawing a
    /// bordered box with nothing in it.
    #[test]
    fn an_empty_queue_says_what_would_fill_it() {
        let model = WorkspaceModel::new();
        let ui = DeckUi {
            queue_open: true,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&model, &ui, area, &mut buf);
        let frame = text(&buf);
        assert!(frame.contains("queue · 0 pending"), "{frame}");
        assert!(frame.contains("the queue is empty"), "{frame}");
    }

    /// A queue deeper than the overlay is tall windows on the selection —
    /// the cursor row is drawn, and the overlay never outgrows [`MAX_H`].
    #[test]
    fn a_deep_queue_windows_on_the_selection() {
        let prompts: Vec<String> = (0..40).map(|i| format!("prompt number {i}")).collect();
        let refs: Vec<&str> = prompts.iter().map(String::as_str).collect();
        let model = queued(&refs);
        let ui = DeckUi {
            queue_open: true,
            queue_sel: 30,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 80, 40);
        let mut buf = Buffer::empty(area);
        render(&model, &ui, area, &mut buf);
        let frame = text(&buf);
        assert!(frame.contains("▸ 31. prompt number 30"), "{frame}");
        assert!(!frame.contains("prompt number 0 "), "{frame}");
        let drawn = frame.lines().filter(|l| l.contains('╮') || l.contains('╯'));
        assert_eq!(drawn.count(), 2, "one top and one bottom border:\n{frame}");
    }

    /// A prompt wider than the overlay is cut on display columns rather than
    /// wrapped into the row below — a wrapped row would push the rest of the
    /// list out of the window the selection was computed against.
    #[test]
    fn a_long_prompt_is_cut_to_its_row() {
        let model = queued(&["日本語のテキスト".repeat(20).as_str()]);
        let ui = DeckUi {
            queue_open: true,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&model, &ui, area, &mut buf);
        let frame = text(&buf);
        assert!(frame.contains('…'), "{frame}");
        assert_eq!(
            frame.lines().filter(|l| l.contains('日')).count(),
            1,
            "the prompt stays on one row:\n{frame}"
        );
    }

    /// A frame with no room for the overlay draws none of it, rather than a
    /// rect reaching past the buffer.
    #[test]
    fn a_degenerate_frame_draws_nothing_and_does_not_panic() {
        let model = queued(&["write the tests"]);
        let ui = DeckUi {
            queue_open: true,
            ..Default::default()
        };
        for (w, h) in [(0u16, 0u16), (1, 1), (4, 2), (8, 3), (80, 0)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            render(&model, &ui, area, &mut buf);
        }
    }
}
