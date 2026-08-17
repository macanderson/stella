// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The v16 → v17 schema migration: the first and so far only *pure removal*
//! in the ladder.
//!
//! It has its own module rather than joining a group because it is the one
//! step whose test has to prove a negative on two axes at once — that the
//! dead schema is gone and that the live rows underneath it are not — and
//! because a second removal step, when one comes, belongs beside it.

use crate::Result;

/// v16 → v17: the first pure removal in the chain. `graph_nodes`/`graph_edges`
/// were a v0-era seam reserved for a context plane that shipped its own stores
/// instead (`stella-context`, `stella-graph`) — no shipping code ever wrote or
/// read them, so every deployed pair is empty and dropping loses nothing.
/// `agent_uses_by_agent` and `reflections_by_kind` indexed access paths no
/// reader takes (both tables are only ever walked whole — the JSON export and
/// the observatory), so each was pure write-amplification on its table's
/// insert path. `IF EXISTS` mirrors the house `IF NOT EXISTS` tolerance:
/// partial legacy files may hold any subset of these.
pub(super) fn migrate_v16_to_v17(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "DROP TABLE IF EXISTS graph_nodes;
         DROP TABLE IF EXISTS graph_edges;
         DROP INDEX IF EXISTS agent_uses_by_agent;
         DROP INDEX IF EXISTS reflections_by_kind;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::{apply_migration, table_exists};
    use rusqlite::{Connection, params};

    #[test]
    fn v17_migration_drops_the_unused_schema_and_keeps_the_tables_that_carry_data() {
        // A v16 file holds the reserved graph pair and both dead indexes; the
        // indexed tables carry real rows that must survive their index.
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE graph_nodes (id TEXT PRIMARY KEY, label TEXT NOT NULL,
               properties TEXT NOT NULL DEFAULT '{}');
             CREATE TABLE graph_edges (src TEXT NOT NULL, dst TEXT NOT NULL,
               edge_type TEXT NOT NULL, properties TEXT NOT NULL DEFAULT '{}');
             CREATE TABLE agent_uses (execution_id INTEGER NOT NULL, agent TEXT NOT NULL,
               version INTEGER NOT NULL, reason TEXT NOT NULL DEFAULT '',
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE INDEX agent_uses_by_agent ON agent_uses(agent, version, execution_id);
             CREATE TABLE reflections (id INTEGER PRIMARY KEY AUTOINCREMENT,
               execution_id INTEGER, kind TEXT NOT NULL, content TEXT NOT NULL,
               domains TEXT NOT NULL DEFAULT '[]', occurred_at INTEGER NOT NULL);
             CREATE INDEX reflections_by_kind ON reflections(kind);
             INSERT INTO agent_uses (execution_id, agent, version) VALUES (1, 'reviewer', 2);
             INSERT INTO reflections (kind, content, occurred_at) VALUES ('lesson', 'x', 0);",
        )
        .expect("v16 schema");

        apply_migration(&mut conn, migrate_v16_to_v17, 17).expect("migrate");

        for table in ["graph_nodes", "graph_edges"] {
            assert!(!table_exists(&conn, table).unwrap(), "{table} must be gone");
        }
        for index in ["agent_uses_by_agent", "reflections_by_kind"] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
                    params![index],
                    |r| r.get(0),
                )
                .expect("probe");
            assert_eq!(n, 0, "{index} must be gone");
        }
        // The indexed tables keep their rows — only the access path is gone.
        let uses: i64 = conn
            .query_row("SELECT count(*) FROM agent_uses", [], |r| r.get(0))
            .expect("agent_uses survives");
        let lessons: i64 = conn
            .query_row("SELECT count(*) FROM reflections", [], |r| r.get(0))
            .expect("reflections survives");
        assert_eq!((uses, lessons), (1, 1));

        // Idempotent on a partial file already past the removal.
        apply_migration(&mut conn, migrate_v16_to_v17, 17).expect("idempotent");
    }
}
