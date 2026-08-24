// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The context-receipts plane, born at v11 and reshaped four times since:
//! the three tables themselves (v10 → v11), reconstruction support
//! (v11 → v12), the `call_seq` re-key that stopped auxiliary calls replacing
//! the worker's receipt (v12 → v13), per-occurrence call attribution
//! (v14 → v15), and the compiled frame's identity (v15 → v16).
//!
//! They are grouped because they all reshape the same three tables —
//! `context_blocks`, `step_manifest`, `step_receipt` — and each one's doc
//! comment is only readable against the shape the previous one left behind.
//! v13 → v14 is not here despite sitting between them: `forgotten` is an
//! unrelated additive table, and lives with the rest of those in
//! [`super::additive_tables`].

use super::column_exists;
use crate::Result;
use crate::ddl::CONTEXT_BLOCKS_DDL;

/// v10 → v11: the context-receipts plane (spec §4/§5). Three purely additive
/// tables — `context_blocks`, `step_manifest`, `step_receipt` — and their
/// indexes. No existing table changes shape, so no §7 rebuild; `IF NOT EXISTS`
/// also tolerates a partial file that somehow already grew them.
pub(super) fn migrate_v10_to_v11(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(CONTEXT_BLOCKS_DDL)?;
    // The receipt tables changed shape in v13 (the `call_seq` key column), so
    // they left the shared DDL constants — but a v11 database has its ERA's
    // shape, which this step must keep producing (the v13 rebuild later in the
    // chain runs against it).
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS step_manifest (
           execution_id INTEGER NOT NULL,
           turn_instance INTEGER NOT NULL,
           step INTEGER NOT NULL,
           ordinal INTEGER NOT NULL,
           block_id TEXT NOT NULL,
           cache_zone TEXT NOT NULL,
           resident_since_step INTEGER NOT NULL,
           PRIMARY KEY (execution_id, turn_instance, step, ordinal)
         );
         CREATE INDEX IF NOT EXISTS step_manifest_by_block
           ON step_manifest(execution_id, block_id);",
    )?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS step_receipt (
           execution_id INTEGER NOT NULL,
           turn_instance INTEGER NOT NULL,
           step INTEGER NOT NULL,
           provider TEXT NOT NULL,
           model TEXT NOT NULL,
           call_role TEXT NOT NULL,
           effective_budget_tokens INTEGER NOT NULL,
           calibration_factor REAL NOT NULL,
           estimated_input_tokens INTEGER NOT NULL,
           PRIMARY KEY (execution_id, turn_instance, step)
         );",
    )?;
    Ok(())
}

