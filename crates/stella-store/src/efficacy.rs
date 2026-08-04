//! Efficacy-attribution reads — Phase 4 (#715).
//!
//! The `store.db` side of the context-use extractor: which turns have closed,
//! what each one cited, and which of its tool calls failed. Everything here is
//! a *read*; the records these rows become are written to the append-only
//! ledger in `context.db`, not here.
//!
//! Its own module rather than three more methods on the already-oversized
//! `lib.rs`, per the phase's own working rule — new code goes in a new module
//! instead of raising a grandfathered ceiling.

use rusqlite::params;

use crate::{MemoryCitationRow, Result, Store};

/// One closed-out execution, as the context-use extractor sees it (Phase 4,
/// #715). Only the fields an attribution needs: which turn it was, whose
/// session, how it ended, and when.
///
/// `session_id` and `finished_at` are `Option` because the columns are
/// nullable and an honest reader says so — `set_execution_session` is a second
/// write that a crash can miss, and a row written by an older binary may
/// predate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedExecution {
    pub execution_id: i64,
    pub session_id: Option<String>,
    /// `completed` | `aborted` | `cancelled` — the same vocabulary
    /// `record_execution_end` writes.
    pub outcome: String,
    pub finished_at: Option<String>,
}

impl Store {
    /// One finished execution's citations, in the order they were written.
    ///
    /// [`Self::memory_citation_stats`] folds the whole table and drops
    /// `execution_id` on the way; the context-use extractor (Phase 4, #715)
    /// needs the rows of *one* execution, because a citation is the feedback on
    /// that execution's uses and nothing else. Ordered by `rowid` so a replay
    /// derives identical record ids.
    pub fn memory_citations_for_execution(
        &self,
        execution_id: i64,
    ) -> Result<Vec<MemoryCitationRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT memory_id, useful_score, truthful, remark FROM memory_citations
             WHERE execution_id = ? ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map(params![execution_id], |row| {
            Ok(MemoryCitationRow {
                memory_id: row.get(0)?,
                useful_score: row.get(1)?,
                truthful: row.get(2)?,
                remark: row.get(3)?,
            })
        })?;
        let mut citations = Vec::new();
        for row in rows {
            citations.push(row?);
        }
        Ok(citations)
    }

    /// One execution's *failed* tool calls, oldest first (Phase 4, #715).
    ///
    /// The evidence source behind [`ObservationSource::ToolOutcome`]. Only
    /// failures: a tool that worked is the expected case and mining it would
    /// bury the signal under thousands of successes. `name` and `error` are
    /// returned rather than the whole row because an observation is a sentence
    /// about what went wrong, and `args_json` can carry secrets that have no
    /// business reaching the miner.
    ///
    /// [`ObservationSource::ToolOutcome`]: https://docs.rs/stella-core
    pub fn failed_tool_calls_for_execution(
        &self,
        execution_id: i64,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT name, error FROM tool_calls
             WHERE execution_id = ? AND ok = 0 AND error <> ''
             ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![execution_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut failures = Vec::new();
        for row in rows {
            failures.push(row?);
        }
        Ok(failures)
    }

    /// Finished executions after `after_execution_id`, oldest first — the
    /// context-use extractor's source (Phase 4, #715).
    ///
    /// **Finished only.** `outcome IS NOT NULL` is the whole point: an
    /// in-flight execution has no outcome to attribute a use to, and extracting
    /// it early would mint a `ContextUse` whose feedback can never arrive.
    /// `record_execution_end` sets `outcome` and the citations in the same
    /// close-out, so an execution that passes this filter has both or neither.
    ///
    /// Ascending by id so the cursor can advance monotonically; ids are
    /// `AUTOINCREMENT` and never reused, so "after id N" is a durable watermark.
    pub fn finished_executions_after(
        &self,
        after_execution_id: i64,
        limit: u32,
    ) -> Result<Vec<FinishedExecution>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, outcome, finished_at FROM executions
             WHERE id > ? AND outcome IS NOT NULL
             ORDER BY id ASC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![after_execution_id, i64::from(limit)], |row| {
            Ok(FinishedExecution {
                execution_id: row.get(0)?,
                session_id: row.get(1)?,
                outcome: row.get(2)?,
                finished_at: row.get(3)?,
            })
        })?;
        let mut executions = Vec::new();
        for row in rows {
            executions.push(row?);
        }
        Ok(executions)
    }
}
