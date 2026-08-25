// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The TRACES tab — the cross-agent event timeline, one row per event.
//!
//! ```text
//!  all agents · 32 events · following                              f  filter
//!  00:00  lead      [stage] execute
//!  00:00  lead      [tool] edit_file()
//!  00:00  sub:ci    [file] created apps/api/routes/v1/automations.ts +3/-0
//! ```
//!
//! Every row is one [`TraceRow`] from [`WorkspaceModel::trace`], oldest →
//! newest top → bottom, following the tail by default exactly like the
//! transcript pane (`render::render_transcript_window`, L-T4).
//! [`DeckUi::trace_filter`] narrows the timeline to one agent
//! (`TraceLog::for_agent`); `None` interleaves every agent. Both branches walk
//! the same `VecDeque` order, so filtering never reorders events.
//!
//! One header row carries what the tab knows about itself — whose events these
//! are, how many, and where in them the viewport sits — and the `f` key that
//! changes the first of those. Everything else in `area` is timeline: the
//! frame ([`super::frame`]) already spends a row on the tab name, so nothing
//! here draws a box, a title or a second name for the tab.
//!
//! The kind chip keeps its brackets rather than becoming another `·`-separated
//! clause. SPEC 13 asks every state to be legible without colour, and with the
//! palette stripped `[tool]` is still a delimited field where `tool` alone
//! would be the first word of the summary.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget};
use stella_tui_theme::token;

use crate::deck::{TraceRow, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::theme;

/// Columns of air down the left edge, shared by the header and every row so
/// the timeline sits on one margin.
const GUTTER_W: usize = 1;

pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let rows: Vec<&TraceRow> = match ui.trace_filter.as_deref() {
        Some(id) => model.trace.for_agent(id).collect(),
        None => model.trace.rows.iter().collect(),
    };
    let total = rows.len();
    let width = area.width as usize;
    let body = Rect {
        y: area.y + 1,
        height: area.height - 1,
        ..area
    };
    let height = body.height as usize;

    // Record viewport metrics for the pure key handler (`handle_traces_key`)
    // to clamp/scroll on the next keypress — the same contract every
    // scrollable tab follows.
    ui.metrics.trace_total = total;
    ui.metrics.trace_height = height;

    let window = ui.trace_scroll.window(total, height);
    Paragraph::new(header_line(
        ui.trace_filter.as_deref(),
        total,
        &window,
        ui.trace_scroll.follow,
        width,
    ))
    .render(Rect { height: 1, ..area }, buf);

    if body.height == 0 {
        return;
    }
    if total == 0 {
        render_empty_hint(body, buf);
        return;
    }

    let lines: Vec<Line<'static>> = rows[window]
        .iter()
        .map(|row| {
            if ui.accessible {
                row_record(row, model.now_ms, width)
            } else {
                row_line(row, model.now_ms, width)
            }
        })
        .collect();

    Paragraph::new(Text::from(lines)).render(body, buf);
}

/// The header: whose events, how many, where the viewport sits, and the key
/// that changes the first of those.
///
/// The scope takes the filtered agent's own colour ([`theme::agent_color`]),
/// which is the colour its rows carry below — so "which agent am I looking
/// at" is answered by the same hue in both places.
fn header_line(
    filter: Option<&str>,
    total: usize,
    window: &Range<usize>,
    following: bool,
    width: usize,
) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let sep = Span::styled(" · ", dim);

    let mut left = vec![Span::raw(" ".repeat(GUTTER_W))];
    match filter {
        Some(id) => left.push(Span::styled(
            id.to_string(),
            Style::new()
                .fg(theme::agent_color(id))
                .add_modifier(Modifier::BOLD),
        )),
        None => left.push(Span::styled("all agents", muted)),
    }
    left.push(sep.clone());
    if total == 0 {
        left.push(Span::styled("no events yet", dim));
    } else {
        left.push(Span::styled(format!("{total} events"), muted));
        left.push(sep);
        left.push(Span::styled(
            if following {
                "following".to_string()
            } else {
                format!(
                    "{}-{} / {total}",
                    window.start.min(total),
                    window.end.min(total)
                )
            },
            muted,
        ));
    }

    let right = vec![
        Span::styled("f", muted),
        Span::styled(" filter ", dim),
        Span::raw(" ".repeat(GUTTER_W)),
    ];
    let left_w: usize = left.iter().map(Span::width).sum();
    let right_w: usize = right.iter().map(Span::width).sum();
    let mut spans = left;
    if left_w + right_w < width {
        spans.push(Span::raw(" ".repeat(width - left_w - right_w)));
        spans.extend(right);
    }
    Line::from(spans)
}

