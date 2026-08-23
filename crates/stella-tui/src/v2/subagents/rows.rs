// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The rows the SUB-AGENTS overlay lists, and the two messages it can send
//! on a row's behalf.
//!
//! Stella has two sub-agent mechanisms and the overlay lists both:
//!
//! * a **lane** — `sub:<task>` / `req:<n>`, an OS-thread session the driver
//!   spawned (`crates/stella-cli/src/subsession.rs`), registered on the deck
//!   with a role of `subagent` and answering `WorkspaceInput::Control`;
//! * a **`delegate` child** — a bounded turn *inside* its parent's turn
//!   (`stella_core::subagent`), known to the deck only as the
//!   `TranscriptEntry::SubAgent` start/finish bracket on the parent's
//!   transcript. It has no lane, no per-event attribution, and no control
//!   plane (#4347).
//!
//! A child is listed under the agent whose transcript holds its bracket,
//! with the verbs it cannot take drawn disabled and swallowed rather than
//! sent — a row that silently did nothing was the failure #4347 names. What
//! *can* be done about a child is done to its parent: `⏎` opens the parent's
//! transcript, where the child's calls are, and `f` flags it to the parent.
//!
//! ## Nudge and flag
//!
//! Two asks a person has at this overlay and had no key for:
//!
//! * **`n` nudge** — "are you alive?" to the lane itself: a steer at its next
//!   step boundary asking for a one-line position report, and to continue.
//!   The lane answers on its own transcript; the pulse row and this overlay's
//!   lifecycle sentence move when it does.
//! * **`f` flag** — "this one might be dead" to whoever dispatched it: the
//!   lane's vitals as the deck sees them — status, quiet time, what it was
//!   last doing — sent to the parent as a steer if the parent is running and
//!   as its next prompt otherwise, asking it to check, stop, or take the task
//!   over. The operator decides nothing here; the dispatcher is the one with
//!   the context to decide, and this hands it the facts.

use crate::deck::{AgentEntry, WorkspaceModel};
use crate::envelope::{AgentStatus, WorkspaceInput};
use crate::model::{SubAgentSummary, TranscriptEntry};
use crate::v2::pulse::STALL_AFTER_MS;
use crate::views::cards;

/// One listed row.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A worker lane: an index into `model.agents`.
    Lane(usize),
    /// A `delegate` child, read off its parent's transcript.
    Delegate(Delegate),
}

/// A `delegate` child as the overlay knows it — the start row's preview and
/// the finish row's summary, joined on `agent_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Delegate {
    /// Index into `model.agents` of the agent whose turn it ran inside.
    pub parent: usize,
    pub agent_id: String,
    pub instruction_preview: String,
    pub write_access: bool,
    /// `None` while the child is still inside its parent's turn.
    pub finished: Option<SubAgentSummary>,
}

impl Row {
    /// The agent `⏎` opens and `f` flags to: the lane itself, or a child's
    /// parent.
    pub fn opens(&self) -> usize {
        match self {
            Row::Lane(i) => *i,
            Row::Delegate(d) => d.parent,
        }
    }

    /// Whether the row answers `WorkspaceInput::Control` at all.
    pub fn controllable(&self) -> bool {
        matches!(self, Row::Lane(_))
    }
}

/// Every row, in registration order: each lane, followed by the children
/// its transcript holds. The lead is not a row, but its children are listed
/// first under it, because a `delegate` from the lead is the common case.
pub fn rows(model: &WorkspaceModel) -> Vec<Row> {
    let mut out = Vec::new();
    for (idx, agent) in model.agents.iter().enumerate() {
        if agent.is_subagent() {
            out.push(Row::Lane(idx));
        }
        out.extend(delegates_of(idx, agent).into_iter().map(Row::Delegate));
    }
    out
}

