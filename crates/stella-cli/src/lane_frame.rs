// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a lane leaves behind when it dies mid-turn.
//!
//! [`stella_protocol::ResumeAuthority`] says which lanes owe this. A lane
//! under `Own` re-enters its own turn, so its record is a resume point and it
//! owes nothing. A lane under `Parent` or `Redispatch` is never re-entered:
//! the lead reads what it left in order to report it, and the fleet reads what
//! it left in order to decide the re-run. Both of those owe one record — the
//! **terminal frame** — holding the steps the lane got through and the
//! messages that got it there, written once when the lane dies.
//!
//! # Why the lane's sink keeps a copy
//!
//! [`stella_core::Engine::drive`] drops the resume point at every end of a
//! turn, an abort too, and it does not say which end it is on. So the engine
//! deleted a failed lane's messages before a reader existed. The file edits
//! stayed in the shared tree. The talk that made them did not.
//!
//! [`LaneSink`] keeps what that drop throws away. [`LaneRecorder::settle`] is
//! where a lane that finished and a lane that died part ways. The first clears
//! its frame. The second writes one.
//!
//! The copy is taken at the drop, not at every `persist`. That is one
//! `git show` per turn, in place of a copy of the whole talk per step.
//!
//! # Two lanes, one recorder
//!
//! The deck's sub-session (`crate::subsession`, a `Parent` lane) and a fleet
//! attempt (`crate::fleet_cmd::durability`, the `Redispatch` one) both bind a
//! record of their own and both hit the same drop. Their readers differ —
//! `subsession::terminal_frame::parent_report` renders a line for the lead,
//! and the fleet's `initial_messages` re-enters the transcript on the next
//! dispatch — so the reader stays with each lane and the writer is shared.
//!
//! # What this is not
//!
//! Not a resume path. Nothing here re-opens a lane's turn from a running deck.

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use stella_core::EngineConfig;
use stella_core::step::CheckpointSink;
use stella_protocol::CompletionMessage;

use crate::durability::SessionDurability;

/// How a lane's turn ended, as much as the frame needs to know.
///
/// A lane that finished writes no frame, so [`Self::Done`] carries nothing —
/// it is the arm that retires a frame rather than writing one. Each lane maps
/// its own richer outcome onto this: `subsession::WorkerEnd` for a deck lane,
/// the fleet attempt's `success`/stopped pair for a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaneEnd {
    /// The turn reached an answer.
    Done,
    /// The turn gave up: a provider error, the step cap, a budget stop.
    Failed(String),
    /// A person stopped the lane.
    Stopped,
}

/// How a lane died. A lane that finished writes no frame, so there is no third
/// arm here — these are the two ends a reader has something to say about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Ending {
    /// The turn gave up: a provider error, the step cap, a budget stop.
    Failed,
    /// The user stopped the lane (Agents tab `s`, or Esc on its lane).
    Stopped,
}

impl Ending {
    /// The word a report uses.
    fn word(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Stopped => "was stopped",
        }
    }
}

/// The record a dead lane leaves for whoever reads it next.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TerminalFrame {
    /// Which lane this was (`req:1`, `sub:42`, a fleet attempt's claim
    /// holder).
    pub(crate) lane: String,
    /// How many steps the lane got through before it died. It comes from the
    /// checkpoint's own `step`, the index of the step that would have run
    /// next, which is the count of the steps that did run.
    pub(crate) committed_steps: usize,
    pub(crate) ending: Ending,
    /// Why the turn gave up, in the engine's words. Empty for a lane the user
    /// stopped.
    pub(crate) reason: String,
    /// The talk as of that step, copied from the checkpoint written there. A
    /// killed lane's file edits stand in the shared tree. This is the talk that
    /// made them.
    pub(crate) messages: Vec<CompletionMessage>,
}

impl TerminalFrame {
    /// The frame this record's last dead attempt left, or `None` — for a lane
    /// that finished, one that died before its first step, and a record that
    /// will not open. All three mean there is nothing to read.
    pub(crate) fn read(durability: &SessionDurability) -> Option<Self> {
        serde_json::from_str(&durability.terminal_frame()?).ok()
    }
}

