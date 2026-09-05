//! What a worker lane leaves when it dies mid-turn.
//!
//! A lane runs beside a lead chat. It does not resume itself.
//! `doc:turn-lane-assembly` §6 calls that `ResumeAuthority::Parent`. The parent
//! reads the lane's history to report it. Nothing re-enters it.
//!
//! So a lane owes one record, not one per step. That record is the **terminal
//! frame**. It holds the steps the lane got through and the messages that got
//! it there. It is written once, when the lane dies.
//!
//! # Why its own blob
//!
//! The frame sits beside [`crate::work_journal::CHECKPOINT_BLOB`], and that
//! blob's docs give the reason. A turn's record and the file changes it covers
//! belong in one commit graph. Nothing can update two stores at once.
//!
//! They are two blobs because they have two lives. A checkpoint dies with its
//! turn: `Engine::drive` drops it at every end, an abort too. So the engine
//! deletes a failed lane's messages before a reader exists. A frame is written
//! *because* the turn ended badly. It stands until that lane finishes a later
//! try. One blob for both jobs would offer a dead lane's messages back to the
//! resume path.

use crate::Result;
use crate::work_journal::WorkJournal;

/// The reserved blob holding a dead lane's terminal frame.
pub const TERMINAL_FRAME_BLOB: &str = "terminal-frame.json";

/// Write this lane a terminal frame, in place of any earlier one.
///
/// One commit, once per dead lane. No per-step path reaches it.
pub fn record(journal: &WorkJournal, json: &str) -> Result<String> {
    journal.record(
        &[],
        &[(TERMINAL_FRAME_BLOB, Some(json))],
        "stella: lane terminal frame",
    )
}

/// The frame this lane's last dead try left, or `None`.
///
/// A record that will not read answers `None`, as a missing one does. Both
/// mean there is nothing to report. The caller acts the same way on each.
pub fn read(journal: &WorkJournal) -> Option<String> {
    journal.blob_at_tip(TERMINAL_FRAME_BLOB)
}

/// Drop the frame. This lane finished, so it has no dead try to report. A
/// frame left standing would have the parent report a death the lane has
/// since come back from.
///
/// Safe to call twice, and free when there is no frame. Every lane that
/// finishes calls it, so the common case must not cost a commit.
pub fn clear(journal: &WorkJournal) -> Result<()> {
    if read(journal).is_none() {
        return Ok(());
    }
    journal.record(
        &[],
        &[(TERMINAL_FRAME_BLOB, None)],
        "stella: lane terminal frame retired",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A journal over two temp dirs. Never `WorkJournal::open`: it finds its
    /// store through `STELLA_HOME`, so these tests would fight their siblings
    /// over one shared path.
    fn journal(name: &str) -> (tempfile::TempDir, WorkJournal) {
        let guard = tempfile::tempdir().unwrap();
        let ws = guard.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let store = guard.path().join("store");
        let journal = WorkJournal::open_in(&store, &ws, name).unwrap();
        (guard, journal)
    }

    #[test]
    fn a_frame_is_written_read_back_and_retracted() {
        let (_guard, journal) = journal("ses-1__req-1");
        assert_eq!(read(&journal), None, "a lane starts with no frame");

        record(&journal, r#"{"step":7}"#).unwrap();
        assert_eq!(read(&journal).as_deref(), Some(r#"{"step":7}"#));

        record(&journal, r#"{"step":9}"#).unwrap();
        assert_eq!(
            read(&journal).as_deref(),
            Some(r#"{"step":9}"#),
            "a second death replaces the first frame rather than stacking"
        );

        clear(&journal).unwrap();
        assert_eq!(read(&journal), None);
        clear(&journal).unwrap();
    }

    /// The two records have two lives, which is why the frame is its own blob.
    /// Dropping the resume point at the end of a turn leaves the report.
    #[test]
    fn retiring_the_checkpoint_leaves_the_frame_standing() {
        let (_guard, journal) = journal("ses-1__req-2");
        journal.record_checkpoint(r#"{"step":3}"#, None).unwrap();
        record(&journal, r#"{"step":3}"#).unwrap();

        journal.clear_checkpoint().unwrap();

        assert_eq!(journal.checkpoint(), None, "the resume point is retired");
        assert_eq!(
            read(&journal).as_deref(),
            Some(r#"{"step":3}"#),
            "and the frame the parent reports from survives it"
        );
    }
}