/// The `delegate` children bracketed on `agent`'s transcript, oldest first,
/// each start row joined with its finish row by `agent_id`.
pub fn delegates_of(parent: usize, agent: &AgentEntry) -> Vec<Delegate> {
    let mut out: Vec<Delegate> = Vec::new();
    for entry in &agent.model.transcript {
        let TranscriptEntry::SubAgent {
            agent_id,
            finished,
            instruction_preview,
            write_access,
        } = entry
        else {
            continue;
        };
        match finished {
            None => out.push(Delegate {
                parent,
                agent_id: agent_id.clone(),
                instruction_preview: instruction_preview.clone(),
                write_access: *write_access,
                finished: None,
            }),
            Some(summary) => {
                if let Some(d) = out
                    .iter_mut()
                    .rev()
                    .find(|d| d.agent_id == *agent_id && d.finished.is_none())
                {
                    d.finished = Some(summary.clone());
                }
            }
        }
    }
    out
}

/// Milliseconds since the lane's last event.
pub fn quiet_ms(model: &WorkspaceModel, lane: &AgentEntry) -> u64 {
    model.now_ms.saturating_sub(lane.last_activity_ms)
}

/// Whether a running lane has been quiet long enough to call it stalled.
pub fn stalled(model: &WorkspaceModel, lane: &AgentEntry) -> bool {
    lane.status == AgentStatus::Running && quiet_ms(model, lane) >= STALL_AFTER_MS
}

/// The `n` message: a steer at the lane asking where it is.
pub fn nudge(lane: &AgentEntry) -> WorkspaceInput {
    WorkspaceInput::Steer {
        agent: lane.meta.id.clone(),
        texts: vec![
            "The operator is checking on you. In one line, say where you are and what is \
             left, then continue."
                .to_string(),
        ],
    }
}

/// The `f` message: the row's vitals, to its dispatcher. `None` when the
/// row has no dispatcher on the deck.
pub fn flag(model: &WorkspaceModel, row: &Row) -> Option<WorkspaceInput> {
    let (parent, text) = match row {
        Row::Lane(idx) => {
            let lane = model.agents.get(*idx)?;
            let parent = model.parent_of(*idx)?;
            let quiet = cards::fmt_mss(quiet_ms(model, lane));
            let text = format!(
                "[flag from the deck] Lane {} (\"{}\") looks stalled: {}, quiet for {}, last: {}. \
                 Check on it, stop it, or take the task over — say which.",
                lane.meta.id,
                super::lifecycle::purpose(&lane.meta),
                lane.status.label(),
                quiet,
                super::lifecycle::lifecycle(lane),
            );
            (parent, text)
        }
        Row::Delegate(d) => {
            let text = format!(
                "[flag from the deck] Your delegate {} (\"{}\") looks stalled: it is still \
                 inside your turn with no report. Check on it or finish without it — say which.",
                d.agent_id,
                cards::truncate_cols(&d.instruction_preview, 80),
            );
            (d.parent, text)
        }
    };
    let dispatcher = model.agents.get(parent)?;
    Some(if dispatcher.status == AgentStatus::Running {
        WorkspaceInput::Steer {
            agent: dispatcher.meta.id.clone(),
            texts: vec![text],
        }
    } else {
        WorkspaceInput::Enqueue { text }
    })
}

/// A child's lifecycle sentence — honest about what the deck knows, which
/// is the bracket and nothing between.
pub fn delegate_place(model: &WorkspaceModel, d: &Delegate) -> String {
    let parent = model
        .agents
        .get(d.parent)
        .map(|a| a.meta.id.as_str())
        .unwrap_or("its parent");
    match &d.finished {
        None => format!("inside {parent}'s turn · no control plane: stop {parent} to stop it"),
        Some(s) => {
            let status = match s.status {
                stella_protocol::SubAgentStatus::Completed => "finished",
                stella_protocol::SubAgentStatus::Incomplete => "stopped early",
                stella_protocol::SubAgentStatus::Refused => "refused",
            };
            let mut place = format!("{status} · {} steps · ${:.2}", s.steps, s.cost_usd);
            if let Some(reason) = &s.reason {
                place.push_str(" · ");
                place.push_str(&cards::truncate_cols(reason, 60));
            }
            place
        }
    }
}

/// A child's status word, for the head row.
pub fn delegate_status(d: &Delegate) -> AgentStatus {
    match &d.finished {
        None => AgentStatus::Running,
        Some(s) => match s.status {
            stella_protocol::SubAgentStatus::Completed => AgentStatus::Done,
            stella_protocol::SubAgentStatus::Incomplete
            | stella_protocol::SubAgentStatus::Refused => AgentStatus::Failed,
        },
    }
}
