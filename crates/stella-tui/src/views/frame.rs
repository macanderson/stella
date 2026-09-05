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
//! Four rows of chrome, and every one of them earns its cell: the tab row, the
//! hint row, the status bar ([`super::status_bar`]) and the composer between
//! them. Nothing here is a permanent panel — a fact that needs more than a cell
//! is somewhere a key opens, not a row every screen pays for. The stage, model
//! and turn spend are status-bar cells and the label of each turn's opening
//! rule ([`super::transcript::turn_begin`]); the plan is the breadcrumb's
//! `2/6`, with the `/plan` card (`⌃S`) as its expansion (SPEC 7.3); the latest
//! activity is the transcript's own tail.
//!
//! ## The tab row
//!
//! One row, no border. On the SESSION tab it is the breadcrumb strip — the tab
//! name, then the plan's position, or the agent path inside a lane — and the
//! tab list everywhere else, with the active tab in gold. `stella*` holds the
//! right edge on every screen — deck or not ([`render_chrome_row`], SPEC 3.3).
//!
//! A SESSION with **no plan and no opened lane** shows the tab list too: the
//! breadcrumb had nothing to say there (`▸ no plan yet`, dead chrome on the
//! default screen), and that row was the only place a new reader could have
//! learned the other eight tabs exist (#5049). The moment a plan or a lane
//! gives the breadcrumb something to say, it takes the row back — which is
//! also the form the renderings draw.
//!
//! The nine titles cost 65 columns and the mark eight more, so a frame under
//! 74 cannot draw both at full length. The `tab_list` submodule resolves that
//! in the list's favour: it is handed the columns left once the mark is paid
//! for and yields rungs until it fits, so the mark holds the right edge at
//! every width (#5072).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use stella_tui_theme::{glyph, token};

