// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v34 → v35: `tool_calls` grows `sub_agent_id` — which delegate ran this
//! call, or the lead (#4624).
//!
//! The sibling of [`sub_agent_calls`](super::sub_agent_calls) one table over,
//! and for the same reason. A delegate opens no execution row of its own and
//! its `ToolStart`/`ToolResult` events are forwarded onto the parent's stream,
//! so every child's tool traffic was projected under the parent `execution_id`
//! with nothing naming the child. The table that answers *what did this turn
//! actually do* could not answer *which of these did child X do*, and the
//! turn page's per-child view had aggregate spend and no activity under it.
//!
//! # Nullable, no default, no backfill
//!
//! `NULL` means **the lead's own call**, the overwhelming majority of rows and
//! a fact rather than a gap — the same contract `telemetry.sub_agent_id`
//! carries, deliberately, so the two tables read the same way.
//!
//! There is no backfill because there is nothing to backfill *from*. The
//! `sub_agent` bracket events are in `events`, but the engine dispatches
//! independent delegates concurrently, so several children's events interleave
//! and no `Started`/`Finished` pair encloses any particular call. That is the
//! same fact that made a per-event field necessary; a backfill would be the
//! guess the field exists to stop making.

use crate::Result;
use crate::migrations::{column_exists, table_exists};

/// Add `tool_calls.sub_agent_id`, column-guarded so a file that already has it
/// is untouched.
pub(super) fn migrate_v34_to_v35(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if table_exists(tx, "tool_calls")? && !column_exists(tx, "tool_calls", "sub_agent_id")? {
        tx.execute_batch("ALTER TABLE tool_calls ADD COLUMN sub_agent_id TEXT;")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    fn v34_file() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE tool_calls (
               execution_id INTEGER NOT NULL,
               seq INTEGER NOT NULL,
               call_id TEXT NOT NULL DEFAULT '',
               name TEXT NOT NULL,
               state TEXT NOT NULL DEFAULT 'ok'
             );
             INSERT INTO tool_calls (execution_id, seq, call_id, name)
               VALUES (225, 7, 'call_9', 'read_file');",
        )
        .expect("seed a v34 file");
        conn
    }

    /// Every row written before the column existed reads as the lead's own
    /// call — the only reading the store can make without guessing, and the
    /// one the overwhelming majority of them deserve.
    #[test]
    fn an_existing_row_reads_as_the_leads_own_call() {
        let mut conn = v34_file();
        apply_migration(&mut conn, migrate_v34_to_v35, 35).expect("migrate");
        let owner: Option<String> = conn
            .query_row(
                "SELECT sub_agent_id FROM tool_calls WHERE seq = 7",
                [],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(owner, None);
    }

    /// Running it twice is a no-op — the column guard, not the error handler,
    /// is what makes that true.
    #[test]
    fn the_migration_is_idempotent() {
        let mut conn = v34_file();
        apply_migration(&mut conn, migrate_v34_to_v35, 35).expect("first");
        apply_migration(&mut conn, migrate_v34_to_v35, 35).expect("second");
    }

    /// A file with no `tool_calls` table at all still climbs the ladder — the
    /// rebuild path can present exactly that shape.
    #[test]
    fn a_file_with_no_tool_calls_table_still_migrates() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v34_to_v35, 35).expect("migrate");
    }
}