/// v11 → v12: reconstruction support (spec §5, increment 2). `context_blocks`
/// grows a nullable local-only `content` column — the preimage of gap kinds the
/// journal cannot resolve (system prefix, assembled user/recall message);
/// `step_manifest` grows `message_index` so event-granular blocks regroup into
/// the exact `CompletionMessage`s that were sent. Both are plain additive
/// ADD COLUMNs; column-guarded so this is a no-op on a store whose v11 tables
/// were already created at the v12 shape (fresh files, or a v10→v11 upgrade run
/// by this build's [`CONTEXT_BLOCKS_DDL`]/[`STEP_MANIFEST_DDL`](crate::ddl::STEP_MANIFEST_DDL)).
pub(super) fn migrate_v11_to_v12(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "context_blocks", "content")? {
        tx.execute_batch("ALTER TABLE context_blocks ADD COLUMN content TEXT;")?;
    }
    if !column_exists(tx, "step_manifest", "message_index")? {
        tx.execute_batch(
            "ALTER TABLE step_manifest ADD COLUMN message_index INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    Ok(())
}

/// v12 → v13: `call_seq` joins the primary key of both receipt tables so the
/// auxiliary calls that share a step with the worker (overflow summarizer,
/// pipeline management roles) each keep their own receipt instead of replacing
/// one another. SQLite cannot alter a primary key, so both tables are rebuilt;
/// every pre-existing row is a worker call and backfills to seq 0.
pub(super) fn migrate_v12_to_v13(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "step_manifest", "call_seq")? {
        tx.execute_batch(
            "CREATE TABLE step_manifest_v13 (
               execution_id INTEGER NOT NULL,
               turn_instance INTEGER NOT NULL,
               step INTEGER NOT NULL,
               call_seq INTEGER NOT NULL DEFAULT 0,
               ordinal INTEGER NOT NULL,
               block_id TEXT NOT NULL,
               cache_zone TEXT NOT NULL,
               resident_since_step INTEGER NOT NULL,
               message_index INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (execution_id, turn_instance, step, call_seq, ordinal)
             );
             INSERT INTO step_manifest_v13
               (execution_id, turn_instance, step, call_seq, ordinal, block_id,
                cache_zone, resident_since_step, message_index)
             SELECT execution_id, turn_instance, step, 0, ordinal, block_id,
                    cache_zone, resident_since_step, message_index
               FROM step_manifest;
             DROP TABLE step_manifest;
             ALTER TABLE step_manifest_v13 RENAME TO step_manifest;
             CREATE INDEX IF NOT EXISTS step_manifest_by_block
               ON step_manifest(execution_id, block_id);",
        )?;
    }
    if !column_exists(tx, "step_receipt", "call_seq")? {
        tx.execute_batch(
            "CREATE TABLE step_receipt_v13 (
               execution_id INTEGER NOT NULL,
               turn_instance INTEGER NOT NULL,
               step INTEGER NOT NULL,
               call_seq INTEGER NOT NULL DEFAULT 0,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               call_role TEXT NOT NULL,
               effective_budget_tokens INTEGER NOT NULL,
               calibration_factor REAL NOT NULL,
               estimated_input_tokens INTEGER NOT NULL,
               PRIMARY KEY (execution_id, turn_instance, step, call_seq)
             );
             INSERT INTO step_receipt_v13
               (execution_id, turn_instance, step, call_seq, provider, model,
                call_role, effective_budget_tokens, calibration_factor,
                estimated_input_tokens)
             SELECT execution_id, turn_instance, step, 0, provider, model,
                    call_role, effective_budget_tokens, calibration_factor,
                    estimated_input_tokens
               FROM step_receipt;
             DROP TABLE step_receipt;
             ALTER TABLE step_receipt_v13 RENAME TO step_receipt;",
        )?;
    }
    Ok(())
}

/// v14 → v15: per-occurrence tool-call attribution on the manifest (#364 gap 1).
/// `block_id` is content-addressed, so two distinct calls with byte-identical
/// output share one id and `BlockRegistered`/`context_blocks.call_id` keeps only
/// the first — which silently under-reports "which calls left context" when a
/// compaction pass evicts such a block. `step_manifest` grows a nullable
/// `call_id` recorded per entry instead. Plain additive ADD COLUMN, nullable
/// because pre-v15 rows genuinely do not know theirs (and non-tool blocks never
/// have one); column-guarded for stores whose tables were created at the v15
/// shape by this build's [`STEP_MANIFEST_DDL`](crate::ddl::STEP_MANIFEST_DDL).
pub(super) fn migrate_v14_to_v15(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "step_manifest", "call_id")? {
        tx.execute_batch("ALTER TABLE step_manifest ADD COLUMN call_id TEXT;")?;
    }
    Ok(())
}

/// v15 → v16: the compiled frame's identity on the receipt header — Phase 2
/// (#713). Two nullable columns, not a table: ADR 0006 as amended says the
/// compiled frame is the step manifest extended, so a `compiled_frame` table
/// would be the second immutable record of one turn's context that amendment
/// exists to forbid.
///
/// Both columns are nullable and stay null for every row written while
/// `context.lifecycle.enabled` is off, which is the default. A reader must
/// therefore treat "no frame hash" as "the lifecycle was off", never as "this
/// receipt is damaged".
pub(super) fn migrate_v15_to_v16(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "step_receipt", "compiled_frame_id")? {
        tx.execute_batch("ALTER TABLE step_receipt ADD COLUMN compiled_frame_id TEXT;")?;
    }
    if !column_exists(tx, "step_receipt", "frame_hash")? {
        tx.execute_batch("ALTER TABLE step_receipt ADD COLUMN frame_hash TEXT;")?;
    }
    Ok(())
}