/// One line for a reader's own transcript: which lane ended, how, and how far
/// it got.
///
/// The abort reason stays off this line. The lane prints it on itself, and the
/// user has both in view.
pub(crate) fn report_line(frame: &TerminalFrame) -> String {
    let steps = frame.committed_steps;
    let plural = if steps == 1 { "step" } else { "steps" };
    format!(
        "note: lane {} {} after {steps} committed {plural} — its transcript up to \
         that point is saved.",
        frame.lane,
        frame.ending.word(),
    )
}

/// The lane's end of the frame. It holds the lane's own durability handle,
/// hands the engine a sink that keeps what the engine drops, and settles the
/// frame once, at the lane's end.
pub(crate) struct LaneRecorder {
    lane: String,
    durability: SessionDurability,
    retired: Arc<RwLock<Option<String>>>,
}

impl LaneRecorder {
    /// A recorder over `durability`, which must be the lane's OWN handle.
    /// Never a lead session's: that record is a live session's resume point,
    /// not a lane's report.
    pub(crate) fn new(durability: &SessionDurability, lane: &str) -> Self {
        Self {
            lane: lane.to_string(),
            durability: durability.clone(),
            retired: Arc::new(RwLock::new(None)),
        }
    }

    /// Point `config`'s checkpoint sink at this recorder, so the talk the
    /// engine drops at the end of the turn is kept. The sink underneath is
    /// still the lane's own. This wraps it; where a checkpoint goes is
    /// unchanged.
    pub(crate) fn wrap(&self, config: EngineConfig) -> EngineConfig {
        EngineConfig {
            checkpoint_sink: self.sink(),
            ..config
        }
    }

    pub(crate) fn sink(&self) -> Option<Arc<dyn CheckpointSink>> {
        let inner = self.durability.sink()?;
        Some(Arc::new(LaneSink {
            inner,
            durability: self.durability.clone(),
            retired: self.retired.clone(),
        }))
    }

    /// Settle this lane's frame, once, at its end.
    ///
    /// A lane that finished drops any frame an earlier try on the same lane
    /// left, so nothing reports a death the lane has since come back from. A
    /// lane that died writes a frame, as long as it got through a step. A turn
    /// that fell over before its first step has no talk worth keeping, and it
    /// says so by leaving no frame.
    pub(crate) fn settle(&self, end: &LaneEnd) {
        let (ending, reason) = match end {
            LaneEnd::Done => {
                self.durability.clear_terminal_frame();
                return;
            }
            LaneEnd::Failed(reason) => (Ending::Failed, reason.clone()),
            LaneEnd::Stopped => (Ending::Stopped, String::new()),
        };
        let Some(json) = self.last_committed() else {
            return;
        };
        let Ok(checkpoint) = stella_core::step::Checkpoint::from_json(&json) else {
            return;
        };
        let frame = TerminalFrame {
            lane: self.lane.clone(),
            committed_steps: checkpoint.step,
            ending,
            reason,
            messages: checkpoint.messages,
        };
        if let Ok(json) = serde_json::to_string(&frame) {
            self.durability.record_terminal_frame(&json);
        }
    }

    /// The last checkpoint this lane's turn wrote. That is the one the engine
    /// dropped on its way out. A stopped turn reaches no end at all, because
    /// its future is dropped mid-step, so there the answer is the checkpoint
    /// still standing at the lane's tip.
    fn last_committed(&self) -> Option<String> {
        self.retired
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .or_else(|| self.durability.checkpoint())
    }
}

/// The lane's [`CheckpointSink`], wrapped so the drop at the end of a turn
/// does not take the talk with it.
#[derive(Debug)]
struct LaneSink {
    inner: Arc<dyn CheckpointSink>,
    durability: SessionDurability,
    retired: Arc<RwLock<Option<String>>>,
}

impl CheckpointSink for LaneSink {
    fn persist(&self, json: &str) {
        self.inner.persist(json);
    }

    /// Keep, then discard. This runs at every end of a turn the engine has, a
    /// finished one too, so it decides nothing. [`LaneRecorder::settle`] knows
    /// how the lane ended; this does not.
    fn discard(&self) {
        if let Some(retired) = self.durability.checkpoint() {
            *self.retired.write().unwrap_or_else(|p| p.into_inner()) = Some(retired);
        }
        self.inner.discard();
    }
}
