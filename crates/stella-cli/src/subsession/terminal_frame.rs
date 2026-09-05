//! What a lane hands its parent when it dies mid-turn.
//!
//! A sub-session lane runs beside the lead chat. It does not resume itself.
//! `doc:turn-lane-assembly` §6 calls that `ResumeAuthority::Parent`. The lead
//! reads the lane's history to report it, and no one re-enters it. So the lane
//! owes one record: the **terminal frame**. It holds the steps the lane got
//! through and the messages that got it there. It is written once, when the
//! lane dies.
//!
//! # Why the lane's sink keeps a copy
//!
//! [`stella_core::Engine::drive`] drops the resume point at every end of a
//! turn, an abort too. It does not say which end it is on. So the engine
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
//! # What this is not
//!
//! Not a resume path. Nothing here re-opens a lane's turn from a running deck.
//! A lane holds no session record, so nothing can turn a lane key back into a
//! turn. The frame's reader is the lead's report.

use std::path::Path;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use stella_core::EngineConfig;
use stella_core::step::CheckpointSink;
use stella_protocol::CompletionMessage;

use super::{WorkerEnd, lane_journal_key};
use crate::durability::SessionDurability;

/// How a lane died. A lane that finished writes no frame, so there is no third
/// arm here — these are the two ends a parent has something to say about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Ending {
    /// The turn gave up: a provider error, the step cap, a budget stop.
    Failed,
    /// The user stopped the lane (Agents tab `s`, or Esc on its lane).
    Stopped,
}

impl Ending {
    /// The word the parent's report uses.
    fn word(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Stopped => "was stopped",
        }
    }
}

/// The record a dead lane leaves for its parent to report from.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TerminalFrame {
    /// Which lane this was (`req:1`, `sub:42`).
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

/// One line for the lead's own transcript: which lane ended, how, and how far
/// it got.
///
/// The abort reason stays off this line. `subsession::spawn` prints it on the
/// lane itself, and the user has both in view.
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

/// The report for a lane that just ended. `None` when it left no frame, which
/// is what a lane that finished leaves, and so does a lane that died before
/// its first step.
///
/// The parent reads the lane's own record. It is not told over the supervisor
/// channel, because the frame is the part that lives on disk, and living on
/// disk is what the report is about.
pub(crate) fn parent_report(workspace_root: &Path, session_id: &str, lane: &str) -> Option<String> {
    parent_report_in(
        &stella_store::usage::data_dir().join("work"),
        workspace_root,
        session_id,
        lane,
    )
}

