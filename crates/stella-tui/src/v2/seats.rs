// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SEATS pane — which model each **plugin-declared** role runs on:
//!
//! ```text
//! seats · 2 · read-only
//!  acme/second-opinion   anthropic/claude-opus-5   from acme
//!  vera/test_author      default                   from vera
//! ```
//!
//! The third pane of the SETTINGS tab, beside AGENTS and TOOLS, and it exists
//! for the reason [`crate::views::tools`]'s module doc gives for that one:
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
//! cannot yet edit reflects what the driver already knows; editing one whose
//! settings block is about to be restructured would not. The header says
//! `read-only` so a reader who pressed
//! `e` and saw nothing happen learns why from the screen rather than from the
//! source.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use stella_tui_theme::token;

use crate::envelope::SeatRow;

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

/// Cells the seat key keeps however narrow the pane gets. Below this a key is
/// no longer identifiable, and a row whose subject cannot be read is a row
/// that says nothing.
const MIN_KEY_CELLS: usize = 12;

/// Cells between two columns.
const GAP: usize = 3;

/// Draw the SEATS pane into `area`.
///
/// `seats` is `None` while the driver has sent no engine snapshot, which is a
/// different fact from an empty slice and renders as a different line.
pub fn render(seats: Option<&[SeatRow]>, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);

    let mut head = vec![Span::styled(" seats", muted)];
    if let Some(rows) = seats.filter(|rows| !rows.is_empty()) {
        head.push(Span::styled(format!(" · {} · read-only", rows.len()), dim));
    }
    Paragraph::new(Line::from(head)).render(Rect { height: 1, ..area }, buf);

    let body = Rect {
        y: area.y + 1,
        height: area.height - 1,
        ..area
    };
    if body.height == 0 {
        return;
    }

    let rows = match seats {
        None => {
            hint(NO_SNAPSHOT_HINT, muted, body, buf);
            return;
        }
        Some([]) => {
            hint(NO_SEATS_HINT, muted, body, buf);
            return;
        }
        Some(rows) => rows,
    };

    // One row is spent on the `⋯ n more` tail when the list overruns the pane,
    // because a list that simply stops at the last drawn row claims to be
    // complete.
    let height = body.height as usize;
    let visible = if rows.len() > height {
        height.saturating_sub(1).max(1)
    } else {
        height
    };
    let shown = &rows[..visible.min(rows.len())];
    let (key_w, model_w) = columns(shown, body.width as usize);

    let mut lines: Vec<Line<'static>> = shown
        .iter()
        .map(|row| {
            let assigned = row.model.is_some();
            let model = row.model.as_deref().unwrap_or(UNASSIGNED);
            Line::from(vec![
                Span::raw(" "),
                Span::styled(pad(&fit(&row.key, key_w), key_w), text),
                Span::raw(" ".repeat(GAP)),
                // An assignment is a decision someone made and is worth
                // reading; the inherited default is context. Same reasoning as
                // the TOOLS pane giving an explicit switch more weight than an
                // inherited one.
                Span::styled(
                    pad(&fit(model, model_w), model_w),
                    if assigned { text } else { muted },
                ),
                Span::raw(" ".repeat(GAP)),
                Span::styled(format!("from {}", row.from), dim),
            ])
        })
        .collect();
    if rows.len() > visible {
        lines.push(Line::from(Span::styled(
            format!(" ⋯ {} more", rows.len() - visible),
            dim,
        )));
    }
    Paragraph::new(lines).render(body, buf);
}

/// The two padded columns' widths for `rows` in a pane `width` cells wide.
///
/// Shrinking order is meaning order. The plugin name goes first, because it is
/// already the head of every key beside it; the model next; the seat key last,
/// down to [`MIN_KEY_CELLS`] and no further.
fn columns(rows: &[SeatRow], width: usize) -> (usize, usize) {
    let cells = |s: &str| s.chars().count();
    let mut key_w = rows.iter().map(|r| cells(&r.key)).max().unwrap_or(0);
    let mut model_w = rows
        .iter()
        .map(|r| cells(r.model.as_deref().unwrap_or(UNASSIGNED)))
        .max()
        .unwrap_or(0);
    let from_w = rows
        .iter()
        .map(|r| cells(&r.from) + "from ".len())
        .max()
        .unwrap_or(0);

    // A leading cell of air, then the three columns with a gap between each.
    let budget = width.saturating_sub(1 + GAP * 2);
    let mut over = (key_w + model_w + from_w).saturating_sub(budget);
    if over > 0 {
        // The `from` column is the one that gives way first, and it gives way
        // by being pushed off the right edge rather than by being padded
        // shorter: it is last on the row, so the paragraph clips it.
        over = over.saturating_sub(from_w);
    }
    let shed = over.min(model_w);
    model_w -= shed;
    over -= shed;
    key_w = key_w.saturating_sub(over).max(MIN_KEY_CELLS.min(key_w));
    (key_w, model_w)
}

