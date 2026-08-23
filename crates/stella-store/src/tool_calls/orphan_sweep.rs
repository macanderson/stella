// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Settling an execution whose owning session is provably gone (#3952).
//!
//! [`Store::reconcile_interrupted_executions`] repairs the *calls* of a turn
//! that died without its finalizer and stops there on purpose: it runs at
//! store open, and a second session opening the same workspace must not
//! declare a live turn dead. [`Store::mark_execution_interrupted`] is the
//! other half — it stamps the outcome — and nothing called it. So a killed run
//! kept `finished_at IS NULL` forever, and `finished_at IS NULL` is what every
//! surface reads as "in flight": the Observatory's Live panel rendered a
//! three-week-old corpse with `elapsed_ms` still counting up.
//!
//! What was missing is not the write but the **proof**. The cross-process
//! session registry (`~/.stella/sessions`, [`SessionRegistry`]) is where a
//! turn's owner is recorded, and it already answers liveness at read time:
//! [`SessionRegistry::presented_status`] downgrades a live-status record whose
//! pid is gone to `Error`. An execution is settled when the registry *holds* a
//! record for its session and that record is not live.
//!
//! **Absence is not proof.** A registry that is empty, unreadable, or simply
//! not the one this process can see makes every session look gone, and a sweep
//! trusting that would declare another process's live turn dead — the exact
//! risk `reconcile_interrupted_executions` refuses to take. So a missing
//! record leaves its executions alone: a run whose record was pruned stays
//! unfinished rather than settled on a guess.
//!
//! There is deliberately no age cutoff. "Older than an hour" is a proxy for
//! the question the registry answers directly, and it is wrong in both
//! directions — it settles a long-running live turn and spares a run that
//! crashed a second ago.

use std::collections::HashMap;

use crate::sessions::SessionRegistry;
use crate::{Result, Store};

impl Store {
    /// The whole open-time repair of a workspace that was not closed cleanly:
    /// re-fold every unfinished execution's tool calls from its event log,
    /// then settle the executions whose owning session the registry proves is
    /// gone.
    ///
    /// Best-effort throughout, and swallowed by the caller — see
    /// [`Store::open`]. Returns how many executions were settled.
    pub(crate) fn recover_unfinished_at_open(&self) -> Result<usize> {
        self.reconcile_interrupted_executions()?;
        let settled = self.settle_orphaned_executions(&SessionRegistry::open_default())?;
        Ok(settled.len())
    }

    /// Stamp `outcome = 'interrupted'` on every unfinished execution whose
    /// owning session `registry` proves is gone, and answer their ids.
    ///
    /// The registry is a parameter rather than
    /// [`SessionRegistry::open_default`] so the predicate can be exercised
    /// against a registry a test controls, and so a caller holding an already
    /// open registry does not open a second one.
    ///
    /// An execution with no `session_id` is never in scope: there is no owner
    /// to ask about, and the one-shot `stella run` shape leaves that column
    /// NULL. See the module docs for why a *missing* record is also out of
    /// scope.
    pub fn settle_orphaned_executions(&self, registry: &SessionRegistry) -> Result<Vec<i64>> {
        let mut gone: HashMap<String, bool> = HashMap::new();
        let mut settled = Vec::new();
        for (execution_id, session_id) in self.unfinished_executions_by_session()? {
            let owner_is_gone = *gone
                .entry(session_id)
                .or_insert_with_key(|id| session_is_gone(registry, id));
            if owner_is_gone {
                self.mark_execution_interrupted(execution_id)?;
                settled.push(execution_id);
            }
        }
        Ok(settled)
    }

    /// Unfinished executions that name an owning session, oldest first.
    fn unfinished_executions_by_session(&self) -> Result<Vec<(i64, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session_id FROM executions \
             WHERE finished_at IS NULL AND session_id IS NOT NULL \
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

/// Whether `registry` proves the process behind `session_id` is gone. A record
/// it does not hold proves nothing — see the module docs.
fn session_is_gone(registry: &SessionRegistry, session_id: &str) -> bool {
    registry
        .get(session_id)
        .is_some_and(|record| !SessionRegistry::presented_status(&record).is_live())
}

#[cfg(test)]
mod tests {
    use stella_protocol::{AgentEvent, ToolCall};

    use crate::sessions::{SessionRecord, SessionRegistry, SessionStatus};
    use crate::{Store, test_env};

    /// A pid that cannot name a live process: `pid_alive` rejects anything
    /// that does not fit a `pid_t` before it ever reaches `kill`.
    const DEAD_PID: u32 = u32::MAX - 1;

    fn dead_session(registry: &SessionRegistry, id_hint: &str) -> String {
        let mut record = SessionRecord::new("/w", id_hint);
        record.pid = DEAD_PID;
        record.status = SessionStatus::InProgress;
        registry.upsert(&record).expect("register");
        record.id
    }

    /// One announced-but-unreturned call, so the execution has an event to be
    /// dated from and a `running` row to settle.
    fn announced_call() -> AgentEvent {
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c1".into(),
                name: "bash".into(),
                input: serde_json::json!({ "command": "sleep 1000" }),
            },
        }
    }

