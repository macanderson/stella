// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SEATS pane — which model each role of this session runs on:
//!
//! ```text
//! seats · 3 · read-only
//!  default               zai/glm-5.2               from default_model
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
//! # The session's own role leads the list
//!
//! [`rows`] puts the roles the driver **resolved** first
//! ([`EngineConfigState::roles`], an open list of names the driver chose —
//! today just `default`), then the roles installed plugins **declared**. The
//! driver supplies each one, so the pane still names nothing: a session with
//! no plugin shows its one role, and a plugin declaring `reviewer` makes that
//! two rows.
//!
//! Leading with the resolved role is what lets the pane answer the question a
//! reader brings to it. Plugin seats alone say which roles run on a model of
//! their own and leave "then what runs everything else?" unanswered, while
//! `default` is the model every unassigned row above already points at.
//!
//! # What a row says, and what it does not
//!
//! Each row is a name, the model it runs on, and where that answer came from.
//! For a plugin seat the name is a seat key (`<plugin-id>/<role>`,
//! `doc:roleless-core` §8.4) and the source is the plugin. For a resolved role
//! the source is the settings key that chose the model — `default_model`,
//! `agents.default.model`, `session default`, `--model (this invocation)` —
//! pre-rendered driver-side like every other cell in this module tree.
//!
//! A seat key is **rendered whole and never split**: the deck has no business
//! knowing which half is the plugin, which is why [`stella_protocol`]-side
//! callers send [`SeatRow::from`](crate::envelope::SeatRow::from) separately
//! rather than letting this pane parse it out.
//!
//! An unassigned seat renders as `default`, not as a blank. That is the truth —
//! an unassigned seat genuinely runs on the session's model — and a blank cell
//! would read as "unknown" for something the driver knows exactly.
//!
//! # Read-only, for now, and that is a smaller claim than AGENTS makes
//!
//! This pane renders; it does not edit. Assigning a model writes
//! `agent_engine_config.seat_models`, and that editor is #6086. Rendering a
//! seat the user cannot yet edit reflects what the driver already knows;
//! editing one before the write path is proved would not. The header says
//! `read-only` so a reader who pressed `e` and saw nothing happen learns why
//! from the screen rather than from the source.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use stella_tui_theme::token;

use crate::envelope::{EngineConfigState, SeatRow};
use crate::render::columns;

/// Shown when the driver has not delivered an engine snapshot yet — a race
/// right after startup, or a driver error. The same shape (and the same
/// remedy) as the TOOLS panel's.
const NO_SNAPSHOT_HINT: &str = "waiting for the seat list — r to reload";

/// Shown when the snapshot arrived and named no rows at all — no resolved role
/// and no declared seat.
///
/// Not an apology or an error. The line says what runs instead rather than
/// implying something is missing.
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

/// The pane's rows: the roles the driver resolved, then the roles installed
/// plugins declared.
///
/// A pure fold over the snapshot, which is what lets the painter below stay a
/// painter. Every row comes from the driver — this function holds no list of
/// role names, and adding one is the defect it exists to prevent.
///
/// A resolved role takes the same row shape as a declared seat: its name, the
/// model it resolved to, and the settings key that chose that model. Its model
/// is always `Some`, because a resolved role has one by definition; an
/// unassigned plugin seat stays `None` and renders as `default`.
#[must_use]
pub fn rows(state: &EngineConfigState) -> Vec<SeatRow> {
    state
        .roles
        .iter()
        .map(|role| SeatRow {
            key: role.role.clone(),
            model: Some(role.model.clone()),
            from: role.source.clone(),
        })
        .chain(state.seats.iter().cloned())
        .collect()
}

/// Draw the SEATS pane into `area`.
///
/// `seats` is `None` while the driver has sent no engine snapshot, which is a
/// different fact from an empty slice and renders as a different line. Build
/// the slice with [`rows`] rather than handing over a raw seat list, or the
/// session's own role goes missing from its own seat list.
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
    // A seat key, a model slug, and a plugin name are all text a plugin
    // manifest chose (`doc:roleless-core` §8.4). This pane has no say in
    // it. So every width here is a display column, never a `char`.
    let cells = columns::width;
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

/// `s` cut to `width` display columns, with an ellipsis where it was cut.
fn fit(s: &str, width: usize) -> String {
    columns::head(s, width)
}