/// `s` cut to `width` cells, with an ellipsis where it was cut.
fn fit(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `s` padded to `width` cells. `format!("{s:width$}")` counts bytes, which
/// pads a key holding a non-ASCII character short.
fn pad(s: &str, width: usize) -> String {
    let mut out = s.to_string();
    out.push_str(&" ".repeat(width.saturating_sub(s.chars().count())));
    out
}

/// One muted line of explanation where the rows would have been.
fn hint(message: &str, style: Style, area: Rect, buf: &mut Buffer) {
    Paragraph::new(Line::from(Span::styled(format!(" {message}"), style)))
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(seats: Option<&[SeatRow]>, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        render(seats, area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
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
        let rows = [seat(
            "acme/second-opinion",
            Some("anthropic/claude-opus-5"),
            "acme",
        )];
        let text = draw(Some(&rows), 90, 12);
        assert!(text.contains("acme/second-opinion"), "{text}");
        assert!(text.contains("anthropic/claude-opus-5"), "{text}");
        assert!(text.contains("from acme"), "{text}");
    }

    /// An unassigned seat says `default`, because that is what it runs on. A
    /// blank would read as "unknown" for something the driver knows exactly.
    #[test]
    fn an_unassigned_seat_names_the_default_rather_than_blanking() {
        let rows = [seat("stella-plan/planner", None, "stella-plan")];
        let text = draw(Some(&rows), 90, 12);
        assert!(text.contains("stella-plan/planner"), "{text}");
        assert!(text.contains(UNASSIGNED), "{text}");
    }

    /// No plugins is the ordinary fresh-install state, and the pane says what
    /// happens instead rather than apologising or showing an empty box.
    #[test]
    fn no_seats_explains_the_default_rather_than_erroring() {
        let text = draw(Some(&[]), 90, 12);
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
        let text = draw(None, 90, 12);
        assert!(text.contains("waiting for the seat list"), "{text}");
        assert!(!text.contains("no installed plugin"), "{text}");
    }

    /// The key is rendered whole. Splitting it to show the plugin separately
    /// would be the deck reading a string it is contractually ignorant of.
    #[test]
    fn the_seat_key_is_rendered_whole() {
        let rows = [seat(
            "vera/test_author",
            Some("openrouter/openai/gpt-5.5"),
            "vera",
        )];
        let text = draw(Some(&rows), 90, 12);
        assert!(text.contains("vera/test_author"), "{text}");
    }

    /// The pane draws its own content and nothing around it: the tab row, the
    /// hint row and the status bar are the frame's, and a box here would be a
    /// second frame inside the first.
    #[test]
    fn the_pane_draws_no_border() {
        let rows = [seat("acme/reviewer", Some("zai/glm-5"), "acme")];
        let text = draw(Some(&rows), 60, 6);
        assert!(
            !text.contains('│') && !text.contains('╭') && !text.contains('┌'),
            "{text}"
        );
        assert!(text.starts_with(" seats · 1 · read-only"), "{text}");
    }

    /// A list longer than the pane names what it could not draw. Stopping at
    /// the last row that fit would claim the list ended there.
    #[test]
    fn an_overrunning_list_counts_what_it_could_not_draw() {
        let rows: Vec<SeatRow> = (0..9)
            .map(|i| seat(&format!("acme/role-{i}"), None, "acme"))
            .collect();
        let text = draw(Some(&rows), 60, 5);
        assert!(text.contains("acme/role-0"), "{text}");
        assert!(text.contains("⋯ 6 more"), "{text}");
        assert!(!text.contains("acme/role-8"), "{text}");
    }

    /// A pane too narrow for all three columns keeps the seat key readable and
    /// lets the plugin name fall off the right edge — the key is what the row
    /// is about, and the plugin's name is the head of the key anyway.
    #[test]
    fn a_narrow_pane_keeps_the_key_and_sheds_the_plugin_name() {
        let rows = [seat(
            "stella-plan/planner",
            Some("anthropic/claude-opus-5"),
            "stella-plan",
        )];
        let text = draw(Some(&rows), 34, 4);
        let row = text.lines().nth(1).unwrap_or_default().to_string();
        assert!(row.contains("stella-plan/planner"), "{row}");
        assert!(!row.contains("from stella-plan"), "{row}");
    }
}