/// [`parent_report`] against a named store root. `durability::bind_session_in`
/// says why that seam exists: a test binds its own store, not the shared one.
pub(crate) fn parent_report_in(
    store_root: &Path,
    workspace_root: &Path,
    session_id: &str,
    lane: &str,
) -> Option<String> {
    let durability = SessionDurability::default();
    let key = lane_journal_key(session_id, lane);
    // `bind_session_in` answers `Some(warning)` when the record will not open,
    // and `None` when it bound. A record that will not open has nothing to
    // report from, the same answer as a lane that left no frame.
    if crate::durability::bind_session_in(&durability, store_root, workspace_root, &key).is_some() {
        return None;
    }
    let frame = serde_json::from_str::<TerminalFrame>(&durability.terminal_frame()?).ok()?;
    Some(report_line(&frame))
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
    /// Never the lead's `cfg.durability`: that record is a live session's
    /// resume point, not a lane's report.
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

    fn sink(&self) -> Option<Arc<dyn CheckpointSink>> {
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
    /// left. The parent must not report a death the lane has come back from. A
    /// lane that died writes a frame, as long as it got through a step. A turn
    /// that fell over before its first step has no talk worth keeping, and it
    /// says so by leaving no frame.
    pub(crate) fn settle(&self, end: &WorkerEnd) {
        let (ending, reason) = match end {
            WorkerEnd::Done(_) => {
                self.durability.clear_terminal_frame();
                return;
            }
            WorkerEnd::Failed(reason) => (Ending::Failed, reason.clone()),
            WorkerEnd::Stopped => (Ending::Stopped, String::new()),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A checkpoint shaped like one a lane wrote at a step boundary: four
    /// steps got through, and the talk that got there.
    fn checkpoint_at_step_4() -> String {
        stella_core::step::Checkpoint {
            version: stella_core::step::CHECKPOINT_VERSION,
            step: 4,
            messages: vec![
                CompletionMessage::system("system prompt"),
                CompletionMessage::user("do the task"),
                CompletionMessage::assistant("on it"),
            ],
            budget: stella_core::step::BudgetSnapshot {
                mode: stella_protocol::BudgetMode::Observed,
                turn_limit_usd: None,
                session_limit_usd: None,
                turn_spent_usd: 0.0,
                session_spent_usd: 0.0,
            },
            total_cost_usd: 0.0,
            calibration_model: None,
            loop_steered: false,
            loop_steered_pattern: Vec::new(),
            loop_steered_inputs: None,
            transcript_rewrites: 0,
            loop_steers_spent: 0,
        }
        .to_json()
        .expect("the fixture encodes")
    }

    /// A lane bound to its own key in its own store, and the recorder over it.
    fn lane(
        store: &Path,
        workspace: &Path,
        session: &str,
        lane: &str,
    ) -> (SessionDurability, LaneRecorder) {
        let durability = SessionDurability::default();
        assert!(
            crate::durability::bind_session_in(
                &durability,
                store,
                workspace,
                &lane_journal_key(session, lane),
            )
            .is_none(),
            "the lane binds"
        );
        let recorder = LaneRecorder::new(&durability, lane);
        (durability, recorder)
    }

    /// **The witness.** A lane killed mid-turn leaves a terminal frame, and
    /// the parent's report says how far it got.
    ///
    /// This cannot pass without the module under it. `Engine::drive` drops the
    /// resume point at every end of a turn, so the `discard` below is the
    /// engine deleting the only copy of the talk. The frame still holds four
    /// steps once the checkpoint is gone. With nothing keeping that copy there
    /// is nothing left to read.
    #[test]
    fn a_lane_that_failed_leaves_its_parent_a_frame_naming_the_last_committed_step() {
        let workspace = tempfile::tempdir().expect("workspace");
        // Its own store, never the shared `data_dir()`. See
        // `durability::bind_session_in`.
        let store = tempfile::tempdir().expect("store");
        let (durability, recorder) = lane(store.path(), workspace.path(), "ses-1", "req:1");

        // What the engine does for a turn that gave up at step 4: a
        // checkpoint at the step boundary, then the discard at the end.
        let sink = recorder.sink().expect("the lane's sink");
        sink.persist(&checkpoint_at_step_4());
        sink.discard();
        assert_eq!(
            durability.checkpoint(),
            None,
            "the engine retired the resume point, as it does on every terminal path"
        );

        recorder.settle(&WorkerEnd::Failed("provider refused".into()));

        let json = durability.terminal_frame().expect("the frame survives");
        let frame: TerminalFrame = serde_json::from_str(&json).expect("the frame parses");
        assert_eq!(frame.lane, "req:1");
        assert_eq!(frame.committed_steps, 4);
        assert_eq!(frame.ending, Ending::Failed);
        assert_eq!(
            frame.messages.len(),
            3,
            "the transcript the abort would have taken with it"
        );

        let report =
            parent_report_in(store.path(), workspace.path(), "ses-1", "req:1").expect("a report");
        assert!(
            report.contains("req:1") && report.contains("4 committed steps"),
            "the parent's report names the lane and how far it got: {report}"
        );
    }

    /// The other kill: the user stops a lane. The turn's future is dropped
    /// mid-step, so no end of a turn runs at all. The frame is built from the
    /// point still standing at the lane's tip.
    #[test]
    fn a_stopped_lane_frames_the_point_its_dropped_turn_left_standing() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store = tempfile::tempdir().expect("store");
        let (durability, recorder) = lane(store.path(), workspace.path(), "ses-1", "req:2");

        recorder
            .sink()
            .expect("the lane's sink")
            .persist(&checkpoint_at_step_4());
        // No `discard`: a dropped future reaches no end of a turn.
        recorder.settle(&WorkerEnd::Stopped);

        let json = durability.terminal_frame().expect("the frame is written");
        let frame: TerminalFrame = serde_json::from_str(&json).expect("the frame parses");
        assert_eq!(frame.ending, Ending::Stopped);
        assert_eq!(frame.committed_steps, 4);
        assert!(
            frame.reason.is_empty(),
            "a lane the user stopped did not fail, and must not read as having failed"
        );
    }

    /// The other half of the deal, and what the witness is measured against. A
    /// lane that finished has nothing for its parent to report, and it drops
    /// the frame an earlier try on the same lane left.
    #[test]
    fn a_lane_that_completed_leaves_no_frame_and_retires_an_older_one() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store = tempfile::tempdir().expect("store");
        let (durability, recorder) = lane(store.path(), workspace.path(), "ses-1", "req:3");

        let sink = recorder.sink().expect("the lane's sink");
        sink.persist(&checkpoint_at_step_4());
        sink.discard();
        recorder.settle(&WorkerEnd::Failed("first attempt fell over".into()));
        assert!(
            durability.terminal_frame().is_some(),
            "the first death frames"
        );

        // The lane runs again on the same key and finishes this time.
        let second = LaneRecorder::new(&durability, "req:3");
        let sink = second.sink().expect("the lane's sink");
        sink.persist(&checkpoint_at_step_4());
        sink.discard();
        second.settle(&WorkerEnd::Done("here is the answer".into()));

        assert_eq!(
            durability.terminal_frame(),
            None,
            "a lane that finished has no unfinished attempt to report"
        );
        assert_eq!(
            parent_report_in(store.path(), workspace.path(), "ses-1", "req:3"),
            None,
            "and the parent says nothing about it"
        );
    }

    /// A lane that fell over before its first step got through nothing. There
    /// is no talk to hand over, and no frame that claims there is.
    #[test]
    fn a_lane_that_died_before_its_first_step_leaves_nothing_to_report() {
        let workspace = tempfile::tempdir().expect("workspace");
        let store = tempfile::tempdir().expect("store");
        let (durability, recorder) = lane(store.path(), workspace.path(), "ses-1", "req:4");

        recorder.settle(&WorkerEnd::Failed("the provider would not build".into()));

        assert_eq!(durability.terminal_frame(), None);
    }
}
