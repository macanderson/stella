// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The mid-turn routing card — drawn in its own band above the transcript
//! when the deck has parked a prompt rather than guessing where it goes:
//!
//! ```text
//! ╭ ◇ this turn is still running — where does this go? ──────────────╮
//! │ and now add the tests                                            │
//! │  s   steer this turn — inject at the next step                   │
//! │  n   next turn — continue this conversation                      │
//! │  p   parallel — hand it to req:2                                 │
//! │  esc put it back in the composer                                 │
//! ╰──────────────────────────────────────────────────────────────────╯
//! ```
//!
//! The decision itself lives in [`crate::deck_ui::dispatch`]; this is only
//! its face.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};
use stella_tui_theme::{glyph, token};

use crate::views::cards::truncate_cols;

/// The mid-turn routing card: the user's words, held, and the three places
/// they can go.
///
/// It shows the text it is holding because that is the reassurance the old
/// silent spawn never gave — the prompt is not lost, it is right there, and
/// no key on this card discards it. The routes are ordered by how much they
/// respect the conversation already in progress: steer it, continue it, or
/// fork away from it.
pub fn render(pending: &crate::deck_ui::PendingDispatch, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let key = Style::new().fg(token::GOLD);
    let muted = Style::new().fg(token::MUTED);
    // One line, clipped: the card's height is fixed so the routes below can
    // never be pushed out of frame by a long prompt. The full text is still
    // in the composer's hands — Esc returns it verbatim.
    let held = truncate_cols(&pending.text, usize::from(area.width).saturating_sub(4));
    let route = |chord: &str, does: String| {
        Line::from(vec![
            Span::styled(format!(" {chord:<4}"), key),
            Span::styled(does, muted),
        ])
    };
    let lines = vec![
        Line::from(Span::styled(
            format!(" {held}"),
            Style::new().fg(token::TEXT),
        )),
        route("s", "steer this turn — inject at the next step".into()),
        route("n", "next turn — continue this conversation".into()),
        route("p", format!("parallel — hand it to {}", pending.next_lane)),
        route("esc", "put it back in the composer".into()),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        // The gate glyph in the lift gold is what "waiting on you" looks
        // like everywhere on this deck — `v2::sessions` marks a `NeedsInput`
        // session with the same pair.
        .title(Line::from(vec![
            Span::styled(
                format!(" {} ", glyph::GATE),
                Style::new().fg(token::GOLD_BRIGHT),
            ),
            Span::styled(
                "this turn is still running — where does this go? ",
                Style::new().fg(token::TEXT),
            ),
        ]));
    Paragraph::new(lines).block(block).render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck_ui::PendingDispatch;

    fn draw(text: &str, width: u16) -> String {
        let pending = PendingDispatch {
            text: text.to_string(),
            next_lane: "req:2".into(),
            agent_id: "lead".into(),
        };
        let area = Rect::new(0, 0, width, 7);
        let mut buf = Buffer::empty(area);
        render(&pending, area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The card has to answer "where did my prompt go?" on sight: it shows the
    /// held text, all three routes with their keys, and the way back out.
    #[test]
    fn the_card_shows_the_held_prompt_and_every_route() {
        let frame = draw("and now add the tests", 70);
        for needle in [
            "and now add the tests",
            "steer this turn",
            "next turn",
            "req:2",
            "put it back in the composer",
        ] {
            assert!(frame.contains(needle), "missing {needle:?}:\n{frame}");
        }
    }

    /// A long prompt is clipped to one line rather than wrapping, because the
    /// card's height is fixed — wrapping would push the routes out of frame
    /// and leave the user looking at their own text with no way to act on it.
    #[test]
    fn a_long_prompt_is_clipped_so_the_routes_stay_visible() {
        let frame = draw(&"x".repeat(400), 70);
        assert!(frame.contains('…'), "clipped with an ellipsis:\n{frame}");
        for needle in ["steer this turn", "put it back in the composer"] {
            assert!(
                frame.contains(needle),
                "routes survive a long prompt, missing {needle:?}:\n{frame}"
            );
        }
    }
}