use crate::deck::{DeckTab, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::plan::{Plan, PlanState};

mod tab_list;

/// The accent prompt prefix on every composer row. Chrome, never content — it
/// is never part of the submitted string and the caret cannot enter it.
///
/// One definition, because a prompt prefix is a thing a reader learns once.
/// The deck's composer, the AGENTS page's composer and this module had each
/// carried their own copy, and the page's had drifted to a third form (`" ❯ "`,
/// three columns wide) that named the same act with a different glyph (#5051).
pub const PROMPT_PREFIX: &str = ">>> ";
/// Display width of [`PROMPT_PREFIX`].
pub const PROMPT_PREFIX_W: usize = 4;

/// Draw one row of top chrome: `left` from the left edge, the `stella*`
/// wordmark hard against the right with a cell of air after it, and `trailing`
/// — whatever rides just inside the mark — between them (SPEC 3.3).
///
/// Every full-frame surface places the mark through this function rather than
/// aligning it itself. That is the whole point: the deck, the AGENTS page and
/// the fleet dashboard are three different frames, and three copies of "pad to
/// the right edge, less one" is exactly how two of them came to draw no mark at
/// all (#5051).
///
/// On a frame too narrow to hold both, `left` wins and the mark is dropped
/// whole: a reader can always tell which surface they are on, and the brand is
/// on every wider screen.
pub fn render_chrome_row(
    left: Vec<Span<'static>>,
    trailing: Vec<Span<'static>>,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row = Rect { height: 1, ..area };
    let width = row.width as usize;

    let mut right = trailing;
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

/// The tab row's trailing hint (the plan-card chord, Session only) — shared
/// by [`render_tab_row`] and [`tab_row_hit`] so the budget both compute from
/// it is one number.
fn tab_row_trailing(model: &WorkspaceModel, ui: &DeckUi) -> Vec<Span<'static>> {
    let mut trailing: Vec<Span<'static>> = Vec::new();
    if ui.tab == DeckTab::Session && plan_of(model, ui).is_some_and(|p| !p.is_empty()) {
        // The chord comes from the keymap, never from a literal here: this
        // hint is one of three surfaces printing it, and the other two had
        // already drifted apart (#4341).
        trailing.push(Span::styled(
            format!("{} plan", crate::keymap::plan_card_chord()),
            Style::new().fg(token::DIM),
        ));
        trailing.push(Span::raw("   "));
    }
    trailing
}

/// The tab a click on `column` of the tab row selects, or `None`: on the
/// Session breadcrumb (which names plan steps, not tabs), on the air between
/// titles, or past the list. `width` is the frame's — the same rectangle
/// [`render_tab_row`] drew into — so the budget, the rung, and therefore
/// every title's column match what is on screen.
pub fn tab_row_hit(
    model: &WorkspaceModel,
    ui: &DeckUi,
    width: u16,
    column: u16,
) -> Option<DeckTab> {
    if ui.tab == DeckTab::Session && breadcrumb_spans(model, ui).is_some() {
        return None;
    }
    let trailing = tab_row_trailing(model, ui);
    let budget = (width as usize).saturating_sub(right_edge_reserve(&trailing));
    tab_list::hit(ui.tab, budget, column as usize)
}

/// Draw the tab row into the top row of `area`.
pub fn render_tab_row(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let trailing = tab_row_trailing(model, ui);
    let budget = (area.width as usize).saturating_sub(right_edge_reserve(&trailing));
    let left = match ui.tab {
        DeckTab::Session => {
            breadcrumb_spans(model, ui).unwrap_or_else(|| tab_list::spans(DeckTab::Session, budget))
        }
        tab => tab_list::spans(tab, budget),
    };
    render_chrome_row(left, trailing, area, buf);
}

/// The columns [`tab_list::spans`] may not spend: the wordmark, whatever rides
/// inside it, the cell of air [`render_chrome_row`] adds after it, and one
/// column of air before it.
///
/// That last column is what makes the reserve a guarantee rather than an
/// estimate. [`render_chrome_row`] draws the mark only while the two sides are
/// strictly narrower than the row, so a list sized to the exact remainder
/// would meet the edge and lose the mark at the one width the ladder exists to
/// survive. It is also the gap the row is drawn with everywhere else.
///
/// Derived from [`stella_tui_theme::wordmark::spans`] rather than written as a
/// number, so the reserve follows the mark if the mark ever changes.
fn right_edge_reserve(trailing: &[Span<'static>]) -> usize {
    let mark: usize = stella_tui_theme::wordmark::spans()
        .iter()
        .map(Span::width)
        .sum();
    let trailing: usize = trailing.iter().map(Span::width).sum();
    mark + trailing + 2
}

/// The SESSION tab's breadcrumb: `SESSION  ▸ plan · task 3 wire dedup digest
/// · 2/6` (SPEC 5 item 2), or `None` when it has nothing to say — no plan and
/// no opened lane — which hands the row back to the tab list (#5049).
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
fn breadcrumb_spans(model: &WorkspaceModel, ui: &DeckUi) -> Option<Vec<Span<'static>>> {
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
        return Some(spans);
    }
    let plan = plan_of(model, ui).filter(|p| !p.is_empty())?;
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
    Some(spans)
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
        "$cmd" => "$".to_string(),
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
        "$cmd" => "shell",
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

    /// With no plan and no opened lane, SESSION's tab row is the tab list —
    /// the breadcrumb had nothing to say, and the default screen must not be
    /// the one place the other eight tabs are invisible (#5049).
    #[test]
    fn with_no_plan_the_session_row_is_the_tab_list() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        let ui = DeckUi {
            tab: DeckTab::Session,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);
        render_tab_row(&model, &ui, area, &mut buf);
        let row = text(&buf);
        for tab in DeckTab::ALL {
            assert!(
                row.contains(tab.title()),
                "{} missing from {row}",
                tab.title()
            );
        }
        assert!(!row.contains("no plan yet"), "{row}");
        assert!(row.trim_end().ends_with("stella*"), "{row}");
        // SESSION is the lit tab: gold and bold, like every active tab.
        let x = row.find("SESSION").expect("SESSION on the row") as u16;
        assert_eq!(
            buf.cell((x, 0)).expect("cell").fg,
            token::GOLD,
            "the active tab is lit"
        );
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
