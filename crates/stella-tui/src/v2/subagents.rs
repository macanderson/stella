// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SUB-AGENTS overlay (`↓` from an empty prompt, `ctrl-a`, `/subagents`):
//! every lane the lead has dispatched and every `delegate` child inside a
//! turn, with controls. It is the only place the sub-agents are drawn — the
//! SESSION tab stacks nothing above the transcript for them.
//!
//! ```text
//! ╭ sub-agents · 2 running · 1 paused ───────────────────────────────────────╮
//! │ ▸ ◆ sub:2  running · quiet 0:04 · 0:24 · moonshotai/kimi-k3 · effort high │
//! │     Simplify docs/spec so every page reads in plain words.               │
//! │     reading docs/spec/adaptive-context/adaptive-context.md               │
//! │                                                                          │
//! │   ◆ sub:3  running · quiet 4:12 · 5:02 · moonshotai/kimi-k3 · $0.03      │
//! │     Simplify the crate READMEs.                                          │
//! │     ran cargo test -p stella-core                                        │
//! │                                                                          │
//! │   ↳ d:1  running · inside lead's turn                                    │
//! │     Survey how the three planes share a store.                           │
//! │     inside lead's turn · no control plane: stop lead to stop it          │
//! ╰ ↑↓ · →↵ open · n nudge · f flag · l lead · ^x^x kill · p pause · r ── esc ╯
//! ```
//!
//! Three rows per entry. The head is its vitals — status, **quiet time**
//! (since its last event; red past [`crate::v2::pulse::STALL_AFTER_MS`] on a
//! running lane, which is what "this one might be dead" looks like), clock,
//! model, the effort its calls are pinned to, spend. The second row is
//! **what it is for**: the sentence the lead handed it
//! ([`crate::envelope::AgentMeta::purpose`]), its title until the driver supplies one. The
//! third is **where it is**: one sentence derived from the lane's own fold
//! ([`lifecycle::lifecycle`]). Nothing here is a tool's raw arguments or a
//! JSON result; the fold's humanized one-liner is the most a row quotes.
//!
//! The verbs ride the wire the old AGENTS dashboard used
//! ([`WorkspaceInput::Control`], #4334): `ctrl-x` twice kills (`s` too, for
//! the hand that knows the old key), `p` pauses a running lane and resumes a
//! paused one, `r` restarts from the lane's retained spec, and `→` or `⏎`
//! opens the lane — focuses its transcript on the SESSION tab, where the
//! composer steers it and `⌫` comes back. `l` is the way back to the lead.
//! Two verbs are this overlay's own ([`rows`] carries their text): `n`
//! **nudges** the lane for a one-line position report, and `f` **flags** it
//! to whoever dispatched it, vitals attached, asking them to check, stop, or
//! take over. A `delegate` child ([`rows::Row::Delegate`]) has no control
//! plane: its control keys are swallowed and the footer says so; `⏎` opens
//! its parent and `f` flags it to its parent.
//!
//! Kill takes two presses because it is the one verb here with no undo: a
//! stopped worker's turn future is dropped, and Restart begins again from
//! the spec, not from where it was. The first press arms and the footer says
//! so; any other key disarms — the same shape the queue editor's clear-all
//! and the SKILLS uninstall use.

pub mod lifecycle;
pub mod rows;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use stella_tui_theme::{glyph, token};

use crate::deck::{AgentEntry, DeckTab, WorkspaceModel};
use crate::deck_ui::{DeckAction, DeckUi, list_nav};
use crate::envelope::{AgentControl, AgentStatus, WorkspaceInput};
use crate::theme;
use crate::views::cards;
pub use lifecycle::{lifecycle, purpose};
use rows::Row;

/// Rows one entry spends, plus one blank between entries.
const ROWS_PER_LANE: usize = 4;

/// The overlay's own state: whether it is up, the selected row, whether the
/// first `ctrl-x` of a kill has been pressed, and the last thing a key did
/// that the footer should say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentsOverlay {
    pub open: bool,
    pub sel: usize,
    /// `ctrl-x` was pressed once on the selected lane; the next `ctrl-x`
    /// kills it, any other key disarms.
    pub kill_armed: bool,
    /// One line the footer shows after a verb — `nudged sub:2`, `flagged
    /// sub:2 to lead`, or why a key did nothing. Cleared by the next key.
    pub notice: Option<String>,
}

