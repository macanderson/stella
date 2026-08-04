//! Per-session rollups for the delivery scoreboard.
//!
//! Three of the scoreboard's four numbers are countable from rows this store
//! already writes: model calls and the characters a person typed come from
//! `executions`, tool calls from `tool_calls`, and the verdict — when the work
//! produced a pull request — from `pull_requests.status`.
//!
//! Nothing here judges anything. The rollup is arithmetic over recorded events,
//! and the one subjective field is read from a PR that a human merged or closed.
//!
//! # What is deliberately absent
//!
//! **Interrupts.** A follow-up that stopped the agent mid-flight is the one
//! correction signal needing no interpretation, and nothing in this store
//! records it — `executions.outcome` collapses a human cancellation and a
//! provider error into `aborted`. [`SessionRollup`] therefore does not carry an
//! interrupt count at all, rather than carrying a zero that would read as "none
//! happened".

use rusqlite::Result;

use crate::Store;

/// One session's countable facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRollup {
    pub session_id: String,
    /// Executions in the session. Each is one invocation of the model, and each
    /// begins with something a person typed.
    pub executions: u32,
    /// Tool calls across those executions.
    pub tool_calls: u32,
    /// Characters a person typed, summed over the executions' prompts.
    pub prompt_chars: u64,
    /// The status of a pull request produced by this session, when there is
    /// one: `draft` | `open` | `merged` | `closed`.
    pub pr_status: Option<String>,
}

impl Store {
    /// Roll up every session that has at least one finished execution.
    ///
    /// Ordered by session id so the output is stable across runs — a scoreboard
    /// whose row order moves is one nobody can diff.
    ///
    /// Sessions are the unit here because they are the only span this store
    /// already delimits. That is a placeholder for a configurable unit, not a
    /// claim that a session is the right one: the unit that matters is whatever
    /// a person forms an opinion about, which is why the PR join exists.
    pub fn session_rollups(&self) -> Result<Vec<SessionRollup>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT e.session_id,
                    COUNT(DISTINCT e.id),
                    COALESCE(SUM(LENGTH(e.prompt)), 0),
                    (SELECT COUNT(*) FROM tool_calls tc
                       JOIN executions e2 ON e2.id = tc.execution_id
                      WHERE e2.session_id = e.session_id),
                    (SELECT p.status FROM pull_requests p
                      WHERE p.session_id = e.session_id
                      ORDER BY p.updated_at DESC LIMIT 1)
               FROM executions e
              WHERE e.session_id IS NOT NULL
                AND e.session_id <> ''
                AND e.outcome IS NOT NULL
              GROUP BY e.session_id
              ORDER BY e.session_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SessionRollup {
                session_id: row.get(0)?,
                executions: row.get::<_, i64>(1)?.try_into().unwrap_or(u32::MAX),
                prompt_chars: row.get::<_, i64>(2)?.try_into().unwrap_or(0),
                tool_calls: row.get::<_, i64>(3)?.try_into().unwrap_or(u32::MAX),
                pr_status: row.get(4)?,
            })
        })?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    fn store(dir: &tempfile::TempDir) -> Store {
        Store::open(dir.path()).expect("open")
    }

    fn finished(store: &Store, session: &str, prompt: &str) -> i64 {
        let id = store
            .begin_execution("chat", prompt, "anthropic", "claude")
            .expect("begin");
        store.set_execution_session(id, session).expect("session");
        store
            .finish_execution_accounted(id, "completed", 0.0, true)
            .expect("finish");
        id
    }

    #[test]
    fn a_session_with_no_finished_execution_is_not_rolled_up() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        let id = s
            .begin_execution("chat", "hello", "anthropic", "claude")
            .expect("begin");
        s.set_execution_session(id, "sess-open").expect("session");
        assert!(s.session_rollups().expect("rollup").is_empty());
    }

    #[test]
    fn executions_and_prompt_chars_are_summed_per_session() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        finished(&s, "sess-a", "12345");
        finished(&s, "sess-a", "123");
        finished(&s, "sess-b", "1");

        let rollups = s.session_rollups().expect("rollup");
        assert_eq!(rollups.len(), 2, "{rollups:?}");
        assert_eq!(rollups[0].session_id, "sess-a");
        assert_eq!(rollups[0].executions, 2);
        assert_eq!(rollups[0].prompt_chars, 8);
        assert_eq!(rollups[1].session_id, "sess-b");
        assert_eq!(rollups[1].executions, 1);
    }

    #[test]
    fn a_session_without_a_pull_request_has_no_verdict_source() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        finished(&s, "sess-a", "hello");
        assert_eq!(s.session_rollups().expect("rollup")[0].pr_status, None);
    }

    #[test]
    fn the_most_recent_pull_request_status_wins() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        finished(&s, "sess-a", "hello");
        s.upsert_pull_request(
            Some("sess-a"),
            "https://x/pull/1",
            Some(1),
            "open",
            None,
            100,
        )
        .expect("open");
        s.upsert_pull_request(
            Some("sess-a"),
            "https://x/pull/1",
            Some(1),
            "merged",
            None,
            200,
        )
        .expect("merged");
        assert_eq!(
            s.session_rollups().expect("rollup")[0].pr_status.as_deref(),
            Some("merged")
        );
    }

    #[test]
    fn rollups_are_ordered_and_deterministic() {
        let dir = tempfile::tempdir().expect("tmp");
        let s = store(&dir);
        finished(&s, "sess-z", "z");
        finished(&s, "sess-a", "a");
        let first = s.session_rollups().expect("rollup");
        let ids: Vec<&str> = first.iter().map(|r| r.session_id.as_str()).collect();
        assert_eq!(ids, vec!["sess-a", "sess-z"]);
        assert_eq!(first, s.session_rollups().expect("rollup"));
    }
}
