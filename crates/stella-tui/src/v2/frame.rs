// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The deck's chrome — SPEC 5, top to bottom:
//!
//! ```text
//! SESSION  ▸ plan · task 3 wire dedup digest · 2/6            ⌃S plan   stella*
//! …body…
//! >>> █
//! ⏎ queue · esc steer · ^N failure · ^Z fold turn · / commands
//! kimi-k3 · execute · ctx ████▎░░░░ 35% · $0.45 · saved $0.69 · ✉ 21   ? help
//! ```
//!
//! ## What the v1 frame spent, and where it went
//!
//! The v1 deck framed every tab in a three-row bordered tab box, then stacked a
//! one-row identity header, a three-row `stella` stage box, a two-row trace
//! strip and a one-row progress bar around the transcript, and boxed the
//! transcript itself with a quarter-width PLAN rail beside it. Eleven rows and
//! a column of chrome before the first event. Every fact those rows carried
//! has one home now, and none of them is a permanent row:
//!
//! * the **stage**, **model** and **turn spend** of the stage box are cells on
//!   the status bar ([`super::status_bar`]) and the label of every turn's
//!   opening rule ([`super::transcript::turn_begin`]);
//! * the **identity header**'s status word is the status bar's stage cell,
//!   and its clock is the closing rule's `turn 14 done · 0:42`;
//! * the **trace strip**'s "latest activity" is the transcript's own tail,
//!   which now has the rows to show it;
//! * the **progress bar**'s stage position is the breadcrumb's `2/6`, which
//!   counts plan steps rather than mapping a categorical stage onto a fake
//!   percentage;
//! * the **PLAN rail** folds into the breadcrumb on the tab row, with the
//!   `/plan` card (`⌃S`) as its expansion (SPEC 7.3).
//!
//! ## The tab row
//!
//! One row, no border. On the SESSION tab it is the breadcrumb strip: the tab
//! name, then the plan's position. Everywhere else it is the tab list with the
//! active tab in gold. `stella*` holds the right edge on every screen
//! (SPEC 3.3).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use stella_tui_theme::{glyph, token};

use crate::deck::{DeckTab, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::plan::{Plan, PlanState};

/// The accent prompt prefix on every composer row. Chrome, never content.
pub const PROMPT_PREFIX: &str = ">>> ";
/// Display width of [`PROMPT_PREFIX`].
pub const PROMPT_PREFIX_W: usize = 4;

/// Draw the tab row into the top row of `area`.
pub fn render_tab_row(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row = Rect { height: 1, ..area };
    let width = row.width as usize;

    let left = match ui.tab {
        DeckTab::Session => breadcrumb_spans(model, ui),
        tab => tab_list_spans(tab),
    };

    // The wordmark holds the right edge, with one cell of air after it. On a
    // frame too narrow to hold both the tabs and the mark, the tabs win: a
    // reader can always tell which tab is lit, and the brand is on every
    // other screen.
    let mut right: Vec<Span<'static>> = Vec::new();
    if ui.tab == DeckTab::Session && plan_of(model, ui).is_some_and(|p| !p.is_empty()) {
        right.push(Span::styled("⌃S plan", Style::new().fg(token::DIM)));
        right.push(Span::raw("   "));
    }
    right.extend(stella_tui_theme::wordmark::spans());
    right.push(Span::raw(" "));

    let left_w: usize = left.iter().map(Span::width).sum();
    let right_w: usize = right.iter().map(Span::width).sum();
    let mut spans = left;
    if left_w + right_w < width {
        spans.push(Span::raw(" ".repeat(width - left_w - right_w)));
        spans.extend(right);
    }
    Paragraph::new(Line::from(spans)).render(row, buf);
}

/// `SESSION AGENTS TRACES   GRAPH   FILES SKILLS MCP ISSUES SETTINGS` — the
/// active tab in gold with a cell of air either side, the rest muted.
fn tab_list_spans(active: DeckTab) -> Vec<Span<'static>> {
    let muted = Style::new().fg(token::MUTED);
    let lit = Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::raw(" ")];
    for (i, tab) in DeckTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        if *tab == active {
            spans.push(Span::styled(format!("  {}  ", tab.title()), lit));
        } else {
            spans.push(Span::styled(tab.title(), muted));
        }
    }
    spans
}