/// `↓` from an empty composer on the Session tab opens the overlay when
/// there are sub-agents to list — `↑`'s mirror (the queue editor). `None`
/// lets the key fall through: with none an empty session scrolls as it
/// always did, and with text in the composer `↓` is cursor motion. Gated on
/// *full* composer emptiness, chips included, like `↑`.
///
/// Only from the transcript's **tail**: with a message highlighted `↓` walks
/// the highlight down (the focus tree's list step), and the overlay — the
/// children below the last message — opens on the press after the highlight
/// drops off the end.
pub fn down_opens(key: KeyEvent, model: &WorkspaceModel, ui: &mut DeckUi) -> Option<DeckAction> {
    if ui.tab == DeckTab::Session
        && ui.composer.is_empty()
        && ui.session_selected.is_none()
        && !rows::rows(model).is_empty()
        && matches!(key.code, KeyCode::Down)
    {
        return Some(open(ui));
    }
    None
}

/// The lanes the overlay lists, in registration order.
pub fn lanes(model: &WorkspaceModel) -> Vec<(usize, &AgentEntry)> {
    model
        .agents
        .iter()
        .enumerate()
        .filter(|(_, a)| a.is_subagent())
        .collect()
}

/// Open the overlay on the first row.
pub fn open(ui: &mut DeckUi) -> DeckAction {
    ui.subagents.open = true;
    ui.subagents.sel = 0;
    ui.subagents.kill_armed = false;
    ui.subagents.notice = None;
    DeckAction::Handled
}

/// The overlay's keys: the list keys select, `→`/`⏎` open the row (focus
/// its transcript — a child's parent's), `l` back to the lead, `n` nudge,
/// `f` flag to the dispatcher, `ctrl-x` twice (or `s`) stop, `p` pause a
/// running lane / resume a paused one, `r` restart, Esc/`←`/`q` close.
/// Modal: every other key is swallowed.
pub fn handle_key(key: KeyEvent, model: &WorkspaceModel, ui: &mut DeckUi) -> DeckAction {
    let list = rows::rows(model);
    let count = list.len();
    ui.subagents.sel = ui.subagents.sel.min(count.saturating_sub(1));
    ui.subagents.notice = None;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // The kill arm survives exactly one key: the second `ctrl-x`.
    let armed = std::mem::take(&mut ui.subagents.kill_armed);

    if list_nav::closes(key) || matches!(key.code, KeyCode::Left) {
        ui.subagents.open = false;
        return DeckAction::Handled;
    }
    if list_nav::select(key, &mut ui.subagents.sel, count, true) {
        return DeckAction::Handled;
    }
    let selected = list.get(ui.subagents.sel);
    let lane = |row: &Row| match row {
        Row::Lane(i) => model.agents.get(*i),
        Row::Delegate(_) => None,
    };
    let control = |lane: &AgentEntry, control: AgentControl| {
        DeckAction::Send(WorkspaceInput::Control {
            agent: lane.meta.id.clone(),
            control,
        })
    };
    // A child takes no control verb; say so where the verb was pressed.
    let no_plane = |ui: &mut DeckUi, row: &Row| {
        if let Row::Delegate(d) = row {
            ui.subagents.notice = Some(format!(
                "{} runs inside {}'s turn — no control plane; f flags it, ⏎ opens {}",
                d.agent_id,
                model
                    .agents
                    .get(d.parent)
                    .map(|a| a.meta.id.as_str())
                    .unwrap_or("its parent"),
                model
                    .agents
                    .get(d.parent)
                    .map(|a| a.meta.id.as_str())
                    .unwrap_or("its parent"),
            ));
        }
        DeckAction::Handled
    };
    match key.code {
        KeyCode::Enter | KeyCode::Right => match selected {
            Some(row) => {
                ui.subagents.open = false;
                ui.focus_agent(row.opens());
                DeckAction::Handled
            }
            None => DeckAction::Handled,
        },
        KeyCode::Char('l') => {
            ui.subagents.open = false;
            if let Some(idx) = model.agents.iter().position(|a| !a.is_subagent()) {
                ui.focus_agent(idx);
            }
            DeckAction::Handled
        }
        KeyCode::Char('n') => match selected.and_then(lane) {
            Some(l) if l.status.is_active() || l.status == AgentStatus::Paused => {
                ui.subagents.notice = Some(format!("nudged {}", l.meta.id));
                DeckAction::Send(rows::nudge(l))
            }
            Some(l) => {
                ui.subagents.notice = Some(format!(
                    "{} is {} — nothing to nudge",
                    l.meta.id,
                    l.status.label()
                ));
                DeckAction::Handled
            }
            None => match selected {
                Some(row) => no_plane(ui, row),
                None => DeckAction::Handled,
            },
        },
        KeyCode::Char('f') => match selected {
            Some(row) => match rows::flag(model, row) {
                Some(input) => {
                    let to = model
                        .agents
                        .get(model.parent_of(row.opens()).unwrap_or(row.opens()))
                        .map(|a| a.meta.id.clone())
                        .unwrap_or_default();
                    let who = match row {
                        Row::Lane(i) => model.agents[*i].meta.id.clone(),
                        Row::Delegate(d) => d.agent_id.clone(),
                    };
                    ui.subagents.notice = Some(format!("flagged {who} to {to}"));
                    DeckAction::Send(input)
                }
                None => {
                    ui.subagents.notice = Some("no dispatcher on the deck to flag to".into());
                    DeckAction::Handled
                }
            },
            None => DeckAction::Handled,
        },
        KeyCode::Char('x') if ctrl => match selected {
            Some(row) if !row.controllable() => no_plane(ui, row),
            Some(row) => match lane(row) {
                Some(l) if l.status.is_terminal() => DeckAction::Handled,
                Some(l) if armed => control(l, AgentControl::Stop),
                Some(_) => {
                    ui.subagents.kill_armed = true;
                    DeckAction::Handled
                }
                None => DeckAction::Handled,
            },
            None => DeckAction::Handled,
        },
        KeyCode::Char('s') => match selected {
            Some(row) if !row.controllable() => no_plane(ui, row),
            Some(row) => match lane(row) {
                Some(l) if !l.status.is_terminal() => control(l, AgentControl::Stop),
                _ => DeckAction::Handled,
            },
            None => DeckAction::Handled,
        },
        KeyCode::Char('p') => match selected {
            Some(row) if !row.controllable() => no_plane(ui, row),
            Some(row) => match lane(row) {
                Some(l) if l.status == AgentStatus::Paused => control(l, AgentControl::Resume),
                Some(l) if l.status.is_active() => control(l, AgentControl::Pause),
                _ => DeckAction::Handled,
            },
            None => DeckAction::Handled,
        },
        KeyCode::Char('r') => match selected {
            Some(row) if !row.controllable() => no_plane(ui, row),
            Some(row) => match lane(row) {
                Some(l) => control(l, AgentControl::Restart),
                None => DeckAction::Handled,
            },
            None => DeckAction::Handled,
        },
        _ => DeckAction::Handled,
    }
}