/// One timeline row: muted relative time, a stable per-agent color, a
/// kind-colored chip, then the summary — truncated to fit `width` so a long
/// summary never wraps and breaks the line-exact scroll math (L-T4).
fn row_line(row: &TraceRow, now_ms: u64, width: usize) -> Line<'static> {
    let mmss = format_mmss(now_ms.saturating_sub(row.ts));
    let kind_chip = format!("[{}]", row.kind.label());
    let prefix_width = GUTTER_W
        + mmss.chars().count()
        + 2
        + row.agent.chars().count()
        + 2
        + kind_chip.chars().count()
        + 1;
    let summary = truncate_to_width(&row.summary, width.saturating_sub(prefix_width));

    Line::from(vec![
        Span::raw(" ".repeat(GUTTER_W)),
        Span::styled(mmss, Style::new().fg(token::DIM)),
        Span::raw("  "),
        Span::styled(
            row.agent.clone(),
            Style::new()
                .fg(theme::agent_color(&row.agent))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            kind_chip,
            Style::new()
                .fg(theme::trace_kind_color(row.kind))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(summary, Style::new().fg(token::TEXT)),
    ])
}

/// One timeline row in accessible mode: the same four values, each saying what
/// it is.
///
/// The default row separates time, agent, kind and summary by position and a
/// colour per field. Read aloud that is four unlabelled tokens in a row —
/// `01:05 lead tool ran the tests` — where the first two could be anything.
/// The chip's brackets go too: `[tool]` is a *visual* chip, and spoken it is
/// punctuation.
fn row_record(row: &TraceRow, now_ms: u64, width: usize) -> Line<'static> {
    let identity = crate::views::record::identity(
        format_mmss(now_ms.saturating_sub(row.ts)),
        false,
        token::MUTED,
    );
    let fields = [
        ("agent", row.agent.clone()),
        ("kind", row.kind.label().to_string()),
        ("summary", row.summary.clone()),
    ];
    crate::views::record::record_line(identity, &fields, width)
}

/// `mm:ss` elapsed since `row.ts`, relative to the deck clock. Grows past two
/// digits of minutes rather than clamping, so a long-running agent's early
/// events still read correctly.
fn format_mmss(elapsed_ms: u64) -> String {
    let total_secs = elapsed_ms / 1000;
    format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
}

/// Truncate to at most `width` chars, adding an ellipsis when clipped. Robust
/// to `width == 0` (empty string) — never panics on a too-narrow terminal.
fn truncate_to_width(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let head: String = text.chars().take(width - 1).collect();
    format!("{head}…")
}

/// Centered muted hint shown when the (possibly filtered) timeline is empty.
fn render_empty_hint(area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    Paragraph::new(Span::styled(
        "no activity yet",
        Style::new().fg(token::MUTED),
    ))
    .alignment(Alignment::Center)
    .render(row, buf);
}

