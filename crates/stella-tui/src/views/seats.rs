// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SEATS panel — which model each **plugin-declared** role runs on.
//!
//! The third pane of the SETTINGS tab, beside AGENTS and TOOLS, and it exists
//! for the reason [`crate::v2::tools`]'s module doc gives for that one:
//!
//! > MCP tools and customer-registered custom tools exist nowhere but the
//! > assembled session stack, so the rows come from the driver … never from a
//! > compiled-in table.
//!
//! Plugin seats are in exactly that position. A row is here because an
//! installed plugin declared a role, and it disappears when that plugin is
//! removed — which is the whole contract, and why this file contains no list of
//! roles and no `match` on a role name.
//!
//! # What a row says, and what it deliberately does not
//!
//! Each row is a seat key (`<plugin-id>/<role>`, `doc:roleless-core` §8.4), the
//! model assigned to it, and the plugin it came from. The key is **rendered
//! whole and never split**: the deck has no business knowing which half is the
//! plugin, which is why [`stella_protocol`]-side callers send
//! [`SeatRow::from`](crate::envelope::SeatRow::from) separately rather than
//! letting this pane parse it out.
//!
//! An unassigned seat renders as `default`, not as a blank. That is the truth —
//! an unassigned seat genuinely runs on the session's model — and a blank cell
//! would read as "unknown" for something the driver knows exactly.
//!
//! # Read-only, for now, and that is a smaller claim than AGENTS makes
//!
//! This pane renders; it does not edit. Assigning a model writes
//! `agent_engine_config.seat_models`, which is #3909's second half — the
//! editor arrives with the AGENTS pane's persona tabs leaving
//! (`doc:roleless-core` slice 5b), because until then the two panes would
//! offer two different ways to say the same thing. Rendering a seat the user
//! cannot yet edit is honest; editing one whose settings block is about to be
//! restructured is not.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::deck_ui::DeckUi;
use crate::envelope::SeatRow;
use crate::theme;

/// Shown when the driver has not delivered an engine snapshot yet — a race
/// right after startup, or a driver error. The same shape (and the same
/// remedy) as the TOOLS panel's.
const NO_SNAPSHOT_HINT: &str = "waiting for the seat list — r to reload";

/// Shown when the snapshot arrived and named no seats.
///
/// Deliberately not an apology or an error. No plugin declaring a role is the
/// ordinary state of a fresh install, and the line says what would change it
/// rather than implying something is missing.
const NO_SEATS_HINT: &str = "no installed plugin declares a role — every turn runs on the default \
                             model";

/// The word shown for a seat with no assignment.
///
/// The truth rather than a blank: an unassigned seat runs on the session's
/// model. A blank cell would read as "unknown" for something the driver knows
/// exactly, and would make an unassigned seat indistinguishable from one whose
/// assignment failed to resolve.
const UNASSIGNED: &str = "default";

/// Draw the SEATS pane.
pub fn render_panel(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let (w, h) = (area.width, area.height);
    if w < 4 || h < 4 {
        return; // no readable panel fits — draw nothing rather than garbage
    }

    let seats: Option<&[SeatRow]> = ui.engine.state.as_ref().map(|state| &state.seats[..]);
    let mut lines: Vec<Line<'static>> = Vec::new();

    match seats {
        None => lines.push(Line::from(Span::styled(
            format!("  {NO_SNAPSHOT_HINT}"),
            theme::muted(),
        ))),
        Some([]) => lines.push(Line::from(Span::styled(
            format!("  {NO_SEATS_HINT}"),
            theme::muted(),
        ))),
        Some(rows) => {
            // The key column is sized to the widest key so the model column
            // lines up: a ragged second column is what makes a list of
            // `plugin/role` pairs unreadable at a glance.
            let key_width = rows.iter().map(|row| row.key.len()).max().unwrap_or(0);
            for row in rows {
                lines.push(seat_line(row, key_width));
            }
        }
    }

    let title = match seats {
        Some(rows) if !rows.is_empty() => format!(" seats · {} ", rows.len()),
        _ => " seats ".to_string(),
    };
    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::muted())
                .title(Span::styled(title, theme::muted())),
        )
        .render(area, buf);
}