    fn seed_unfinished(store: &Store, session: Option<&str>) -> i64 {
        let id = store
            .begin_execution("deck", "prompt", "anthropic", "claude")
            .expect("execution");
        if let Some(session) = session {
            store.set_execution_session(id, session).expect("link");
        }
        id
    }

    fn outcome_and_finished(store: &Store, id: i64) -> (Option<String>, Option<String>) {
        let conn = store.lock();
        conn.query_row(
            "SELECT outcome, finished_at FROM executions WHERE id = ?",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read back")
    }

    #[test]
    fn a_dead_session_settles_its_unfinished_executions_at_the_last_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = SessionRegistry::open(dir.path());
        let session = dead_session(&registry, "crashed");

        let store = Store::in_memory().expect("store");
        let id = seed_unfinished(&store, Some(&session));
        store.record_event(id, 1, &announced_call()).expect("event");

        let settled = store.settle_orphaned_executions(&registry).expect("settle");
        assert_eq!(settled, vec![id]);

        let (outcome, finished_at) = outcome_and_finished(&store, id);
        assert_eq!(outcome.as_deref(), Some("interrupted"));
        let last_event_ts: String = {
            let conn = store.lock();
            conn.query_row(
                "SELECT max(ts) FROM events WHERE execution_id = ?",
                [id],
                |row| row.get(0),
            )
            .expect("event ts")
        };
        assert_eq!(
            finished_at.as_deref(),
            Some(last_event_ts.as_str()),
            "a run that died on Friday must not be dated to the Monday it was reopened"
        );
    }

    #[test]
    fn a_live_session_and_an_unregistered_one_are_both_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = SessionRegistry::open(dir.path());

        // Our own pid: the record is live, so the turn may still be running.
        let live = SessionRecord::new("/w", "live");
        registry.upsert(&live).expect("register");

        let store = Store::in_memory().expect("store");
        let running = seed_unfinished(&store, Some(&live.id));
        let unregistered = seed_unfinished(&store, Some("ses-never-registered"));
        let sessionless = seed_unfinished(&store, None);

        let settled = store.settle_orphaned_executions(&registry).expect("settle");
        assert!(
            settled.is_empty(),
            "settled {settled:?}; a live owner, an absent record and a NULL session are all unprovable"
        );
        for id in [running, unregistered, sessionless] {
            assert_eq!(outcome_and_finished(&store, id).1, None);
        }
    }

    /// The witness for #3952: the sweep has to run where the crash is
    /// discovered — at `Store::open` — or nothing calls it in production.
    #[test]
    fn opening_a_workspace_settles_a_crashed_session_s_execution() {
        let _guard = test_env::lock();
        let home = tempfile::tempdir().expect("tempdir");
        let _restore = test_env::EnvRestore::capture(&["STELLA_HOME", "STELLA_DATA_DIR"]);
        // SAFETY: the env lock is held for the whole test.
        unsafe {
            std::env::set_var("STELLA_HOME", home.path());
            std::env::remove_var("STELLA_DATA_DIR");
        }

        let workspace = tempfile::tempdir().expect("tempdir");
        let id = {
            let store = Store::open(workspace.path()).expect("open");
            let session = dead_session(&SessionRegistry::open_default(), "crashed");
            let id = seed_unfinished(&store, Some(&session));
            store.record_event(id, 1, &announced_call()).expect("event");
            id
        };

        let reopened = Store::open(workspace.path()).expect("reopen");
        let (outcome, finished_at) = outcome_and_finished(&reopened, id);
        assert_eq!(
            outcome.as_deref(),
            Some("interrupted"),
            "a crashed run must not stay in flight forever"
        );
        assert!(finished_at.is_some());
    }
}
