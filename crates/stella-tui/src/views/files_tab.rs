// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The FILES tab — the session's file ledger, and the diff behind any row:
//!
//! ```text
//!  ▤ 2 files · +7 -1 · 0 reads
//!   File                           Agent         Op +       -       reads   ×
//! ▸ apps/app/automations/page.tsx  lead          U  +4      -1           0  ×1
//!   apps/api/routes/v1/automation… sub:auth      C  +3      -0           0  ×1
//!
//!  ↵ diff · ↑↓ pick
//! ```
//!
//! One row per (agent, path) touched this session, from
//! [`crate::deck::WorkspaceModel::ledger`]. `⏎` (handled in
//! `deck_ui::handle_files_key`) opens a diff pane under the list; the diff
//! TEXT is looked up via the owning agent's `SessionModel::files[].latest_diff()`
//! — the single event-borne diff data path (`deck.rs` L-T5) — never re-derived
//! here.
//!
//! ## What the port changed
//!
//! The totals moved from a footer pinned under the list to the head strip, so
//! the tab answers "how much did this session touch" in its first row whether
//! or not the diff pane is open, and the two counts a reader compares — the
//! selected row's `+a -b` and the session's — no longer sit a pane apart.
//! Counts at the top, keys at the bottom is the split every other surface
//! makes: [`crate::views::graph_tab`]'s `nodes · n` strip over its `↵ open file`
//! band, and the same in the tools and MCP panes.
//!
//! The keys band is the tab's own, not a second copy of
//! [`crate::views::frame::render_hint_row`]: the FILES rows in [`crate::keymap`]
//! are unhinted, so the deck-wide hint row never names them and this is the
//! only place `esc` and the diff's `↑↓` are written down. It is carved off
//! the bottom of the whole tab area rather than the list's, so it stays put
//! when `⏎` opens the diff pane between them — and the empty ledger draws no
//! band at all, since a hint for a key that would do nothing is one the
//! reader learns to ignore.
//!
//! The op badge takes SPEC 6.2's rail metals rather than a status palette:
//! read silver, write and edit gold, delete red. A CRUD letter
//! ([`crate::textline::crud_letter`]) carries the distinction gold cannot,
//! since `C` and `U` share a metal by that rule.
//!
//! Selection is a `▸` in the gutter as well as a highlight, so it survives
//! `NO_COLOR` and a style-stripped golden frame (SPEC 13 — never colour
//! alone). The path column splits the way every other surface splits one
//! ([`crate::views::transcript`]'s subject): the directory recedes, the basename
//! is the identity a scan down the column is hunting.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget};
use stella_tui_theme::{glyph, token};

use stella_protocol::FileChangeKind;

use crate::deck::{FileLedger, FileRecord, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::diff;

/// The selection gutter: `▸ ` on the selected row, blank on every other.
const GUTTER_W: usize = 2;
/// The rows the tab needs — head strip, column header, one record, keys band —
/// before the keys band is worth the line it costs. Under this, the band is
/// dropped and the content keeps the height.
const KEYS_BAND_FLOOR: u16 = 4;
/// Column widths, in characters, for the fixed (non-path) columns — each
/// includes its own trailing separator space.
const AGENT_W: usize = 13;
const OP_W: usize = 4;
const ADD_W: usize = 8;
const REM_W: usize = 8;
const READS_W: usize = 7;
const CHANGES_W: usize = 5;
/// Floor on the path column so a narrow terminal still shows *something*
/// legible rather than collapsing to zero.
const MIN_PATH_W: usize = 10;

pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let records = &model.ledger.records;

    if records.is_empty() {
        ui.metrics.files_diff_total = 0;
        ui.metrics.files_diff_height = 0;
        render_empty(area, buf);
        return;
    }

    // The keys band comes off the bottom of the WHOLE tab, before the diff
    // pane is split out of what remains — so `⏎` opens the pane between the
    // list and the band rather than pushing the band off the pane.
    let (content, keys_area) = if area.height >= KEYS_BAND_FLOOR {
        let bands = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
        (bands[0], Some(bands[1]))
    } else {
        (area, None)
    };

    let (list_area, diff_area) = if ui.files_diff_open {
        let bands = Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(content);
        (bands[0], Some(bands[1]))
    } else {
        (content, None)
    };

    render_list(&model.ledger, ui.files_sel, list_area, buf);

    match diff_area {
        Some(diff_area) => render_diff_pane(model, ui, records, diff_area, buf),
        None => {
            ui.metrics.files_diff_total = 0;
            ui.metrics.files_diff_height = 0;
        }
    }

    if let Some(keys_area) = keys_area {
        Paragraph::new(keys_line(ui.files_diff_open)).render(keys_area, buf);
    }
}

