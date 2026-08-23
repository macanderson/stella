// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SESSIONS overlay (empty-prompt `←`, `/sessions`):
//!
//! ```text
//! ╭ sessions · 2 live · 3 recent ──────────────────── n new · h 12 more ╮
//! │ ◐ stella: wire the dedup digest              this session · 3h ago │
//! │   Wired a dedup digest into the finding store and its tests.       │
//! │   14 turns · $0.45 · glm-5.2                                       │
//! │ ○ stella: fix the parser                                 ↩ 2d ago │
//! │   …                                                                │
//! ╰ ↑↓ select · ↵ resume · n new · h all · a archive · x delete · esc ╯
//! ```
//!
//! Three rows per session: the title with its phase glyph and age, the
//! one-sentence description the driver wrote for it (the session's own
//! summary until then), and its numbers — turns, spend, the model. The rows
//! come from [`crate::deck_ui::sessions::visible_session_rows`], so what is
//! listed and in what order is decided once, for the keys and the paint.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use stella_tui_theme::{glyph, token};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::deck_ui::sessions::{hidden_session_rows, is_live, visible_session_rows};
use crate::envelope::SessionPhase;

/// Rows one session spends.
const ROWS_PER_SESSION: usize = 3;

/// Draw the overlay over `area`.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let w = area.width.saturating_sub(6).min(110);
    let h = area.height.saturating_sub(4).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    let rows = visible_session_rows(ui, model.now_ms);
    let hidden = hidden_session_rows(ui, model.now_ms);
    let live = rows.iter().filter(|s| is_live(s.phase)).count();
    let selected = ui.sessions_sel.min(rows.len().saturating_sub(1));
    let inner_w = (w as usize).saturating_sub(4);
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if rows.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            if hidden > 0 {
                format!("  nothing recent in this workspace · h shows {hidden} more")
            } else {
                "  no stella sessions registered yet · n starts one".to_string()
            },
            muted,
        )));
    }

    // Window on the selected session so long lists keep it in view.
    let visible = ((h as usize).saturating_sub(2) / ROWS_PER_SESSION).max(1);
    let start = selected
        .saturating_sub(visible.saturating_sub(1) / 2)
        .min(rows.len().saturating_sub(visible));
    for (i, session) in rows.iter().enumerate().skip(start).take(visible) {
        let is_sel = i == selected;
        let (mark, metal) = phase_mark(session.phase);
        let mut title_style = text;
        if is_sel {
            title_style = title_style.bg(token::HL).add_modifier(Modifier::BOLD);
        }
        let age = fmt_age(model.now_ms.saturating_sub(session.updated_ms));
        let tag = if session.mine {
            "this session".to_string()
        } else if session.resumable {
            format!("↩ resume · {age}")
        } else {
            age
        };
        let title_w = inner_w.saturating_sub(tag.chars().count() + 4);
        let mut head = vec![
            Span::styled(
                if is_sel { "▸ " } else { "  " }.to_string(),
                Style::new().fg(token::GOLD),
            ),
            Span::styled(format!("{mark} "), Style::new().fg(metal)),
            Span::styled(truncate(&session.title, title_w), title_style),
        ];
        let used: usize = head.iter().map(Span::width).sum();
        if used + tag.chars().count() < inner_w + 2 {
            head.push(Span::styled(
                " ".repeat(inner_w + 2 - used - tag.chars().count()),
                dim,
            ));
        }
        head.push(Span::styled(tag, if session.mine { muted } else { dim }));
        lines.push(Line::from(head));

        let about = session
            .description
            .as_deref()
            .filter(|d| !d.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if session.summary.is_empty() {
                    "no work recorded yet".to_string()
                } else {
                    session.summary.clone()
                }
            });
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(truncate(&about, inner_w.saturating_sub(4)), muted),
        ]));

        let mut stats: Vec<Span<'static>> = vec![Span::raw("    ")];
        stats.push(Span::styled(
            format!(
                "{} {}",
                session.turns,
                if session.turns == 1 { "turn" } else { "turns" }
            ),
            text,
        ));
        stats.push(Span::styled(" · ", dim));
        stats.push(Span::styled(
            format!("${:.2}", session.spend_micros as f64 / 1_000_000.0),
            Style::new().fg(token::GOLD),
        ));
        if let Some(model) = &session.model {
            stats.push(Span::styled(" · ", dim));
            stats.push(Span::styled(model.clone(), muted));
        }
        stats.push(Span::styled(" · ", dim));
        stats.push(Span::styled(basename(&session.workspace), dim));
        lines.push(Line::from(stats));
    }

    let mut title = vec![
        Span::styled(" sessions", text),
        Span::styled(format!(" · {live} live"), muted),
    ];
    let recent = rows.len().saturating_sub(live);
    if recent > 0 {
        title.push(Span::styled(format!(" · {recent} recent"), muted));
    }
    title.push(Span::raw(" "));
    let mut right = vec![
        Span::styled(" n", Style::new().fg(token::GOLD)),
        Span::styled(" new", dim),
    ];
    if hidden > 0 {
        right.push(Span::styled(" · ", dim));
        right.push(Span::styled("h", Style::new().fg(token::GOLD)));
        right.push(Span::styled(format!(" {hidden} more "), dim));
    } else if ui.sessions_show_all {
        right.push(Span::styled(" · ", dim));
        right.push(Span::styled("h", Style::new().fg(token::GOLD)));
        right.push(Span::styled(" recent only ", dim));
    } else {
        right.push(Span::raw(" "));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(Line::from(title))
        .title(Line::from(right).right_aligned())
        .title_bottom(
            Line::from(Span::styled(
                " ↑↓ select · ↵ resume · n new · h all · a archive · x delete · esc ",
                dim,
            ))
            .right_aligned(),
        );
    Paragraph::new(lines).block(block).render(popup, buf);
}