/// The SESSION tab's breadcrumb: `SESSION  ▸ plan · task 3 wire dedup digest
/// · 2/6`, or `SESSION  ▸ no plan yet` (SPEC 5 item 2).
///
/// At an opened lane it is the **agent path** instead — `SESSION  ▸ lead ▸
/// sub:2 · running · ⌫ back` — because the plan is the lead's and a reader
/// inside a lane needs to know where they are and how to get out more than
/// they need the lead's step count. The path is [`WorkspaceModel::ancestry`],
/// the same tree `⌫` walks.
///
/// The plan carries no revision number yet — the `r3` of the renderings is a
/// plan-graph fact this deck does not fold (#4333) — so the strip names the
/// plan by its state word when it is not simply running.
fn breadcrumb_spans(model: &WorkspaceModel, ui: &DeckUi) -> Vec<Span<'static>> {
    let lit = Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD);
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled("SESSION", lit),
        Span::raw("  "),
        Span::styled(format!("{} ", glyph::COLLAPSED), dim),
    ];
    if let Some(lane) = model.agents.get(ui.focused).filter(|a| a.is_subagent()) {
        let path = model.ancestry(ui.focused);
        for (i, id) in path.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(format!(" {} ", glyph::COLLAPSED), dim));
            }
            let last = i + 1 == path.len();
            spans.push(Span::styled((*id).clone(), if last { text } else { muted }));
        }
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(
            lane.status.label().to_string(),
            Style::new().fg(crate::theme::status_color(lane.status)),
        ));
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled("⌫", muted));
        spans.push(Span::styled(" back", dim));
        return spans;
    }
    let Some(plan) = plan_of(model, ui).filter(|p| !p.is_empty()) else {
        spans.push(Span::styled("no plan yet", dim));
        return spans;
    };
    spans.push(Span::styled("plan", muted));
    if plan.state != PlanState::Started {
        spans.push(Span::styled(format!(" {}", plan.state.label()), muted));
    }
    if let Some(step) = plan.active() {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(format!("task {} ", step.id), muted));
        spans.push(Span::styled(step.title, text));
    }
    let (done, total) = plan.progress();
    spans.push(Span::styled(" · ", dim));
    spans.push(Span::styled(format!("{done}/{total}"), muted));
    spans
}

/// The focused agent's plan, if there is a focused agent.
fn plan_of<'a>(model: &'a WorkspaceModel, ui: &DeckUi) -> Option<&'a Plan> {
    model.agents.get(ui.focused).map(|a| &a.model.plan)
}

/// The keybinding hint row under the composer (SPEC 5 item 4).
///
/// `esc steer` draws only while the focused lane is running — that is the
/// only time the key steers, and a hint for a key that would do nothing is a
/// hint the reader learns to ignore. The queue depth rides the `⏎ queue` hint
/// when there is one: the hint says what `⏎` does, the count says what it has
/// already done. `↓ N sub-agents` draws only while lanes exist, for the same
/// reason — it is the one place the SESSION tab says the lanes are there,
/// now that nothing stacks above the transcript for them.
pub fn render_hint_row(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let key = Style::new().fg(token::MUTED);
    let dim = Style::new().fg(token::DIM);
    let sep = Span::styled(" · ", dim);

    let pending = model.queue.pending();
    let focused = model.agents.get(ui.focused);
    let running = focused.is_some_and(|a| a.status == crate::AgentStatus::Running);
    let lane = focused.filter(|a| a.is_subagent());

    let mut spans = vec![Span::raw(" "), Span::styled("⏎", key)];
    // At an opened lane `⏎` steers that lane (`dispatch::route`) and the
    // queue is the lead's, so the hint names the lane rather than promising
    // a queue the key does not touch.
    spans.push(Span::styled(
        match (lane, pending, ui.dispatch_held) {
            (Some(l), _, _) if l.status.is_active() || l.status == crate::AgentStatus::Paused => {
                format!(" steer {}", l.meta.id)
            }
            (Some(l), _, _) => format!(" ask the lead about {}", l.meta.id),
            (None, 0, _) => " queue".to_string(),
            (None, n, true) => format!(" queue · {n} held"),
            (None, n, false) => format!(" queue · {n} queued"),
        },
        dim,
    ));
    if lane.is_some() {
        spans.push(sep.clone());
        spans.push(Span::styled("⌫", key));
        spans.push(Span::styled(" back", dim));
    } else if running {
        spans.push(sep.clone());
        spans.push(Span::styled("esc", key));
        spans.push(Span::styled(" steer", dim));
    }
    let lanes = model.subagent_count();
    if lanes > 0 {
        spans.push(sep.clone());
        spans.push(Span::styled("↓", key));
        spans.push(Span::styled(
            format!(" {lanes} sub-agent{}", if lanes == 1 { "" } else { "s" }),
            dim,
        ));
    }
    // The rest is the keymap's hinted rows (`crate::keymap::hints`), in the
    // short form the hint row has room for.
    for b in crate::keymap::hints(ui.tab) {
        let (k, label) = short_hint(b);
        spans.push(sep.clone());
        spans.push(Span::styled(k, key));
        spans.push(Span::styled(format!(" {label}"), dim));
    }
    Paragraph::new(Line::from(spans)).render(Rect { height: 1, ..area }, buf);
}

