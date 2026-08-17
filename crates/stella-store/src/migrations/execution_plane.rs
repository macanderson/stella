// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The steps that reshape the `executions` row itself and the per-execution
//! facts hanging off it: the session link (v7 → v8), the paid-call accounting
//! that became an explicit lifecycle (v8 → v9, v9 → v10), the journal-era
//! stamp (v21 → v22), and the reflection parse-failure record (v22 → v23).
//!
//! They are grouped because each one's real content is a *backfill decision*
//! about rows already at rest — what an old execution's absent value means —
//! and those decisions read as a set. All of them are column-guarded
//! `ADD COLUMN`s, so none needs a §7 rebuild.

use super::column_exists;
use crate::Result;
use crate::ddl::{PULL_REQUESTS_DDL, TASKS_DDL};

/// v7 → v8: the session plane. `executions` grows the nullable `session_id`
/// column linking each per-turn row to the cross-process session registry id
/// (a plain ADD COLUMN — nullable, no default rewrite, so no §7 rebuild),
/// plus the `executions_by_session` index and the additive `tasks` and
/// `pull_requests` tables. The ALTER is guarded by a column probe, matching
/// the house `IF NOT EXISTS` tolerance for partial files that somehow
/// already grew the shape.
pub(super) fn migrate_v7_to_v8(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "executions", "session_id")? {
        tx.execute_batch("ALTER TABLE executions ADD COLUMN session_id TEXT;")?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS executions_by_session
           ON executions(session_id, id);",
    )?;
    tx.execute_batch(TASKS_DDL)?;
    tx.execute_batch(PULL_REQUESTS_DDL)?;
    Ok(())
}

/// v8 → v9: every legacy execution and telemetry row fails closed. The v10
/// lifecycle migration supersedes v9's former writer-side optimistic start.
pub(super) fn migrate_v8_to_v9(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "executions", "usage_complete")? {
        tx.execute_batch(
            "ALTER TABLE executions ADD COLUMN usage_complete INTEGER NOT NULL DEFAULT 0
               CHECK(usage_complete IN (0, 1));",
        )?;
    }
    if !column_exists(tx, "telemetry", "call_role")? {
        tx.execute_batch(
            "ALTER TABLE telemetry ADD COLUMN call_role TEXT NOT NULL DEFAULT 'unknown';",
        )?;
    }
    if !column_exists(tx, "telemetry", "usage_complete")? {
        tx.execute_batch(
            "ALTER TABLE telemetry ADD COLUMN usage_complete INTEGER NOT NULL DEFAULT 0
               CHECK(usage_complete IN (0, 1));",
        )?;
    }
    Ok(())
}

/// v9 → v10: replace the optimistic execution bit with an explicit lifecycle.
/// Historic finalized rows retain `complete` only when their v9 bit was true;
/// unfinished rows become pending and every other row remains fail-closed.
pub(super) fn migrate_v9_to_v10(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "executions", "usage_status")? {
        tx.execute_batch(
            "ALTER TABLE executions ADD COLUMN usage_status TEXT NOT NULL DEFAULT 'incomplete'
               CHECK(usage_status IN ('pending', 'complete', 'incomplete'));
             UPDATE executions
                SET usage_status = CASE
                    WHEN finished_at IS NULL THEN 'pending'
                    WHEN usage_complete = 1 THEN 'complete'
                    ELSE 'incomplete'
                END,
                    usage_complete = CASE
                    WHEN finished_at IS NOT NULL AND usage_complete = 1 THEN 1
                    ELSE 0
                END;",
        )?;
    }
    Ok(())
}

