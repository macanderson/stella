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

#[cfg(test)]
mod tests {
    use crate::{MemoryCitationRow, Store, ToolCallRow, ToolCallState};

    /// One store with one finished execution, ready to hang rows off.
    fn store() -> Store {
        Store::in_memory().expect("in-memory store")
    }

    fn started(store: &Store, prompt: &str) -> i64 {
        store
            .begin_execution("run", prompt, "zai", "glm-5.2")
            .expect("execution")
    }

    fn finished(store: &Store, prompt: &str) -> i64 {
        let id = started(store, prompt);
        store
            .finish_execution(id, "completed", 0.0)
            .expect("finish execution");
        id
    }

    fn citation(memory_id: &str, remark: &str) -> MemoryCitationRow {
        MemoryCitationRow {
            memory_id: memory_id.into(),
            useful_score: 4,
            truthful: true,
            remark: remark.into(),
        }
    }

    fn call(name: &str, state: ToolCallState, error: &str) -> ToolCallRow {
        ToolCallRow {
            call_id: format!("call-{name}-{error}"),
            name: name.into(),
            surface: "native".into(),
            args_json: "{}".into(),
            args_digest: String::new(),
            reason: String::new(),
            state,
            error: error.into(),
            error_class: None,
            bytes_out: 0,
            duration_ms: 1,
            sub_agent_id: None,
        }
    }

    /// The `outcome IS NOT NULL` filter is the whole point of the read: an
    /// in-flight execution has no outcome to attribute a use to, and
    /// extracting it early mints a `ContextUse` whose feedback can never
    /// arrive.
    #[test]
    fn an_unfinished_execution_is_never_attributed() {
        let store = store();
        let running = started(&store, "still going");
        let closed = finished(&store, "done");

        let seen: Vec<i64> = store
            .finished_executions_after(0, 10)
            .expect("read")
            .into_iter()
            .map(|row| row.execution_id)
            .collect();

        assert_eq!(seen, vec![closed], "only the closed turn is attributable");
        assert!(
            !seen.contains(&running),
            "an execution with a NULL outcome must not be extracted"
        );
    }

    /// `after_execution_id` is a durable watermark: paging with it must walk
    /// every finished row exactly once, in id order, with no gap and no
    /// repeat.
    #[test]
    fn paging_by_the_watermark_walks_every_row_once_in_order() {
        let store = store();
        let ids: Vec<i64> = (0..5)
            .map(|n| finished(&store, &format!("turn {n}")))
            .collect();

        let mut cursor = 0;
        let mut walked = Vec::new();
        loop {
            let page = store.finished_executions_after(cursor, 2).expect("read");
            if page.is_empty() {
                break;
            }
            assert!(page.len() <= 2, "the limit is honoured: {}", page.len());
            for row in &page {
                assert!(
                    row.execution_id > cursor,
                    "a page must start strictly after the watermark"
                );
                walked.push(row.execution_id);
            }
            cursor = page.last().expect("non-empty").execution_id;
        }

        assert_eq!(walked, ids, "every finished row, once, oldest first");
    }

    /// A finished execution's outcome and session id are read back as written
    /// — the fields an attribution is keyed on.
    #[test]
    fn a_finished_row_carries_its_outcome_and_session() {
        let store = store();
        let id = started(&store, "aborted turn");
        store
            .set_execution_session(id, "sess-1")
            .expect("stamp session");
        store
            .finish_execution(id, "aborted", 0.0)
            .expect("finish execution");

        let row = store
            .finished_executions_after(0, 10)
            .expect("read")
            .pop()
            .expect("one finished execution");
        assert_eq!(row.execution_id, id);
        assert_eq!(row.outcome, "aborted");
        assert_eq!(row.session_id.as_deref(), Some("sess-1"));
        assert!(
            row.finished_at.is_some(),
            "a closed-out row carries its finish timestamp"
        );
    }

    /// Only real failures are mined: a call that succeeded is the expected
    /// case, and a call with no error text has nothing to say — a `running`
    /// row is a turn still in flight, not a failure.
    ///
    /// `ok = 0` and `error <> ''` are not redundant, which is why `noisy` is
    /// in the fixture: the columns are independent (`ok` is derived from
    /// `state`, `error` is free text), so a succeeded call carrying text in
    /// its `error` column is excluded by the `ok` clause alone, and a failed
    /// call with nothing to say is excluded by the `error` clause alone. Drop
    /// either and this test goes red.
    #[test]
    fn only_calls_that_failed_with_a_message_are_mined() {
        let store = store();
        let id = finished(&store, "mixed calls");
        store
            .record_tool_calls(
                id,
                &[
                    call("bash", ToolCallState::Ok, ""),
                    call("noisy", ToolCallState::Ok, "retried once, then worked"),
                    call("read_file", ToolCallState::Error, "no such file"),
                    call("search", ToolCallState::Running, ""),
                    call("edit_file", ToolCallState::Error, ""),
                ],
            )
            .expect("record calls");

        let failures = store.failed_tool_calls_for_execution(id).expect("read");

        assert_eq!(
            failures,
            vec![("read_file".to_string(), "no such file".to_string())],
            "a success (however noisy), an in-flight call, and an empty error are all excluded"
        );
    }

    /// Failures come back in call order, and only for the execution asked
    /// for.
    #[test]
    fn failures_are_ordered_by_call_and_scoped_to_one_execution() {
        let store = store();
        let mine = finished(&store, "mine");
        let other = finished(&store, "someone else's");
        store
            .record_tool_calls(
                mine,
                &[
                    call("first", ToolCallState::Error, "one"),
                    call("second", ToolCallState::Error, "two"),
                    call("third", ToolCallState::Error, "three"),
                ],
            )
            .expect("record mine");
        store
            .record_tool_calls(other, &[call("theirs", ToolCallState::Error, "not mine")])
            .expect("record theirs");

        let names: Vec<String> = store
            .failed_tool_calls_for_execution(mine)
            .expect("read")
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert_eq!(
            names,
            vec!["first", "second", "third"],
            "seq ASC, no leakage"
        );
    }

    /// Citations replay in insert order (`rowid ASC`), so a replay derives
    /// identical record ids — and only this execution's citations come back.
    #[test]
    fn citations_replay_in_insert_order_for_one_execution() {
        let store = store();
        let mine = finished(&store, "mine");
        let other = finished(&store, "someone else's");
        // Deliberately not sorted by memory id: insert order is the contract,
        // and an id-sorted read would pass on an already-sorted fixture.
        store
            .record_memory_citations(
                mine,
                &[
                    citation("nod_c", "third by id, first written"),
                    citation("nod_a", "second"),
                    citation("nod_b", "third"),
                ],
            )
            .expect("record mine");
        store
            .record_memory_citations(other, &[citation("nod_z", "not mine")])
            .expect("record theirs");

        let ids: Vec<String> = store
            .memory_citations_for_execution(mine)
            .expect("read")
            .into_iter()
            .map(|row| row.memory_id)
            .collect();

        assert_eq!(ids, vec!["nod_c", "nod_a", "nod_b"]);
    }
}