/// The head row's vitals after the status: quiet time, clock, model, effort,
/// spend. Quiet time is the first number because it is the one that changes
/// the reader's next action.
fn vitals(model: &WorkspaceModel, lane: &AgentEntry) -> Vec<Span<'static>> {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let mut spans = Vec::new();
    if !lane.status.is_terminal() {
        let quiet = rows::quiet_ms(model, lane);
        let style = if rows::stalled(model, lane) {
            Style::new().fg(token::RED).add_modifier(Modifier::BOLD)
        } else {
            muted
        };
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(
            format!("quiet {}", cards::fmt_mss(quiet)),
            style,
        ));
    }
    let mut parts = vec![cards::fmt_mss(lane.elapsed_ms(model.now_ms))];
    if let Some(model) = &lane.meta.model {
        parts.push(model.clone());
    }
    if let Some(effort) = &lane.meta.effort {
        parts.push(format!("effort {effort}"));
    }
    parts.push(format!("${:.2}", lane.cost_usd));
    spans.push(Span::styled(" · ", dim));
    spans.push(Span::styled(parts.join(" · "), muted));
    spans
}

/// The same vitals for a `delegate` child, which has fewer of them (#4369).
///
/// Two differences from a lane's, and both are about what the deck can
/// honestly say. The quiet time is the **parent's** and is labelled that way,
/// because the wire does not attribute a child's own events to the child
/// (#4347). The elapsed is absent rather than zero when no bracket was
/// stamped — a replayed session has the transcript rows and none of the
/// clock readings behind them.
fn delegate_vitals(model: &WorkspaceModel, d: &rows::Delegate) -> Vec<Span<'static>> {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let mut spans = Vec::new();
    if let Some(quiet) = rows::delegate_parent_quiet_ms(model, d) {
        let style = if rows::delegate_stalled(model, d) {
            Style::new().fg(token::RED).add_modifier(Modifier::BOLD)
        } else {
            muted
        };
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(
            format!("parent quiet {}", cards::fmt_mss(quiet)),
            style,
        ));
    }
    if let Some(elapsed) = d.elapsed_ms(model.now_ms) {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(cards::fmt_mss(elapsed), muted));
    }
    spans
}

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

    let list = rows::rows(model);
    let selected = ui.subagents.sel.min(list.len().saturating_sub(1));
    let inner_w = (w as usize).saturating_sub(4);
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let mark = Style::new().fg(theme::SUBAGENT);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if list.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  no sub-agents dispatched in this session yet".to_string(),
            muted,
        )));
    }

    let visible = ((h as usize).saturating_sub(2) / ROWS_PER_LANE).max(1);
    let start = selected
        .saturating_sub(visible.saturating_sub(1) / 2)
        .min(list.len().saturating_sub(visible));
    for (i, row) in list.iter().enumerate().skip(start).take(visible) {
        let is_sel = i == selected;
        let mut id_style = mark;
        if is_sel {
            id_style = id_style.bg(token::HL).add_modifier(Modifier::BOLD);
        }
        let cursor = Span::styled(
            if is_sel { "▸ " } else { "  " }.to_string(),
            Style::new().fg(token::GOLD),
        );
        let (head, what, place) = match row {
            Row::Lane(idx) => {
                let lane = &model.agents[*idx];
                let status_style = Style::new().fg(theme::status_color(lane.status));
                let mut head = vec![
                    cursor,
                    Span::styled(format!("{} ", glyph::MEMORY), mark),
                    Span::styled(lane.meta.id.clone(), id_style),
                    Span::styled("  ", dim),
                    Span::styled(lane.status.label().to_string(), status_style),
                ];
                head.extend(vitals(model, lane));
                (head, purpose(&lane.meta).to_string(), lifecycle(lane))
            }
            Row::Delegate(d) => {
                let status = rows::delegate_status(d);
                let parent = model
                    .agents
                    .get(d.parent)
                    .map(|a| a.meta.id.clone())
                    .unwrap_or_default();
                let mut head = vec![
                    cursor,
                    Span::styled(format!("{} ", glyph::TOOL_DELEGATE), mark),
                    Span::styled(d.agent_id.clone(), id_style),
                    Span::styled("  ", dim),
                    Span::styled(
                        status.label().to_string(),
                        Style::new().fg(theme::status_color(status)),
                    ),
                    Span::styled(" · ", dim),
                    Span::styled(format!("inside {parent}'s turn"), muted),
                    Span::styled(
                        if d.write_access { " · writes" } else { "" }.to_string(),
                        dim,
                    ),
                ];
                head.extend(delegate_vitals(model, d));
                (
                    head,
                    d.instruction_preview.clone(),
                    rows::delegate_place(model, d),
                )
            }
        };
        // The head truncates to the row rather than wrapping into the next.
        let mut used = 0usize;
        let mut clipped: Vec<Span<'static>> = Vec::new();
        for span in head {
            let width = span.width();
            if used + width > inner_w {
                let room = inner_w.saturating_sub(used);
                if room > 1 {
                    clipped.push(Span::styled(
                        cards::truncate_cols(&span.content, room),
                        span.style,
                    ));
                }
                break;
            }
            used += width;
            clipped.push(span);
        }
        lines.push(Line::from(clipped));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(cards::truncate_cols(&what, inner_w.saturating_sub(5)), text),
        ]));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(
                cards::truncate_cols(&place, inner_w.saturating_sub(5)),
                muted,
            ),
        ]));
        lines.push(Line::default());
    }

    let lanes = lanes(model);
    let running = lanes
        .iter()
        .filter(|(_, a)| a.status == AgentStatus::Running)
        .count();
    let paused = lanes
        .iter()
        .filter(|(_, a)| a.status == AgentStatus::Paused)
        .count();
    let done = lanes.iter().filter(|(_, a)| a.status.is_terminal()).count();
    let stalled = lanes
        .iter()
        .filter(|(_, a)| rows::stalled(model, a))
        .count();
    let children = list.iter().filter(|r| !r.controllable()).count();
    let mut title = vec![Span::styled(" sub-agents", text)];
    for (n, word) in [(running, "running"), (paused, "paused"), (done, "finished")] {
        if n > 0 {
            title.push(Span::styled(format!(" · {n} {word}"), muted));
        }
    }
    if stalled > 0 {
        title.push(Span::styled(
            format!(" · {stalled} quiet"),
            Style::new().fg(token::RED).add_modifier(Modifier::BOLD),
        ));
    }
    if children > 0 {
        title.push(Span::styled(
            format!(
                " · {children} delegate{}",
                if children == 1 { "" } else { "s" }
            ),
            muted,
        ));
    }
    title.push(Span::raw(" "));
    let footer = if ui.subagents.kill_armed {
        Span::styled(
            " ctrl-x again kills the selected lane · any other key keeps it ",
            Style::new().fg(token::RED).add_modifier(Modifier::BOLD),
        )
    } else if let Some(notice) = &ui.subagents.notice {
        Span::styled(format!(" {notice} "), Style::new().fg(token::GOLD))
    } else {
        Span::styled(
            " ↑↓ · →↵ open · n nudge · f flag · l lead · ^x^x kill · p pause/resume · r restart · esc ",
            dim,
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(Line::from(title))
        .title_bottom(Line::from(footer).right_aligned());
    Paragraph::new(lines).block(block).render(popup, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};
    use crossterm::event::KeyModifiers;
    use stella_protocol::{AgentEvent, SubAgentPhase, SubAgentStatus, ToolCall};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn model_with(lanes: &[(&str, AgentStatus)]) -> WorkspaceModel {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(
            AgentMeta::new("lead", "stella", 0).with_role("lead"),
        ));
        for (id, status) in lanes {
            model.apply_inbound(&Inbound::Register(
                AgentMeta::new(*id, format!("task {id}"), 0)
                    .with_role("subagent")
                    .with_parent("lead"),
            ));
            model.apply_inbound(&Inbound::Status {
                agent: (*id).to_string(),
                status: *status,
            });
        }
        model
    }

    fn tool_start(model: &mut WorkspaceModel, agent: &str, name: &str, path: &str) {
        model.apply_inbound(&Inbound::Event {
            agent: agent.to_string(),
            event: AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: format!("c-{name}-{path}"),
                    name: name.to_string(),
                    input: serde_json::json!({ "path": path }),
                },
            },
        });
    }

    fn delegate_started(model: &mut WorkspaceModel, parent: &str, id: &str, preview: &str) {
        model.apply_inbound(&Inbound::Event {
            agent: parent.to_string(),
            event: AgentEvent::SubAgent {
                phase: SubAgentPhase::Started {
                    agent_id: id.to_string(),
                    instruction_preview: preview.to_string(),
                    effort: None,
                    write_access: false,
                    budget_usd: Some(0.5),
                    depth: 1,
                },
            },
        });
    }

    fn text_of(model: &WorkspaceModel, ui: &DeckUi, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render(model, ui, area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// **The witness for #4334.** The verbs reach the wire for the row under
    /// the cursor: `s` stops, `p` pauses a running lane and resumes a paused
    /// one, `r` restarts, and `⏎` focuses the lane and closes the overlay.
    #[test]
    fn the_keys_control_the_selected_lane() {
        let model = model_with(&[
            ("sub:1", AgentStatus::Running),
            ("sub:2", AgentStatus::Paused),
        ]);
        let mut ui = DeckUi::default();
        open(&mut ui);
        assert!(ui.subagents.open);

        let sent = |action: DeckAction| match action {
            DeckAction::Send(WorkspaceInput::Control { agent, control }) => (agent, control),
            other => panic!("expected a control, got {other:?}"),
        };
        assert_eq!(
            sent(handle_key(key(KeyCode::Char('s')), &model, &mut ui)),
            ("sub:1".to_string(), AgentControl::Stop)
        );
        assert_eq!(
            sent(handle_key(key(KeyCode::Char('p')), &model, &mut ui)),
            ("sub:1".to_string(), AgentControl::Pause)
        );
        handle_key(key(KeyCode::Char('j')), &model, &mut ui);
        assert_eq!(
            sent(handle_key(key(KeyCode::Char('p')), &model, &mut ui)),
            ("sub:2".to_string(), AgentControl::Resume),
            "p on a paused lane resumes it; j moved the cursor"
        );
        assert_eq!(
            sent(handle_key(key(KeyCode::Char('r')), &model, &mut ui)),
            ("sub:2".to_string(), AgentControl::Restart)
        );
        assert_eq!(
            handle_key(key(KeyCode::Enter), &model, &mut ui),
            DeckAction::Handled
        );
        assert!(!ui.subagents.open, "enter closes the overlay");
        assert_eq!(
            ui.focused, 2,
            "and focuses the lane's transcript (index into model.agents)"
        );
    }

    /// **The witness for `n` and `f`.** A nudge is a steer at the lane asking
    /// where it is; a flag carries the lane's vitals to its dispatcher — as a
    /// steer while the dispatcher runs, as its next prompt when it is idle.
    #[test]
    fn nudge_steers_the_lane_and_flag_tells_the_dispatcher() {
        let mut model = model_with(&[("sub:2", AgentStatus::Running)]);
        model.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status: AgentStatus::Running,
        });
        tool_start(&mut model, "sub:2", "read_file", "docs/spec/a.md");
        model.now_ms = model.agents[1].last_activity_ms + 252_000;
        let mut ui = DeckUi::default();
        open(&mut ui);

        match handle_key(key(KeyCode::Char('n')), &model, &mut ui) {
            DeckAction::Send(WorkspaceInput::Steer { agent, texts }) => {
                assert_eq!(agent, "sub:2");
                assert!(texts[0].contains("say where you are"), "{texts:?}");
            }
            other => panic!("a nudge is a steer at the lane, got {other:?}"),
        }
        assert_eq!(ui.subagents.notice.as_deref(), Some("nudged sub:2"));

        match handle_key(key(KeyCode::Char('f')), &model, &mut ui) {
            DeckAction::Send(WorkspaceInput::Steer { agent, texts }) => {
                assert_eq!(agent, "lead", "to the dispatcher, not the lane");
                let t = &texts[0];
                assert!(t.contains("Lane sub:2"), "{t}");
                assert!(t.contains("quiet for 4:12"), "{t}");
                assert!(t.contains("reading docs/spec/a.md"), "{t}");
                assert!(
                    t.contains("Check on it, stop it, or take the task over"),
                    "{t}"
                );
            }
            other => panic!("a flag steers the running dispatcher, got {other:?}"),
        }
        assert_eq!(
            ui.subagents.notice.as_deref(),
            Some("flagged sub:2 to lead")
        );

        model.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status: AgentStatus::Done,
        });
        assert!(
            matches!(
                handle_key(key(KeyCode::Char('f')), &model, &mut ui),
                DeckAction::Send(WorkspaceInput::Enqueue { .. })
            ),
            "an idle dispatcher gets the flag as its next prompt"
        );
    }

    /// **The witness for #4347.** A `delegate` child is a row under its
    /// parent with the truth about what it is; its control keys are swallowed
    /// with a notice, `⏎` opens the parent, and `f` flags it to the parent.
    #[test]
    fn a_delegate_child_is_listed_under_its_parent_without_a_control_plane() {
        let mut model = model_with(&[("sub:1", AgentStatus::Running)]);
        model.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status: AgentStatus::Running,
        });
        delegate_started(
            &mut model,
            "lead",
            "d:1",
            "Survey how the three planes share a store.",
        );
        let list = rows::rows(&model);
        assert_eq!(list.len(), 2);
        assert!(
            matches!(&list[0], Row::Delegate(d) if d.parent == 0 && d.agent_id == "d:1"),
            "the lead's child comes first, under the lead: {list:?}"
        );

        let mut ui = DeckUi::default();
        open(&mut ui);
        let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        for k in [
            key(KeyCode::Char('s')),
            key(KeyCode::Char('p')),
            key(KeyCode::Char('r')),
            ctrl_x,
            ctrl_x,
        ] {
            assert_eq!(
                handle_key(k, &model, &mut ui),
                DeckAction::Handled,
                "swallowed, never sent"
            );
            assert!(!ui.subagents.kill_armed, "a child never arms a kill");
            assert!(
                ui.subagents
                    .notice
                    .as_deref()
                    .is_some_and(|n| n.contains("no control plane")),
                "{:?}",
                ui.subagents.notice
            );
        }
        match handle_key(key(KeyCode::Char('f')), &model, &mut ui) {
            DeckAction::Send(WorkspaceInput::Steer { agent, texts }) => {
                assert_eq!(agent, "lead");
                assert!(texts[0].contains("Your delegate d:1"), "{texts:?}");
            }
            other => panic!("{other:?}"),
        }
        handle_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(ui.focused, 0, "⏎ opens the parent's transcript");

        let text = text_of(
            &model,
            &DeckUi {
                subagents: SubagentsOverlay {
                    open: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            110,
            14,
        );
        assert!(
            text.contains("↳ d:1  running · inside lead's turn"),
            "{text}"
        );
        assert!(
            text.contains("Survey how the three planes share a store."),
            "{text}"
        );
        assert!(
            text.contains("no control plane: stop lead to stop it"),
            "{text}"
        );
        assert!(text.contains("1 delegate"), "{text}");

        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::SubAgent {
                phase: SubAgentPhase::Finished {
                    agent_id: "d:1".into(),
                    status: SubAgentStatus::Completed,
                    summary: "three planes, one store".into(),
                    truncated: false,
                    cost_usd: 0.12,
                    steps: 4,
                    absorbed_messages: 9,
                    reason: None,
                },
            },
        });
        let list = rows::rows(&model);
        assert!(
            matches!(&list[0], Row::Delegate(d) if d.finished.as_ref().is_some_and(|f| f.steps == 4)),
            "the finish row joins the start row: {list:?}"
        );
    }

    /// **The witness for #4369.** A child's head states how long it has run
    /// and how long its parent has been quiet, red past the stall threshold,
    /// and the elapsed freezes at the finish bracket.
    ///
    /// The quiet number is labelled `parent quiet` and not `quiet`: the wire
    /// does not attribute a child's own events to the child (#4347), so
    /// printing a bare `quiet` would claim the deck knows something it does
    /// not.
    #[test]
    fn a_delegate_childs_head_states_its_elapsed_and_its_parents_quiet() {
        let mut model = model_with(&[]);
        model.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status: AgentStatus::Running,
        });
        delegate_started(&mut model, "lead", "d:1", "Survey the three planes.");
        let started = model.now_ms;

        // Ninety seconds in, with the parent quiet the whole time.
        model.now_ms = started + 90_000;
        let mut ui = DeckUi::default();
        open(&mut ui);
        let text = text_of(&model, &ui, 110, 14);
        assert!(
            text.contains("parent quiet "),
            "a child's head says whose quiet time it is printing: {text}"
        );
        assert!(
            text.contains("1:30"),
            "a running child states its elapsed: {text}"
        );
        let child = match rows::rows(&model).into_iter().next() {
            Some(Row::Delegate(d)) => d,
            other => panic!("the first row is not a delegate: {other:?}"),
        };
        assert!(
            rows::delegate_stalled(&model, &child),
            "90s past the {}ms threshold is stalled",
            crate::v2::pulse::STALL_AFTER_MS
        );
        assert_eq!(child.elapsed_ms(model.now_ms), Some(90_000));

        // The finish freezes the clock: another minute passes and the number
        // does not move.
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::SubAgent {
                phase: SubAgentPhase::Finished {
                    agent_id: "d:1".into(),
                    status: SubAgentStatus::Completed,
                    summary: "three planes, one store".into(),
                    truncated: false,
                    cost_usd: 0.12,
                    steps: 4,
                    absorbed_messages: 9,
                    reason: None,
                },
            },
        });
        model.now_ms = started + 150_000;
        let after = text_of(&model, &ui, 110, 14);
        assert!(
            after.contains("1:30"),
            "a finished child's elapsed is frozen at its bracket: {after}"
        );
        assert!(
            !after.contains("parent quiet"),
            "quiet time is a question about something still running: {after}"
        );
    }

    /// **The witness for the arrow-key way in.** `↓` on an empty composer
    /// opens the overlay when sub-agents exist and falls through when none
    /// do; inside it `→` opens the selected lane like `⏎`, `←` closes like
    /// Esc, and `l` goes back to the lead.
    #[test]
    fn down_opens_the_overlay_and_the_arrows_walk_in_and_out() {
        let mut ui = DeckUi {
            tab: DeckTab::Session,
            ..DeckUi::default()
        };
        let empty = model_with(&[]);
        assert_eq!(
            down_opens(key(KeyCode::Down), &empty, &mut ui),
            None,
            "no lanes: `↓` is the transcript's"
        );
        let model = model_with(&[("sub:1", AgentStatus::Running)]);
        assert_eq!(
            down_opens(key(KeyCode::Down), &model, &mut ui),
            Some(DeckAction::Handled)
        );
        assert!(ui.subagents.open);
        handle_key(key(KeyCode::Right), &model, &mut ui);
        assert!(!ui.subagents.open, "`→` opens the lane");
        assert_eq!(ui.focused, 1);

        ui.composer.load("draft".to_string());
        assert_eq!(
            down_opens(key(KeyCode::Down), &model, &mut ui),
            None,
            "with text in the composer `↓` is cursor motion"
        );
        ui.composer.clear();
        ui.session_selected = Some(0);
        assert_eq!(
            down_opens(key(KeyCode::Down), &model, &mut ui),
            None,
            "with a message highlighted `↓` walks the highlight, not the overlay"
        );
        ui.session_selected = None;
        down_opens(key(KeyCode::Down), &model, &mut ui);
        handle_key(key(KeyCode::Char('l')), &model, &mut ui);
        assert!(!ui.subagents.open);
        assert_eq!(ui.focused, 0, "`l` focuses the lead");

        down_opens(key(KeyCode::Down), &model, &mut ui);
        handle_key(key(KeyCode::Left), &model, &mut ui);
        assert!(!ui.subagents.open, "`←` closes without moving focus");
    }

    /// **The witness for `ctrl-x ctrl-x`.** The first press arms and sends
    /// nothing; the second sends the stop for the selected lane. Any other
    /// key between them disarms, and a finished lane never arms.
    #[test]
    fn ctrl_x_twice_kills_the_selected_lane_and_any_other_key_disarms() {
        let model = model_with(&[
            ("sub:1", AgentStatus::Running),
            ("sub:2", AgentStatus::Done),
        ]);
        let mut ui = DeckUi::default();
        open(&mut ui);
        let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);

        assert_eq!(handle_key(ctrl_x, &model, &mut ui), DeckAction::Handled);
        assert!(ui.subagents.kill_armed, "the first press arms");
        assert_eq!(
            handle_key(ctrl_x, &model, &mut ui),
            DeckAction::Send(WorkspaceInput::Control {
                agent: "sub:1".into(),
                control: AgentControl::Stop,
            }),
            "the second press kills"
        );
        assert!(!ui.subagents.kill_armed);

        handle_key(ctrl_x, &model, &mut ui);
        handle_key(key(KeyCode::Up), &model, &mut ui);
        assert!(!ui.subagents.kill_armed, "another key disarms");
        assert_eq!(
            handle_key(ctrl_x, &model, &mut ui),
            DeckAction::Handled,
            "…so the next ctrl-x only arms again"
        );

        ui.subagents.kill_armed = false;
        handle_key(key(KeyCode::Down), &model, &mut ui);
        handle_key(ctrl_x, &model, &mut ui);
        assert!(!ui.subagents.kill_armed, "a finished lane never arms");
    }

    /// The armed state is visible: the footer says the next ctrl-x kills.
    #[test]
    fn the_footer_says_so_while_a_kill_is_armed() {
        let model = model_with(&[("sub:1", AgentStatus::Running)]);
        let mut ui = DeckUi::default();
        open(&mut ui);
        ui.subagents.kill_armed = true;
        let text = text_of(&model, &ui, 100, 12);
        assert!(text.contains("ctrl-x again kills"), "{text}");
    }

    /// A finished lane takes no stop; the key is swallowed rather than sent.
    #[test]
    fn a_finished_lane_cannot_be_stopped() {
        let model = model_with(&[("sub:1", AgentStatus::Done)]);
        let mut ui = DeckUi::default();
        open(&mut ui);
        assert_eq!(
            handle_key(key(KeyCode::Char('s')), &model, &mut ui),
            DeckAction::Handled
        );
        assert_eq!(
            handle_key(key(KeyCode::Char('p')), &model, &mut ui),
            DeckAction::Handled
        );
        assert_eq!(
            handle_key(key(KeyCode::Char('n')), &model, &mut ui),
            DeckAction::Handled,
            "nothing to nudge"
        );
        assert!(
            ui.subagents
                .notice
                .as_deref()
                .is_some_and(|n| n.contains("nothing to nudge")),
            "{:?}",
            ui.subagents.notice
        );
    }

    /// The paint carries the model, the effort, the quiet time, both
    /// sentences, and no brace — the row quotes the fold's one-liner, never
    /// the call's JSON. A long silence on a running lane is red and counted
    /// in the title.
    #[test]
    fn the_overlay_paints_model_effort_quiet_purpose_and_place() {
        let mut model = model_with(&[("sub:2", AgentStatus::Running)]);
        let meta = AgentMeta::new("sub:2", "task #2", 0)
            .with_role("subagent")
            .with_purpose("Simplify docs/spec into plain words.")
            .with_effort("high");
        let mut meta = meta;
        meta.model = Some("moonshotai/kimi-k3".into());
        model.apply_inbound(&Inbound::Register(meta));
        tool_start(&mut model, "sub:2", "read_file", "docs/spec/a.md");
        model.now_ms = model.agents[1].last_activity_ms + 4_000;
        let mut ui = DeckUi::default();
        open(&mut ui);
        let text = text_of(&model, &ui, 110, 14);
        assert!(text.contains("moonshotai/kimi-k3"), "{text}");
        assert!(text.contains("effort high"), "{text}");
        assert!(text.contains("quiet 0:04"), "{text}");
        assert!(
            text.contains("Simplify docs/spec into plain words."),
            "{text}"
        );
        assert!(text.contains("reading docs/spec/a.md"), "{text}");
        assert!(!text.contains('{'), "no JSON reaches the overlay:\n{text}");
        assert!(text.contains("n nudge · f flag"), "{text}");
        assert!(!text.contains("1 quiet"), "nobody is stalled yet: {text}");

        model.now_ms = model.agents[1].last_activity_ms + crate::v2::pulse::STALL_AFTER_MS;
        let area = Rect::new(0, 0, 110, 14);
        let mut buf = Buffer::empty(area);
        render(&model, &ui, area, &mut buf);
        let text = text_of(&model, &ui, 110, 14);
        assert!(
            text.contains("1 quiet"),
            "the title counts the stalled lane: {text}"
        );
        let (y, row) = text
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("quiet 1:30"))
            .expect("quiet time drawn");
        let x = row.find("quiet 1:30").expect("on this row");
        let x = row[..x].chars().count() as u16;
        assert_eq!(
            buf.cell((x, y as u16)).map(|c| c.fg),
            Some(token::RED),
            "{row}"
        );
    }
}
