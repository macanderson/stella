// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The spend-cap editor (`/budget`): shows the session's spend against the
//! cap the budget stream has folded, and takes a new cap as typed input.
//! The *edit* leaves as `WorkspaceInput::SetBudget`; the number shown here
//! is only ever the folded one, so an ignored or clamped request can never
//! render as a cap in force.
//!
//! A floating card draws its own frame (`views::cards::card_frame`, named
//! rather than linked: it is `pub(crate)`, and an intra-doc link to it
//! resolves only under `--document-private-items`, which the doc gate does not
//! pass) because it is an overlay rather than a tab body — the frame
//! [`crate::views::frame`] carves
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
        // Money renders gold, and its meter is gold on the border gray —
        // SPEC 5's rule for every spend figure. Green is verdict ink here,
        // and spend is a fact, not a pass.
        let mut spend_row = vec![
            Span::styled("run     ", dim),
            Span::styled(format!("${spent:.2}"), Style::new().fg(token::GOLD)),
        ];
        match model.budget_cap_usd {
            Some(cap) if cap > 0.0 => {
                spend_row.push(Span::styled(format!(" of ${cap:.2} "), dim));
                let pct = ((spent / cap).clamp(0.0, 1.0) * 100.0).round() as usize;
                spend_row.extend(cards::mini_fraction_bar(pct, 100, 9, token::GOLD));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};

    /// SPEC 5: money renders gold and its meter is gold on the border gray.
    /// Green is verdict ink; a spend figure is a fact, not a pass, so the
    /// card may not paint a single green cell.
    #[test]
    fn money_and_its_meter_are_gold_never_green() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.agents[0].cost_usd = 0.42;
        model.budget_cap_usd = Some(1.0);
        let ui = DeckUi::default();
        let frame = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(frame);
        render(&model, &ui, frame, &mut buf);

        // A metal map of the card: every visible cell's symbol with the token
        // it wears, so each half of the rule is pinned by the cells that
        // carry it — the `▰` fill gold, the `▱` groove gray, the `$` money
        // gold, and no green anywhere.
        let (mut fills, mut grooves, mut money) = (0usize, 0usize, 0usize);
        for y in 0..frame.height {
            for x in 0..frame.width {
                let cell = buf.cell((x, y)).expect("cell in area");
                assert_ne!(
                    cell.fg,
                    token::GREEN,
                    "green cell at ({x},{y}): {:?}",
                    cell.symbol()
                );
                match cell.symbol() {
                    "▰" => {
                        assert_eq!(cell.fg, token::GOLD, "meter fill at ({x},{y}) is not gold");
                        fills += 1;
                    }
                    "▱" => {
                        assert_eq!(
                            cell.fg,
                            token::MUTED,
                            "meter groove at ({x},{y}) is not the gray groove"
                        );
                        grooves += 1;
                    }
                    "$" if cell.fg == token::GOLD => money += 1,
                    _ => {}
                }
            }
        }
        assert!(fills > 0, "no gold meter fill rendered");
        assert!(grooves > 0, "no meter groove rendered");
        assert!(money > 0, "no gold money cell rendered");
    }
}
