// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v30 → v31: `agent_uses` grows `kind` — which of the log's two writers
//! minted the row's `agent` name (#3822).
//!
//! `agent_uses` had one writer for its whole life: the deck, recording an
//! installed agent definition by name (`reviewer`) at its pinned version.
//! #3821 gave it a second — the `task` tool, recording one row per delegation
//! under the child id the model's own description was slugged into
//! (`find-retry-policy-2`). Both are correct keys for their own writer and
//! they are not the same kind of name: the first repeats across sessions and
//! is worth counting, the second is unique per delegation by construction. In
//! one column with no discriminator, an agent invoked eight times and eight
//! separate delegations are indistinguishable from each other.
//!
//! # Why a column and not a convention
//!
//! The alternative was to keep the schema and have the Observatory group
//! delegations by matching their name shape. That makes a reader's answer
//! depend on what the model happened to type, which is the shape this
//! repository treats as a defect: a fact that a writer knows must be recorded
//! by the writer, not re-derived downstream from a spelling.
//!
//! # Why the default is `'definition'` and not NULL
//!
//! Unlike `tasks.contract` (v28 → v29), the fact here IS recoverable, so a
//! default is a record rather than an invention: the delegation writer landed
//! in #3821, after every row this migration can see was written, so every
//! pre-v31 row is a definition invocation. `NOT NULL DEFAULT 'definition'`
//! states that; a nullable column would make every historical row read as
//! *unknown* when it is known.

use crate::Result;
use crate::migrations::{column_exists, table_exists};

/// Add `agent_uses.kind`, column-guarded so a file that already has it is
/// untouched.
///
/// The `CHECK` rides the `ADD COLUMN` (SQLite allows one on a new column) so
/// the migrated shape matches [`AGENT_USES_DDL`](crate::ddl::AGENT_USES_DDL)
/// exactly rather than being a laxer twin of it. It constrains new writes
/// only — SQLite does not re-check rows already at rest — which is the whole
/// requirement here, since every one of them is being defaulted to a value the
/// check accepts.
pub(super) fn migrate_v30_to_v31(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if table_exists(tx, "agent_uses")? && !column_exists(tx, "agent_uses", "kind")? {
        tx.execute_batch(
            "ALTER TABLE agent_uses ADD COLUMN kind TEXT NOT NULL DEFAULT 'definition' \
             CHECK (kind IN ('definition', 'delegation'));",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    fn seed_v30(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE agent_uses (
               execution_id INTEGER NOT NULL,
               agent TEXT NOT NULL,
               version INTEGER NOT NULL,
               reason TEXT NOT NULL DEFAULT '',
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );
             INSERT INTO agent_uses (execution_id, agent, version, reason)
               VALUES (1, 'reviewer', 2, 'review the diff');",
        )
        .expect("seed a v30 file");
    }

    /// Every row written before the delegation writer existed is a definition
    /// invocation, and reads as one.
    #[test]
    fn v31_migration_reads_existing_rows_as_definition_invocations() {
        let mut conn = Connection::open_in_memory().expect("db");
        seed_v30(&conn);

        apply_migration(&mut conn, migrate_v30_to_v31, 31).expect("migrate");

        let kind: String = conn
            .query_row(
                "SELECT kind FROM agent_uses WHERE agent = 'reviewer'",
                [],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(kind, "definition");
    }

    /// The `CHECK` constrains the writes that come after the migration, which
    /// is what keeps the migrated shape and the fresh-file DDL one shape.
    #[test]
    fn the_migrated_column_rejects_a_kind_outside_the_two() {
        let mut conn = Connection::open_in_memory().expect("db");
        seed_v30(&conn);
        apply_migration(&mut conn, migrate_v30_to_v31, 31).expect("migrate");

        let rejected = conn.execute(
            "INSERT INTO agent_uses (execution_id, agent, version, kind) \
             VALUES (1, 'x', 1, 'whatever')",
            [],
        );
        assert!(rejected.is_err(), "an unknown kind must not be storable");
        conn.execute(
            "INSERT INTO agent_uses (execution_id, agent, version, kind) \
             VALUES (1, 'find-retry-policy', 1, 'delegation')",
            [],
        )
        .expect("a delegation is storable");
    }

    /// Running it twice is a no-op — the guard, not the error handler, is what
    /// makes that true.
    #[test]
    fn the_migration_is_idempotent() {
        let mut conn = Connection::open_in_memory().expect("db");
        seed_v30(&conn);
        apply_migration(&mut conn, migrate_v30_to_v31, 31).expect("first");
        apply_migration(&mut conn, migrate_v30_to_v31, 31).expect("second");
    }
}
