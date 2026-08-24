// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The pulse row — the live agent's last real words, on every tab.
//!
//! ```text
//! ◆ lead  running · quiet 0:04 · running cargo test -p stella-tui   ┆ "Wired the tap; running the tests now."   +2 lanes running
//! ```
//!
//! SPEC 5 folded the v1 trace strip into the transcript's tail, which is
//! right on the SESSION tab and leaves every other tab blind: a reader on
//! FILES or ISSUES had no way to know whether the agent was still working,
//! stuck, or finished, short of tabbing back. This row is that answer, one
//! line tall, and it costs nothing — it draws in the row of air above the
//! composer that every tab already spends.
//!
//! Four facts, in the order a reader asks them: **who** (the agent and its
//! status), **how long since it last did anything** (the quiet time, which is
//! how a stalled lane is told from a busy one), **where it is** (the same
//! lifecycle sentence the SUB-AGENTS overlay prints), and **what it last
//! said** — the newest assistant prose on its transcript, first line only.
//! Tool arguments and results never appear here; "real feedback" is the
//! model's own words or nothing.
//!
//! ## Which agent
//!
//! Off the SESSION tab, the agent most worth watching: the lead while it
//! runs — it is the conversation, and its lanes ride the `+N running` tail —
//! else the running lane that spoke or acted most recently, else whoever is
//! focused. On SESSION the transcript is the pulse, so the row stays air —
//! except at an opened lane, where it carries the **lead**: walking into a
//! lane must never cost sight of the conversation it belongs to.
//!
//! Pure over `(WorkspaceModel, DeckUi)`; [`pulse`] is the decision and
//! [`render_row`] only paints it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use stella_tui_theme::token;

use crate::deck::{AgentEntry, DeckTab, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::envelope::AgentStatus;
use crate::model::TranscriptEntry;
use crate::theme;
use crate::views::cards;

/// What the row says about one agent.
#[derive(Debug, Clone)]
pub struct Pulse<'a> {
    /// Index into `model.agents`.
    pub index: usize,
    pub entry: &'a AgentEntry,
    /// Milliseconds since the agent's last event.
    pub quiet_ms: u64,
    /// Where the agent is — the SUB-AGENTS overlay's lifecycle sentence.
    pub place: String,
    /// The first line of the newest assistant prose, if it has said anything.
    pub said: Option<String>,
    /// Other agents running right now, for the `+N running` tail.
    pub others_running: usize,
}

/// The agent the row is about, or `None` when the row should stay air.
pub fn subject(model: &WorkspaceModel, ui: &DeckUi) -> Option<usize> {
    if model.agents.is_empty() {
        return None;
    }
    if ui.tab == DeckTab::Session {
        // At a lane, the lead; at the lead, nothing — the transcript is it.
        let at_lane = model
            .agents
            .get(ui.focused)
            .is_some_and(|a| a.is_subagent());
        return at_lane
            .then(|| model.ancestry(ui.focused).first().copied())
            .flatten()
            .and_then(|id| model.index_of(id));
    }
    // The lead while it runs — it is the conversation, and the lanes ride the
    // `+N running` tail; otherwise the lane that acted most recently.
    let lead = model
        .agents
        .iter()
        .position(|a| !a.is_subagent() && a.status == AgentStatus::Running);
    let running = model
        .agents
        .iter()
        .enumerate()
        .filter(|(_, a)| a.status == AgentStatus::Running)
        .max_by_key(|(_, a)| a.last_activity_ms)
        .map(|(i, _)| i);
    lead.or(running)
        .or(Some(ui.focused.min(model.agents.len() - 1)))
}

/// The row's content, or `None` when there is nothing to say.
pub fn pulse<'a>(model: &'a WorkspaceModel, ui: &DeckUi) -> Option<Pulse<'a>> {
    let index = subject(model, ui)?;
    let entry = model.agents.get(index)?;
    let others_running = model
        .agents
        .iter()
        .enumerate()
        .filter(|(i, a)| *i != index && a.status == AgentStatus::Running)
        .count();
    Some(Pulse {
        index,
        entry,
        quiet_ms: model.now_ms.saturating_sub(entry.last_activity_ms),
        place: crate::v2::subagents::lifecycle(entry),
        said: last_said(entry),
        others_running,
    })
}

