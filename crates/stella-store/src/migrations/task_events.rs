// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v38 → v39: `events` grows `task_id` — which board task an event is
//! evidence for (#5039) — and the partial index a task's ledger reads through.
//!
//! Nullable, no default, no backfill. `NULL` means the event is in no task's
//! ledger: nothing was running when it was dispatched, or the case carries no
//! tag at all (`stella_protocol::event::task_tag` names the six that do). That
//! is a fact about the row rather than a gap in it, and board ids start at
//! `"1"`, so no value could stand for "unknown" without colliding with a real
//! task. Nor is there anything to backfill *from*: every pre-v39 row was
//! written by a build whose events carried no tag, and reconstructing
//! attribution from `tasks.updated_at` windows would be a guess that reads as
//! evidence.
//!
//! The payload has carried the tag since the same change, so this could be
//! answered by decoding every row's JSON — which is why the column earns its
//! migration: "task 3's events" over a long session is a filter, and a `WHERE`
//! clause should be doing it.

use crate::Result;
use crate::migrations::{column_exists, table_exists};

/// The partial index a task's evidence ledger reads through. Named in one
/// place so [`migrate_v38_to_v39`] and `create_latest_schema` cannot disagree
/// about its shape — a fresh file and a migrated one must plan the same query
/// the same way.
///
/// Unlike the ADD COLUMN steps around it this one ships an index, because
/// `events` is the largest table the store keeps and a task's ledger read
/// without one is a full scan of a session's whole history — the shape that
/// makes a panel feel broken rather than slow. `(task_id, execution_id, seq)`
/// so the filter and the reader's ordering are one traversal, and partial
/// (`WHERE task_id IS NOT NULL`) because most rows carry no tag and an entry
/// for each of them would cost storage on every write to answer a query that
/// never asks for them.
pub(crate) const EVENTS_BY_TASK_INDEX: &str = "CREATE INDEX IF NOT EXISTS events_by_task \
     ON events (task_id, execution_id, seq) WHERE task_id IS NOT NULL;";

/// Add `events.task_id` and its partial index, both guarded so a file that
/// already has them is untouched.
pub(super) fn migrate_v38_to_v39(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !table_exists(tx, "events")? {
        return Ok(());
    }
    if !column_exists(tx, "events", "task_id")? {
        tx.execute_batch("ALTER TABLE events ADD COLUMN task_id TEXT;")?;
    }
    tx.execute_batch(EVENTS_BY_TASK_INDEX)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    fn v38_file() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE events (
               execution_id INTEGER NOT NULL,
               seq INTEGER NOT NULL,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               event_type TEXT NOT NULL,
               payload TEXT NOT NULL,
               UNIQUE (execution_id, seq)
             );
             INSERT INTO events (execution_id, seq, event_type, payload)
               VALUES (225, 7, 'tool_start', '{\"type\":\"tool_start\"}');",
        )
        .expect("seed a v38 file");
        conn
    }

    /// A row written before the column existed reads as belonging to no task,
    /// which is exactly what it is: the build that wrote it emitted no tags.
    #[test]
    fn an_existing_row_reads_as_belonging_to_no_task() {
        let mut conn = v38_file();
        apply_migration(&mut conn, migrate_v38_to_v39, 39).expect("migrate");
        let task: Option<String> = conn
            .query_row("SELECT task_id FROM events WHERE seq = 7", [], |r| r.get(0))
            .expect("read back");
        assert_eq!(task, None);
    }

    /// The index is what makes the column worth having; asserting it exists is
    /// the difference between a migration that adds a filter and one that adds
    /// a full scan.
    #[test]
    fn the_ledger_index_is_created() {
        let mut conn = v38_file();
        apply_migration(&mut conn, migrate_v38_to_v39, 39).expect("migrate");
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'events_by_task'",
                [],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(count, 1);
    }

    /// Running it twice is a no-op — the column guard and `IF NOT EXISTS`, not
    /// an error handler, are what make that true.
    #[test]
    fn the_migration_is_idempotent() {
        let mut conn = v38_file();
        apply_migration(&mut conn, migrate_v38_to_v39, 39).expect("first");
        apply_migration(&mut conn, migrate_v38_to_v39, 39).expect("second");
    }

    /// A file with no events table at all still climbs the ladder — the
    /// v0 → v1 rebuild path can present exactly that shape mid-flight.
    #[test]
    fn a_file_with_no_events_table_still_migrates() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v38_to_v39, 39).expect("migrate");
    }
}
