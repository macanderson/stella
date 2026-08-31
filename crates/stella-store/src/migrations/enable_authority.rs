// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! v41 → v42: `foundry_tools` learns how each tool got turned on.

use crate::Result;
use crate::migrations::{column_exists, table_exists};

/// v41 → v42: `foundry_tools` grows `enabled_authority`. The column names
/// how a tool got turned on: a typed yes, a `--yes` flag, the autonomy
/// loop, or a rollback. The tags live in `crate::foundry::EnableAuthority`.
///
/// Nothing is backfilled. No old row wrote this fact, so `''` reads as
/// unknown for all of them. That holds even for a row that is on. An old
/// grant must not turn into a claim about how it was made.
pub(super) fn migrate_v41_to_v42(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    // Column-guarded, like every ALTER in this chain: a legacy file that
    // walked the ladder got today's `foundry_tools` shape at v19 → v20 (the
    // DDL constants describe the current schema), so the column may already
    // be there.
    if table_exists(tx, "foundry_tools")?
        && !column_exists(tx, "foundry_tools", "enabled_authority")?
    {
        tx.execute_batch(
            "ALTER TABLE foundry_tools ADD COLUMN enabled_authority TEXT NOT NULL DEFAULT '';",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    /// A v41 file: the old table shape, with one tool a person had
    /// already turned on.
    fn v41_file() -> Connection {
        let conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE foundry_tools (
               name TEXT PRIMARY KEY,
               signature TEXT NOT NULL DEFAULT '',
               manifest_digest TEXT NOT NULL,
               script_digest TEXT NOT NULL,
               witness TEXT NOT NULL DEFAULT '',
               witness_input TEXT NOT NULL DEFAULT '{}',
               witness_expect TEXT NOT NULL DEFAULT '',
               enabled INTEGER NOT NULL DEFAULT 0,
               adopted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               enabled_at TEXT,
               disabled_reason TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO foundry_tools (name, manifest_digest, script_digest, enabled, enabled_at)
               VALUES ('cat_file', 'm0', 's0', 1, CURRENT_TIMESTAMP);",
        )
        .expect("seed a v41 file");
        conn
    }

    /// **Witness.** A row turned on before the column existed reads as
    /// unknown — not as a typed yes, not as `--yes`. Nothing wrote the
    /// fact, so nothing may claim it.
    #[test]
    fn a_pre_existing_enablement_reads_as_unknown() {
        let mut conn = v41_file();
        apply_migration(&mut conn, migrate_v41_to_v42, 42).expect("migrate");
        let authority: String = conn
            .query_row(
                "SELECT enabled_authority FROM foundry_tools WHERE name = 'cat_file'",
                [],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(authority, "", "unknown, never a back-dated grant");
    }

    /// Running it twice is a no-op. The column guard makes that true,
    /// not the error handler.
    #[test]
    fn the_migration_is_idempotent() {
        let mut conn = v41_file();
        apply_migration(&mut conn, migrate_v41_to_v42, 42).expect("first");
        apply_migration(&mut conn, migrate_v41_to_v42, 42).expect("second");
    }

    /// A file with no `foundry_tools` table still climbs the ladder. The
    /// rebuild path can present that shape.
    #[test]
    fn a_file_with_no_foundry_table_still_migrates() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v41_to_v42, 42).expect("migrate");
    }
}
