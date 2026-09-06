//! What a deck lane hands its parent when it dies mid-turn.
//!
//! A sub-session lane runs beside the lead chat and is never re-entered:
//! [`stella_protocol::ResumeAuthority::Parent`] is the answer
//! `BuiltinLane::SubSession` gives. So the lane owes one record, the terminal
//! frame, and the lead reads it to report how far the lane got.
//!
//! The writer is shared with the fleet's `Redispatch` lane and lives in
//! [`crate::lane_frame`]. What stays here is the sub-session's own half: the
//! reader the deck driver calls, and the mapping from [`WorkerEnd`] — which
//! carries a finished worker's answer, and so is richer than a frame needs —
//! onto [`LaneEnd`].

use std::path::Path;

use super::{WorkerEnd, lane_journal_key};
use crate::durability::SessionDurability;
use crate::lane_frame::{LaneEnd, TerminalFrame, report_line};

impl From<&WorkerEnd> for LaneEnd {
    fn from(end: &WorkerEnd) -> Self {
        match end {
            WorkerEnd::Done(_) => Self::Done,
            WorkerEnd::Failed(reason) => Self::Failed(reason.clone()),
            WorkerEnd::Stopped => Self::Stopped,
        }
    }
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
    Some(report_line(&TerminalFrame::read(&durability)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    use stella_protocol::CompletionMessage;

    use crate::lane_frame::{Ending, LaneRecorder};

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

    /// **The witness for the `Parent` arm.** A lane killed mid-turn leaves a
    /// terminal frame, and the parent's report says how far it got.
    ///
    /// This cannot pass without [`crate::lane_frame`]. `Engine::drive` drops
    /// the resume point at every end of a turn, so the `discard` below is the
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

        recorder.settle(&LaneEnd::from(&WorkerEnd::Failed(
            "provider refused".into(),
        )));

        let frame = TerminalFrame::read(&durability).expect("the frame survives");
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
        recorder.settle(&LaneEnd::from(&WorkerEnd::Stopped));

        let frame = TerminalFrame::read(&durability).expect("the frame is written");
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
        recorder.settle(&LaneEnd::from(&WorkerEnd::Failed(
            "first attempt fell over".into(),
        )));
        assert!(
            durability.terminal_frame().is_some(),
            "the first death frames"
        );

        // The lane runs again on the same key and finishes this time.
        let second = LaneRecorder::new(&durability, "req:3");
        let sink = second.sink().expect("the lane's sink");
        sink.persist(&checkpoint_at_step_4());
        sink.discard();
        second.settle(&LaneEnd::from(&WorkerEnd::Done(
            "here is the answer".into(),
        )));

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

        recorder.settle(&LaneEnd::from(&WorkerEnd::Failed(
            "the provider would not build".into(),
        )));

        assert_eq!(durability.terminal_frame(), None);
    }
}