/// The first sentence-like line of the newest assistant prose on `entry`'s
/// transcript. Blank lines, markdown headings, rules and fence markers are
/// skipped so the quote is what the model said, not how it formatted it.
pub fn last_said(entry: &AgentEntry) -> Option<String> {
    entry.model.transcript.iter().rev().find_map(|e| match e {
        TranscriptEntry::Text(text) => text
            .lines()
            .map(str::trim)
            .find(|l| {
                !l.is_empty()
                    && !l.starts_with('#')
                    && !l.chars().all(|c| matches!(c, '-' | '*' | '=' | '`' | '_'))
            })
            .map(str::to_string),
        _ => None,
    })
}

/// Draw the row into the top row of `area`. Draws nothing when the row
/// should stay air.
pub fn render_row(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(p) = pulse(model, ui) else {
        return;
    };
    let width = area.width as usize;
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let mark = if p.entry.is_subagent() {
        Style::new().fg(theme::SUBAGENT)
    } else {
        Style::new().fg(token::GOLD)
    };
    let sep = Span::styled(" · ", dim);

    let mut left: Vec<Span<'static>> = vec![
        Span::raw(" "),
        Span::styled(format!("{} ", theme::status_glyph(p.entry.status)), mark),
        Span::styled(p.entry.meta.id.clone(), mark.add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            p.entry.status.label().to_string(),
            Style::new().fg(theme::status_color(p.entry.status)),
        ),
    ];
    // Quiet time only while something is expected to happen: a finished
    // agent is not "quiet", it is done.
    if !p.entry.status.is_terminal() {
        left.push(sep.clone());
        left.push(Span::styled(
            format!("quiet {}", cards::fmt_mss(p.quiet_ms)),
            quiet_style(p.quiet_ms, p.entry.status),
        ));
    }
    left.push(sep.clone());
    left.push(Span::styled(p.place.clone(), muted));

    let mut right: Vec<Span<'static>> = Vec::new();
    if p.others_running > 0 {
        right.push(Span::styled(
            format!(" +{} running ", p.others_running),
            muted,
        ));
    }

    let left_w: usize = left.iter().map(Span::width).sum();
    let right_w: usize = right.iter().map(Span::width).sum();
    // The quote takes what is left between the facts and the tail; a quote
    // with no room is dropped whole rather than shown as three characters.
    if let Some(said) = &p.said {
        let room = width.saturating_sub(left_w + right_w + 6);
        if room >= 12 {
            left.push(Span::styled("   ┆ ", dim));
            left.push(Span::styled(
                format!("“{}”", cards::truncate_cols(said, room - 2)),
                text,
            ));
        }
    }
    let left_w: usize = left.iter().map(Span::width).sum();
    let mut spans = left;
    if left_w + right_w < width {
        spans.push(Span::raw(" ".repeat(width - left_w - right_w)));
        spans.extend(right);
    }
    Paragraph::new(Line::from(spans)).render(Rect { height: 1, ..area }, buf);
}

/// Quiet time past thirty seconds on a running agent is the one number on
/// this row that should pull the eye — it is what "this one might be dead"
/// looks like before anyone says so.
fn quiet_style(quiet_ms: u64, status: AgentStatus) -> Style {
    if status == AgentStatus::Running && quiet_ms >= STALL_AFTER_MS {
        Style::new().fg(token::RED).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(token::MUTED)
    }
}

