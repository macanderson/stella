//! The mid-turn routing card — drawn when the deck has parked a prompt
//! rather than guessing where it goes. The decision itself lives in
//! [`crate::deck_ui::dispatch`]; this is only its face.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::theme;

/// The mid-turn routing card: the user's words, held, and the three places
/// they can go.
///
/// It shows the text it is holding because that is the reassurance the old
/// silent spawn never gave — the prompt is not lost, it is right there, and
/// no key on this card discards it. The routes are ordered by how much they
/// respect the conversation already in progress: steer it, continue it, or
/// fork away from it.
pub fn render(pending: &crate::deck_ui::PendingDispatch, area: Rect, buf: &mut Buffer) {
    let key = Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(theme::MUTED);
    // One line, clipped: the card's height is fixed so the routes below can
    // never be pushed out of frame by a long prompt. The full text is still
    // in the composer's hands — Esc returns it verbatim.
    let budget = area.width.saturating_sub(4) as usize;
    let held: String = if pending.text.chars().count() > budget {
        pending
            .text
            .chars()
            .take(budget.saturating_sub(1))
            .collect::<String>()
            + "…"
    } else {
        pending.text.clone()
    };
    let lines = vec![
        Line::from(Span::styled(held, Style::new().fg(theme::VIOLET))),
        Line::from(vec![
            Span::styled("  s  ", key),
            Span::styled("steer this turn — inject at the next step", dim),
        ]),
        Line::from(vec![
            Span::styled("  n  ", key),
            Span::styled("next turn — continue this conversation", dim),
        ]),
        Line::from(vec![
            Span::styled("  p  ", key),
            Span::styled(format!("parallel — hand it to {}", pending.next_lane), dim),
        ]),
        Line::from(vec![
            Span::styled("  esc  ", key),
            Span::styled("put it back in the composer", dim),
        ]),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::ACCENT))
        .title(" this turn is still running — where does this go? ");
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