/// The tab's own key band, in the shorthand every surface writes keys in
/// (chord muted, verb dim, `·` between): what `⏎` does now, and the two verbs
/// that exist only while the diff pane is open and are named nowhere else.
fn keys_line(diff_open: bool) -> Line<'static> {
    let key = Style::new().fg(token::MUTED);
    let dim = Style::new().fg(token::DIM);
    let verbs: &[(&str, &str)] = if diff_open {
        &[("↵/esc", "close diff"), ("↑↓", "scroll")]
    } else {
        &[("↵", "diff"), ("↑↓", "pick")]
    };
    let mut spans = vec![Span::raw(" ")];
    for (i, (chord, label)) in verbs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled((*chord).to_string(), key));
        spans.push(Span::styled(format!(" {label}"), dim));
    }
    Line::from(spans)
}

// ── Empty state ──────────────────────────────────────────────────────────

/// Nothing touched yet: one muted line, no chrome. The frame already says
/// which tab this is.
fn render_empty(area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let row = Rect {
        height: 1,
        y: area.y + area.height / 2,
        ..area
    };
    Paragraph::new(Line::from(Span::styled(
        " no files touched yet",
        Style::new().fg(token::MUTED),
    )))
    .render(row, buf);
}

// ── The head strip + the ledger table ────────────────────────────────────

fn render_list(ledger: &FileLedger, selected: usize, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let width = area.width as usize;

    let bands = Layout::vertical([
        Constraint::Length(1), // totals strip
        Constraint::Length(1), // column header
        Constraint::Min(0),    // rows
    ])
    .split(area);

    Paragraph::new(head_line(ledger)).render(bands[0], buf);
    if bands[1].height == 0 {
        return;
    }
    Paragraph::new(header_line(width)).render(bands[1], buf);

    let body_area = bands[2];
    if body_area.height == 0 {
        return;
    }

    let records = &ledger.records;
    let total = records.len();
    let visible_rows = body_area.height as usize;
    let start = if total <= visible_rows {
        0
    } else {
        // Keep the selected row in view, centered when possible.
        selected
            .saturating_sub(visible_rows.saturating_sub(1) / 2)
            .min(total - visible_rows)
    };
    let end = (start + visible_rows).min(total);

    let lines: Vec<Line<'static>> = records[start..end]
        .iter()
        .enumerate()
        .map(|(offset, rec)| record_line(rec, width, start + offset == selected))
        .collect();
    Paragraph::new(Text::from(lines)).render(body_area, buf);
}

/// `▤ 2 files · +7 -1 · 0 reads` — what this session did to the tree, in the
/// tab's first row. The keys that read it live in the band at the bottom.
fn head_line(ledger: &FileLedger) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    Line::from(vec![
        Span::styled(
            format!(" {} ", glyph::NODE_FILE),
            Style::new().fg(token::GOLD),
        ),
        Span::styled(format!("{} files", ledger.file_count()), muted),
        Span::styled(" · ", dim),
        Span::styled(
            format!("+{}", ledger.total_added()),
            Style::new().fg(token::GREEN),
        ),
        Span::raw(" "),
        Span::styled(
            format!("-{}", ledger.total_removed()),
            Style::new().fg(token::RED),
        ),
        Span::styled(" · ", dim),
        Span::styled(format!("{} reads", ledger.total_reads()), muted),
    ])
}

/// The path column width given the row's total available width: the
/// leftovers after the gutter and the fixed columns, floored at
/// [`MIN_PATH_W`] — but never wider than the row itself, so on a terminal
/// narrower than the fixed columns the path (the row's most meaningful cell)
/// still fits and only the tail columns clip, instead of the path column
/// alone overflowing the pane.
fn path_width(total_width: usize) -> usize {
    let fixed = GUTTER_W + AGENT_W + OP_W + ADD_W + REM_W + READS_W + CHANGES_W;
    total_width
        .saturating_sub(fixed)
        .max(MIN_PATH_W)
        .min(total_width)
}

