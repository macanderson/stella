// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v32 → v33: `telemetry` grows `sub_agent_id` — which delegate spent this
//! call, or the lead (#4383).
//!
//! A sub-agent opens no execution row of its own, and its `StepUsage` events
//! ride the parent's stream. So every delegate's model call was persisted under
//! the parent `execution_id` with `call_role = 'worker'` and nothing naming the
//! child — which makes the `(role, model)` census AGENTS.md tells a bench
//! reader to run unable to separate a turn's lead from its fan-out. In session
//! `ses-1787465453163-60967`, execution 225 carries ninety telemetry rows, all
//! `worker`; five of them complete within one second of each other at
//! ~1 950 input tokens apiece, which is a five-way parallel delegate fan-out
//! that the table cannot say is one.
//!
//! # Nullable, no default, no backfill
//!
//! `NULL` means **the lead's own call**, which is the overwhelming majority of
//! rows and is a fact rather than a gap — so unlike
//! [`partial_reflection`](super::partial_reflection) there is nothing here that
//! a default would have to invent, and every historical row reads correctly as
//! written.
//!
//! There is no backfill because there is nothing to backfill *from*. The
//! `sub_agent` bracket events are in `events`, but the engine dispatches
//! independent delegates concurrently, so several children's events interleave
//! and no `Started`/`Finished` pair encloses any particular call. That is the
//! same fact that made a per-event field necessary in the first place; a
//! backfill would be the guess the field exists to stop making.

use crate::Result;
use crate::migrations::{column_exists, table_exists};

/// Add `telemetry.sub_agent_id`, column-guarded so a file that already has it
/// is untouched.
pub(super) fn migrate_v32_to_v33(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if table_exists(tx, "telemetry")? && !column_exists(tx, "telemetry", "sub_agent_id")? {
        tx.execute_batch("ALTER TABLE telemetry ADD COLUMN sub_agent_id TEXT;")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    fn v32_file() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE telemetry (
               execution_id INTEGER NOT NULL,
               step INTEGER NOT NULL,
               provider TEXT NOT NULL,
               call_role TEXT NOT NULL DEFAULT 'unknown',
               model TEXT NOT NULL,
               cost_usd REAL NOT NULL
             );
             INSERT INTO telemetry (execution_id, step, provider, call_role, model, cost_usd)
               VALUES (225, 7, 'openrouter', 'worker', 'moonshotai/kimi-k3', 0.02);",
        )
        .expect("seed a v32 file");
        conn
    }

    /// Every row written before the column existed reads as the lead's own
    /// call, which is what the overwhelming majority of them were — and the
    /// only reading the store can make without guessing.
    #[test]
    fn an_existing_row_reads_as_the_leads_own_call() {
        let mut conn = v32_file();
        apply_migration(&mut conn, migrate_v32_to_v33, 33).expect("migrate");
        let owner: Option<String> = conn
            .query_row(
                "SELECT sub_agent_id FROM telemetry WHERE step = 7",
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
        let mut conn = v32_file();
        apply_migration(&mut conn, migrate_v32_to_v33, 33).expect("first");
        apply_migration(&mut conn, migrate_v32_to_v33, 33).expect("second");
    }

    /// A file with no telemetry table at all still climbs the ladder — the
    /// rebuild path can present exactly that shape.
    #[test]
    fn a_file_with_no_telemetry_table_still_migrates() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v32_to_v33, 33).expect("migrate");
    }
}
