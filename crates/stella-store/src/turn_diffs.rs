// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `session_turn_diffs` table: one row per `(session, turn)` holding the
//! turn's precomputed workspace diff (#1870).
//!
//! The DDL lives in `crate::ddl` (private); this module is the read/write API
//! over it. The row is written by the session that owns the work journal, at
//! the moment it marks the turn (`stella-cli`'s `turn_diff` module holds the
//! computation and the boundary ruling); it is read back by the observatory's
//! `/api/session-turn-diff` route, which deliberately reads this projection
//! instead of the bare git repo the marks live in.
//!
//! `files` is opaque JSON here on purpose. Its shape (per-file entries
//! carrying `stella-diff` hunks) is decided by the writer and consumed by the
//! dashboard; the store persists and returns bytes, exactly as it does for
//! `events.payload`.

use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

/// One `(session, turn)` diff read back out of the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTurnDiffRow {
    /// The work journal's turn ordinal the row describes.
    pub turn: u32,
    /// The execution the turn ran as, when one was open — the join key the
    /// Sessions view uses; the journal ordinal and `executions.id` have no
    /// other persisted correspondence.
    pub execution_id: Option<i64>,
    /// When the diff was recorded (SQLite `CURRENT_TIMESTAMP`, UTC).
    pub recorded_at: String,
    /// The writer's JSON payload, verbatim.
    pub files: String,
}

impl Store {
    /// Persist one turn's diff, replacing any earlier record of the same
    /// `(session, turn)` — the table mirrors the ref namespace, where a turn
    /// mark is a single ref.
    pub fn record_session_turn_diff(
        &self,
        session_id: &str,
        turn: u32,
        execution_id: Option<i64>,
        files_json: &str,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO session_turn_diffs (session_id, turn, execution_id, files) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT (session_id, turn) DO UPDATE SET \
                 execution_id = excluded.execution_id, \
                 files = excluded.files, \
                 recorded_at = CURRENT_TIMESTAMP",
            params![session_id, turn, execution_id, files_json],
        )?;
        Ok(())
    }

    /// One turn's recorded diff, or `None` when that turn never recorded one
    /// — a session older than this table, a turn that touched no files, or a
    /// diff whose write failed (the write path is best-effort by design).
    pub fn session_turn_diff(
        &self,
        session_id: &str,
        turn: u32,
    ) -> Result<Option<SessionTurnDiffRow>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT turn, execution_id, recorded_at, files \
                 FROM session_turn_diffs WHERE session_id = ? AND turn = ?",
                params![session_id, turn],
                |r| {
                    Ok(SessionTurnDiffRow {
                        turn: r.get::<_, i64>(0)? as u32,
                        execution_id: r.get(1)?,
                        recorded_at: r.get(2)?,
                        files: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    /// The #1870 store half: a recorded turn diff reads back verbatim, a
    /// re-record replaces rather than duplicates, and an unrecorded turn is
    /// `None`. Fails on a pre-v21 schema (no table).
    #[test]
    fn a_turn_diff_round_trips_and_re_recording_replaces() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let store = Store::open(dir.path()).expect("real migrations");

        assert_eq!(
            store.session_turn_diff("ses-1", 1).expect("read"),
            None,
            "an unrecorded turn has no diff"
        );

        store
            .record_session_turn_diff("ses-1", 1, Some(7), r#"{"files":[]}"#)
            .expect("record");
        let row = store
            .session_turn_diff("ses-1", 1)
            .expect("read")
            .expect("present");
        assert_eq!(row.turn, 1);
        assert_eq!(row.execution_id, Some(7));
        assert_eq!(row.files, r#"{"files":[]}"#, "stored verbatim");

        store
            .record_session_turn_diff("ses-1", 1, Some(8), r#"{"files":[{"path":"a"}]}"#)
            .expect("re-record");
        let row = store
            .session_turn_diff("ses-1", 1)
            .expect("read")
            .expect("present");
        assert_eq!(row.execution_id, Some(8), "replaced, not duplicated");
        assert_eq!(row.files, r#"{"files":[{"path":"a"}]}"#);

        assert_eq!(
            store.session_turn_diff("ses-2", 1).expect("read"),
            None,
            "another session's turn 1 is not this one's"
        );
    }
}
