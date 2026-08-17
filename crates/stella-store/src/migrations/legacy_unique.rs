//! v0 -> v1: retrofitting the UNIQUE constraints the write paths always
//! assumed.
//!
//! Split out of `migrations.rs` (#3395): that file sat at exactly its
//! 1500-line ceiling, and this is its largest single migration -- a full
//! lang_altertable section 7 table rebuild carrying the dedupe keep-rule
//! argument. Moving it whole keeps the reasoning with the code and leaves the
//! ladder file room to grow a step. Behaviour is unchanged: the function is
//! the same as the one that lived inline.

use rusqlite::params;

use crate::Result;
use crate::ddl::{TELEMETRY_INDEX, UNCHANGED_TABLES, events_ddl, telemetry_ddl};

use super::{column_exists, table_exists};

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
