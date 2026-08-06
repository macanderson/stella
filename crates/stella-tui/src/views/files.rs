//! Files tab — the file ledger (CRUD + line +/-).
//!
//! Renders [`crate::deck::WorkspaceModel::ledger`] — one row per (agent,
//! path) touched this session — as a table of File · Agent · Op · + · - · ×,
//! plus a summary footer. `Enter` (handled in
//! `deck_ui::handle_files_key`) toggles a diff pane below the list; the diff
//! TEXT is looked up via the owning agent's `SessionModel::files[].latest_diff`
//! — the single event-borne diff data path (`deck.rs` L-T5) — never
//! re-derived here. All colors come from [`crate::theme`].

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use stella_protocol::FileChangeKind;

use crate::deck::{FileLedger, FileRecord, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::{diff, theme};

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

    let (list_area, diff_area) = if ui.files_diff_open {
        let bands =
            Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);
        (bands[0], Some(bands[1]))
    } else {
        (area, None)
    };

    render_list(&model.ledger, ui.files_sel, list_area, buf);

    match diff_area {
        Some(diff_area) => render_diff_pane(model, ui, records, diff_area, buf),
        None => {
            ui.metrics.files_diff_total = 0;
            ui.metrics.files_diff_height = 0;
        }
    }
}

// ── Empty state ──────────────────────────────────────────────────────────

fn render_empty(area: Rect, buf: &mut Buffer) {
    let block = Block::default().borders(Borders::ALL).title(" Files ");
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let row = Rect {
        x: inner.x,
        y: inner.y + inner.height / 2,
        width: inner.width,
        height: 1,
    };
    Paragraph::new(Line::from(Span::styled(
        "no files touched yet",
        theme::muted(),
    )))
    .alignment(Alignment::Center)
    .render(row, buf);
}

// ── The ledger table + summary footer ───────────────────────────────────

fn render_list(ledger: &FileLedger, selected: usize, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Files · {} ", ledger.file_count()));
    let inner = block.inner(area);
    block.render(area, buf);

    let (table_area, footer_area) = if inner.height >= 2 {
        let bands = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
        (bands[0], Some(bands[1]))
    } else {
        (inner, None)
    };

    if table_area.height == 0 || table_area.width == 0 {
        return;
    }

    let header_bands =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(table_area);
    let header_area = header_bands[0];
    let body_area = header_bands[1];

    Paragraph::new(header_line(table_area.width as usize)).render(header_area, buf);

    let records = &ledger.records;
    let total = records.len();
    let visible_rows = body_area.height as usize;
    let start = if visible_rows == 0 || total <= visible_rows {
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
        .map(|(offset, rec)| {
            record_line(rec, table_area.width as usize, start + offset == selected)
        })
        .collect();
    Paragraph::new(Text::from(lines)).render(body_area, buf);

    if let Some(footer_area) = footer_area {
        render_footer(ledger, footer_area, buf);
    }
}

fn render_footer(ledger: &FileLedger, area: Rect, buf: &mut Buffer) {
    let line = Line::from(vec![
        Span::styled(format!("{} files", ledger.file_count()), theme::muted()),
        Span::raw("   "),
        Span::styled(
            format!("+{}", ledger.total_added()),
            Style::default().fg(theme::OK).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("-{}", ledger.total_removed()),
            Style::default().fg(theme::BAD).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format!("{} reads", ledger.total_reads()), theme::muted()),
    ]);
    Paragraph::new(line).render(area, buf);
}

/// The path column width given the row's total available width: the
/// leftovers after the fixed columns, floored at [`MIN_PATH_W`] — but never
/// wider than the row itself, so on a terminal narrower than the fixed
/// columns the path (the row's most meaningful cell) still fits and only the
/// tail columns clip, instead of the path column alone overflowing the pane.
fn path_width(total_width: usize) -> usize {
    let fixed = AGENT_W + OP_W + ADD_W + REM_W + READS_W + CHANGES_W;
    total_width
        .saturating_sub(fixed)
        .max(MIN_PATH_W)
        .min(total_width)
}

fn header_line(width: usize) -> Line<'static> {
    let pw = path_width(width);
    let text = format!(
        "{:<pw$}{:<aw$}{:^ow$}{:>dw$}{:>rw$}{:>sw$}{:>cw$}",
        "File",
        "Agent",
        "Op",
        "+",
        "-",
        "reads",
        "×",
        pw = pw,
        aw = AGENT_W,
        ow = OP_W,
        dw = ADD_W,
        rw = REM_W,
        sw = READS_W,
        cw = CHANGES_W,
    );
    Line::from(Span::styled(text, theme::muted()))
}