/// The column header. `+` and `-` are left-aligned because their values are:
/// the pre-port header right-aligned both, which parked each sign at the far
/// end of a column whose numbers start at the near end.
fn header_line(width: usize) -> Line<'static> {
    let pw = path_width(width);
    let text = format!(
        "{:<gw$}{:<pw$}{:<aw$}{:^ow$}{:<dw$}{:<rw$}{:>sw$}{:>cw$}",
        "",
        "File",
        "Agent",
        "Op",
        "+",
        "-",
        "reads",
        "×",
        gw = GUTTER_W,
        pw = pw,
        aw = AGENT_W,
        ow = OP_W,
        dw = ADD_W,
        rw = REM_W,
        sw = READS_W,
        cw = CHANGES_W,
    );
    Line::from(Span::styled(text, Style::new().fg(token::DIM)))
}

fn record_line(rec: &FileRecord, width: usize, selected: bool) -> Line<'static> {
    let muted = Style::new().fg(token::MUTED);
    let pw = path_width(width);
    let path = elide_left(&rec.path, pw);
    let pad = pw.saturating_sub(path.chars().count());
    let agent = elide_left(&rec.agent, AGENT_W.saturating_sub(1));
    let (op_letter, op_metal) = op_style(rec.kind);

    let mut spans = vec![Span::styled(
        if selected {
            format!("{} ", glyph::COLLAPSED)
        } else {
            " ".repeat(GUTTER_W)
        },
        Style::new().fg(token::GOLD),
    )];
    spans.extend(path_spans(&path));
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend([
        // Padded to the full column, unlike the pre-port row, which padded to
        // one less and left every op badge sitting a column left of its own
        // header cell.
        Span::styled(format!("{agent:<aw$}", aw = AGENT_W), muted),
        Span::styled(
            format!("{op_letter:^ow$}", ow = OP_W),
            Style::new().fg(op_metal).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("+{:<w$}", rec.added, w = ADD_W.saturating_sub(1)),
            Style::new().fg(token::GREEN),
        ),
        Span::styled(
            format!("-{:<w$}", rec.removed, w = REM_W.saturating_sub(1)),
            Style::new().fg(token::RED),
        ),
        Span::styled(format!("{:>w$}", rec.reads, w = READS_W), muted),
        Span::styled(
            format!("{:>w$}", format!("×{}", rec.changes), w = CHANGES_W),
            Style::new().fg(token::DIM),
        ),
    ]);

    let mut line = Line::from(spans);
    if selected {
        line.style = Style::new().bg(token::HL).add_modifier(Modifier::BOLD);
    }
    line
}

/// The path split so the basename carries the emphasis and the directory
/// recedes — the rule [`crate::views::transcript`]'s subject follows, applied to
/// a column a reader scans rather than a head they read.
fn path_spans(path: &str) -> Vec<Span<'static>> {
    // Byte-index slicing is safe on the result of `rfind('/')`: `/` is ASCII,
    // so both halves land on char boundaries whatever else the path holds. A
    // trailing separator has no basename to emphasise and renders whole.
    match path.rfind('/').filter(|cut| cut + 1 < path.len()) {
        Some(cut) => vec![
            Span::styled(path[..=cut].to_owned(), Style::new().fg(token::DIM)),
            Span::styled(path[cut + 1..].to_owned(), Style::new().fg(token::TEXT)),
        ],
        _ => vec![Span::styled(path.to_owned(), Style::new().fg(token::TEXT))],
    }
}

/// CRUD badge for one [`FileChangeKind`]: the letter comes from the shared
/// files-panel vocabulary (`textline::crud_letter`, one table for both
/// rendering surfaces — issue #66); the metal is SPEC 6.2's rail palette, so
/// this column reads the same way the transcript's rails do. Create and
/// modify share gold there, which is why the letter is what tells them apart.
fn op_style(kind: FileChangeKind) -> (&'static str, Color) {
    let metal = match kind {
        FileChangeKind::Read => token::SILVER,
        FileChangeKind::Created | FileChangeKind::Modified => token::GOLD,
        FileChangeKind::Deleted => token::RED,
    };
    (crate::textline::crud_letter(kind), metal)
}

/// Left-elide `text` to at most `max` chars, keeping the tail (the
/// meaningful end of a path) and marking the cut with `…`.
fn elide_left(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    if max == 1 {
        return "…".to_string();
    }
    let tail: String = chars[chars.len() - (max - 1)..].iter().collect();
    format!("…{tail}")
}