/// A hinted binding in the hint row's shorthand: the chord as `^N`, the
/// description as a word or two. The sheet has the sentence.
fn short_hint(b: &crate::keymap::Binding) -> (String, &'static str) {
    let chord = match b.keys {
        "!cmd" => "!".to_string(),
        keys => keys
            .split(" / ")
            .next()
            .unwrap_or(keys)
            .replace("ctrl-", "^")
            .to_uppercase(),
    };
    let label = match b.keys {
        "ctrl-n / ctrl-p" => "failure",
        "ctrl-z" => "fold turn",
        "ctrl-o" => "expand",
        "!cmd" => "shell",
        "/" => "commands",
        _ => b.does.split([' ', '—', '·']).next().unwrap_or(b.does),
    };
    (chord, label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};
    use stella_protocol::{AgentEvent, ScopeProposal, TaskItem, TaskStatus};

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

    fn planned_model() -> WorkspaceModel {
        let mut m = WorkspaceModel::new();
        m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        let ev = |event| Inbound::Event {
            agent: "lead".into(),
            event,
        };
        m.apply_inbound(&ev(AgentEvent::ScopeReview {
            proposal: ScopeProposal {
                summary: "wire dedup".into(),
                steps: vec![
                    "read sources".into(),
                    "implement".into(),
                    "wire dedup digest".into(),
                ],
                ..Default::default()
            },
        }));
        let item = |id: &str, subject: &str, status| TaskItem {
            id: id.into(),
            subject: subject.into(),
            description: None,
            status,
            owner: None,
            contract: None,
        };
        m.apply_inbound(&ev(AgentEvent::TaskUpdate {
            tasks: vec![
                item("1", "read sources", TaskStatus::Completed),
                item("2", "implement", TaskStatus::Completed),
                item("3", "wire dedup digest", TaskStatus::InProgress),
            ],
        }));
        m
    }

    /// SPEC 5 item 2: on SESSION the tab row is the breadcrumb, naming the
    /// running step and the plan's fraction, with the wordmark on the right.
    #[test]
    fn the_session_tab_row_is_the_plan_breadcrumb() {
        let model = planned_model();
        let ui = DeckUi {
            tab: DeckTab::Session,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);
        render_tab_row(&model, &ui, area, &mut buf);
        let row = text(&buf);
        assert!(row.contains("SESSION  ▸ plan"), "{row}");
        assert!(row.contains("task 3 wire dedup digest"), "{row}");
        assert!(row.contains("2/3"), "{row}");
        assert!(row.trim_end().ends_with("stella*"), "{row}");
    }

    /// At an opened lane the tab row is the agent path — where the reader is
    /// and the key out — and the hint row names the lane `⏎` steers.
    #[test]
    fn at_a_lane_the_tab_row_is_the_agent_path() {
        let mut model = planned_model();
        model.apply_inbound(&Inbound::Register(
            AgentMeta::new("sub:2", "task 2", 0)
                .with_role("subagent")
                .with_parent("lead"),
        ));
        model.apply_inbound(&Inbound::Status {
            agent: "sub:2".into(),
            status: crate::AgentStatus::Running,
        });
        let mut ui = DeckUi {
            tab: DeckTab::Session,
            ..Default::default()
        };
        ui.focus_agent(1);
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);
        render_tab_row(&model, &ui, area, &mut buf);
        let row = text(&buf);
        assert!(
            row.contains("SESSION  ▸ lead ▸ sub:2 · running · ⌫ back"),
            "{row}"
        );
        assert!(
            !row.contains("plan"),
            "the lead's plan is not this lane's: {row}"
        );

        let mut buf = Buffer::empty(area);
        render_hint_row(&model, &ui, area, &mut buf);
        let hint = text(&buf);
        assert!(hint.contains("⏎ steer sub:2"), "{hint}");
        assert!(hint.contains("⌫ back"), "{hint}");
        assert!(!hint.contains("esc steer"), "{hint}");

        model.apply_inbound(&Inbound::Status {
            agent: "sub:2".into(),
            status: crate::AgentStatus::Done,
        });
        let mut buf = Buffer::empty(area);
        render_hint_row(&model, &ui, area, &mut buf);
        let hint = text(&buf);
        assert!(hint.contains("⏎ ask the lead about sub:2"), "{hint}");
    }

    /// Every other tab draws the list, active tab padded, wordmark right.
    #[test]
    fn another_tab_draws_the_list_with_the_active_tab_lit() {
        let model = WorkspaceModel::new();
        let ui = DeckUi {
            tab: DeckTab::Graph,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);
        render_tab_row(&model, &ui, area, &mut buf);
        let row = text(&buf);
        assert!(row.contains("TRACES   GRAPH   FILES"), "{row}");
        assert!(row.trim_end().ends_with("stella*"), "{row}");
        let x = row.find("GRAPH").expect("active tab drawn");
        assert_eq!(
            buf.cell((x as u16, 0)).map(|c| c.fg),
            Some(token::GOLD),
            "the active tab is gold"
        );
    }

    /// The hint row names `esc steer` only while the lane runs.
    #[test]
    fn esc_steer_is_hinted_only_while_running() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        let ui = DeckUi::default();
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);
        render_hint_row(&model, &ui, area, &mut buf);
        let idle = text(&buf);
        assert!(idle.contains("⏎ queue"), "{idle}");
        assert!(!idle.contains("esc steer"), "{idle}");

        model.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status: crate::AgentStatus::Running,
        });
        let mut buf = Buffer::empty(area);
        render_hint_row(&model, &ui, area, &mut buf);
        let running = text(&buf);
        assert!(running.contains("esc steer"), "{running}");
    }
}
