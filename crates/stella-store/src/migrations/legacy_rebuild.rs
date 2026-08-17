// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The two pre-versioning repairs: v0 → v1 and v1 → v2, which retrofit the
//! UNIQUE keys the write paths had always assumed onto tables that predate
//! them.
//!
//! They are grouped here because they share one shape — a `lang_altertable`
//! §7 table rebuild with a newest-row keep-rule — and one audience: a legacy
//! file stamped `user_version` 0, which is the only kind of file that can
//! still hold historic duplicates. Nothing later in the ladder needs to
//! re-read them.

use rusqlite::params;

use super::{column_exists, table_exists};
use crate::Result;
use crate::ddl::{TELEMETRY_INDEX, UNCHANGED_TABLES, events_ddl, files_touched_ddl, telemetry_ddl};

/// v0 → v1: retrofit the UNIQUE constraints the write paths have always
/// assumed (see [`events_ddl`]/[`telemetry_ddl`]), deduping first — a
/// constraint cannot land on a table holding historic duplicates.
///
/// Keep-rule: the newest row per natural key — `max(rowid)`, which is
/// insertion order. A duplicate key can only come from a double-write of
/// the same logical record (the writers' counters are monotonic per
/// execution), and readers want the writer's final word: replay renders one
/// event per stream position, and analytics prices one row per committed
/// call — exactly the row an upsert would have retained.
///
/// SQLite cannot ALTER a UNIQUE constraint in, so both tables are rebuilt
/// per the documented procedure (lang_altertable §7): create-new →
/// INSERT SELECT → DROP old → RENAME. The old tables' indexes drop with
/// them; `telemetry_by_model` is recreated and `events_by_execution` is
/// superseded by the UNIQUE constraint's implicit index on exactly its
/// columns. No store table declares foreign keys in either direction, so
/// the rebuild moves no FK edges — but the runner still follows the full §7
/// procedure (`foreign_keys` OFF outside the transaction, `foreign_key_check`
/// before commit) so a future FK-bearing schema cannot be corrupted by this
/// path.
///
/// A v0 file is not guaranteed to hold every table (partial files exist —
/// e.g. pre-drift fixtures with only `telemetry`), so missing tables are
/// created fresh in the v1 shape: empty, nothing to dedupe.
pub(super) fn migrate_v0_to_v1(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(UNCHANGED_TABLES)?;
    // executions changed shape in v8 (the session_id column), so it left
    // UNCHANGED_TABLES — but a v1 database has its ERA's shape, which this
    // step must keep producing (the v8 ALTER later in the chain runs
    // against it).
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS executions (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           kind TEXT NOT NULL,
           prompt TEXT NOT NULL,
           provider TEXT NOT NULL,
           model TEXT NOT NULL,
           started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           finished_at TEXT,
           outcome TEXT,
           cost_usd REAL NOT NULL DEFAULT 0
         );",
    )?;
    // files_touched changed shape again in v2, so it left UNCHANGED_TABLES —
    // but a v1 database has its ERA's shape, which this step must keep
    // producing (the v2 rebuild right after runs against it).
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS files_touched (
           execution_id INTEGER NOT NULL,
           path TEXT NOT NULL,
           ops TEXT NOT NULL
         );",
    )?;
    // New executions must never reuse an id that historic rows already
    // reference: a reused id mis-attributes those orphaned rows to the new
    // run, and — with the UNIQUE keys this migration retrofits — collides
    // with their (execution_id, seq/step) positions. A partial v0 file can
    // hold events/telemetry that outlive their executions table (whose
    // AUTOINCREMENT counter then restarts at 1), so the counter is seeded
    // past every execution id in sight. sqlite_sequence exists here:
    // creating any AUTOINCREMENT table (executions, just ensured) creates
    // it, and its content is plain-DML-writable by design.
    let max_in_executions: Option<i64> =
        tx.query_row("SELECT max(id) FROM executions", [], |row| row.get(0))?;
    let mut max_execution_id = max_in_executions.unwrap_or(0);
    // events and telemetry may still be missing here (they are ensured or
    // rebuilt below), so each referencing table is probed individually.
    for table in ["events", "telemetry", "files_touched"] {
        if table_exists(tx, table)? {
            let max_id: Option<i64> = tx.query_row(
                &format!("SELECT max(execution_id) FROM {table}"),
                [],
                |row| row.get(0),
            )?;
            max_execution_id = max_execution_id.max(max_id.unwrap_or(0));
        }
    }
    if max_execution_id > 0 {
        let seeded = tx.execute(
            "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'executions' AND seq < ?1",
            params![max_execution_id],
        )?;
        if seeded == 0 {
            // No row updated: either the counter is already past the ids
            // (nothing to do) or the counter row does not exist yet.
            let exists: i64 = tx.query_row(
                "SELECT count(*) FROM sqlite_sequence WHERE name = 'executions'",
                [],
                |row| row.get(0),
            )?;
            if exists == 0 {
                tx.execute(
                    "INSERT INTO sqlite_sequence (name, seq) VALUES ('executions', ?1)",
                    params![max_execution_id],
                )?;
            }
        }
    }
    if table_exists(tx, "events")? {
        tx.execute_batch(&events_ddl("events_v1"))?;
        tx.execute_batch(
            "INSERT INTO events_v1 (execution_id, seq, ts, event_type, payload)
             SELECT execution_id, seq, ts, event_type, payload FROM events
             WHERE rowid IN (SELECT max(rowid) FROM events GROUP BY execution_id, seq);
             DROP TABLE events;
             ALTER TABLE events_v1 RENAME TO events;",
        )?;
    } else {
        tx.execute_batch(&events_ddl("events"))?;
    }
    if table_exists(tx, "telemetry")? {
        // Pre-drift files lack estimated_input_tokens; the rebuild
        // backfills 0 = "no estimate was taken", which drift_samples
        // excludes as signal-free — same semantics the old ALTER-based
        // migration gave those rows.
        let estimated = if column_exists(tx, "telemetry", "estimated_input_tokens")? {
            "estimated_input_tokens"
        } else {
            "0"
        };
        tx.execute_batch(&telemetry_ddl("telemetry_v1"))?;
        tx.execute_batch(&format!(
            "INSERT INTO telemetry_v1 (execution_id, step, ts, provider, model, input_tokens,
               estimated_input_tokens, output_tokens, cache_read_tokens, cache_miss_tokens,
               cache_write_tokens, cost_usd, duration_ms, retries, tool_calls)
             SELECT execution_id, step, ts, provider, model, input_tokens,
               {estimated}, output_tokens, cache_read_tokens, cache_miss_tokens,
               cache_write_tokens, cost_usd, duration_ms, retries, tool_calls
             FROM telemetry
             WHERE rowid IN (SELECT max(rowid) FROM telemetry GROUP BY execution_id, step);
             DROP TABLE telemetry;
             ALTER TABLE telemetry_v1 RENAME TO telemetry;",
        ))?;
    } else {
        tx.execute_batch(&telemetry_ddl("telemetry"))?;
    }
    tx.execute_batch(TELEMETRY_INDEX)?;
    Ok(())
}

