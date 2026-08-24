// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v35 → v36: `executions` grows `parent_execution_id` — which turn dispatched
//! this one (#4628).
//!
//! A deck worker lane (`req:<n>` / `sub:<task-id>`) opens a real execution row
//! of its own, unlike a `delegate` child. Its parentage existed only in the
//! deck's in-memory `AgentMeta::with_parent`, which never reached the store, so
//! a lane was indistinguishable from a lead turn in every session-scoped query
//! and no turn-scoped query for its fan-out could exist at all.
//!
//! # Nullable, no default, no backfill
//!
//! `NULL` means **nobody's turn dispatched this**, and it is the ordinary
//! answer rather than a gap: a lead turn has no dispatcher, and neither does a
//! lane a person started from the composer between turns, which is most of
//! them. Forcing a parent would invent one for both. Distinguishing "the user
//! asked for this" from "a turn asked for this" is the whole point of the
//! column; a default would collapse it.
//!
//! There is no backfill because nothing recorded the fact. `executions` holds
//! `session_id` and `kind`, which together say *this lane belongs to that
//! session* and never *that turn asked for it* — a session runs many turns, and
//! picking whichever one was open would be a guess dressed as a record.

use crate::Result;
use crate::migrations::{column_exists, table_exists};

/// Add `executions.parent_execution_id`, column-guarded so a file that already
/// has it is untouched.
pub(super) fn migrate_v35_to_v36(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if table_exists(tx, "executions")? && !column_exists(tx, "executions", "parent_execution_id")? {
        tx.execute_batch("ALTER TABLE executions ADD COLUMN parent_execution_id INTEGER;")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    fn v35_file() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE executions (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               kind TEXT NOT NULL,
               prompt TEXT NOT NULL,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               session_id TEXT
             );
             INSERT INTO executions (kind, prompt, provider, model, session_id)
               VALUES ('deck-sub', 'fix the router', 'zai', 'glm-5.2', 'ses-1');",
        )
        .expect("seed a v35 file");
        conn
    }

    /// Every row written before the column existed reads as undispatched,
    /// which is the only reading the store can make: nothing recorded which
    /// turn asked, and `session_id` cannot answer it.
    #[test]
    fn an_existing_lane_reads_as_dispatched_by_nobody() {
        let mut conn = v35_file();
        apply_migration(&mut conn, migrate_v35_to_v36, 36).expect("migrate");
        let parent: Option<i64> = conn
            .query_row(
                "SELECT parent_execution_id FROM executions WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(parent, None);
    }

    /// Running it twice is a no-op — the column guard, not the error handler,
    /// is what makes that true.
    #[test]
    fn the_migration_is_idempotent() {
        let mut conn = v35_file();
        apply_migration(&mut conn, migrate_v35_to_v36, 36).expect("first");
        apply_migration(&mut conn, migrate_v35_to_v36, 36).expect("second");
    }

    /// A file with no `executions` table at all still climbs the ladder — the
    /// rebuild path can present exactly that shape.
    #[test]
    fn a_file_with_no_executions_table_still_migrates() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v35_to_v36, 36).expect("migrate");
    }
}