/// v29 → v30: the stall rung's own number on the receipt header (#3621).
///
/// `turn_stall_seconds` computes the pure-`sleep` seconds a turn has asked
/// for on every step of every turn, and until now nothing persisted it: a
/// bench trial that slept away its allowance could only be counted by joining
/// `tool_start` → `tool_result` in the event stream and re-running the
/// classifier by hand. One nullable column on the header the engine already
/// writes per model call.
///
/// Nullable with no default and no backfill, and the distinction is the
/// point: NULL means "this emitter did not classify", which is what every
/// pre-v30 row is and what the replay path — a receipt rebuilt from stored
/// messages, with no detector window around it — keeps writing. A `0` default
/// would claim every historical turn was observed and found not to sleep,
/// which is the invented answer this column exists to replace.
pub(super) fn migrate_v29_to_v30(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "step_receipt", "stall_seconds_requested")? {
        tx.execute_batch("ALTER TABLE step_receipt ADD COLUMN stall_seconds_requested INTEGER;")?;
    }
    Ok(())
}

/// v33 → v34: who actually served a gateway-routed call, on the receipt
/// header (#3054).
///
/// `AgentEvent::StepUsage` has carried `upstream_provider` since #2786, but
/// only inside the raw `events` payload: `step_receipt.provider` is always
/// the gateway's own id (`openrouter`) for a gateway call, so `stella
/// inspect` — which reads receipts, not events — could not answer "which
/// vendor served this call" for any stored execution. One nullable column on
/// the header, projected from the manifest the engine already emits at the
/// settled boundary, where the served vendor is known.
///
/// Nullable with no default and no backfill: NULL means "no upstream was
/// named" — every direct-endpoint call, and every pre-v34 row, where the
/// answer genuinely was not recorded on this table. The `events` payload
/// remains the only source for history, and inventing a backfill from it
/// here would make one migration re-parse every journal ever written.
pub(super) fn migrate_v33_to_v34(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "step_receipt", "upstream_provider")? {
        tx.execute_batch("ALTER TABLE step_receipt ADD COLUMN upstream_provider TEXT;")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::{apply_migration, table_exists};
    use rusqlite::Connection;

    #[test]
    fn v11_migration_adds_the_receipts_tables_additively_to_an_existing_store() {
        // Start from a minimal v10-shaped file with a live execution row: the
        // migration must add the three receipts tables without touching it.
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE executions (id INTEGER PRIMARY KEY, kind TEXT);
             INSERT INTO executions (id, kind) VALUES (1, 'run');",
        )
        .expect("v10 schema");

        apply_migration(&mut conn, migrate_v10_to_v11, 11).expect("migrate");

        // The pre-existing row is untouched.
        let kind: String = conn
            .query_row("SELECT kind FROM executions WHERE id = 1", [], |r| r.get(0))
            .expect("execution preserved");
        assert_eq!(kind, "run");

        // All three tables now exist and accept a row at the new shape.
        for table in ["context_blocks", "step_manifest", "step_receipt"] {
            assert!(table_exists(&conn, table).unwrap(), "{table} missing");
        }
        conn.execute_batch(
            "INSERT INTO context_blocks
               (execution_id, block_id, kind, origin_turn, origin_step, token_cost, content_digest)
               VALUES (1, 'blk_x', 'tool_result', 0, 2, 40, 'sha256:ab');
             INSERT INTO step_manifest
               (execution_id, turn_instance, step, ordinal, block_id, cache_zone, resident_since_step)
               VALUES (1, 0, 2, 0, 'blk_x', 'cacheable', 2);
             INSERT INTO step_receipt
               (execution_id, turn_instance, step, provider, model, call_role,
                effective_budget_tokens, calibration_factor, estimated_input_tokens)
               VALUES (1, 0, 2, 'anthropic', 'opus', 'worker', 136363, 1.1, 40);",
        )
        .expect("new tables accept rows at the v11 shape");
    }

    #[test]
    fn v11_migration_is_idempotent_on_a_partial_file() {
        // A partial file that somehow already grew one receipts table must not
        // fail the migration (IF NOT EXISTS tolerance).
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(CONTEXT_BLOCKS_DDL).expect("pre-grown");
        apply_migration(&mut conn, migrate_v10_to_v11, 11).expect("migrate is idempotent");
        assert!(table_exists(&conn, "step_manifest").unwrap());
    }

    #[test]
    fn v12_migration_adds_reconstruction_columns_and_is_idempotent() {
        // Build v11-shaped receipts tables WITHOUT the v12 columns (the shape a
        // store created on the increment-1 branch has), then upgrade.
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE context_blocks (
               execution_id INTEGER NOT NULL, block_id TEXT NOT NULL, kind TEXT NOT NULL,
               origin_turn INTEGER NOT NULL, origin_step INTEGER NOT NULL, call_id TEXT,
               memory_id TEXT, token_cost INTEGER NOT NULL, content_digest TEXT NOT NULL,
               citation_label TEXT, first_seen_ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               PRIMARY KEY (execution_id, block_id));
             CREATE TABLE step_manifest (
               execution_id INTEGER NOT NULL, turn_instance INTEGER NOT NULL, step INTEGER NOT NULL,
               ordinal INTEGER NOT NULL, block_id TEXT NOT NULL, cache_zone TEXT NOT NULL,
               resident_since_step INTEGER NOT NULL,
               PRIMARY KEY (execution_id, turn_instance, step, ordinal));",
        )
        .expect("v11 receipts shape");
        assert!(!column_exists(&conn, "context_blocks", "content").unwrap());

        apply_migration(&mut conn, migrate_v11_to_v12, 12).expect("migrate");
        assert!(column_exists(&conn, "context_blocks", "content").unwrap());
        assert!(column_exists(&conn, "step_manifest", "message_index").unwrap());

        // Idempotent on tables already at the v12 shape (fresh files, or a
        // v10→v11 upgrade run by this build's DDL).
        apply_migration(&mut conn, migrate_v11_to_v12, 12).expect("idempotent");
    }

    #[test]
    fn v13_migration_rekeys_receipts_on_call_seq_preserving_existing_rows_as_worker_calls() {
        // A v12-shaped file with one recorded worker step. The rebuild must
        // keep that row verbatim, backfill it to seq 0, and then accept a
        // SECOND row at the same (turn, step) — the summarizer receipt that
        // the old primary key silently replaced.
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE step_manifest (
               execution_id INTEGER NOT NULL, turn_instance INTEGER NOT NULL, step INTEGER NOT NULL,
               ordinal INTEGER NOT NULL, block_id TEXT NOT NULL, cache_zone TEXT NOT NULL,
               resident_since_step INTEGER NOT NULL, message_index INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (execution_id, turn_instance, step, ordinal));
             CREATE TABLE step_receipt (
               execution_id INTEGER NOT NULL, turn_instance INTEGER NOT NULL, step INTEGER NOT NULL,
               provider TEXT NOT NULL, model TEXT NOT NULL, call_role TEXT NOT NULL,
               effective_budget_tokens INTEGER NOT NULL, calibration_factor REAL NOT NULL,
               estimated_input_tokens INTEGER NOT NULL,
               PRIMARY KEY (execution_id, turn_instance, step));
             INSERT INTO step_manifest
               (execution_id, turn_instance, step, ordinal, block_id, cache_zone,
                resident_since_step, message_index)
               VALUES (1, 0, 2, 0, 'blk_sys', 'stable_prefix', 0, 0);
             INSERT INTO step_receipt
               (execution_id, turn_instance, step, provider, model, call_role,
                effective_budget_tokens, calibration_factor, estimated_input_tokens)
               VALUES (1, 0, 2, 'anthropic', 'opus', 'worker', 136363, 1.1, 40);",
        )
        .expect("v12 receipts shape");
        assert!(!column_exists(&conn, "step_receipt", "call_seq").unwrap());

        apply_migration(&mut conn, migrate_v12_to_v13, 13).expect("migrate");

        assert!(column_exists(&conn, "step_receipt", "call_seq").unwrap());
        assert!(column_exists(&conn, "step_manifest", "call_seq").unwrap());

        // The pre-existing worker row survives the rebuild, at seq 0.
        let (role, seq, budget): (String, i64, i64) = conn
            .query_row(
                "SELECT call_role, call_seq, effective_budget_tokens FROM step_receipt
                 WHERE execution_id = 1 AND turn_instance = 0 AND step = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("worker receipt preserved");
        assert_eq!((role.as_str(), seq, budget), ("worker", 0, 136363));
        let block: String = conn
            .query_row(
                "SELECT block_id FROM step_manifest WHERE execution_id = 1 AND call_seq = 0",
                [],
                |r| r.get(0),
            )
            .expect("manifest row preserved");
        assert_eq!(block, "blk_sys");

        // The regression this migration exists for: a summarizer receipt at the
        // SAME (turn, step) now coexists instead of replacing the worker's.
        conn.execute_batch(
            "INSERT INTO step_receipt
               (execution_id, turn_instance, step, call_seq, provider, model, call_role,
                effective_budget_tokens, calibration_factor, estimated_input_tokens)
               VALUES (1, 0, 2, 1, 'anthropic', 'haiku', 'summarization', 136363, 1.1, 900);",
        )
        .expect("an auxiliary call at the same step is no longer a key collision");
        let calls: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM step_receipt WHERE execution_id = 1 AND step = 2",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(calls, 2, "both calls of the step are recorded");

        // Idempotent on a file already at the v13 shape.
        apply_migration(&mut conn, migrate_v12_to_v13, 13).expect("idempotent");
    }

    #[test]
    fn v15_migration_adds_manifest_call_id_leaving_older_rows_honestly_null() {
        // A v14-shaped file with one recorded manifest row. The pre-v15 row
        // genuinely does not know its call, so it must migrate to NULL rather
        // than borrow the block's birth provenance — an inferred attribution
        // would be exactly the under-reporting this column exists to fix.
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE step_manifest (
               execution_id INTEGER NOT NULL, turn_instance INTEGER NOT NULL, step INTEGER NOT NULL,
               call_seq INTEGER NOT NULL DEFAULT 0, ordinal INTEGER NOT NULL,
               block_id TEXT NOT NULL, cache_zone TEXT NOT NULL,
               resident_since_step INTEGER NOT NULL, message_index INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (execution_id, turn_instance, step, call_seq, ordinal));
             INSERT INTO step_manifest
               (execution_id, turn_instance, step, call_seq, ordinal, block_id, cache_zone,
                resident_since_step, message_index)
               VALUES (1, 0, 2, 0, 0, 'blk_tool', 'volatile', 0, 1);",
        )
        .expect("v14 receipts shape");
        assert!(!column_exists(&conn, "step_manifest", "call_id").unwrap());

        apply_migration(&mut conn, migrate_v14_to_v15, 15).expect("migrate");

        assert!(column_exists(&conn, "step_manifest", "call_id").unwrap());
        let (block, call_id): (String, Option<String>) = conn
            .query_row(
                "SELECT block_id, call_id FROM step_manifest WHERE execution_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("manifest row preserved");
        assert_eq!(block, "blk_tool", "the existing row survives verbatim");
        assert_eq!(call_id, None, "pre-v15 rows admit they do not know");

        // Two occurrences of one content-addressed block, each with its own
        // call — the shape the old schema could not express at all.
        conn.execute_batch(
            "INSERT INTO step_manifest
               (execution_id, turn_instance, step, call_seq, ordinal, block_id, cache_zone,
                resident_since_step, message_index, call_id)
               VALUES (1, 0, 3, 0, 0, 'blk_dup', 'volatile', 0, 1, 'c1'),
                      (1, 0, 3, 0, 1, 'blk_dup', 'volatile', 0, 2, 'c2');",
        )
        .expect("duplicate block, distinct calls");
        // Scoped so the statement's borrow of `conn` ends before the
        // idempotency re-run below needs it mutably.
        let calls: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT call_id FROM step_manifest
                     WHERE block_id = 'blk_dup' ORDER BY ordinal",
                )
                .expect("prepare");
            stmt.query_map([], |r| r.get::<_, String>(0))
                .expect("query")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("rows")
        };
        assert_eq!(calls, vec!["c1", "c2"], "both calls are attributable");

        // Idempotent on a file already at the v15 shape.
        apply_migration(&mut conn, migrate_v14_to_v15, 15).expect("idempotent");
    }

    #[test]
    fn v16_migration_adds_the_frame_identity_and_leaves_legacy_receipts_null() {
        // Phase 2 (#713). A v15 file's receipt header has no frame columns and
        // its rows predate the compiled frame entirely. After the migration the
        // columns exist and the old row reads back NULL — which a reader must
        // interpret as "the lifecycle was off", never as a damaged receipt.
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE step_receipt (
               execution_id INTEGER NOT NULL,
               turn_instance INTEGER NOT NULL,
               step INTEGER NOT NULL,
               call_seq INTEGER NOT NULL DEFAULT 0,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               call_role TEXT NOT NULL,
               effective_budget_tokens INTEGER NOT NULL,
               calibration_factor REAL NOT NULL,
               estimated_input_tokens INTEGER NOT NULL,
               PRIMARY KEY (execution_id, turn_instance, step, call_seq)
             );
             INSERT INTO step_receipt VALUES (1, 0, 3, 0, 'anthropic', 'opus', 'worker', 100, 1.0, 40);",
        )
        .expect("v15 schema");

        apply_migration(&mut conn, migrate_v15_to_v16, 16).expect("migrate");

        let (id, hash): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT compiled_frame_id, frame_hash FROM step_receipt WHERE execution_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("the legacy row survives with both columns readable");
        assert_eq!(id, None, "a pre-frame receipt has no frame id");
        assert_eq!(hash, None);

        // A post-migration write round-trips both halves of the identity.
        conn.execute(
            "INSERT INTO step_receipt VALUES (1, 0, 4, 0, 'anthropic', 'opus', 'worker', 100, 1.0, 40, 'cf_abc', 'sha256:abc')",
            [],
        )
        .expect("the new shape accepts a frame");

        // Idempotent on a file already at the v16 shape.
        apply_migration(&mut conn, migrate_v15_to_v16, 16).expect("idempotent");
    }

    /// **Witness (#3621).** A file written before the stall column existed
    /// gains it, and its rows read back NULL — "nobody classified this", which
    /// is exactly true of every receipt written before the rung recorded
    /// anything. A `0` default would have claimed each of them was observed
    /// and found not to sleep.
    #[test]
    fn v30_migration_adds_the_stall_column_and_leaves_legacy_receipts_null() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE step_receipt (
               execution_id INTEGER NOT NULL,
               turn_instance INTEGER NOT NULL,
               step INTEGER NOT NULL,
               call_seq INTEGER NOT NULL DEFAULT 0,
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               call_role TEXT NOT NULL,
               effective_budget_tokens INTEGER NOT NULL,
               calibration_factor REAL NOT NULL,
               estimated_input_tokens INTEGER NOT NULL,
               compiled_frame_id TEXT,
               frame_hash TEXT,
               PRIMARY KEY (execution_id, turn_instance, step, call_seq)
             );
             INSERT INTO step_receipt VALUES
               (1, 0, 3, 0, 'anthropic', 'opus', 'worker', 100, 1.0, 40, NULL, NULL);",
        )
        .expect("v29 schema");

        apply_migration(&mut conn, migrate_v29_to_v30, 30).expect("migrate");

        let stall: Option<i64> = conn
            .query_row(
                "SELECT stall_seconds_requested FROM step_receipt WHERE execution_id = 1",
                [],
                |r| r.get(0),
            )
            .expect("the legacy row survives with the column readable");
        assert_eq!(stall, None, "a pre-v30 receipt classified nothing");

        conn.execute(
            "INSERT INTO step_receipt VALUES
               (1, 0, 4, 0, 'anthropic', 'opus', 'worker', 100, 1.0, 40, NULL, NULL, 900)",
            [],
        )
        .expect("the new shape accepts the rung's number");

        // Idempotent on a file already at the v30 shape.
        apply_migration(&mut conn, migrate_v29_to_v30, 30).expect("idempotent");
    }
}
