// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v23 → v24: `tool_calls.state` gains `'abandoned'` (#3146).
//!
//! Before this step a call whose turn ended first was stored as
//! `state = 'error'`, distinguishable from a real failure only by its
//! `error` prose — the crate-private [`ABANDONED`] sentinel. Every
//! error-rate reader groups on `state`, and none of them can (or should)
//! match that string: the Observatory reads this file directly without
//! linking this crate, so the sentinel is structurally out of its reach.
//! The result was that a SIGKILL or a ctrl-C charged an "error" to
//! whatever tool happened to be in flight, inflating exactly the per-tool
//! rates a reliability ceiling reads.
//!
//! # Why a §7 rebuild rather than nothing
//!
//! Two table populations exist, and *neither* admits the new value. A file
//! migrated from pre-v18 grew `state` via `ALTER TABLE ADD COLUMN`, which
//! carried no CHECK constraint — but a file created fresh at v18..=v23 has
//! `CHECK(state IN ('running', 'ok', 'error'))` baked into its table SQL,
//! and the first `'abandoned'` write would abort with a constraint
//! violation. SQLite cannot alter a CHECK in place, so the table is
//! rebuilt through the canonical DDL (which now includes `'abandoned'`),
//! normalizing both populations to one shape.
//!
//! The copy doubles as the backfill: a row whose `state = 'error'` carries
//! the exact sentinel prose is rewritten to `'abandoned'`. The sentinel
//! has been byte-stable since v18 (two writers were required to agree on
//! it), which is what makes the historic split recoverable at all.

use crate::Result;
use crate::ddl::{TOOL_CALLS_INDEXES, tool_calls_ddl};
use crate::tool_calls::ABANDONED;

pub(super) fn migrate_v23_to_v24(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(&tool_calls_ddl("tool_calls_v24"))?;
    tx.execute(
        "INSERT INTO tool_calls_v24 \
           (execution_id, seq, call_id, name, surface, args_json, args_digest, \
            reason, ok, state, error, bytes_out, duration_ms, ts) \
         SELECT execution_id, seq, call_id, name, surface, args_json, args_digest, \
                reason, ok, \
                CASE WHEN state = 'error' AND error = ?1 THEN 'abandoned' \
                     ELSE state END, \
                error, bytes_out, duration_ms, ts \
         FROM tool_calls",
        rusqlite::params![ABANDONED],
    )?;
    // Dropping the old table drops its indexes with it; they are recreated
    // against the renamed copy, which is why they live apart from the shape.
    tx.execute_batch(
        "DROP TABLE tool_calls;
         ALTER TABLE tool_calls_v24 RENAME TO tool_calls;",
    )?;
    tx.execute_batch(TOOL_CALLS_INDEXES)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    use super::super::apply_migration;
    use crate::tool_calls::ABANDONED;

    /// The v23 shape a fresh v18..=v23 file carries: the three-state CHECK
    /// that would reject the first `'abandoned'` write.
    const V23_TOOL_CALLS: &str = "CREATE TABLE tool_calls (
           execution_id INTEGER NOT NULL,
           seq INTEGER NOT NULL,
           call_id TEXT NOT NULL DEFAULT '',
           name TEXT NOT NULL,
           surface TEXT NOT NULL DEFAULT 'native',
           args_json TEXT NOT NULL DEFAULT '{}',
           args_digest TEXT NOT NULL DEFAULT '',
           reason TEXT NOT NULL DEFAULT '',
           ok INTEGER NOT NULL DEFAULT 1,
           state TEXT NOT NULL DEFAULT 'ok'
             CHECK(state IN ('running', 'ok', 'error')),
           error TEXT NOT NULL DEFAULT '',
           bytes_out INTEGER NOT NULL DEFAULT 0,
           duration_ms INTEGER NOT NULL DEFAULT 0,
           ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           UNIQUE (execution_id, seq)
         );";

    /// The rebuild admits the new state AND rewrites history: a legacy row
    /// spelling abandonment as `'error'` + the sentinel becomes
    /// `'abandoned'`, while a genuine error keeps its state. Fails on the
    /// old code twice over — the migration did not exist, and the v23 CHECK
    /// rejects `'abandoned'` outright.
    #[test]
    fn v24_backfills_the_sentinel_and_admits_the_new_state() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(V23_TOOL_CALLS).expect("v23 shape");
        conn.execute(
            "INSERT INTO tool_calls (execution_id, seq, name, ok, state, error) \
             VALUES (1, 0, 'bash', 0, 'error', ?1), \
                    (1, 1, 'bash', 0, 'error', 'boom'), \
                    (1, 2, 'grep', 1, 'ok', '')",
            params![ABANDONED],
        )
        .expect("legacy rows");
        conn.pragma_update(None, "user_version", 23).expect("stamp");

        apply_migration(&mut conn, super::migrate_v23_to_v24, 24).expect("migrate");

        let states: Vec<String> = conn
            .prepare("SELECT state FROM tool_calls WHERE execution_id = 1 ORDER BY seq")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(states, ["abandoned", "error", "ok"]);

        conn.execute(
            "INSERT INTO tool_calls (execution_id, seq, name, ok, state, error) \
             VALUES (2, 0, 'bash', 0, 'abandoned', ?1)",
            params![ABANDONED],
        )
        .expect("the rebuilt CHECK admits 'abandoned'");
    }
}