/// The glyph and metal for a phase (SPEC 4): running is gold, parked is
/// silver, finished is dim, and only an error is red.
fn phase_mark(phase: SessionPhase) -> (char, Color) {
    match phase {
        SessionPhase::InProgress => (glyph::RUNNING, token::GOLD),
        SessionPhase::NeedsInput => (glyph::GATE, token::GOLD_BRIGHT),
        SessionPhase::Paused => (glyph::QUEUED, token::SILVER),
        SessionPhase::Complete => (glyph::DONE, token::MUTED),
        SessionPhase::Cancelled | SessionPhase::Stopped | SessionPhase::Archived => {
            (glyph::QUEUED, token::DIM)
        }
        SessionPhase::Error => (glyph::FAILED, token::RED),
    }
}

/// The last path component, for the dim workspace tag.
fn basename(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_string()
}

/// Cut to `max` characters with an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// A compact `3m ago`-style age from a millisecond delta.
fn fmt_age(delta_ms: u64) -> String {
    let secs = delta_ms / 1000;
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::SessionInfo;

    fn text(buf: &Buffer) -> String {
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

    fn row(id: &str, phase: SessionPhase, updated_ms: u64, mine: bool) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            title: format!("stella: {id}"),
            summary: format!("summary of {id}"),
            description: None,
            workspace: "/w/stella".into(),
            phase,
            started_ms: 0,
            updated_ms,
            mine,
            resumable: !mine,
            turns: 14,
            spend_micros: 450_000,
            model: Some("glm-5.2".into()),
        }
    }

    /// A row states what the session did and what it cost — the description
    /// when the driver has written one, the summary until then — and the
    /// registry's history is counted behind `h`, never listed over it.
    #[test]
    fn a_row_carries_description_turns_spend_and_model() {
        let mut model = WorkspaceModel::new();
        model.now_ms = 40 * 24 * 60 * 60 * 1000;
        let mut ui = DeckUi {
            sessions_open: true,
            ..Default::default()
        };
        let mut described = row(
            "digest",
            SessionPhase::InProgress,
            model.now_ms - 3_600_000,
            true,
        );
        described.description = Some("Wired a dedup digest into the finding store.".into());
        ui.sessions = vec![
            described,
            row(
                "parser",
                SessionPhase::Complete,
                model.now_ms - 2 * 24 * 3_600_000,
                false,
            ),
            row("ancient", SessionPhase::Complete, 0, false),
        ];
        let area = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(area);
        render(&model, &ui, area, &mut buf);
        let frame = text(&buf);
        assert!(frame.contains("Wired a dedup digest"), "{frame}");
        assert!(frame.contains("summary of parser"), "{frame}");
        assert!(frame.contains("14 turns · $0.45 · glm-5.2"), "{frame}");
        assert!(frame.contains("this session"), "{frame}");
        assert!(frame.contains("↩ resume · 2d ago"), "{frame}");
        assert!(
            !frame.contains("stella: ancient"),
            "old history stays behind h:\n{frame}"
        );
        assert!(frame.contains("h 1 more"), "{frame}");
        assert!(frame.contains("n new"), "{frame}");
    }
}