fn record_line(rec: &FileRecord, width: usize, selected: bool) -> Line<'static> {
    let pw = path_width(width);
    let path = elide_left(&rec.path, pw);
    let agent = elide_left(&rec.agent, AGENT_W.saturating_sub(1));
    let (op_glyph, op_color) = op_style(rec.kind);

    let mut spans = vec![
        Span::styled(format!("{path:<pw$}"), theme::body()),
        Span::styled(
            format!("{agent:<aw$}", aw = AGENT_W.saturating_sub(1)),
            theme::muted(),
        ),
        Span::styled(
            format!("{op_glyph:^ow$}", ow = OP_W),
            Style::default().fg(op_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("+{:<w$}", rec.added, w = ADD_W.saturating_sub(1)),
            Style::default().fg(theme::OK),
        ),
        Span::styled(
            format!("-{:<w$}", rec.removed, w = REM_W.saturating_sub(1)),
            Style::default().fg(theme::BAD),
        ),
        Span::styled(format!("{:>w$}", rec.reads, w = READS_W), theme::muted()),
        Span::styled(
            format!("{:>w$}", format!("×{}", rec.changes), w = CHANGES_W),
            theme::muted(),
        ),
    ];

    if selected {
        for span in &mut spans {
            span.style = span.style.add_modifier(Modifier::REVERSED);
        }
    }

    Line::from(spans)
}

/// CRUD badge for one [`FileChangeKind`]: the letter comes from the shared
/// files-panel vocabulary (`textline::crud_letter`, one table for both
/// rendering surfaces — issue #66); only the palette is this view's.
fn op_style(kind: FileChangeKind) -> (&'static str, ratatui::style::Color) {
    let color = match kind {
        FileChangeKind::Read => theme::MUTED,
        FileChangeKind::Created => theme::OK,
        FileChangeKind::Modified => theme::WARN,
        FileChangeKind::Deleted => theme::BAD,
    };
    (crate::textline::crud_letter(kind), color)
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
            let mut lines = diff::body_lines(text, record.map(|r| r.path.as_str()));
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
                        theme::muted(),
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
                theme::muted(),
            )))
            .render(body, buf);
        }
    }
    Paragraph::new(diff::footer_line(added, removed, w)).render(bands[2], buf);
}

/// The diff TEXT for a ledger record: found via the owning agent's
/// `SessionModel::files[].latest_diff` (`deck.rs` L-T5) — never re-derived.
/// Borrowed, not cloned: this runs on every ~30 fps frame the diff pane is
/// open, and a single mutation's diff can be hundreds of KiB.
fn find_diff<'a>(model: &'a WorkspaceModel, rec: &FileRecord) -> Option<(&'a str, bool)> {
    let agent = model.agents.iter().find(|a| a.meta.id == rec.agent)?;
    let file = agent.model.files.iter().find(|f| f.path == rec.path)?;
    file.best_diff()
}

#[cfg(test)]
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
            "expected total added in footer:\n{text}"
        );
        assert!(
            text.contains(&format!("-{}", model.ledger.total_removed())),
            "expected total removed in footer:\n{text}"
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
    /// file that has a diff. `touch_file`'s mutation arm assigned
    /// `latest_diff = diff.clone()` unconditionally, so a mutating
    /// `FileChange` carrying `diff: None` erased it — while `remember_diff`
    /// early-returns on `None`, leaving the good text in `recent_diffs` one
    /// field away, which `find_diff` never read.
    ///
    /// That event shape is reachable, not theoretical: the pipeline's adoption
    /// re-emit builds each change from `git diff --name-status` and then
    /// attaches numstat and diff text in two independent calls. A path only
    /// the numstat named keeps `diff: None` deliberately — `attach_diffs`
    /// would rather report nothing than misattribute a patch. So counts
    /// arrive without text, and if a tool had already edited that path in
    /// session, the good diff was destroyed rather than merely absent.
    ///
    /// Both halves are asserted. Showing the older diff silently would trade
    /// this defect for #1740's — an earlier change's text under the footer's
    /// cumulative counts, with nothing on screen to say which mutation it is.
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
    /// `WorkspaceModel::ledger`, which `Inbound::SessionReset` deliberately
    /// leaves alone, and the diff text from the agent's `SessionModel` (L-T5).
    /// Replacing that model wholesale cut the second and not the first, so
    /// every row survived a clear with its counts intact and lost its diff —
    /// the reported shape was `+64 -6` on the row, footer agreeing, and a pane
    /// saying nothing had been captured. Both halves are asserted, because a
    /// "fix" that dropped the rows too would satisfy a body-only assertion.
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
            session.files[0].latest_diff.as_deref(),
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
        assert!(text.contains("2 reads"), "footer totals the reads:\n{text}");
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
        let fixed = AGENT_W + OP_W + ADD_W + REM_W + READS_W + CHANGES_W;
        assert_eq!(
            path_width(120),
            120 - fixed,
            "wide rows: path fills the leftovers"
        );
        assert_eq!(path_width(fixed + 2), MIN_PATH_W, "floored at MIN_PATH_W");
        assert_eq!(path_width(8), 8, "capped to the row on very narrow panes");
        assert_eq!(path_width(0), 0);
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
