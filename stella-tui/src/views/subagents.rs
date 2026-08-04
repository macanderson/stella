//! The SESSION tab's nested subagent rows (D5): under the lead's identity
//! header, one block per registered subagent —
//!
//! ```text
//! └─ ◆ sub:auth  subagent · running                                   2:04
//!      model glm-5.2 · ctx 5k/200k · spend $0.01 · scope apps/api/routes/v1/**
//!      ⚒ grep {"pattern":"trigger"}
//!      returns: wire automations triggers API → lead inbox
//! ```
//!
//! The `◆` mark is [`theme::SUBAGENT`] — a categorical role deliberately
//! distinct from the lead's gold `✦`. Focus stays on the existing
//! focused-agent mechanism (`ui.focused` via the AGENTS tab / `⏎`); these
//! rows render the *other* lanes under whichever agent is focused, so they
//! only appear when the focused lane is not itself a subagent.
//!
//! In accessible mode every line is a labeled record (`· subagent auth ·
//! status running …`) — full-width rows, so nothing here ever shares a
//! rendered row with the transcript.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::deck::{AgentEntry, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::model::TranscriptEntry;
use crate::theme;
use crate::views::cards;

/// Rows one subagent block takes.
const ROWS_PER_SUB: u16 = 4;
/// Most subagent blocks the band shows before summarizing the rest — the
/// transcript is still the tab's load-bearing surface.
const MAX_BLOCKS: usize = 2;

/// The band's height for this frame: 0 when the focused lane is a subagent
/// (its own session is the view) or no subagents exist.
pub fn band_height(model: &WorkspaceModel, ui: &DeckUi) -> u16 {
    if model.agents.get(ui.focused).is_some_and(|a| a.is_subagent()) {
        return 0;
    }
    let subs = model.subagent_count();
    let blocks = subs.min(MAX_BLOCKS) as u16;
    if subs > MAX_BLOCKS {
        blocks * ROWS_PER_SUB + 1 // the "+N more" summary row
    } else {
        blocks * ROWS_PER_SUB
    }
}

/// The most recent tool line in a lane's transcript, as `name input`.
fn last_tool_line(agent: &AgentEntry) -> Option<String> {
    agent.model.transcript.iter().rev().find_map(|e| match e {
        TranscriptEntry::ToolStart { name, input, .. } => Some(format!("{name} {input}")),
        _ => None,
    })
}

/// One subagent's block rows.
fn block(sub: &AgentEntry, now_ms: u64, width: usize, accessible: bool) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let mark = Style::new().fg(theme::SUBAGENT);
    let status_style = Style::new().fg(theme::status_color(sub.status));
    let elapsed = cards::fmt_mss(sub.elapsed_ms(now_ms));

    // The vitals: model · ctx · spend, plus the sub's approved write scope
    // when its own fold carries one.
    let model_id = sub.meta.model.clone().unwrap_or_else(|| "—".to_string());
    let ctx = format!("{}/{}", fmt_k(sub.context_tokens), fmt_k(crate::statline::CTX_WINDOW));
    let scope = sub
        .model
        .approved_scope
        .as_ref()
        .or(sub.model.pending_scope_review.as_ref())
        .filter(|p| !p.write_globs.is_empty())
        .map(|p| p.write_globs.join(" · "));

    if accessible {
        let mut rows = vec![Line::from(Span::styled(
            format!(
                "· subagent {} · status {} · elapsed {elapsed}",
                sub.meta.id,
                sub.status.label()
            ),
            Style::new().fg(theme::TEXT_PRIMARY),
        ))];
        let mut vitals = format!("· model {model_id} · ctx {ctx} · spend ${:.2}", sub.cost_usd);
        if let Some(scope) = &scope {
            vitals.push_str(&format!(" · scope {scope}"));
        }
        rows.push(Line::from(Span::styled(vitals, dim)));
        if let Some(tool) = last_tool_line(sub) {
            rows.push(Line::from(Span::styled(
                format!("· last tool {}", cards::truncate_cols(&tool, width.saturating_sub(14))),
                dim,
            )));
        } else {
            rows.push(Line::from(Span::styled("· last tool none yet", dim)));
        }
        rows.push(Line::from(Span::styled(
            format!("· returns {} → lead inbox", sub.meta.title),
            dim,
        )));
        return rows;
    }

    let header = cards::pad_right(
        vec![
            Span::styled("└─ ", dim),
            Span::styled("◆ ", mark),
            Span::styled(sub.meta.id.clone(), mark),
            Span::styled("  subagent", dim),
            Span::styled(" · ", dim),
            Span::styled(sub.status.label().to_string(), status_style),
        ],
        Span::styled(elapsed, dim),
        width,
    );
    let mut vitals = format!("model {model_id} · ctx {ctx} · spend ${:.2}", sub.cost_usd);
    if let Some(scope) = &scope {
        vitals.push_str(&format!(" · scope {scope}"));
    }
    let tool = last_tool_line(sub)
        .map(|t| format!("⚒ {}", cards::truncate_cols(&t, width.saturating_sub(8))))
        .unwrap_or_else(|| "⚒ no tool calls yet".to_string());
    vec![
        Line::from(header),
        Line::from(vec![
            Span::raw("     "),
            Span::styled(cards::truncate_cols(&vitals, width.saturating_sub(6)), dim),
        ]),
        Line::from(vec![Span::raw("     "), Span::styled(tool, dim)]),
        Line::from(vec![
            Span::raw("     "),
            Span::styled(
                cards::truncate_cols(
                    &format!("returns: {} → lead inbox", sub.meta.title),
                    width.saturating_sub(6),
                ),
                dim,
            ),
        ]),
    ]
}

/// Render the subagent band. `area` must be [`band_height`] rows tall.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let width = area.width as usize;
    let subs: Vec<&AgentEntry> = model.agents.iter().filter(|a| a.is_subagent()).collect();
    let mut rows: Vec<Line<'static>> = Vec::new();
    for sub in subs.iter().take(MAX_BLOCKS) {
        rows.extend(block(sub, model.now_ms, width, ui.accessible));
    }
    if subs.len() > MAX_BLOCKS {
        let more = subs.len() - MAX_BLOCKS;
        rows.push(Line::from(Span::styled(
            if ui.accessible {
                format!("· and {more} more subagents · AGENTS tab lists all")
            } else {
                format!("└─ ◆ +{more} more · AGENTS tab lists all")
            },
            Style::new().fg(theme::TEXT_TERTIARY),
        )));
    }
    Paragraph::new(rows).render(area, buf);
}

/// Format a token count compactly (`5k`, `1.2k`, `950`) — the subagent
/// vitals' own copy of the statline convention.
fn fmt_k(n: u64) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}
