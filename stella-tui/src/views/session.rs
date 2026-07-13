//! Session tab — the focused agent's REPL surface (identity header + HUD +
//! any pending gate card + transcript). It **reuses** the single-session
//! renderers (`render_hud`, `render_transcript`, `render_scope_review`,
//! `render_ask_user`, `entry_lines`) so the classic view is pixel-identical,
//! just scoped to whichever agent `ui.focused` points at. No transcript
//! rendering is duplicated — there is one implementation of "draw a session".

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::deck::{AgentEntry, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::render::{
    entry_lines, inner_height, render_ask_user, render_hud, render_scope_review, render_transcript,
};
use crate::theme;

pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let Some(agent) = model.agents.get(ui.focused) else {
        empty_state(area, buf);
        return;
    };
    let sm = &agent.model;

    // A pending gate (scope review / ask-user) claims its own band; 0 otherwise.
    let gate_h: u16 = if sm.pending_scope_review.is_some() {
        8
    } else if let Some(p) = &sm.pending_ask_user {
        (p.options.len() as u16 + 5).min(12)
    } else {
        0
    };

    let bands = Layout::vertical([
        Constraint::Length(1),      // identity header
        Constraint::Length(3),      // HUD
        Constraint::Length(gate_h), // pending gate (0 = collapsed)
        Constraint::Min(1),         // transcript
    ])
    .split(area);

    render_header(agent, bands[0], buf);
    render_hud(&sm.hud, bands[1], buf);
    if let Some(proposal) = &sm.pending_scope_review {
        render_scope_review(proposal, false, bands[2], buf);
    } else if let Some(prompt) = &sm.pending_ask_user {
        render_ask_user(prompt, false, bands[2], buf);
    }

    // Transcript: fold the focused agent's entries into styled lines, then reuse
    // the line-exact scrolling transcript renderer.
    let mut lines: Vec<Line<'static>> = Vec::new();
    for entry in &sm.transcript {
        entry_lines(entry, &mut lines);
    }
    let height = inner_height(bands[3]);
    ui.metrics.session_total = lines.len();
    ui.metrics.session_height = height;
    let window = ui.session_scroll.window(lines.len(), height);
    render_transcript(&lines, window, ui.session_scroll.follow, bands[3], buf);
}

/// The one-line identity header: `▶ lead · running   goal…`.
fn render_header(agent: &AgentEntry, area: Rect, buf: &mut Buffer) {
    let st = agent.status;
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", theme::status_glyph(st)),
            Style::new().fg(theme::status_color(st)),
        ),
        Span::styled(agent.meta.id.clone(), theme::accent()),
        Span::styled("  ·  ", theme::rule()),
        Span::styled(st.label().to_string(), Style::new().fg(theme::status_color(st))),
        Span::raw("   "),
        Span::styled(agent.meta.title.clone(), theme::muted()),
    ]);
    Paragraph::new(line).render(area, buf);
}

/// Shown when there are no agents at all.
fn empty_state(area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let mid = Rect {
        x: area.x,
        y: area.y + area.height / 2,
        width: area.width,
        height: 1,
    };
    Paragraph::new(Span::styled(
        "no active session — type a prompt and press Enter to dispatch one",
        theme::muted(),
    ))
    .alignment(Alignment::Center)
    .render(mid, buf);
}
