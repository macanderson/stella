// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The spend-cap editor (`/budget`): shows the session's spend against the
//! cap the budget stream has folded, and takes a new cap as typed input.
//! The *edit* leaves as `WorkspaceInput::SetBudget`; the number shown here
//! is only ever the folded one, so an ignored or clamped request can never
//! render as a cap in force.
//!
//! A floating card draws its own frame (`cards::card_frame` — not a link: it
//! is `pub(crate)`) because it is
//! an overlay rather than a tab body — the frame [`crate::v2::frame`] carves
//! out is what it floats *above*, and a card with no border of its own would
//! read as content the deck had appended to whatever is underneath it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use stella_tui_theme::token;

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::views::cards;

/// Render the budget editor over `frame`.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, frame: Rect, buf: &mut Buffer) {
    let dim = Style::new().fg(token::MUTED);
    let primary = Style::new().fg(token::TEXT);
    let spent = model.total_cost();
    let mut rows: Vec<Line<'static>> = Vec::new();
    if ui.accessible {
        let cap = match model.budget_cap_usd {
            Some(cap) => format!("${cap:.2}"),
            None => "none".to_string(),
        };
        rows.push(Line::from(Span::styled(
            format!("· spent ${spent:.2} · cap {cap}"),
            primary,
        )));
        rows.push(Line::from(Span::styled(
            format!("· new cap {}", ui.cards.budget_input),
            primary,
        )));
    } else {
        let mut spend_row = vec![
            Span::styled("run     ", dim),
            Span::styled(format!("${spent:.2}"), Style::new().fg(token::GREEN)),
        ];
        match model.budget_cap_usd {
            Some(cap) if cap > 0.0 => {
                spend_row.push(Span::styled(format!(" of ${cap:.2} "), dim));
                let pct = ((spent / cap).clamp(0.0, 1.0) * 100.0).round() as usize;
                spend_row.extend(cards::mini_fraction_bar(pct, 100, 9, token::GREEN));
            }
            _ => spend_row.push(Span::styled(" · no cap set", dim)),
        }
        rows.push(Line::from(spend_row));
        rows.push(Line::from(vec![
            Span::styled("new cap ", dim),
            Span::styled("$", dim),
            Span::styled(ui.cards.budget_input.clone(), primary),
            // A steady block caret — the reversed cell, never a blink.
            Span::styled(" ", Style::new().bg(token::GOLD)),
        ]));
        rows.push(Line::from(Span::styled(
            "the cap applies when the driver's budget stream folds it back",
            dim,
        )));
    }
    let area = cards::card_area(frame, rows.len() as u16, cards::CARD_MAX_W, ui.accessible);
    let inner = cards::card_frame(
        area,
        "budget",
        vec![Span::styled("session spend cap", dim)],
        "⏎ set · ⌫ edit · esc close",
        buf,
    );
    cards::render_body(rows, None, inner, buf);
}
