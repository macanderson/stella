// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v31 → v32: `execution_reflection` grows `partial_run` — whether the turn
//! this row assesses was stopped rather than finished (#3808).
//!
//! A cancelled execution used to get the same reflection row as any other, and
//! it was always a stub: the cancel lands before the reflection model call, so
//! `critique` is empty and `self_rating` is NULL. Nothing said why. A reader —
//! the Improve tab, an export, the next session's miner — could not tell that
//! row from a turn the model looked at and declined to assess, which is the
//! reading that makes the runs most worth learning from look like the ones with
//! nothing to say.
//!
//! # `NOT NULL DEFAULT 0`, and backfilled
//!
//! Unlike [`task_contract`](super::task_contract)'s nullable column, the
//! historical fact here is **recorded**, one join away: `executions.outcome`
//! has always carried `'cancelled'`. So there is nothing to invent and nothing
//! to guess — the backfill below reads the answer rather than assuming one, and
//! every row it does not touch really did run to its own end.
//!
//! # What it does not claim
//!
//! Not that the row is worthless, and not that the work is unassessable. The
//! objective half — `produced_output`, `wrote_files`, `truncated` — is measured
//! from the event and file-touch logs and is exactly as true of a cancelled
//! turn as of any other: in the audited session (`ses-1787092335250-47513`,
//! execution 157) the run was stopped three minutes after a passing build,
//! having made 66 edits across 35 files. Those edits happened. This column
//! states the scope that judgement covers; it does not withhold it.
//!
//! `executions.usage_complete` is a different fact and is untouched here: it is
//! about the provider's usage envelope, which a cancel makes genuinely
//! unknowable. This is about the reflection.

use crate::Result;
use crate::migrations::{column_exists, table_exists};

/// Add `execution_reflection.partial_run` and backfill it from the outcome the
/// store already recorded. Column-guarded, so a file that already has it is
/// untouched.
pub(super) fn migrate_v31_to_v32(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !table_exists(tx, "execution_reflection")?
        || column_exists(tx, "execution_reflection", "partial_run")?
    {
        return Ok(());
    }
    tx.execute_batch(
        "ALTER TABLE execution_reflection \
         ADD COLUMN partial_run INTEGER NOT NULL DEFAULT 0;",
    )?;
    // Guarded because a file can carry the reflection table without the
    // executions table it hangs off — the legacy rebuild path builds them one
    // at a time — and an unguarded UPDATE would fail the whole ladder there.
    if table_exists(tx, "executions")? {
        tx.execute_batch(
            "UPDATE execution_reflection SET partial_run = 1 \
             WHERE execution_id IN (SELECT id FROM executions WHERE outcome = 'cancelled');",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    /// Seed a v31 file with one cancelled execution and one that finished,
    /// each carrying the stub reflection row the old code wrote.
    fn v31_file() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE executions (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               prompt TEXT NOT NULL,
               outcome TEXT
             );
             CREATE TABLE execution_reflection (
               execution_id INTEGER PRIMARY KEY,
               prompt TEXT NOT NULL DEFAULT '',
               self_rating INTEGER,
               critique TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO executions (id, prompt, outcome)
               VALUES (157, 'port the build', 'cancelled'), (158, 'and again', 'success');
             INSERT INTO execution_reflection (execution_id, prompt)
               VALUES (157, 'port the build'), (158, 'and again');",
        )
        .expect("seed a v31 file");
        conn
    }

    fn partial_run(conn: &Connection, execution_id: i64) -> i64 {
        conn.query_row(
            "SELECT partial_run FROM execution_reflection WHERE execution_id = ?1",
            [execution_id],
            |r| r.get(0),
        )
        .expect("read back")
    }

    /// The historical fact is recorded rather than guessed, so the backfill
    /// reads it: the stub rows a cancelled run already left behind become
    /// visibly stubs *of a stopped run*.
    #[test]
    fn the_backfill_marks_exactly_the_cancelled_runs() {
        let mut conn = v31_file();
        apply_migration(&mut conn, migrate_v31_to_v32, 32).expect("migrate");
        assert_eq!(partial_run(&conn, 157), 1);
        assert_eq!(
            partial_run(&conn, 158),
            0,
            "a run that reached its own end is not partial"
        );
    }

    /// Running it twice is a no-op — the column guard, not the error handler,
    /// is what makes that true.
    #[test]
    fn the_migration_is_idempotent() {
        let mut conn = v31_file();
        apply_migration(&mut conn, migrate_v31_to_v32, 32).expect("first");
        apply_migration(&mut conn, migrate_v31_to_v32, 32).expect("second");
        assert_eq!(partial_run(&conn, 157), 1);
    }

    /// A file with the reflection table and no executions table still migrates
    /// — the rebuild path can present exactly that shape, and a ladder step
    /// that failed there would strand the whole file.
    #[test]
    fn a_file_with_no_executions_table_still_grows_the_column() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE execution_reflection (execution_id INTEGER PRIMARY KEY);
             INSERT INTO execution_reflection (execution_id) VALUES (1);",
        )
        .expect("seed");
        apply_migration(&mut conn, migrate_v31_to_v32, 32).expect("migrate");
        assert_eq!(partial_run(&conn, 1), 0);
    }
}