/// How long a running agent may go without an event before its quiet time
/// is drawn as a warning. A model call under load takes tens of seconds;
/// minutes is a stall.
pub const STALL_AFTER_MS: u64 = 90_000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};
    use stella_protocol::{AgentEvent, ToolCall};

    fn text_of(buf: &Buffer) -> String {
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

    fn model() -> WorkspaceModel {
        let mut m = WorkspaceModel::new();
        m.apply_inbound(&Inbound::Register(
            AgentMeta::new("lead", "goal", 0).with_role("lead"),
        ));
        m.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status: AgentStatus::Running,
        });
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Text {
                text: "## Plan\n\nWired the tap; running the tests now.\nMore.".into(),
            },
        });
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({ "command": "cargo test -p stella-tui" }),
                },
                sub_agent_id: None,
            },
        });
        m.now_ms = 4_000;
        m
    }

    fn draw(m: &WorkspaceModel, ui: &DeckUi, w: u16) -> String {
        let area = Rect::new(0, 0, w, 1);
        let mut buf = Buffer::empty(area);
        render_row(m, ui, area, &mut buf);
        text_of(&buf)
    }

    /// **The witness.** Off the SESSION tab the row carries the live agent's
    /// status, quiet time, place, and last words — and none of the tool's
    /// arguments beyond the humanized place.
    #[test]
    fn the_row_carries_who_how_long_where_and_what_it_said() {
        let m = model();
        let ui = DeckUi {
            tab: DeckTab::Files,
            ..Default::default()
        };
        let row = draw(&m, &ui, 140);
        assert!(row.contains("lead  running"), "{row}");
        assert!(row.contains("quiet 0:04"), "{row}");
        assert!(row.contains("running cargo test -p stella-tui"), "{row}");
        assert!(
            row.contains("“Wired the tap; running the tests now.”"),
            "the first real line, not the heading: {row}"
        );
        assert!(!row.contains("## Plan"), "{row}");
    }

    /// On SESSION at the lead the row is air; at a lane it is the lead.
    #[test]
    fn on_session_the_row_is_air_at_the_lead_and_the_lead_at_a_lane() {
        let mut m = model();
        let mut ui = DeckUi::default();
        assert_eq!(subject(&m, &ui), None);
        assert_eq!(draw(&m, &ui, 100).trim(), "");

        m.apply_inbound(&Inbound::Register(
            AgentMeta::new("sub:2", "task 2", 0)
                .with_role("subagent")
                .with_parent("lead"),
        ));
        ui.focus_agent(1);
        assert_eq!(subject(&m, &ui), Some(0), "the lead, not the lane");
        assert!(draw(&m, &ui, 100).contains("lead"));
    }

    /// Elsewhere a running agent wins over an idle one, the most recently
    /// active among several, and the tail counts the rest.
    #[test]
    fn a_running_agent_wins_and_the_tail_counts_the_others() {
        let mut m = model();
        m.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status: AgentStatus::Done,
        });
        for (id, ms) in [("sub:1", 1_000), ("sub:2", 3_000)] {
            m.apply_inbound(&Inbound::Register(
                AgentMeta::new(id, id, 0).with_role("subagent"),
            ));
            m.apply_inbound(&Inbound::Status {
                agent: id.into(),
                status: AgentStatus::Running,
            });
            let idx = m.index_of(id).expect("registered");
            m.agents[idx].last_activity_ms = ms;
        }
        let ui = DeckUi {
            tab: DeckTab::Issues,
            ..Default::default()
        };
        let p = pulse(&m, &ui).expect("a subject");
        assert_eq!(
            p.entry.meta.id, "sub:2",
            "most recently active running agent"
        );
        assert_eq!(p.others_running, 1);
        let row = draw(&m, &ui, 120);
        assert!(row.contains("+1 running"), "{row}");
        assert!(row.contains("quiet 0:01"), "{row}");
    }

    /// A long silence on a running agent is drawn as the warning it is.
    #[test]
    fn a_stalled_agent_is_drawn_in_red() {
        let mut m = model();
        m.now_ms = m.agents[0].last_activity_ms + STALL_AFTER_MS;
        let ui = DeckUi {
            tab: DeckTab::Traces,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 120, 1);
        let mut buf = Buffer::empty(area);
        render_row(&m, &ui, area, &mut buf);
        let row = text_of(&buf);
        let x = row.find("quiet").expect("quiet time drawn");
        assert_eq!(
            buf.cell((x as u16, 0)).map(|c| c.fg),
            Some(token::RED),
            "{row}"
        );
    }

    /// A narrow frame drops the quote whole rather than a three-letter stub.
    #[test]
    fn a_narrow_frame_drops_the_quote_whole() {
        let m = model();
        let ui = DeckUi {
            tab: DeckTab::Files,
            ..Default::default()
        };
        let row = draw(&m, &ui, 60);
        assert!(row.contains("lead"), "{row}");
        assert!(!row.contains('“'), "{row}");
    }
}