#[cfg(test)]
// The lint is wrong here: these fixtures build with `Type::default()` and
// then set the few fields the test cares about, which reads better than a
// full struct literal that lists every field.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::deck::WorkspaceModel;
    use crate::envelope::{AgentMeta, Inbound};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use stella_protocol::{AgentEvent, StageKind};

    fn reg(id: &str) -> Inbound {
        Inbound::Register(AgentMeta::new(id, format!("goal for {id}"), 0))
    }
    fn ev(agent: &str, event: AgentEvent) -> Inbound {
        Inbound::Event {
            agent: agent.into(),
            event,
        }
    }

    /// Flatten a rendered buffer to one string, styling stripped — content is
    /// what tests assert on, per L-T6 (no raw ANSI in assertions).
    fn buffer_text(buf: &Buffer) -> String {
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

    fn draw(model: &WorkspaceModel, ui: &mut DeckUi, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render(model, ui, area, f.buffer_mut());
            })
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn two_agent_model() -> WorkspaceModel {
        let mut model = WorkspaceModel::new();
        model.now_ms = 65_000; // so elapsed-time formatting has something to chew on
        model.apply_inbound(&reg("a"));
        model.apply_inbound(&reg("b"));
        model.apply_inbound(&ev(
            "a",
            AgentEvent::Stage {
                name: StageKind::Execute.into(),
                scope: stella_protocol::StageScope::Run,
            },
        ));
        model.apply_inbound(&ev(
            "a",
            AgentEvent::Text {
                text: "building the auth refactor".into(),
            },
        ));
        model.apply_inbound(&ev(
            "b",
            AgentEvent::FileChange {
                path: "src/lib.rs".into(),
                kind: stella_protocol::FileChangeKind::Modified,
                added: 1,
                removed: 1,
                diff: Some("+one\n-two\n".into()),
            },
        ));
        model
    }

    /// **The witness.** The tab spends one row on itself and gives the rest to
    /// the timeline: no border, no box-drawing, no second copy of the tab name
    /// that [`super::frame`] already draws.
    #[test]
    fn the_tab_draws_no_chrome_of_its_own() {
        let model = two_agent_model();
        let mut ui = DeckUi::default();
        let text = draw(&model, &mut ui, 100, 20);
        assert!(
            !text.contains(['┌', '┐', '└', '┘', '│', '─']),
            "the frame owns the chrome; the tab draws content:\n{text}"
        );
        let first = text.lines().next().unwrap_or_default();
        assert!(first.contains("all agents · 3 events"), "{first}");
        assert!(first.trim_end().ends_with("f filter"), "{first}");
        assert_eq!(
            ui.metrics.trace_height, 19,
            "the timeline is every row but the header"
        );
    }

    #[test]
    fn empty_timeline_shows_the_centered_hint() {
        let model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        let text = draw(&model, &mut ui, 60, 12);
        assert!(text.contains("no activity yet"), "empty hint:\n{text}");
        assert!(
            text.contains("all agents · no events yet"),
            "the header still says whose events are missing:\n{text}"
        );
        assert_eq!(ui.metrics.trace_total, 0);
    }

    /// A frame with no room for a timeline still draws the header and reports
    /// a zero-height viewport rather than indexing past the buffer.
    #[test]
    fn a_one_row_frame_is_the_header_alone() {
        let model = two_agent_model();
        let mut ui = DeckUi::default();
        let text = draw(&model, &mut ui, 80, 1);
        assert!(text.contains("all agents"), "{text}");
        assert_eq!(ui.metrics.trace_height, 0);
    }

    #[test]
    fn unfiltered_timeline_renders_rows_from_every_agent() {
        let model = two_agent_model();
        let mut ui = DeckUi::default();
        let text = draw(&model, &mut ui, 100, 20);
        assert!(
            text.contains("building the auth refactor"),
            "agent a's text row is visible:\n{text}"
        );
        assert!(
            text.contains("src/lib.rs"),
            "agent b's file row is visible:\n{text}"
        );
        assert!(
            text.contains("all agents"),
            "header shows unfiltered scope:\n{text}"
        );
        assert!(
            text.contains("f filter"),
            "header shows the filter key:\n{text}"
        );
        assert_eq!(ui.metrics.trace_total, model.trace.rows.len());
    }

    #[test]
    fn filtering_to_one_agent_hides_the_others_rows() {
        let model = two_agent_model();
        let mut ui = DeckUi::default();
        ui.trace_filter = Some("a".to_string());
        let text = draw(&model, &mut ui, 100, 20);
        assert!(
            text.contains("building the auth refactor"),
            "agent a's row still shows:\n{text}"
        );
        assert!(
            !text.contains("src/lib.rs"),
            "agent b's row is filtered out:\n{text}"
        );
        let first = text.lines().next().unwrap_or_default();
        assert!(
            first.contains("a · 2 events"),
            "header names the active filter:\n{first}"
        );
        assert_eq!(ui.metrics.trace_total, model.trace.for_agent("a").count());
    }

    #[test]
    fn format_mmss_renders_minutes_and_seconds() {
        assert_eq!(format_mmss(0), "00:00");
        assert_eq!(format_mmss(65_000), "01:05");
        assert_eq!(format_mmss(3_661_000), "61:01");
    }

    #[test]
    fn truncate_to_width_adds_an_ellipsis_only_when_clipped() {
        assert_eq!(truncate_to_width("short", 10), "short");
        assert_eq!(truncate_to_width("a very long summary line", 8), "a very …");
        assert_eq!(truncate_to_width("anything", 0), "");
    }
}
