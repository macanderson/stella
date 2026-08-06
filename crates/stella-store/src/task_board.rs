//! The `tasks` table: the latest task-board snapshot per session, one row
//! per `(session_id, task_id)`, mirrored from the protocol's `TaskUpdate`
//! snapshots.
//!
//! Board state, not history — the `events` stream already keeps every
//! snapshot, so each write REPLACES a task's row. The DDL lives in
//! `crate::ddl` (private); this module is the read/write API over it.
//!
//! ## Why `/clear` deletes a session's rows (#1692)
//!
//! `/clear` is a session destroy-and-reset: it empties the live board
//! (`stella_core::tasks::TaskBoard::clear`), and board ids restart at "1".
//! Leaving the mirror alone would be wrong in both directions — a post-clear
//! task "1" silently upserts over an unrelated pre-clear task's row, and
//! every pre-clear id ABOVE the new board's length survives as a row this
//! session no longer has any task for. That is a persisted copy of a board
//! the user destroyed, still attributed to the session that destroyed it.
//!
//! [`Store::clear_session_tasks`] is therefore part of the clear, not an
//! optional tidy-up. It deletes only the rows keyed to that session id, and
//! only the `tasks` mirror: the execution rows, the telemetry, and the events
//! journal that recorded what the pre-clear board actually did are untouched,
//! because those are the audit trail and the board mirror is not.

use rusqlite::params;
use stella_protocol::TaskItem;

use crate::{Result, Store, task_status_from_string, task_status_to_string};

impl Store {
    /// Mirror one task-board snapshot into `tasks`: every item is upserted
    /// on (session_id, task_id), so the table always holds each task's
    /// LATEST state (the `events` stream keeps the full snapshot history).
    /// `status`/`owner` are stored as the protocol's serde snake_case
    /// strings (e.g. `"in_progress"`); `description` is stored as-is.
    /// NOTE: with `session_id` `None` the UNIQUE key never conflicts (SQL
    /// NULLs are pairwise distinct), so session-less rows append rather than
    /// replace — dedup is a per-session guarantee.
    ///
    /// One transaction, like every sibling fan-out writer in [`crate`] (see
    /// [`Store::record_files_touched`]): a snapshot is a whole board, so an
    /// item that fails partway must not leave the table holding half of one —
    /// and the batch costs one WAL commit instead of one per task on a path
    /// that runs at every `TaskUpdate`.
    pub fn record_task_board(
        &self,
        execution_id: i64,
        session_id: Option<&str>,
        tasks: &[TaskItem],
        now_ms: u64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for item in tasks {
            let status = task_status_to_string(item.status)?;
            tx.execute(
                "INSERT INTO tasks \
                 (execution_id, session_id, task_id, subject, description, status, owner, \
                  updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (session_id, task_id) DO UPDATE SET \
                 execution_id = excluded.execution_id, subject = excluded.subject, \
                 description = excluded.description, status = excluded.status, \
                 owner = excluded.owner, updated_at = excluded.updated_at",
                params![
                    execution_id,
                    session_id,
                    item.id,
                    item.subject,
                    item.description,
                    status,
                    item.owner,
                    now_ms as i64,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Read back a session's task board — each task's latest recorded state,
    /// ordered numerically by the board's ordinal task id (`CAST(task_id AS
    /// INTEGER)`, so "10" sorts after "2"). A stored status that no longer
    /// parses as a [`stella_protocol::TaskStatus`] is an error: the store
    /// wrote it, so a bad token is corruption, not drift.
    pub fn list_session_tasks(&self, session_id: &str) -> Result<Vec<TaskItem>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT task_id, subject, description, status, owner FROM tasks \
             WHERE session_id = ? ORDER BY CAST(task_id AS INTEGER) ASC, task_id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut board = Vec::new();
        for row in rows {
            let (task_id, subject, description, status, owner) = row?;
            board.push(TaskItem {
                id: task_id,
                subject,
                description,
                status: task_status_from_string(&status)?,
                owner,
            });
        }
        Ok(board)
    }

    /// Delete a session's whole task-board mirror — the persisted half of
    /// `/clear` (#1692). Returns the number of rows removed.
    ///
    /// Scoped by `session_id` alone, deliberately: the board a clear
    /// destroys is the session's, and rows recorded without a session id
    /// (SQL NULLs never match `=`) belong to no session, so they are not
    /// this clear's to delete.
    pub fn clear_session_tasks(&self, session_id: &str) -> Result<usize> {
        let conn = self.lock();
        let removed = conn.execute(
            "DELETE FROM tasks WHERE session_id = ?",
            params![session_id],
        )?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use stella_protocol::TaskStatus;

    use crate::Store;

    fn item(id: &str, subject: &str) -> stella_protocol::TaskItem {
        stella_protocol::TaskItem {
            id: id.into(),
            subject: subject.into(),
            description: None,
            status: TaskStatus::Completed,
            owner: Some("sub:1".into()),
        }
    }

    /// **Witness for #1692's persisted half.** A cleared board must not
    /// survive in the mirror, and the deletion must stop at the session
    /// boundary — one session clearing its board must never erase another's.
    ///
    /// The stale-row half is the one that bites: board ids restart at "1"
    /// after a clear, so only the ids the new board happens to reach are
    /// overwritten by later snapshots. Everything above them would sit in
    /// `tasks` forever, attributed to a session that has no such task.
    #[test]
    fn clearing_a_session_removes_its_whole_mirror_and_no_one_elses() {
        let store = Store::in_memory().unwrap();
        let exec = store
            .begin_execution("deck", "do the work", "anthropic", "claude")
            .unwrap();
        let board = [item("1", "one"), item("2", "two"), item("3", "three")];
        store
            .record_task_board(exec, Some("ses-clear"), &board, 1)
            .unwrap();
        store
            .record_task_board(exec, Some("ses-other"), &board[..1], 1)
            .unwrap();

        let removed = store.clear_session_tasks("ses-clear").unwrap();

        assert_eq!(removed, 3, "every row of the cleared session goes");
        assert!(store.list_session_tasks("ses-clear").unwrap().is_empty());
        assert_eq!(
            store.list_session_tasks("ses-other").unwrap().len(),
            1,
            "a clear is scoped to its own session"
        );
        // And a second clear is a no-op rather than an error, so the clear
        // path never has to know whether a board was ever mirrored.
        assert_eq!(store.clear_session_tasks("ses-clear").unwrap(), 0);
    }
}