// ── Diff pane ────────────────────────────────────────────────────────────

fn render_diff_pane(
    model: &WorkspaceModel,
    ui: &mut DeckUi,
    records: &[FileRecord],
    area: Rect,
    buf: &mut Buffer,
) {
    if area.height < 2 || area.width == 0 {
        ui.metrics.files_diff_total = 0;
        ui.metrics.files_diff_height = 0;
        return;
    }
    // PR-style chrome from `crate::diff`: the path rides the top rule, the
    // body carries a line-number gutter, and the bottom rule counts the +/−
    // of the diff actually shown.
    let bands = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);
    let w = area.width as usize;
    let record = records.get(ui.files_sel);
    let title = record
        .map(|r| r.path.clone())
        .unwrap_or_else(|| "diff".to_string());
    Paragraph::new(diff::header_line(&title, w)).render(bands[0], buf);

    let body = bands[1];
    let inner_h = body.height as usize;
    let diff_text = record.and_then(|rec| find_diff(model, rec));
    // The footer states the RECORD's measured delta — the same number its row
    // shows. It used to count the rendered diff instead, so the row and the
    // footer could disagree about one file.
    let (added, removed) = record.map(|r| (r.added, r.removed)).unwrap_or((0, 0));
    match diff_text {
        Some((text, current)) if !text.is_empty() => {
            // A viewer scrolls, so its budget is the generous one — but it is
            // still a budget: a generated file arrives as one hunk of
            // thousands of `+` lines, and this pane re-renders on every frame.
            let mut lines = diff::body_lines_capped(
                text,
                record.map(|r| r.path.as_str()),
                stella_diff::view::VIEW_CAP,
                None,
            )
            .0;
            // Say so when this is an EARLIER mutation's diff. The most recent
            // mutation can legitimately arrive without one (the adoption
            // re-emit attaches numstat and diff text in separate calls, either
            // of which can fail), and showing the previous diff under the
            // footer's cumulative counts without a word would be the same
            // quiet mismatch this pane already has too much of (#1741, #1740).
            if !current {
                lines.insert(
                    0,
                    Line::from(Span::styled(
                        "(an earlier change to this file — the latest one \
                         reported no diff)",
                        Style::new().fg(token::MUTED),
                    )),
                );
            }
            let total = lines.len();
            ui.metrics.files_diff_total = total;
            ui.metrics.files_diff_height = inner_h;
            let window = ui.files_diff_scroll.window(total, inner_h);
            let visible: Vec<Line<'static>> =
                lines.get(window).map(<[Line]>::to_vec).unwrap_or_default();
            Paragraph::new(Text::from(visible)).render(body, buf);
        }
        _ => {
            ui.metrics.files_diff_total = 0;
            ui.metrics.files_diff_height = inner_h;
            Paragraph::new(Line::from(Span::styled(
                "(no diff captured)",
                Style::new().fg(token::MUTED),
            )))
            .render(body, buf);
        }
    }
    Paragraph::new(diff::footer_line(added, removed, w)).render(bands[2], buf);
}

/// The diff TEXT for a ledger record: found via the owning agent's
/// `SessionModel::files[].latest_diff()` (`deck.rs` L-T5) — never re-derived.
/// Borrowed, not cloned: this runs on every ~30 fps frame the diff pane is
/// open, and a single mutation's diff can be hundreds of KiB.
fn find_diff<'a>(model: &'a WorkspaceModel, rec: &FileRecord) -> Option<(&'a str, bool)> {
    let agent = model.agents.iter().find(|a| a.meta.id == rec.agent)?;
    let file = agent.model.files.iter().find(|f| f.path == rec.path)?;
    file.best_diff()
}

