// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SUB-AGENTS overlay (`↓` from an empty prompt, `ctrl-a`, `/subagents`):
//! every lane the lead has dispatched, with controls. It is the only place
//! the lanes are drawn — the SESSION tab stacks nothing above the transcript
//! for them.
//!
//! ```text
//! ╭ sub-agents · 2 running · 1 paused ─────────────────────────────────────╮
//! │ ▸ ◆ sub:2  running · 0:24 · moonshotai/kimi-k3 · effort high · $0.01   │
//! │     Simplify docs/spec so every page reads in plain words.             │
//! │     reading docs/spec/adaptive-context/adaptive-context.md             │
//! │                                                                        │
//! │   ◆ sub:3  paused · 1:02 · moonshotai/kimi-k3 · effort high · $0.03    │
//! │     Simplify the crate READMEs.                                        │
//! │     paused after edit_file crates/stella-core/README.md                │
//! ╰ ↑↓ select · →↵ open · l lead · ^x^x kill · p pause · r restart · esc ──╯
//! ```
//!
//! Three rows per lane. The head is its vitals — status, clock, model, the
//! effort its calls are pinned to, spend. The second row is **what it is
//! for**: the sentence the lead handed it ([`AgentMeta::purpose`]), its title
//! until the driver supplies one. The third is **where it is**: one sentence
//! derived from the lane's own fold ([`lifecycle`]) — the tool it is inside,
//! the tool it just finished, the report it is writing, or why it stopped.
//! Nothing here is a tool's raw arguments or a JSON result; the fold's
//! humanized one-liner is the most a row quotes.
//!
//! The verbs ride the wire the old AGENTS dashboard used
//! ([`WorkspaceInput::Control`], #4334): `ctrl-x` twice kills (`s` too, for
//! the hand that knows the old key), `p` pauses a running lane and resumes a
//! paused one, `r` restarts from the lane's retained spec, and `→` or `⏎`
//! opens the lane — focuses its transcript on the SESSION tab, where the
//! composer steers it and Esc steers or stops it, as for the lead. `l` is
//! the way back to the lead. The lane list is [`WorkspaceModel::agents`]
//! filtered to sub-agents, in registration order, so the keys and the paint
//! agree on which row is which.
//!
//! Kill takes two presses because it is the one verb here with no undo: a
//! stopped worker's turn future is dropped, and Restart begins again from
//! the spec, not from where it was. The first press arms and the footer says
//! so; any other key disarms — the same shape the queue editor's clear-all
//! and the SKILLS uninstall use.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use stella_tui_theme::token;

use crate::deck::{AgentEntry, DeckTab, WorkspaceModel};
use crate::deck_ui::{DeckAction, DeckUi};
use crate::envelope::{AgentControl, AgentMeta, AgentStatus, WorkspaceInput};
use crate::model::TranscriptEntry;
use crate::theme;
use crate::views::cards;

/// Rows one lane spends, plus one blank between lanes.
const ROWS_PER_LANE: usize = 4;

/// The overlay's own state: whether it is up, the selected row, and whether
/// the first `ctrl-x` of a kill has been pressed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentsOverlay {
    pub open: bool,
    pub sel: usize,
    /// `ctrl-x` was pressed once on the selected lane; the next `ctrl-x`
    /// kills it, any other key disarms.
    pub kill_armed: bool,
}

