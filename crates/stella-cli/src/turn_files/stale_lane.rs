// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Whether a turn ended with the lead's task lane still occupied while it
//! changed files, and the diagnostic that records the rate.
//!
//! `TaskBoard::set_status` refuses a second unowned `InProgress` card. That
//! fixes the *next* `task_start`. It does nothing for a turn that never
//! calls `task_start` and just starts editing. The board cannot tell "this
//! edit belongs to a later step" on its own. So the first move is to
//! measure how often a turn ends this way, before anything gets injected
//! into the model's context.
//!
//! Split out of [`super`] instead of grown inside it: that file sits at its
//! own 1500-line ceiling (AGENTS.md § "God files — plan around them, never
//! into them"). New logic here lands in this sibling instead.

use stella_diag::{Cx, Dx, Fields, Level, Record};

use super::DIAG_TARGET;

/// How many changed files make an untouched board worth a record.
///
/// One file is the normal shape of a turn still on the card it says it is
/// on. Three is a guess. The point of the record is to find the real rate,
/// before anything is injected into the model's context.
const STALE_LANE_FILES: usize = 3;

/// Record a turn that ended with the lead's task lane still occupied while
/// it changed files.
///
/// # What this does not claim
///
/// Not a defect signal. A turn that ends mid-card, having edited three
/// files, is the normal shape of work in progress. This fires on it too.
/// What matters is the rate: how often a turn ends this way at all.
/// `agent.turn_complete` fires once per turn, so the ratio is a join, not a
/// second counter.
///
/// # Which drivers it covers
///
/// The two that close a turn through `close_turn_boundary`: `run_turn`
/// (`stella run`) and the deck's lead turn. That is the surface the
/// divergence was reported on. The goal arcs and `run_resume` measure per
/// round instead, and pay one terminator at the end. A lane check there
/// asks about a round, not a turn — a different question, worth asking
/// only if this data says so.
///
/// # Why counts only
///
/// A task id and a subject are model output. `stella-diag`'s field types
/// refuse both. Naming the card the agent walked past would put
/// model-authored text into the diagnostic plane, for a signal that only
/// needs a number.
///
/// `dx` is a parameter, not the global handle. That lets a test pass a
/// capturing one and check the record it gets.
pub(super) fn note_stale_lane(
    dx: &Dx,
    registry: &stella_tools::ToolRegistry,
    files_changed: usize,
) {
    let board = registry.task_board();
    let board = board
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(open_tasks) = stale_lane_open_tasks(&board, files_changed) else {
        return;
    };
    dx.emit(Record::new(
        Level::Debug,
        "agent.task.stale_lane",
        DIAG_TARGET,
        Cx::EMPTY,
        Fields::new()
            .with("files_changed", files_changed as u64)
            .with("open_tasks", open_tasks as u64)
            .with("threshold", STALE_LANE_FILES as u64),
    ));
}

/// The decision [`note_stale_lane`] makes, as a total function of the board
/// and the count. Returns `Some(open_tasks)` when the record is owed.
///
/// Split out because the emit half needs a `ToolRegistry`, a lock, and a
/// `Dx`. None of those is the part that can be wrong. What can be wrong is
/// which boards count. Two edges a reader would miss: a delegated card has
/// an owner, so it does not occupy the lead's lane. And `open_tasks` counts
/// pending cards too, because a plan the agent stopped walking is exactly
/// the shape worth reporting, even from a one-card board.
fn stale_lane_open_tasks(
    board: &stella_core::tasks::TaskBoard,
    files_changed: usize,
) -> Option<usize> {
    if files_changed < STALE_LANE_FILES {
        return None;
    }
    board.unowned_in_progress()?;
    Some(
        board
            .items()
            .iter()
            .filter(|task| task.status.is_open())
            .count(),
    )
}

#[cfg(test)]
mod tests {
    use stella_core::tasks::TaskBoard;
    use stella_diag::FieldValue::Uint;
    use stella_protocol::event::TaskStatus;

    use super::*;

    /// **Witness.** A turn that changed files while the lead's task lane
    /// sat occupied gets recorded. Three boards that look like it, but are
    /// not, stay silent.
    ///
    /// The delegated case is the one worth pinning down. `task_assign`
    /// gives a card an owner and skips `set_status`. So a session that
    /// fanned work out to sub-agents can show N in-progress cards with
    /// nothing wrong. A predicate that counted them would flag every
    /// delegating session, and the measurement would be worthless.
    #[test]
    fn only_an_unowned_in_progress_card_over_the_file_threshold_is_recorded() {
        let mut walked_past = TaskBoard::new();
        walked_past.create("investigate the embedding pass", None, None);
        walked_past.create("rewrite the chunker", None, None);
        walked_past.set_status("1", TaskStatus::InProgress).unwrap();

        assert_eq!(
            stale_lane_open_tasks(&walked_past, STALE_LANE_FILES),
            Some(2),
            "this is the shape the check looks for"
        );
        assert_eq!(
            stale_lane_open_tasks(&walked_past, STALE_LANE_FILES - 1),
            None,
            "under the threshold a turn is normal work in progress, not a signal"
        );

        let mut delegated = TaskBoard::new();
        delegated.create("run the fan-out", None, None);
        delegated.assign("1", "sub:worker-a").unwrap();
        assert_eq!(
            stale_lane_open_tasks(&delegated, STALE_LANE_FILES * 10),
            None,
            "a delegated card has an owner and does not occupy the lead's lane"
        );

        let mut finished = TaskBoard::new();
        finished.create("rewrite the chunker", None, None);
        finished.set_status("1", TaskStatus::InProgress).unwrap();
        finished.set_status("1", TaskStatus::Completed).unwrap();
        assert_eq!(
            stale_lane_open_tasks(&finished, STALE_LANE_FILES * 10),
            None,
            "a board the agent kept up to date must stay silent"
        );

        assert_eq!(
            stale_lane_open_tasks(&TaskBoard::new(), STALE_LANE_FILES * 10),
            None,
            "a session with no board at all cannot have walked past a card"
        );
    }

    /// **Witness.** Calls [`note_stale_lane`] and checks its record: task 1
    /// in progress plus three file writes emits the code, a completed task
    /// does not.
    #[test]
    fn note_stale_lane_emits_only_for_the_walked_past_shape() {
        let registry = stella_tools::ToolRegistry::new(std::env::temp_dir());
        {
            let handle = registry.task_board();
            let mut board = handle.lock().unwrap();
            board.create("investigate", None, None);
            board.create("rewrite the chunker", None, None);
            board.set_status("1", TaskStatus::InProgress).unwrap();
        }

        let (dx, records) = Dx::capturing();
        note_stale_lane(&dx, &registry, STALE_LANE_FILES);
        let record = records.find("agent.task.stale_lane").expect("walked past");
        assert_eq!(record.level, Level::Debug);
        assert_eq!(
            record.fields.get("files_changed"),
            Some(&Uint(STALE_LANE_FILES as u64))
        );
        assert_eq!(record.fields.get("open_tasks"), Some(&Uint(2)));

        registry
            .task_board()
            .lock()
            .unwrap()
            .set_status("1", TaskStatus::Completed)
            .unwrap();
        let (dx, records) = Dx::capturing();
        note_stale_lane(&dx, &registry, STALE_LANE_FILES);
        assert!(
            records.find("agent.task.stale_lane").is_none(),
            "a completed task must stay silent"
        );
    }
}