#[cfg(test)]
// The lint is wrong here: these fixtures build with `Type::default()` and
// then set the few fields the test cares about, which reads better than a
// full struct literal that lists every field.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::envelope::AgentMeta;
    use crate::envelope::Inbound;
    use stella_protocol::AgentEvent;

    /// Flatten a `Buffer` to one `String` per row (mirrors the convention in
    /// `crate::render`'s tests) — content is what we assert on.
    fn buffer_rows(buf: &Buffer) -> Vec<String> {
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect()
    }

    fn buffer_text(buf: &Buffer) -> String {
        buffer_rows(buf).join("\n")
    }

    fn sample_model() -> WorkspaceModel {
        let mut m = WorkspaceModel::new();
        m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::FileChange {
                path: "src/new_file.rs".into(),
                kind: FileChangeKind::Created,
                added: 2,
                removed: 0,
                diff: Some("+one\n+two\n".into()),
            },
        });
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::FileChange {
                path: "src/existing.rs".into(),
                kind: FileChangeKind::Modified,
                added: 2,
                removed: 1,
                diff: Some("@@ -1,2 +1,3 @@\n context\n-old\n+new\n+another\n".into()),
            },
        });
        m
    }

    #[test]
    fn renders_ledger_rows_and_totals() {
        let model = sample_model();
        let mut ui = DeckUi::default();
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(
            text.contains("new_file.rs"),
            "expected created file path in output:\n{text}"
        );
        assert!(
            text.contains("existing.rs"),
            "expected modified file path in output:\n{text}"
        );
        assert!(
            text.contains("2 files"),
            "expected file count summary:\n{text}"
        );
        assert!(
            text.contains(&format!("+{}", model.ledger.total_added())),
            "expected total added in the head strip:\n{text}"
        );
        assert!(
            text.contains(&format!("-{}", model.ledger.total_removed())),
            "expected total removed in the head strip:\n{text}"
        );
    }

    /// The port's own witness: the tab draws no box of its own. The SPEC 5
    /// frame already carved the content area out and named the tab on the tab
    /// row, so a border here is the deleted rows of chrome coming back one
    /// pane at a time.
    #[test]
    fn the_tab_draws_no_chrome_of_its_own() {
        let model = sample_model();
        let mut ui = DeckUi::default();
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        for glyph in ['┌', '┐', '└', '┘', '│', '╭', '╮', '╰', '╯'] {
            assert!(
                !text.contains(glyph),
                "the FILES tab drew a border glyph {glyph:?}:\n{text}"
            );
        }
    }

    /// SPEC 13: never colour alone. The selected row carries `▸` in the
    /// gutter, which is what survives `NO_COLOR` and a style-stripped golden
    /// frame.
    #[test]
    fn the_selected_row_is_marked_with_a_glyph_not_only_a_highlight() {
        let model = sample_model();
        let mut ui = DeckUi::default();
        ui.files_sel = 1;
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let rows = buffer_rows(&buf);
        let marked: Vec<&String> = rows.iter().filter(|row| row.starts_with('▸')).collect();
        assert_eq!(marked.len(), 1, "exactly one row is marked: {rows:?}");
        assert!(
            marked[0].contains("existing.rs"),
            "the marked row is the selected one: {:?}",
            marked[0]
        );
    }

    /// **Witness.** The keys sit in a band on the bottom row — the split every
    /// other v2 surface makes (`graph_tab`'s `↵ open file`, the tools and MCP
    /// panes' footers), not on the head strip's right edge where the port
    /// first put them. The totals stay where the same surfaces put counts:
    /// the top.
    #[test]
    fn the_keys_ride_a_bottom_band_and_the_totals_the_head_strip() {
        let model = sample_model();
        let mut ui = DeckUi::default();
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let rows = buffer_rows(&buf);

        let added = format!("+{}", model.ledger.total_added());
        assert!(
            rows[0].contains("2 files") && rows[0].contains(&added),
            "the totals stay in the head strip: {:?}",
            rows[0]
        );
        assert!(
            !rows[0].contains('↵'),
            "and the head strip no longer carries a key: {:?}",
            rows[0]
        );
        assert!(
            rows[19].contains("↵ diff") && rows[19].contains("↑↓ pick"),
            "the keys ride the bottom row: {:?}",
            rows[19]
        );
    }

    /// The band names the two verbs that exist only while the pane is open and
    /// are written down nowhere else — the FILES rows in `keymap` are unhinted,
    /// so the deck-wide hint row never reaches them.
    #[test]
    fn the_open_diff_pane_names_its_own_close_and_scroll_keys() {
        let model = sample_model();
        let mut ui = DeckUi::default();
        ui.files_sel = 1;
        ui.files_diff_open = true;
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let rows = buffer_rows(&buf);
        assert!(
            rows[23].contains("↵/esc close diff") && rows[23].contains("↑↓ scroll"),
            "the band follows the pane's state: {:?}",
            rows[23]
        );
        assert!(
            rows[22].contains('─'),
            "and the diff pane's own footer rule is still above it, so the \
             band was carved off the tab rather than out of the pane: {:?}",
            rows[22]
        );
    }

    /// An empty ledger draws no band: `⏎` is guarded on a non-empty ledger in
    /// `handle_files_key`, and `frame`'s hint row states the rule — a hint for
    /// a key that would do nothing is one the reader learns to ignore.
    #[test]
    fn the_empty_ledger_hints_at_no_keys() {
        let model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("no files touched yet"));
        assert!(
            !text.contains('↵'),
            "no key band over an empty tab:\n{text}"
        );
    }

    #[test]
    fn opening_diff_records_metrics_and_shows_diff_text() {
        let model = sample_model();
        let mut ui = DeckUi::default();
        ui.files_sel = 1; // "existing.rs" — the Modified record with a diff
        ui.files_diff_open = true;
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        assert!(ui.metrics.files_diff_total > 0, "diff line count recorded");
        assert!(ui.metrics.files_diff_height > 0, "inner height recorded");
        let text = buffer_text(&buf);
        assert!(text.contains("new"), "expected diff body content:\n{text}");
    }

    /// **Witness.** A later mutation that carries no diff must not destroy the
    /// diff an earlier one delivered (#1741).
    ///
    /// This is the second, independent route to `(no diff captured)` for a
    /// file that has a diff. `touch_file`'s mutation arm used to assign an
    /// owned `latest_diff` field unconditionally, so a mutating `FileChange`
    /// carrying `diff: None` erased it — while `remember_diff` early-returned
    /// on `None`, leaving the good text in `recent_diffs` one field away,
    /// which `find_diff` never read. There is no second field to disagree
    /// with the history now (#4365): `FileState::latest_diff` reads it.
    ///
    /// That event shape is reachable, not theoretical: the pipeline's adoption
    /// re-emit builds each change from `git diff --name-status` and then
    /// attaches numstat and diff text in two independent calls. A path only
    /// the numstat named keeps `diff: None` — `attach_diffs`
    /// would rather report nothing than misattribute a patch. So counts
    /// arrive without text, and if a tool had already edited that path in
    /// session, the good diff was destroyed rather than merely absent.
    ///
    /// The diff has to be shown *and* labelled. Showing the older one
    /// silently would trade this defect for #1740's — an earlier change's text
    /// under the footer's cumulative counts, with nothing on screen to say
    /// which mutation it is.
    #[test]
    fn a_later_mutation_without_a_diff_does_not_erase_the_one_before_it() {
        let mut model = sample_model();
        // The adoption shape: mutating, real counts, no text.
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::FileChange {
                path: "src/existing.rs".into(),
                kind: FileChangeKind::Modified,
                added: 5,
                removed: 2,
                diff: None,
            },
        });

        let mut ui = DeckUi::default();
        ui.files_sel = 1; // "existing.rs"
        ui.files_diff_open = true;
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);

        assert!(
            !text.contains("no diff captured"),
            "the earlier mutation's diff is still remembered, so the pane must \
             not claim nothing was captured:\n{text}"
        );
        assert!(
            text.contains("new"),
            "and it must be that diff's actual body:\n{text}"
        );
        assert!(
            text.contains("earlier change"),
            "shown, but labelled — an older diff under the footer's cumulative \
             counts with no note is the mismatch #1740 is about:\n{text}"
        );
    }

    /// **Witness.** A `/clear` mid-session must not turn every diff pane into
    /// `(no diff captured)`.
    ///
    /// The tab reads two stores with different lifetimes: the row comes from
    /// `WorkspaceModel::ledger`, which `Inbound::SessionReset`
    /// leaves alone, and the diff text from the agent's `SessionModel` (L-T5).
    /// Replacing that model wholesale cut the second and not the first, so
    /// every row survived a clear with its counts intact and lost its diff —
    /// the reported shape was `+64 -6` on the row, footer agreeing, and a pane
    /// saying nothing had been captured. The row has to survive as well as the
    /// diff: a "fix" that dropped the rows too would satisfy a body-only
    /// assertion.
    #[test]
    fn a_session_reset_keeps_the_diff_the_surviving_ledger_row_renders() {
        let mut model = sample_model();
        model.apply_inbound(&Inbound::SessionReset {
            agent: "lead".into(),
        });

        let mut ui = DeckUi::default();
        ui.files_sel = 1; // "existing.rs" — the Modified record
        ui.files_diff_open = true;
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);

        assert!(
            text.contains("existing.rs"),
            "the ledger row itself survives a clear:\n{text}"
        );
        assert!(
            !text.contains("no diff captured"),
            "the row kept its +/- counts, so it must keep its diff too:\n{text}"
        );
        assert!(
            text.contains("+another"),
            "expected the diff body the pre-clear event carried:\n{text}"
        );
        assert!(
            ui.metrics.files_diff_total > 0,
            "a rendered diff records its line count"
        );
    }

    /// The other half of the same reset: the conversation really is gone. A
    /// reset that kept the file ledger by keeping *everything* would pass the
    /// witness above and defeat `/clear`.
    #[test]
    fn a_session_reset_still_blanks_the_transcript() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Text {
                text: "an answer the clear must remove".into(),
            },
        });
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::FileChange {
                path: "src/kept.rs".into(),
                kind: FileChangeKind::Modified,
                added: 1,
                removed: 0,
                diff: Some("@@ -1,0 +1,1 @@\n+kept\n".into()),
            },
        });
        model.apply_inbound(&Inbound::SessionReset {
            agent: "lead".into(),
        });

        let session = &model.agents[0].model;
        assert!(session.transcript.is_empty(), "the conversation is cleared");
        assert!(session.streaming_text.is_empty());
        assert_eq!(
            session.files.len(),
            1,
            "what the session did to the tree is not conversation"
        );
        assert_eq!(
            session.files[0].latest_diff(),
            Some("@@ -1,0 +1,1 @@\n+kept\n")
        );
    }

    #[test]
    fn record_without_a_diff_shows_the_fallback_and_zero_total() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::FileChange {
                path: "src/no_diff.rs".into(),
                kind: FileChangeKind::Deleted,
                added: 0,
                removed: 0,
                diff: None,
            },
        });
        let mut ui = DeckUi::default();
        ui.files_diff_open = true;
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        assert_eq!(ui.metrics.files_diff_total, 0);
        let text = buffer_text(&buf);
        assert!(
            text.contains("no diff captured"),
            "expected fallback text:\n{text}"
        );
    }

    #[test]
    fn read_only_files_render_with_a_read_count() {
        let mut m = WorkspaceModel::new();
        m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        for _ in 0..2 {
            m.apply_inbound(&Inbound::Event {
                agent: "lead".into(),
                event: AgentEvent::FileChange {
                    path: "src/read_me.rs".into(),
                    kind: FileChangeKind::Read,
                    added: 0,
                    removed: 0,
                    diff: None,
                },
            });
        }
        let mut ui = DeckUi::default();
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&m, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(
            text.contains("read_me.rs"),
            "read files appear in the tab:\n{text}"
        );
        assert!(
            text.contains("2 reads"),
            "the head strip totals the reads:\n{text}"
        );
    }

    #[test]
    fn empty_ledger_shows_hint_without_panicking() {
        let model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("no files touched yet"));
    }

    #[test]
    fn path_width_never_exceeds_the_available_row_width() {
        let fixed = GUTTER_W + AGENT_W + OP_W + ADD_W + REM_W + READS_W + CHANGES_W;
        assert_eq!(
            path_width(120),
            120 - fixed,
            "wide rows: path fills the leftovers"
        );
        assert_eq!(path_width(fixed + 2), MIN_PATH_W, "floored at MIN_PATH_W");
        assert_eq!(path_width(8), 8, "capped to the row on very narrow panes");
        assert_eq!(path_width(0), 0);
    }

    /// The directory recedes and the basename does not, the same split the
    /// transcript's subject makes — one path, one reading, on both surfaces.
    #[test]
    fn a_paths_basename_carries_the_emphasis() {
        let spans = path_spans("crates/stella-tui/src/views/files_tab.rs");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "crates/stella-tui/src/views/");
        assert_eq!(spans[0].style.fg, Some(token::DIM));
        assert_eq!(spans[1].content, "files_tab.rs");
        assert_eq!(spans[1].style.fg, Some(token::TEXT));

        let bare = path_spans("Makefile");
        assert_eq!(bare.len(), 1, "a basename alone is already the identity");
        assert_eq!(bare[0].style.fg, Some(token::TEXT));
    }

    #[test]
    fn tiny_area_does_not_panic() {
        let model = sample_model();
        let mut ui = DeckUi::default();
        ui.files_diff_open = true;
        for (w, h) in [(0, 0), (1, 1), (3, 2), (5, 3)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            render(&model, &mut ui, area, &mut buf);
        }
    }
}
