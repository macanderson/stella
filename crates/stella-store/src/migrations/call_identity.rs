// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The v27 → v28 schema migration: a `tool_calls` row stops being identified
//! by its `call_id` and starts being identified by the event that announced it
//! (#4033).
//!
//! It sits beside the three earlier steps that reshaped the same table —
//! [`super::live_tool_calls`] (v17 → v18), [`super::abandoned_state`]
//! (v23 → v24) and [`super::error_class`] (v24 → v25) — and is the one that
//! undoes a decision the first of them made: v17 → v18 *deleted* the rows
//! whose `call_id` collided so its new UNIQUE index would build. That delete
//! is where the loss was first baked in, and every fold since reproduced it.

use super::column_exists;
use crate::{Result, tool_calls::refold_tool_calls};

/// v27 → v28: `tool_calls` grows `event_seq`, loses the uniqueness assumption
/// on `call_id`, and every history the old key collapsed is re-folded from the
/// event log.
///
/// Three steps, in this order, and the order is load-bearing:
///
/// 1. **Add the column.** Additive, column-guarded `ADD COLUMN`, defaulting to
///    `-1` = the announcing event is unknown — which is exactly what every
///    pre-v28 row is, because nothing recorded it.
/// 2. **Replace the indexes.** The old `tool_calls_by_call_id` is UNIQUE over
///    `(execution_id, call_id)`; the re-fold in step 3 inserts precisely the
///    rows that constraint forbids, so it must be gone before the re-fold
///    runs, not after. It is recreated non-unique, because a `tool_result`
///    still finds its row by `call_id` and that lookup wants an index.
/// 3. **Re-fold every execution that has tool events.** This is the recovery:
///    the erased calls were never lost, only never projected —
///    `events` holds every `tool_start` verbatim, which is the entire reason
///    the log is the source of truth and this table is not. On the workspace
///    that produced the bug report, 595 rows come back.
///
/// The re-fold carries each call's announcing `ts` from its event rather than
/// stamping the clock, so a store upgraded this morning does not re-date its
/// whole history to this morning and flatten every per-day rollup with it.
///
/// Cost is one pass over the tool events of every execution that has any. It
/// is bounded by the log the store already holds and happens once; the
/// alternative — leaving the histories folded and fixing only new turns —
/// would mean every number computed over a past turn stays wrong forever, with
/// nothing to mark which ones.
pub(super) fn migrate_v27_to_v28(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "tool_calls", "event_seq")? {
        tx.execute_batch(
            "ALTER TABLE tool_calls ADD COLUMN event_seq INTEGER NOT NULL DEFAULT -1;",
        )?;
    }
    tx.execute_batch(
        "DROP INDEX IF EXISTS tool_calls_by_call_id;
         CREATE INDEX IF NOT EXISTS tool_calls_by_call_id
           ON tool_calls(execution_id, call_id);
         CREATE UNIQUE INDEX IF NOT EXISTS tool_calls_by_event_seq
           ON tool_calls(execution_id, event_seq) WHERE event_seq >= 0;",
    )?;

    let executions: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT execution_id FROM events \
             WHERE event_type IN ('tool_start', 'tool_result') \
             ORDER BY execution_id ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        out
    };
    for execution_id in executions {
        refold_tool_calls(tx, execution_id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    /// The migration recovers the calls the `call_id` key erased — the exact
    /// shape of the bug report, at the smallest size that shows it: one
    /// execution, two steps, each announcing `read_file:0` with different
    /// arguments, folded by a pre-v28 store into one row.
    #[test]
    fn refolds_a_collapsed_history() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE events (
               execution_id INTEGER NOT NULL,
               seq INTEGER NOT NULL,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               event_type TEXT NOT NULL,
               payload TEXT NOT NULL,
               UNIQUE (execution_id, seq)
             );
             CREATE TABLE tool_calls (
               execution_id INTEGER NOT NULL,
               seq INTEGER NOT NULL,
               call_id TEXT NOT NULL DEFAULT '',
               name TEXT NOT NULL,
               surface TEXT NOT NULL DEFAULT 'native',
               args_json TEXT NOT NULL DEFAULT '{}',
               args_digest TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL DEFAULT '',
               ok INTEGER NOT NULL DEFAULT 1,
               state TEXT NOT NULL DEFAULT 'ok',
               error TEXT NOT NULL DEFAULT '',
               error_class TEXT NOT NULL DEFAULT '',
               bytes_out INTEGER NOT NULL DEFAULT 0,
               duration_ms INTEGER NOT NULL DEFAULT 0,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               UNIQUE (execution_id, seq)
             );
             CREATE UNIQUE INDEX tool_calls_by_call_id
               ON tool_calls(execution_id, call_id) WHERE call_id != '';",
        )
        .expect("legacy schema");

        let start = |offset: u32| {
            format!(
                r#"{{"type":"tool_start","call":{{"call_id":"read_file:0",
                   "name":"read_file","input":{{"offset":{offset}}}}}}}"#
            )
        };
        let result = r#"{"type":"tool_result","call_id":"read_file:0",
             "output":{"type":"ok","content":"window"},"duration_ms":5}"#;
        for (seq, (kind, payload)) in [
            ("tool_start", start(1)),
            ("tool_result", result.to_string()),
            ("tool_start", start(41)),
            ("tool_result", result.to_string()),
        ]
        .into_iter()
        .enumerate()
        {
            conn.execute(
                "INSERT INTO events (execution_id, seq, ts, event_type, payload) \
                 VALUES (1, ?1, '2026-01-01 00:00:00', ?2, ?3)",
                rusqlite::params![seq as i64, kind, payload],
            )
            .expect("event");
        }
        // What a pre-v28 store projected from that stream: one row.
        conn.execute(
            "INSERT INTO tool_calls (execution_id, seq, call_id, name, state) \
             VALUES (1, 0, 'read_file:0', 'read_file', 'ok')",
            [],
        )
        .expect("collapsed row");

        let tx = conn.transaction().expect("tx");
        super::migrate_v27_to_v28(&tx).expect("migrate");
        let offsets: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT args_json FROM tool_calls WHERE execution_id = 1 ORDER BY seq")
                .expect("prepare");
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .expect("query");
            rows.map(|r| r.expect("row")).collect()
        };
        assert_eq!(
            offsets.len(),
            2,
            "two announcements are two calls, not one re-announcement: {offsets:?}"
        );
        assert!(offsets[0].contains("\"offset\":1"), "{offsets:?}");
        assert!(offsets[1].contains("\"offset\":41"), "{offsets:?}");

        // The recovered rows keep the day they ran, not the day of the upgrade.
        let ts: String = tx
            .query_row(
                "SELECT ts FROM tool_calls WHERE execution_id = 1 AND seq = 1",
                [],
                |row| row.get(0),
            )
            .expect("ts");
        assert_eq!(ts, "2026-01-01 00:00:00");
    }
}
