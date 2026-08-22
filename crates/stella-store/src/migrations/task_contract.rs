// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v28 → v29: `tasks` grows `contract` — what a task promised, not only what
//! became of it (#4238).
//!
//! The board is an audit surface: a cancelled task keeps its row precisely so
//! a reader can go back and ask what happened. Until this column, a recorded
//! board could answer *what became of a task* and not *what it undertook to
//! prove* — which is the half that matters when the question is whether a
//! closed task deserved to close.
//!
//! Nothing about the live guarantee changes, and that is worth being precise
//! about rather than overclaiming: `stella_core::tasks::TaskBoard` holds the
//! session's real board in memory and never rehydrates from SQLite, so
//! `set_status`'s refusal has always read the real contract. This is the audit
//! record catching up with the type.
//!
//! # NULL is a fact, not a gap
//!
//! Nullable, with no default and no backfill. `NULL` means *this row records no
//! contract*, and it is deliberately **not** the same fact as the stored
//! `{"kind":"read_only"}`, which means *someone looked and there was nothing to
//! prove*. A `NOT NULL DEFAULT` here would have to invent one of those two for
//! every row written before contracts existed, and inventing the second is
//! exactly the self-report `TaskContract` was built to end.
//!
//! There is no backfill for the same reason: what a pre-contract task promised
//! is not recorded anywhere, so it cannot be recovered, only guessed.

use crate::Result;
use crate::migrations::{column_exists, table_exists};

/// Add `tasks.contract`, column-guarded so a file that already has it is
/// untouched.
pub(super) fn migrate_v28_to_v29(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if table_exists(tx, "tasks")? && !column_exists(tx, "tasks", "contract")? {
        tx.execute_batch("ALTER TABLE tasks ADD COLUMN contract TEXT;")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    /// A v28 file grows the column, and every row already in it reads as
    /// *no contract recorded* — never as `read_only`, which would be inventing
    /// a declaration nobody made.
    #[test]
    fn v29_migration_leaves_existing_rows_with_no_contract() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE tasks (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               execution_id INTEGER NOT NULL,
               session_id TEXT,
               task_id TEXT NOT NULL,
               subject TEXT NOT NULL,
               description TEXT,
               status TEXT NOT NULL,
               owner TEXT,
               updated_at INTEGER NOT NULL,
               UNIQUE(session_id, task_id)
             );
             INSERT INTO tasks
               (execution_id, session_id, task_id, subject, status, updated_at)
             VALUES (1, 'ses', '1', 'a task from before contracts', 'completed', 0);",
        )
        .expect("seed a v28 file");

        apply_migration(&mut conn, migrate_v28_to_v29, 29).expect("migrate");

        let contract: Option<String> = conn
            .query_row("SELECT contract FROM tasks WHERE task_id = '1'", [], |r| {
                r.get(0)
            })
            .expect("read back");
        assert_eq!(
            contract, None,
            "a pre-contract row must read as unrecorded, not as a declaration"
        );
    }

    /// Running it twice is a no-op — the guard, not the error handler, is what
    /// makes that true.
    #[test]
    fn the_migration_is_idempotent() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE tasks (
               task_id TEXT NOT NULL, subject TEXT NOT NULL,
               status TEXT NOT NULL, updated_at INTEGER NOT NULL
             );",
        )
        .expect("seed");
        apply_migration(&mut conn, migrate_v28_to_v29, 29).expect("first");
        apply_migration(&mut conn, migrate_v28_to_v29, 29).expect("second");
    }
}
