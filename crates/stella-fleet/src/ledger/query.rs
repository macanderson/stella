//! Read-side queries over the ledger, split out of `ledger.rs`: spend sums,
//! lineage lookups, attempt status, and the two warmth-signal timestamps.

use rusqlite::{OptionalExtension, params};

use super::{AttemptId, Ledger, LedgerError};
use crate::gc::WorktreeActivity;

impl Ledger {
    /// Total USD spend recorded against a run (sum over all its tasks'
    /// attempts).
    pub fn total_spend(&self, run_id: &str) -> Result<f64, LedgerError> {
        let total = self.conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM spend WHERE run_id = ?1",
            params![run_id],
            |row| row.get::<_, f64>(0),
        )?;
        Ok(total)
    }

    /// USD spend recorded against a single task within a run.
    pub fn task_spend(&self, run_id: &str, task_id: &str) -> Result<f64, LedgerError> {
        let total = self.conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM spend WHERE run_id = ?1 AND task_id = ?2",
            params![run_id, task_id],
            |row| row.get::<_, f64>(0),
        )?;
        Ok(total)
    }

    /// Child task ids recorded as lineage under a parent run, sorted.
    pub fn lineage_children(&self, parent_run_id: &str) -> Result<Vec<String>, LedgerError> {
        let mut stmt = self.conn.prepare(
            "SELECT child_task_id FROM lineage WHERE parent_run_id = ?1 ORDER BY child_task_id",
        )?;
        let rows = stmt.query_map(params![parent_run_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// How many attempts a task has had (retries show up as extra rows).
    pub fn attempt_count(&self, run_id: &str, task_id: &str) -> Result<u32, LedgerError> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM attempts WHERE run_id = ?1 AND task_id = ?2",
            params![run_id, task_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count as u32)
    }

    /// Whether an attempt row's outcome has been stamped yet (`false` while a
    /// worker is still in flight or if it crashed before finishing).
    pub fn attempt_is_finished(&self, attempt_id: AttemptId) -> Result<bool, LedgerError> {
        let finished: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT finished_at_ms FROM attempts WHERE id = ?1",
                params![attempt_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?;
        Ok(matches!(finished, Some(Some(_))))
    }

    /// One row per worktree path this ledger has ever dispatched into: how
    /// many of its attempts are still unfinished, and when its last attempt
    /// finished — the ledger half of the GC decision (issue #1217).
    ///
    /// The unfinished count is what makes a sweep safe: an attempt row is
    /// opened *before* its worker runs, so a worktree with an unfinished
    /// attempt may still be in use and [`crate::gc::Gc`] keeps it
    /// unconditionally. `NULL` finish times are excluded from the `MAX`, so an
    /// in-flight attempt never dates a worktree.
    pub fn worktree_activity(&self) -> Result<Vec<WorktreeActivity>, LedgerError> {
        let mut stmt = self.conn.prepare(
            "SELECT worktree_path,
                    SUM(CASE WHEN finished_at_ms IS NULL THEN 1 ELSE 0 END),
                    MAX(finished_at_ms)
             FROM attempts
             GROUP BY worktree_path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(WorktreeActivity {
                worktree_path: row.get(0)?,
                unfinished_attempts: row.get::<_, i64>(1)?.max(0) as u32,
                last_finished_ms: row.get::<_, Option<i64>>(2)?.map(|ms| ms as u64),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(LedgerError::from)
    }

    /// When a task last finished an attempt, across **every** run in this
    /// ledger. This is the per-task half of the prompt-cache warmth signal
    /// (issue #1222). On a re-run of a plan the task ids repeat, so this is
    /// the timestamp the caller projects to "seconds until this task's
    /// prefix expires". Unfinished attempts (in flight, or crashed before
    /// stamping) carry no signal — the last provider call of a finished
    /// attempt is ~its finish time; an unfinished row's start time would
    /// only mis-date it. `None` when the task has never finished one.
    ///
    /// The value is whatever clock the writing fleet stamped. The CLI stamps
    /// wall-clock (Unix-epoch ms); rows written by older builds carry
    /// process-relative ms, which read as decades-stale against a wall
    /// clock — i.e. cold, the conservative direction for a warmth signal.
    pub fn last_attempt_finish_ms(&self, task_id: &str) -> Result<Option<u64>, LedgerError> {
        let last: Option<i64> = self.conn.query_row(
            "SELECT MAX(finished_at_ms) FROM attempts WHERE task_id = ?1",
            params![task_id],
            |row| row.get(0),
        )?;
        Ok(last.map(|ms| ms as u64))
    }

    /// When **any** task in this workspace last finished an attempt — the
    /// shared-prefix half of the warmth signal (issue #1222). Within one run
    /// every worker shares the same byte-stable workspace prefix, so a task
    /// with no history of its own inherits the prefix's last touch. Uniform
    /// across a ready set of first-time tasks, which makes it a
    /// no-op reorder there; it only separates tasks once per-task history
    /// exists. Same timestamp caveat as
    /// [`last_attempt_finish_ms`](Self::last_attempt_finish_ms).
    pub fn latest_attempt_finish_ms(&self) -> Result<Option<u64>, LedgerError> {
        let last: Option<i64> =
            self.conn
                .query_row("SELECT MAX(finished_at_ms) FROM attempts", [], |row| {
                    row.get(0)
                })?;
        Ok(last.map(|ms| ms as u64))
    }
}