/// `↓` from an empty composer on the Session tab opens the overlay when
/// there are lanes to list — `↑`'s mirror (the queue editor). `None` lets
/// the key fall through: with no lanes an empty session scrolls as it always
/// did, and with text in the composer `↓` is cursor motion. Gated on *full*
/// composer emptiness, chips included, like `↑`.
pub fn down_opens(key: KeyEvent, model: &WorkspaceModel, ui: &mut DeckUi) -> Option<DeckAction> {
    if ui.tab == DeckTab::Session
        && ui.composer.is_empty()
        && model.subagent_count() > 0
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

/// What the lane is for: the purpose the driver supplied, else its title.
pub fn purpose(meta: &AgentMeta) -> &str {
    meta.purpose
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(&meta.title)
}

/// The present participle and past tense of a built-in tool's verb. Unknown
/// tools — MCP, custom — are called by name.
fn verb(tool: &str) -> (String, String) {
    let (doing, done) = match tool {
        "read_file" => ("reading", "read"),
        "write_file" => ("writing", "wrote"),
        "edit_file" => ("editing", "edited"),
        "delete_file" => ("deleting", "deleted"),
        "search" => ("searching", "searched"),
        "bash" => ("running", "ran"),
        "delegate" => ("delegating", "delegated"),
        "ask_question" => ("asking", "asked"),
        "get_environment" => ("probing the environment", "probed the environment"),
        _ => return (format!("calling {tool}"), format!("called {tool}")),
    };
    (doing.to_string(), done.to_string())
}

/// Where the lane is in its task, as one sentence.
///
/// A supervisor state (queued, paused, killed) is stated as such — with the
/// last tool beside it where there is one, so a paused lane says what it was
/// doing when it stopped. A live lane is read off the tail of its fold: an
/// open tool call is `reading <path>`, a closed one `read <path>`, prose is
/// the report being written, and an empty fold is a lane that has not made
/// its first model call yet.
pub fn lifecycle(lane: &AgentEntry) -> String {
    let tail = last_tool(lane);
    match lane.status {
        AgentStatus::Queued => "waiting for a worker slot".to_string(),
        AgentStatus::Paused => match tail {
            Some(t) => format!("paused after {} {}", verb(&t.name).1, t.input),
            None => "paused before its first tool call".to_string(),
        },
        AgentStatus::Killed => match tail {
            Some(t) => format!("stopped after {} {}", verb(&t.name).1, t.input),
            None => "stopped before its first tool call".to_string(),
        },
        AgentStatus::WaitingInput => "waiting on an answer from you".to_string(),
        AgentStatus::Failed => lane
            .model
            .transcript
            .iter()
            .rev()
            .find_map(|e| match e {
                TranscriptEntry::Error { message, .. } => Some(format!("failed: {message}")),
                _ => None,
            })
            .unwrap_or_else(|| "failed".to_string()),
        AgentStatus::Done => lane
            .model
            .transcript
            .iter()
            .rev()
            .find_map(|e| match e {
                TranscriptEntry::Complete { cost_usd, .. } => {
                    Some(format!("finished · report delivered · ${cost_usd:.2}"))
                }
                _ => None,
            })
            .unwrap_or_else(|| "finished · report delivered".to_string()),
        AgentStatus::Running => running(lane),
    }
}

/// The most recent tool call on the lane, with whether it has returned.
struct LastTool {
    name: String,
    input: String,
}

fn last_tool(lane: &AgentEntry) -> Option<LastTool> {
    lane.model.transcript.iter().rev().find_map(|e| match e {
        TranscriptEntry::ToolStart { name, input, .. } => Some(LastTool {
            name: name.clone(),
            input: input.clone(),
        }),
        _ => None,
    })
}

/// A running lane's sentence, from the newest entry that says anything
/// about progress.
fn running(lane: &AgentEntry) -> String {
    for entry in lane.model.transcript.iter().rev() {
        match entry {
            TranscriptEntry::ToolResult {
                name, ok, summary, ..
            } => {
                // The result names the call; the start row before it holds
                // the input. Find it so the sentence says what was read.
                let input = input_of(lane, name);
                let (_, done) = verb(name);
                return if *ok {
                    format!("{done} {input}")
                } else {
                    let why = cards::truncate_cols(summary, 40);
                    format!("{done} {input} — failed: {why}")
                };
            }
            TranscriptEntry::ToolStart { name, input, .. } => {
                // An open call is the newest entry only when no result has
                // followed it; the `ToolResult` arm above wins otherwise.
                let (doing, _) = verb(name);
                return format!("{doing} {input}");
            }
            TranscriptEntry::Text(_) => return "writing its report".to_string(),
            TranscriptEntry::Reasoning(_) => return "thinking".to_string(),
            TranscriptEntry::Retry { attempt, .. } => {
                return format!("retrying the model call · attempt {attempt}");
            }
            TranscriptEntry::Parked { description, .. } => {
                return format!("parked: {description}");
            }
            TranscriptEntry::Compaction { .. } => {
                return "compacting its context".to_string();
            }
            TranscriptEntry::User(_) => {
                return "starting: reading its briefing".to_string();
            }
            _ => {}
        }
    }
    "starting: no model call yet".to_string()
}

/// The input of the newest `ToolStart` named `name` on the lane.
fn input_of(lane: &AgentEntry, name: &str) -> String {
    lane.model
        .transcript
        .iter()
        .rev()
        .find_map(|e| match e {
            TranscriptEntry::ToolStart { name: n, input, .. } if n == name => Some(input.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Open the overlay on the first lane.
pub fn open(ui: &mut DeckUi) -> DeckAction {
    ui.subagents.open = true;
    ui.subagents.sel = 0;
    ui.subagents.kill_armed = false;
    DeckAction::Handled
}

/// The overlay's keys: ↑/↓ select, `→`/`⏎` open the lane (focus its
/// transcript), `l` back to the lead, `ctrl-x` twice (or `s`) stop, `p`
/// pause a running lane / resume a paused one, `r` restart, Esc/`←`/`q`
/// close. Modal: every other key is swallowed.
pub fn handle_key(key: KeyEvent, model: &WorkspaceModel, ui: &mut DeckUi) -> DeckAction {
    let rows = lanes(model);
    let count = rows.len();
    ui.subagents.sel = ui.subagents.sel.min(count.saturating_sub(1));
    let selected = rows.get(ui.subagents.sel).copied();
    let control = |lane: &AgentEntry, control: AgentControl| {
        DeckAction::Send(WorkspaceInput::Control {
            agent: lane.meta.id.clone(),
            control,
        })
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // The kill arm survives exactly one key: the second `ctrl-x`.
    let armed = std::mem::take(&mut ui.subagents.kill_armed);
    match key.code {
        KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => {
            ui.subagents.open = false;
            DeckAction::Handled
        }
        KeyCode::Up => {
            ui.subagents.sel = ui.subagents.sel.saturating_sub(1);
            DeckAction::Handled
        }
        KeyCode::Down => {
            if count > 0 {
                ui.subagents.sel = (ui.subagents.sel + 1).min(count - 1);
            }
            DeckAction::Handled
        }
        KeyCode::Enter | KeyCode::Right => match selected {
            Some((idx, _)) => {
                ui.subagents.open = false;
                ui.focus_agent(idx);
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
        KeyCode::Char('x') if ctrl => match selected {
            Some((_, lane)) if lane.status.is_terminal() => DeckAction::Handled,
            Some((_, lane)) if armed => control(lane, AgentControl::Stop),
            Some(_) => {
                ui.subagents.kill_armed = true;
                DeckAction::Handled
            }
            None => DeckAction::Handled,
        },
        KeyCode::Char('s') => match selected {
            Some((_, lane)) if !lane.status.is_terminal() => control(lane, AgentControl::Stop),
            _ => DeckAction::Handled,
        },
        KeyCode::Char('p') => match selected {
            Some((_, lane)) if lane.status == AgentStatus::Paused => {
                control(lane, AgentControl::Resume)
            }
            Some((_, lane)) if lane.status.is_active() => control(lane, AgentControl::Pause),
            _ => DeckAction::Handled,
        },
        KeyCode::Char('r') => match selected {
            Some((_, lane)) => control(lane, AgentControl::Restart),
            None => DeckAction::Handled,
        },
        _ => DeckAction::Handled,
    }
}

/// The head row's vitals after the status: clock, model, effort, spend.
fn vitals(lane: &AgentEntry, now_ms: u64) -> String {
    let mut parts = vec![cards::fmt_mss(lane.elapsed_ms(now_ms))];
    if let Some(model) = &lane.meta.model {
        parts.push(model.clone());
    }
    if let Some(effort) = &lane.meta.effort {
        parts.push(format!("effort {effort}"));
    }
    parts.push(format!("${:.2}", lane.cost_usd));
    parts.join(" · ")
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

    let rows = lanes(model);
    let selected = ui.subagents.sel.min(rows.len().saturating_sub(1));
    let inner_w = (w as usize).saturating_sub(4);
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let mark = Style::new().fg(theme::SUBAGENT);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if rows.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  no sub-agents dispatched in this session yet".to_string(),
            muted,
        )));
    }

    let visible = ((h as usize).saturating_sub(2) / ROWS_PER_LANE).max(1);
    let start = selected
        .saturating_sub(visible.saturating_sub(1) / 2)
        .min(rows.len().saturating_sub(visible));
    for (i, (_, lane)) in rows.iter().enumerate().skip(start).take(visible) {
        let is_sel = i == selected;
        let mut id_style = mark;
        if is_sel {
            id_style = id_style.bg(token::HL).add_modifier(Modifier::BOLD);
        }
        let status_style = Style::new().fg(theme::status_color(lane.status));
        let head = vec![
            Span::styled(
                if is_sel { "▸ " } else { "  " }.to_string(),
                Style::new().fg(token::GOLD),
            ),
            Span::styled("◆ ", mark),
            Span::styled(lane.meta.id.clone(), id_style),
            Span::styled("  ", dim),
            Span::styled(lane.status.label().to_string(), status_style),
            Span::styled(" · ", dim),
            Span::styled(
                cards::truncate_cols(&vitals(lane, model.now_ms), inner_w.saturating_sub(24)),
                muted,
            ),
        ];
        lines.push(Line::from(head));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(
                cards::truncate_cols(purpose(&lane.meta), inner_w.saturating_sub(5)),
                text,
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw("     "),
            Span::styled(
                cards::truncate_cols(&lifecycle(lane), inner_w.saturating_sub(5)),
                muted,
            ),
        ]));
        lines.push(Line::default());
    }

    let running = rows
        .iter()
        .filter(|(_, a)| a.status == AgentStatus::Running)
        .count();
    let paused = rows
        .iter()
        .filter(|(_, a)| a.status == AgentStatus::Paused)
        .count();
    let done = rows.iter().filter(|(_, a)| a.status.is_terminal()).count();
    let mut title = vec![Span::styled(" sub-agents", text)];
    for (n, word) in [(running, "running"), (paused, "paused"), (done, "finished")] {
        if n > 0 {
            title.push(Span::styled(format!(" · {n} {word}"), muted));
        }
    }
    title.push(Span::raw(" "));
    let footer = if ui.subagents.kill_armed {
        Span::styled(
            " ctrl-x again kills the selected lane · any other key keeps it ",
            Style::new().fg(token::RED).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " ↑↓ select · →↵ open · l lead · ^x^x kill · p pause/resume · r restart · esc ",
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
    use crate::envelope::Inbound;
    use crossterm::event::KeyModifiers;
    use stella_protocol::{AgentEvent, ToolCall, ToolOutput};

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
                AgentMeta::new(*id, format!("task {id}"), 0).with_role("subagent"),
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

    fn tool_result(model: &mut WorkspaceModel, agent: &str, name: &str, path: &str) {
        model.apply_inbound(&Inbound::Event {
            agent: agent.to_string(),
            event: AgentEvent::ToolResult {
                call_id: format!("c-{name}-{path}"),
                output: ToolOutput::ok("ok"),
                duration_ms: 3,
                speculated: false,
            },
        });
    }

    fn lane<'a>(model: &'a WorkspaceModel, id: &str) -> &'a AgentEntry {
        model
            .agents
            .iter()
            .find(|a| a.meta.id == id)
            .expect("lane registered")
    }

    /// **The witness.** The second sentence is where the lane is, read off
    /// its fold: inside a call it is `reading <path>`; once the call returns
    /// it is `read <path>`; once it narrates, it is writing its report. None
    /// of those is the call's arguments or its result.
    #[test]
    fn the_lifecycle_sentence_follows_the_lanes_fold() {
        let mut model = model_with(&[("sub:2", AgentStatus::Running)]);
        assert_eq!(
            lifecycle(lane(&model, "sub:2")),
            "starting: no model call yet"
        );

        tool_start(&mut model, "sub:2", "read_file", "docs/spec/a.md");
        assert_eq!(
            lifecycle(lane(&model, "sub:2")),
            "reading docs/spec/a.md",
            "an open call is the participle"
        );

        tool_result(&mut model, "sub:2", "read_file", "docs/spec/a.md");
        // A result re-folds the lane to Running; the sentence is the past tense.
        assert_eq!(lifecycle(lane(&model, "sub:2")), "read docs/spec/a.md");

        model.apply_inbound(&Inbound::Event {
            agent: "sub:2".into(),
            event: AgentEvent::Text {
                text: "Done. I simplified".into(),
            },
        });
        assert_eq!(lifecycle(lane(&model, "sub:2")), "writing its report");
    }

    /// A supervisor state names itself and what the lane was doing.
    #[test]
    fn paused_and_stopped_lanes_say_what_they_were_doing() {
        let mut model = model_with(&[("sub:3", AgentStatus::Running)]);
        tool_start(&mut model, "sub:3", "edit_file", "README.md");
        model.apply_inbound(&Inbound::Status {
            agent: "sub:3".into(),
            status: AgentStatus::Paused,
        });
        assert_eq!(
            lifecycle(lane(&model, "sub:3")),
            "paused after edited README.md"
        );
        model.apply_inbound(&Inbound::Status {
            agent: "sub:3".into(),
            status: AgentStatus::Killed,
        });
        assert_eq!(
            lifecycle(lane(&model, "sub:3")),
            "stopped after edited README.md"
        );
        // A killed lane stays killed — the fold refuses to revive a terminal
        // lane — so the queued sentence is pinned on a lane that never ran.
        let queued = model_with(&[("sub:4", AgentStatus::Queued)]);
        assert_eq!(
            lifecycle(lane(&queued, "sub:4")),
            "waiting for a worker slot"
        );
    }

    /// The purpose is the driver's sentence, the title until there is one.
    #[test]
    fn purpose_prefers_the_drivers_sentence_over_the_title() {
        let meta = AgentMeta::new("sub:1", "task #1: Simplify", 0);
        assert_eq!(purpose(&meta), "task #1: Simplify");
        let meta = meta.with_purpose("Simplify the crate READMEs into plain words.");
        assert_eq!(
            purpose(&meta),
            "Simplify the crate READMEs into plain words."
        );
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
        handle_key(key(KeyCode::Down), &model, &mut ui);
        assert_eq!(
            sent(handle_key(key(KeyCode::Char('p')), &model, &mut ui)),
            ("sub:2".to_string(), AgentControl::Resume),
            "p on a paused lane resumes it"
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

    /// **The witness for the arrow-key way in.** `↓` on an empty composer
    /// opens the overlay when lanes exist and falls through when none do;
    /// inside it `→` opens the selected lane like `⏎`, `←` closes like Esc,
    /// and `l` goes back to the lead.
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
        let area = Rect::new(0, 0, 100, 12);
        let mut buf = Buffer::empty(area);
        render(&model, &ui, area, &mut buf);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
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
    }

    /// The paint carries the model, the effort, both sentences, and no
    /// brace — the row quotes the fold's one-liner, never the call's JSON.
    #[test]
    fn the_overlay_paints_model_effort_purpose_and_place() {
        let mut model = model_with(&[("sub:2", AgentStatus::Running)]);
        let meta = AgentMeta::new("sub:2", "task #2", 0)
            .with_role("subagent")
            .with_purpose("Simplify docs/spec into plain words.")
            .with_effort("high");
        let mut meta = meta;
        meta.model = Some("moonshotai/kimi-k3".into());
        model.apply_inbound(&Inbound::Register(meta));
        tool_start(&mut model, "sub:2", "read_file", "docs/spec/a.md");
        let mut ui = DeckUi::default();
        open(&mut ui);
        let area = Rect::new(0, 0, 100, 14);
        let mut buf = Buffer::empty(area);
        render(&model, &ui, area, &mut buf);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("moonshotai/kimi-k3"), "{text}");
        assert!(text.contains("effort high"), "{text}");
        assert!(
            text.contains("Simplify docs/spec into plain words."),
            "{text}"
        );
        assert!(text.contains("reading docs/spec/a.md"), "{text}");
        assert!(!text.contains('{'), "no JSON reaches the overlay:\n{text}");
        assert!(
            text.contains("^x^x kill · p pause/resume · r restart"),
            "{text}"
        );
    }
}