/// One seat's row: the key, the model serving it, and the plugin it came from.
fn seat_line(row: &SeatRow, key_width: usize) -> Line<'static> {
    let assigned = row.model.is_some();
    let model = row.model.clone().unwrap_or_else(|| UNASSIGNED.to_string());
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{:key_width$}", row.key), theme::body()),
        Span::raw("  "),
        // An assignment is a decision someone made and is worth reading; the
        // inherited default is context. Same reasoning as the TOOLS pane
        // giving an explicit switch more weight than an inherited one.
        Span::styled(
            model,
            if assigned {
                theme::body()
            } else {
                theme::muted()
            },
        ),
        Span::styled(format!("   from {}", row.from), theme::muted()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::EngineConfigState;

    fn draw(state: Option<EngineConfigState>) -> String {
        let mut ui = DeckUi::default();
        ui.engine.state = state;
        let area = Rect::new(0, 0, 90, 12);
        let mut buf = Buffer::empty(area);
        render_panel(&ui, area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn state_with(seats: Vec<SeatRow>) -> EngineConfigState {
        EngineConfigState {
            seats,
            ..EngineConfigState::default()
        }
    }

    fn seat(key: &str, model: Option<&str>, from: &str) -> SeatRow {
        SeatRow {
            key: key.to_string(),
            model: model.map(str::to_string),
            from: from.to_string(),
        }
    }

    /// **The witness.** A role no core enum has ever heard of renders, because
    /// the rows come from the driver rather than from a compiled-in table. This
    /// is the pane's whole reason to exist, and it fails the day someone adds a
    /// list of roles it will accept.
    #[test]
    fn a_role_core_has_never_heard_of_renders() {
        let text = draw(Some(state_with(vec![seat(
            "acme/second-opinion",
            Some("anthropic/claude-opus-5"),
            "acme",
        )])));
        assert!(text.contains("acme/second-opinion"), "{text}");
        assert!(text.contains("anthropic/claude-opus-5"), "{text}");
        assert!(text.contains("from acme"), "{text}");
    }

    /// An unassigned seat says `default`, because that is what it runs on. A
    /// blank would read as "unknown" for something the driver knows exactly.
    #[test]
    fn an_unassigned_seat_names_the_default_rather_than_blanking() {
        let text = draw(Some(state_with(vec![seat(
            "stella-plan/planner",
            None,
            "stella-plan",
        )])));
        assert!(text.contains("stella-plan/planner"), "{text}");
        assert!(text.contains(UNASSIGNED), "{text}");
    }

    /// No plugins is the ordinary fresh-install state, and the pane says what
    /// happens instead rather than apologising or showing an empty box.
    #[test]
    fn no_seats_explains_the_default_rather_than_erroring() {
        let text = draw(Some(state_with(Vec::new())));
        assert!(
            text.contains("no installed plugin declares a role"),
            "{text}"
        );
        assert!(text.contains("default model"), "{text}");
    }

    /// No snapshot is a different state from no seats, and must not be
    /// rendered as "you have no plugins" — that would be the deck answering a
    /// question the driver has not answered yet.
    #[test]
    fn a_missing_snapshot_is_not_reported_as_an_empty_seat_list() {
        let text = draw(None);
        assert!(text.contains("waiting for the seat list"), "{text}");
        assert!(!text.contains("no installed plugin"), "{text}");
    }

    /// The key is rendered whole. Splitting it to show the plugin separately
    /// would be the deck reading a string it is contractually ignorant of.
    #[test]
    fn the_seat_key_is_rendered_whole() {
        let text = draw(Some(state_with(vec![seat(
            "vera/test_author",
            Some("openrouter/openai/gpt-5.5"),
            "vera",
        )])));
        assert!(text.contains("vera/test_author"), "{text}");
    }
}