/// v1 → v2: `files_touched` grows per-file line-delta totals and the ordered
/// JSON audit log ([`FileTouchRow`](crate::FileTouchRow)), plus the UNIQUE (execution_id, path)
/// key its writer has always assumed (the ledger emits exactly one record
/// per normalized path per execution). SQLite cannot ALTER a UNIQUE
/// constraint in, so the table is rebuilt per lang_altertable §7 with the
/// same newest-row keep-rule as [`migrate_v0_to_v1`]. Legacy rows predate
/// line telemetry and are backfilled with the column defaults: zero deltas,
/// `'[]'` audit log.
pub(super) fn migrate_v1_to_v2(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if table_exists(tx, "files_touched")? {
        tx.execute_batch(&files_touched_ddl("files_touched_v2"))?;
        tx.execute_batch(
            "INSERT INTO files_touched_v2 (execution_id, path, ops)
             SELECT execution_id, path, ops FROM files_touched
             WHERE rowid IN (SELECT max(rowid) FROM files_touched GROUP BY execution_id, path);
             DROP TABLE files_touched;
             ALTER TABLE files_touched_v2 RENAME TO files_touched;",
        )?;
    } else {
        // Partial v1 files exist just like partial v0 files: nothing to
        // rebuild, create the v2 shape fresh.
        tx.execute_batch(&files_touched_ddl("files_touched"))?;
    }
    Ok(())
}