/// `s` padded to `width` display columns. `format!("{s:width$}")` counts
/// `char`s, which under-pads a key holding a CJK or emoji glyph.
fn pad(s: &str, width: usize) -> String {
    columns::pad(s, width)
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

    /// One resolved role as the driver sends it: a name, the model it landed
    /// on, and the settings key that chose it.
    fn resolved(role: &str, model: &str, source: &str) -> crate::envelope::RoleWiringRow {
        crate::envelope::RoleWiringRow {
            role: role.to_string(),
            model: model.to_string(),
            source: source.to_string(),
            ..Default::default()
        }
    }

    fn snapshot(
        roles: Vec<crate::envelope::RoleWiringRow>,
        seats: Vec<SeatRow>,
    ) -> EngineConfigState {
        EngineConfigState {
            roles,
            seats,
            ..Default::default()
        }
    }

    /// The keys of the rows [`rows`] folds out of a snapshot, in order.
    fn keys(state: &EngineConfigState) -> Vec<String> {
        rows(state).into_iter().map(|row| row.key).collect()
    }

    /// **The witness.** One plugin declaring `reviewer`, and the pane is two
    /// rows: the session's own role first, then the plugin's.
    ///
    /// The fold is what puts the first row there. A pane handed `state.seats`
    /// alone draws one row and never names the model everything else runs on,
    /// which is what this asserts against.
    #[test]
    fn the_session_role_leads_the_plugin_seats() {
        let state = snapshot(
            vec![resolved("default", "zai/glm-5.2", "default_model")],
            vec![seat("acme/reviewer", None, "acme")],
        );
        assert_eq!(keys(&state), ["default", "acme/reviewer"]);

        let folded = rows(&state);
        let text = draw(Some(&folded), 90, 12);
        assert!(text.contains("seats · 2 · read-only"), "{text}");
        assert!(text.contains("zai/glm-5.2"), "{text}");
        assert!(text.contains("from default_model"), "{text}");
        assert!(text.contains("acme/reviewer"), "{text}");
    }

    /// A fresh install has no plugin, and the pane is the one row the session
    /// does have rather than a hint that it has none.
    #[test]
    fn a_session_with_no_plugin_still_shows_its_own_role() {
        let state = snapshot(
            vec![resolved(
                "default",
                "anthropic/claude-opus-5",
                "session default",
            )],
            Vec::new(),
        );
        assert_eq!(keys(&state), ["default"]);

        let folded = rows(&state);
        let text = draw(Some(&folded), 90, 12);
        assert!(text.contains("anthropic/claude-opus-5"), "{text}");
        assert!(!text.contains("no installed plugin"), "{text}");
    }

    /// The fold names nothing. A driver that calls the session's own role
    /// something else gets that word back, which is what proves the row is
    /// read rather than written here.
    #[test]
    fn the_leading_row_is_named_by_the_driver_not_by_this_pane() {
        let state = snapshot(
            vec![resolved("lead", "zai/glm-5.2", "default_model")],
            vec![],
        );
        assert_eq!(keys(&state), ["lead"]);
    }

    /// A snapshot the driver sent no rows in draws the hint, not an invented
    /// `default` row: the pane must not answer a question the driver has not.
    #[test]
    fn an_empty_snapshot_invents_no_row() {
        assert!(rows(&EngineConfigState::default()).is_empty());
    }

    /// An unassigned plugin seat stays unassigned through the fold. Filling it
    /// with the leading row's model would make it indistinguishable from a
    /// seat somebody pinned there.
    #[test]
    fn the_fold_leaves_an_unassigned_seat_unassigned() {
        let state = snapshot(
            vec![resolved("default", "zai/glm-5.2", "default_model")],
            vec![seat("vera/test_author", None, "vera")],
        );
        let folded = rows(&state);
        assert_eq!(folded[0].model.as_deref(), Some("zai/glm-5.2"));
        assert_eq!(folded[1].model, None);
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

    /// A CJK seat key is measured in the columns it draws, not its chars.
    /// A plugin can name a role however it likes.
    ///
    /// 13 CJK glyphs are 13 chars but 26 columns — the same 13 chars as the
    /// ASCII key, whose 13 chars are also 13 columns. Old code would still
    /// call the shared key column 13, leaving the CJK row 13 columns over
    /// its padded budget.
    #[test]
    fn columns_measures_a_wide_character_key_in_display_columns() {
        let wide_key = "圈".repeat(13);
        let rows = [
            seat("acme/reviewer", Some("m"), "acme"),
            seat(&wide_key, Some("m"), "acme2"),
        ];
        assert_eq!(columns(&rows, 90), (26, 1));
    }

    /// The same rows, rendered: the CJK row's `from` column lands where
    /// the ASCII row's does, because the key column was sized to the
    /// widest key's real columns, not its char count.
    ///
    /// Checked by buffer column index, not by string search. A wide glyph
    /// fills two buffer cells — the glyph, then an empty one — so a byte
    /// offset in flattened text is not a column index once one is on
    /// screen.
    #[test]
    fn a_wide_character_key_does_not_shift_a_shared_column() {
        let wide_key = "圈".repeat(13);
        let rows = [
            seat("acme/reviewer", Some("m"), "acme"),
            seat(&wide_key, Some("m"), "acme2"),
        ];
        let area = Rect::new(0, 0, 90, 12);
        let mut buf = Buffer::empty(area);
        render(Some(&rows), area, &mut buf);
        let (key_w, model_w) = columns(&rows, area.width as usize);
        let from_col = 1 + key_w + GAP + model_w + GAP;
        let ascii_row_y = 1; // row 0 is the head strip
        let wide_row_y = 2;
        assert_eq!(
            buf.cell((from_col as u16, ascii_row_y)).map(|c| c.symbol()),
            Some("f"),
            "the ascii row's `from` should start at the shared column"
        );
        assert_eq!(
            buf.cell((from_col as u16, wide_row_y)).map(|c| c.symbol()),
            Some("f"),
            "the CJK row's `from` shifted off the shared column"
        );
    }
}