/// v21 → v22: `executions` grows `journal_era` (#1981) — the writer's own
/// statement of which compaction-journaling era produced this row's events.
///
/// The backfill is the column default and needs no `UPDATE`: a row already at
/// rest was written by a build that had no era to state, and for all but the
/// few days between PR #1979 landing and this stamp that build genuinely could
/// not journal a rewrite. Rows from inside that window are the one place era 0
/// is conservative rather than exact — and conservative in the only safe
/// direction, since it can under-report an integrity signal but never invent
/// one. Backfilling them the other way would need the very inference this
/// column exists to avoid: a reader looking at an old execution's events
/// cannot tell "this build journaled no rewrites" from "this run rewrote
/// nothing", and reading it wrong raises a false alarm on routine
/// housekeeping.
///
/// Plain additive ADD COLUMN with a NOT NULL default, so no §7 rebuild;
/// column-guarded for a file whose `executions` table was created at the v22
/// shape by this build's [`EXECUTIONS_DDL`](crate::ddl::EXECUTIONS_DDL).
pub(super) fn migrate_v21_to_v22(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "executions", "journal_era")? {
        tx.execute_batch(
            "ALTER TABLE executions ADD COLUMN journal_era INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

/// v22 → v23: `execution_reflection` grows the nullable `parse_error` column
/// (#2175) — the excerpt of a reflection response whose lessons array could
/// not be read.
///
/// A reflection whose *model call* succeeded and whose *parse* failed closed
/// its execution row as `outcome = 'completed'`, wrote no `reflections` rows,
/// and said so only in an `eprintln!`. Once the scrollback was gone, a starved
/// learning loop and a workspace that has simply never learned anything were
/// the same picture: both render the Improve tab's "no proposals in the
/// ledger" empty state. This column is the smallest durable difference between
/// them.
pub(super) fn migrate_v22_to_v23(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "execution_reflection", "parse_error")? {
        tx.execute_batch("ALTER TABLE execution_reflection ADD COLUMN parse_error TEXT;")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    #[test]
    fn v9_migration_fails_legacy_usage_closed() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE executions (id INTEGER PRIMARY KEY);
             INSERT INTO executions (id) VALUES (1);
             CREATE TABLE telemetry (execution_id INTEGER, step INTEGER);
             INSERT INTO telemetry (execution_id, step) VALUES (1, 0);",
        )
        .expect("legacy schema");

        apply_migration(&mut conn, migrate_v8_to_v9, 9).expect("migrate");

        let execution_complete: bool = conn
            .query_row(
                "SELECT usage_complete FROM executions WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .expect("execution bit");
        let (role, call_complete): (String, bool) = conn
            .query_row(
                "SELECT call_role, usage_complete FROM telemetry WHERE execution_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("telemetry defaults");
        assert!(!execution_complete);
        assert_eq!(role, "unknown");
        assert!(!call_complete);
    }

    #[test]
    fn v10_migration_derives_lifecycle_without_exporting_unfinished_rows() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE executions (
               id INTEGER PRIMARY KEY,
               finished_at TEXT,
               usage_complete INTEGER NOT NULL
             );
             INSERT INTO executions VALUES (1, NULL, 1);
             INSERT INTO executions VALUES (2, '2026-07-21', 1);
             INSERT INTO executions VALUES (3, '2026-07-21', 0);",
        )
        .expect("v9 schema");

        apply_migration(&mut conn, migrate_v9_to_v10, 10).expect("migrate");

        let mut stmt = conn
            .prepare("SELECT usage_status, usage_complete FROM executions ORDER BY id")
            .unwrap();
        let rows: Vec<(String, bool)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("pending".into(), false),
                ("complete".into(), true),
                ("incomplete".into(), false),
            ]
        );
    }

    #[test]
    fn v22_migration_backfills_every_existing_execution_to_the_pre_rewrite_era() {
        // The backfill is the load-bearing half of #1981: a row already at
        // rest was written by a build that could not journal a compaction
        // rewrite, so era 0 is a fact about it. Reading those rows as the
        // current era would raise an integrity alarm on every compacted block
        // in the user's history.
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE executions (id INTEGER PRIMARY KEY, kind TEXT);
             INSERT INTO executions (id, kind) VALUES (1, 'run');",
        )
        .expect("v21 schema");
        assert!(!column_exists(&conn, "executions", "journal_era").unwrap());

        apply_migration(&mut conn, migrate_v21_to_v22, 22).expect("migrate");

        let era: i64 = conn
            .query_row("SELECT journal_era FROM executions WHERE id = 1", [], |r| {
                r.get(0)
            })
            .expect("stamped");
        assert_eq!(era, 0);
        // Idempotent on a file whose executions table was created at the v22
        // shape by this build's DDL.
        apply_migration(&mut conn, migrate_v21_to_v22, 22).expect("idempotent");
    }
}
