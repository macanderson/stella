// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which turn dispatched which execution (schema v36, #4628).
//!
//! A `delegate` child opens no execution row: its whole recorded life lives
//! under the parent's id, and `sub_agent_id` on the four attributed events is
//! what separates it (#4383, #4624). A **deck worker lane** — `req:<n>` or
//! `sub:<task-id>` — is the other shape: it opens a real row of its own, with
//! its own transcript, and until v36 nothing recorded which turn asked for it.
//! Its parentage existed only in the deck's in-memory lane registry
//! (`AgentMeta::with_parent`), which never reached the store.
//!
//! So a lane was indistinguishable from a lead turn in every session-scoped
//! query, and no turn-scoped query for a turn's lane fan-out could exist at
//! all. These two functions are that query and the write that makes it
//! answerable.
//!
//! # NULL is an answer
//!
//! Most lanes are dispatched by a *person*, from the composer, between turns —
//! no turn asked for those, and their rows stay NULL. So does every lead turn.
//! The column separates "a turn asked for this" from "somebody did"; a default
//! would collapse exactly that distinction, which is why there is none and no
//! backfill (`migrations::dispatched_executions`).

use rusqlite::params;

use crate::{Result, Store};

impl Store {
    /// Stamp the execution that **dispatched** this one.
    ///
    /// Called only when a turn asked for the lane. A lane a person started
    /// from the composer has no dispatching turn and its row is left NULL —
    /// that is the answer, not a missing one.
    pub fn set_execution_parent(&self, execution_id: i64, parent_execution_id: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE executions SET parent_execution_id = ? WHERE id = ?",
            params![parent_execution_id, execution_id],
        )?;
        Ok(())
    }

    /// The executions one turn dispatched, oldest first.
    ///
    /// Served by the partial `executions_by_parent` index, which holds only
    /// the dispatched rows — a small minority of any workspace's executions,
    /// so the question costs an index probe rather than a scan.
    pub fn executions_dispatched_by(&self, parent_execution_id: i64) -> Result<Vec<i64>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT id FROM executions WHERE parent_execution_id = ? ORDER BY id")?;
        let rows = stmt.query_map(params![parent_execution_id], |row| row.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    /// **The witness for #4628.** The store can answer "which executions did
    /// turn N dispatch", and a lane nobody's turn asked for is not attributed
    /// to one.
    ///
    /// Fails before this change: there was no column, so the question had no
    /// query — a lane's parentage lived only in the deck's memory and every
    /// store-side answer was session-scoped, which cannot separate one turn's
    /// fan-out from the session's whole history of lanes.
    #[test]
    fn a_turn_can_name_the_lanes_it_dispatched_and_only_those() {
        let store = Store::in_memory().expect("store");
        let lead = store
            .begin_execution("deck", "fix the router", "zai", "glm-5.2")
            .expect("lead turn");
        let dispatched = store
            .begin_execution("deck-sub", "task #1", "zai", "glm-5.2")
            .expect("dispatched lane");
        let by_hand = store
            .begin_execution("deck-sub", "have a look", "zai", "glm-5.2")
            .expect("composer lane");
        let later = store
            .begin_execution("deck", "and now the tests", "zai", "glm-5.2")
            .expect("a later lead turn");

        store
            .set_execution_parent(dispatched, lead)
            .expect("stamp the dispatcher");

        assert_eq!(
            store.executions_dispatched_by(lead).expect("query"),
            vec![dispatched],
            "the turn names the lane it asked for"
        );
        assert!(
            store
                .executions_dispatched_by(later)
                .expect("query")
                .is_empty(),
            "and not one another turn's session happens to contain"
        );
        // The composer lane is unattributed, which is the fact rather than a
        // gap — and it must not be swept into the open turn just because one
        // was running.
        let parent: Option<i64> = store
            .lock()
            .query_row(
                "SELECT parent_execution_id FROM executions WHERE id = ?",
                [by_hand],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(parent, None);
    }
}
