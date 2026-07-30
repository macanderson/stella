//! Schema versioning for `.stella/private/store.db`: the ordered [`MIGRATIONS`]
//! list, the fresh-file bootstrap ([`create_latest_schema`]), and the
//! transactional runner ([`apply_migration`]) that stamps
//! `PRAGMA user_version` inside the same transaction as the reshape — a
//! crash mid-migration rolls the file back to the old version and old
//! shape, never a mix. The DDL itself lives in [`crate::ddl`]; the
//! fresh-vs-legacy disambiguation and the step loop that drives this
//! module live in `Store::migrate` (see the crate docs' "Schema
//! versioning" section).

use rusqlite::{Connection, TransactionBehavior, params};

use crate::ddl::{
    AGENT_USES_DDL, CONTEXT_BLOCKS_DDL, EXECUTION_REFLECTION_DDL, EXECUTIONS_DDL, FORGOTTEN_DDL,
    MCP_USAGE_DDL, MEMORY_CITATIONS_DDL, PULL_REQUESTS_DDL, REFLECTIONS_DDL, RULES_TABLE,
    SKILL_USAGE_DDL, STEP_MANIFEST_DDL, STEP_RECEIPT_DDL, TABLES, TASKS_DDL, TELEMETRY_INDEX,
    TOOL_CALLS_DDL, UNCHANGED_TABLES, events_ddl, files_touched_ddl, telemetry_ddl,
};
use crate::{Result, StoreError};

mod token_unit;

use token_unit::migrate_v18_to_v19;

/// One schema migration: upgrades an existing database exactly one
/// `user_version` step, inside the transaction the runner opened for it.
/// The runner stamps the new version and commits; the migration only
/// reshapes.
pub(crate) type Migration = fn(&rusqlite::Transaction<'_>) -> Result<()>;

/// Ordered migration list for EXISTING databases: `MIGRATIONS[i]` upgrades
/// a file at `user_version` i to i + 1. Fresh files never run these — they
/// get [`create_latest_schema`] and are stamped at [`SCHEMA_VERSION`]
/// directly.
pub(crate) const MIGRATIONS: [Migration; 19] = [
    // v0 → v1: dedupe events/telemetry, then retrofit the UNIQUE keys
    // their write paths have always assumed.
    migrate_v0_to_v1,
    // v1 → v2: files_touched grows line-delta totals + the JSON audit log,
    // and the UNIQUE (execution_id, path) key.
    migrate_v1_to_v2,
    // v2 → v3: the memory_citations table and the `rules` table
    // (extension-authored workspace rules for the stella-core rules
    // engine) — both purely additive; no existing table changes shape.
    migrate_v2_to_v3,
    // v3 → v4: the agent_uses invocation log (purely additive).
    migrate_v3_to_v4,
    // v4 → v5: the skill_usage invocation log (purely additive — SKILLS tab).
    migrate_v4_to_v5,
    // v5 → v6: the additive `mcp_usage` table (per-call MCP tool telemetry).
    migrate_v5_to_v6,
    // v6 → v7: the data-plane tables (all purely additive) — `tool_calls`
    // (normalized per-call log), `execution_reflection` (per-turn self-review
    // tied to the prompt), and `reflections` (unified durable lessons).
    migrate_v6_to_v7,
    // v7 → v8: the session plane — `executions` grows the nullable
    // `session_id` link (+ its by-session index), plus the additive `tasks`
    // (per-session task-board snapshot) and `pull_requests` tables.
    migrate_v7_to_v8,
    // v8 → v9: fail-closed paid-call accounting state.
    migrate_v8_to_v9,
    // v9 → v10: explicit pending/complete/incomplete execution lifecycle.
    migrate_v9_to_v10,
    // v10 → v11: the context-receipts plane — `context_blocks` (the block
    // registry), `step_manifest` (per-step ordered receipt), and `step_receipt`
    // (the manifest header). All purely additive; no existing table changes
    // shape.
    migrate_v10_to_v11,
    // v11 → v12: reconstruction support — `context_blocks` grows the local-only
    // `content` column (gap-kind preimages the journal cannot resolve), and
    // `step_manifest` grows `message_index` (regroups event-granular blocks into
    // exact messages). Both additive ADD COLUMNs, column-guarded.
    migrate_v11_to_v12,
    // v12 → v13: receipt coverage for the calls that are not engine steps —
    // the overflow summarizer and the pipeline's management roles. Both
    // receipt tables grow `call_seq` and take it into the primary key, so a
    // summarizer receipt no longer collides with (and is replaced by) the
    // worker receipt of the step it precedes. A PK change is a table rebuild;
    // existing rows are all worker calls and backfill to seq 0.
    migrate_v12_to_v13,
    // v13 → v14: `forgotten` — explicit, reversible human tombstones over any
    // context surface. Purely additive; no existing table changes shape.
    migrate_v13_to_v14,
    // v14 → v15: `step_manifest` grows a nullable `call_id` — per-occurrence
    // tool-call attribution, which content-addressed block ids cannot carry
    // (byte-identical blocks share an id, so only the first minting call was
    // ever recorded). Additive ADD COLUMN, column-guarded.
    migrate_v14_to_v15,
    // v15 → v16: `step_receipt` grows the compiled frame's identity —
    // `compiled_frame_id` and `frame_hash`, both nullable. The compiled frame
    // IS the step manifest (ADR 0006 as amended), so its identity is two
    // columns on the receipt header rather than a second table; a parallel
    // frame table would be a second immutable record of one turn's context.
    // Nullable because the frame is gated on `context.lifecycle.enabled` and
    // because every pre-v16 row predates it. Additive ADD COLUMNs,
    // column-guarded.
    migrate_v15_to_v16,
    // v16 → v17: drop the schema nothing ever read or wrote — the reserved
    // `graph_nodes`/`graph_edges` seam and the query-less
    // `agent_uses_by_agent`/`reflections_by_kind` indexes. Pure removal; no
    // surviving table changes shape.
    migrate_v16_to_v17,
    // v17 → v18: `tool_calls` becomes a LIVE projection. It grows `state`
    // ('running' | 'ok' | 'error') and the two indexes the live writer reads
    // through, and existing rows backfill their state from `ok`. Additive
    // ADD COLUMN + CREATE INDEX, both guarded; no existing column changes
    // shape and no row is dropped.
    migrate_v17_to_v18,
    // v18 → v19: one token-counting rule, applied to history as well as to
    // new writes (#925). `context_blocks.token_cost` was minted by a
    // character count while `step_receipt.estimated_input_tokens` was minted
    // by a byte count, so one receipt held two numbers for the same content
    // that reconciled only for ASCII. The emitter now calls the one shared
    // rule; this step recomputes every row already at rest from its own
    // preimage, so the column holds one unit rather than being read through a
    // version branch. Rebuilds `context_blocks` to make `token_cost`
    // nullable — the honest value for a block whose preimage the store no
    // longer holds.
    migrate_v18_to_v19,
    // ── APPEND POINT — RESERVED SLOTS ───────────────────────────────────
    // This is an INDEX-ORDERED array and `SCHEMA_VERSION` is its length, so
    // a slot is claimed by position, not by name. Two branches that each
    // append "the next migration" merge cleanly — git sees two additions to
    // different lines — and produce a ladder where one migration silently
    // runs at the other's version. Nothing in CI catches that: both files
    // compile, and the mis-numbering only shows up as a corrupt store on a
    // user's machine.
    //
    // Adaptive context is being built on two branches in parallel, so the
    // slots are reserved here in advance:
    //
    //   v15 → v16: adaptive-context Phase 2 (#713) — CLAIMED above.
    //   v16 → v17: CLAIMED above (the schema-removal step landed first;
    //              slots are positions, so the reservation moves down).
    //   v17 → v18: CLAIMED above by the live `tool_calls` projection. The
    //              slot had been reserved for adaptive-context Phase 3
    //              (#714), which closed without ever needing a migration —
    //              so per the rule below its line is deleted rather than
    //              left as a hole.
    //   v18 → v19: CLAIMED above by the token-unit reconciliation (#925).
    //
    // Nothing is reserved now: take v19 → v20 and add your own line here.
    // If a reserved phase ships without needing its slot, delete its line
    // rather than leaving a hole — index order is the contract.
];

/// The schema version this build writes — the `PRAGMA user_version` of
/// every database it has opened. Version 0 is the legacy pre-versioning
/// shape (and the default stamp of a fresh empty file, which is why fresh
/// detection also probes for tables).
pub(crate) const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// The full latest schema, applied in one shot to fresh databases only.
/// Existing files never see this — [`MIGRATIONS`] upgrades them shape by
/// shape, so this can always describe the CURRENT shape.
pub(crate) fn create_latest_schema(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(EXECUTIONS_DDL)?;
    tx.execute_batch(UNCHANGED_TABLES)?;
    tx.execute_batch(&events_ddl("events"))?;
    tx.execute_batch(&telemetry_ddl("telemetry"))?;
    tx.execute_batch(&files_touched_ddl("files_touched"))?;
    tx.execute_batch(RULES_TABLE)?;
    tx.execute_batch(TELEMETRY_INDEX)?;
    tx.execute_batch(MEMORY_CITATIONS_DDL)?;
    tx.execute_batch(AGENT_USES_DDL)?;
    tx.execute_batch(SKILL_USAGE_DDL)?;
    tx.execute_batch(MCP_USAGE_DDL)?;
    tx.execute_batch(TOOL_CALLS_DDL)?;
    tx.execute_batch(EXECUTION_REFLECTION_DDL)?;
    tx.execute_batch(REFLECTIONS_DDL)?;
    tx.execute_batch(TASKS_DDL)?;
    tx.execute_batch(PULL_REQUESTS_DDL)?;
    tx.execute_batch(CONTEXT_BLOCKS_DDL)?;
    tx.execute_batch(STEP_MANIFEST_DDL)?;
    tx.execute_batch(STEP_RECEIPT_DDL)?;
    tx.execute_batch(FORGOTTEN_DDL)?;
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        params![table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Whether any store-owned table exists — distinguishes a fresh empty file
/// (create latest schema directly) from a legacy pre-versioning file (run
/// the migration list), since both carry `user_version` 0.
pub(crate) fn any_store_table_exists(conn: &Connection) -> Result<bool> {
    let placeholders = TABLES.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let count: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ({placeholders})"
        ),
        rusqlite::params_from_iter(TABLES),
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_table_info(?) WHERE name = ?",
        params![table, column],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

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
fn migrate_v0_to_v1(tx: &rusqlite::Transaction<'_>) -> Result<()> {
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
fn migrate_v1_to_v2(tx: &rusqlite::Transaction<'_>) -> Result<()> {
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

/// v2 → v3: the `memory_citations` table. Purely additive — no existing
/// table is touched, so no rebuild, no dedupe, no backfill.
fn migrate_v2_to_v3(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(MEMORY_CITATIONS_DDL)?;
    tx.execute_batch(RULES_TABLE)?;
    Ok(())
}

/// v3 → v4: the `agent_uses` invocation log (agent-version usage telemetry,
/// drained per execution like `files_touched`). Purely additive — no
/// existing table changes shape, so no §7 rebuild is needed; `IF NOT
/// EXISTS` also covers a partial file that somehow already grew the table.
fn migrate_v3_to_v4(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(AGENT_USES_DDL)?;
    Ok(())
}

/// v4 → v5: the additive `skill_usage` table (skill-version usage telemetry,
/// SKILLS tab). Purely additive, mirroring [`migrate_v3_to_v4`].
fn migrate_v4_to_v5(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(SKILL_USAGE_DDL)?;
    Ok(())
}

/// v5 → v6: the `mcp_usage` table. Purely additive — no existing table is
/// touched, so no rebuild, no dedupe, no backfill.
fn migrate_v5_to_v6(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(MCP_USAGE_DDL)?;
    Ok(())
}

/// v6 → v7: the additive data-plane tables — `tool_calls`,
/// `execution_reflection`, and `reflections`. No existing table changes shape.
fn migrate_v6_to_v7(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(TOOL_CALLS_DDL)?;
    tx.execute_batch(EXECUTION_REFLECTION_DDL)?;
    tx.execute_batch(REFLECTIONS_DDL)?;
    Ok(())
}

/// v7 → v8: the session plane. `executions` grows the nullable `session_id`
/// column linking each per-turn row to the cross-process session registry id
/// (a plain ADD COLUMN — nullable, no default rewrite, so no §7 rebuild),
/// plus the `executions_by_session` index and the additive `tasks` and
/// `pull_requests` tables. The ALTER is guarded by a column probe, matching
/// the house `IF NOT EXISTS` tolerance for partial files that somehow
/// already grew the shape.
fn migrate_v7_to_v8(tx: &rusqlite::Transaction<'_>) -> Result<()> {
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
fn migrate_v8_to_v9(tx: &rusqlite::Transaction<'_>) -> Result<()> {
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
fn migrate_v9_to_v10(tx: &rusqlite::Transaction<'_>) -> Result<()> {
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

/// v13 → v14: `forgotten`, the explicit-tombstone table. Purely additive —
/// one new table plus its by-surface index, so no §7 rebuild. `IF NOT EXISTS`
/// in the shared DDL also tolerates a partial file that already grew it.
fn migrate_v13_to_v14(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(FORGOTTEN_DDL)?;
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
/// shape by this build's [`STEP_MANIFEST_DDL`].
fn migrate_v14_to_v15(tx: &rusqlite::Transaction<'_>) -> Result<()> {
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
fn migrate_v15_to_v16(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "step_receipt", "compiled_frame_id")? {
        tx.execute_batch("ALTER TABLE step_receipt ADD COLUMN compiled_frame_id TEXT;")?;
    }
    if !column_exists(tx, "step_receipt", "frame_hash")? {
        tx.execute_batch("ALTER TABLE step_receipt ADD COLUMN frame_hash TEXT;")?;
    }
    Ok(())
}

/// v16 → v17: the first pure removal in the chain. `graph_nodes`/`graph_edges`
/// were a v0-era seam reserved for a context plane that shipped its own stores
/// instead (`stella-context`, `stella-graph`) — no shipping code ever wrote or
/// read them, so every deployed pair is empty and dropping loses nothing.
/// `agent_uses_by_agent` and `reflections_by_kind` indexed access paths no
/// reader takes (both tables are only ever walked whole — the JSON export and
/// the observatory), so each was pure write-amplification on its table's
/// insert path. `IF EXISTS` mirrors the house `IF NOT EXISTS` tolerance:
/// partial legacy files may hold any subset of these.
fn migrate_v16_to_v17(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "DROP TABLE IF EXISTS graph_nodes;
         DROP TABLE IF EXISTS graph_edges;
         DROP INDEX IF EXISTS agent_uses_by_agent;
         DROP INDEX IF EXISTS reflections_by_kind;",
    )?;
    Ok(())
}

/// v17 → v18: `tool_calls` grows the lifecycle column the live projection
/// needs, plus the two indexes that projection reads through.
///
/// Every existing row describes a call that already finished, so `state`
/// backfills from `ok` and no row is left in a state the CHECK constraint
/// would reject. The column is added *without* the CHECK: SQLite's
/// `ADD COLUMN` cannot carry one that references the added column on an
/// existing table, so the constraint lives on the fresh-file DDL only and is
/// upheld here by the writer. That asymmetry is deliberate and cheap —
/// [`crate::Store`] is the only writer, and the alternative is a full table
/// rebuild (lang_altertable §7) to gain a constraint on a column this
/// migration is itself the sole populator of.
///
/// The unique index is partial (`WHERE call_id != ''`) so a legacy file
/// holding several rows with the empty pre-`call_id` default still upgrades
/// instead of failing to build the index — which would abort the migration
/// and leave the workspace unable to open its store at all. Any *real*
/// duplicate ids are collapsed first, keeping the earliest position, because
/// that is the row `materialize_tool_calls` would itself have kept.
fn migrate_v17_to_v18(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    if !column_exists(tx, "tool_calls", "state")? {
        tx.execute_batch("ALTER TABLE tool_calls ADD COLUMN state TEXT NOT NULL DEFAULT 'ok';")?;
        // Backfill from the boolean this column supersedes. Rows written
        // before v18 are all terminal — the only writer was the end-of-turn
        // fold — so none of them is 'running'.
        tx.execute_batch(
            "UPDATE tool_calls SET state = CASE WHEN ok = 1 THEN 'ok' ELSE 'error' END;",
        )?;
    }
    tx.execute_batch(
        "DELETE FROM tool_calls WHERE call_id != '' AND rowid NOT IN (
             SELECT min(rowid) FROM tool_calls WHERE call_id != ''
             GROUP BY execution_id, call_id
         );
         CREATE INDEX IF NOT EXISTS tool_calls_by_state
           ON tool_calls(state, execution_id, seq);
         CREATE UNIQUE INDEX IF NOT EXISTS tool_calls_by_call_id
           ON tool_calls(execution_id, call_id) WHERE call_id != '';
         CREATE INDEX IF NOT EXISTS executions_unfinished
           ON executions(id) WHERE finished_at IS NULL;",
    )?;
    Ok(())
}

/// v10 → v11: the context-receipts plane (spec §4/§5). Three purely additive
/// tables — `context_blocks`, `step_manifest`, `step_receipt` — and their
/// indexes. No existing table changes shape, so no §7 rebuild; `IF NOT EXISTS`
/// also tolerates a partial file that somehow already grew them.
fn migrate_v10_to_v11(tx: &rusqlite::Transaction<'_>) -> Result<()> {
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

/// v12 → v13: `call_seq` joins the primary key of both receipt tables so the
/// auxiliary calls that share a step with the worker (overflow summarizer,
/// pipeline management roles) each keep their own receipt instead of replacing
/// one another. SQLite cannot alter a primary key, so both tables are rebuilt;
/// every pre-existing row is a worker call and backfills to seq 0.
fn migrate_v12_to_v13(tx: &rusqlite::Transaction<'_>) -> Result<()> {
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

/// v11 → v12: reconstruction support (spec §5, increment 2). `context_blocks`
/// grows a nullable local-only `content` column — the preimage of gap kinds the
/// journal cannot resolve (system prefix, assembled user/recall message);
/// `step_manifest` grows `message_index` so event-granular blocks regroup into
/// the exact `CompletionMessage`s that were sent. Both are plain additive
/// ADD COLUMNs; column-guarded so this is a no-op on a store whose v11 tables
/// were already created at the v12 shape (fresh files, or a v10→v11 upgrade run
/// by this build's [`CONTEXT_BLOCKS_DDL`]/[`STEP_MANIFEST_DDL`]).
fn migrate_v11_to_v12(tx: &rusqlite::Transaction<'_>) -> Result<()> {
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

/// Runs the connection pragmas, absorbing a concurrent first-open's
/// `SQLITE_BUSY`.
///
/// `busy_timeout` is set first so every statement after it can wait, but that
/// is not enough on its own: converting a fresh rollback-journal database into
/// WAL needs an exclusive lock, and **SQLite skips the busy handler on that
/// upgrade** to avoid deadlock. So two processes opening the same new workspace
/// at once — a fleet run beside a `stella stats`, or four threads in a test —
/// gave one of them an immediate `database is locked` from `journal_mode=WAL`
/// itself, which no timeout could absorb, and `Store::open` failed outright.
/// Backing off lets the loser observe the settled WAL file instead.
///
/// Only first opens are affected: once the file is WAL the pragma is a no-op
/// and takes no lock. `stella-media`'s journal already does exactly this
/// (`initialize_journal_database`) — the main store never got the same
/// treatment (#617 item 8).
///
/// WAL also means a read-only caller (`stella stats`) is never blocked by a
/// live session's writes; the `synchronous`/`fullfsync` pair is chosen by
/// [`Durability`], which documents what each level actually survives and what
/// it costs per event.
/// How hard the store tries to survive the machine going away, selected by
/// `STELLA_STORE_DURABILITY`.
///
/// The three levels are not a style preference — they are three genuinely
/// different failure models, and the numbers below are measured on this
/// workspace's own write path (one transaction per event, 2 KiB payload,
/// APFS on SSD), not assumed:
///
/// | level      | ms/event | survives process kill | survives kernel panic | survives power loss |
/// |------------|----------|-----------------------|-----------------------|---------------------|
/// | `normal`   | 0.022    | yes                   | **no**                | **no**              |
/// | `full`     | 0.037    | yes                   | yes                   | **no** (see below)  |
/// | `paranoid` | 3.99     | yes                   | yes                   | yes                 |
///
/// `full` is the default, changed from `normal`, because it closes the
/// kernel-panic and forced-reboot window for **15 microseconds an event** —
/// about 75 ms across a whole turn, which no one can perceive. Telemetry that
/// was committed should not evaporate because the machine was restarted
/// ungracefully.
///
/// `paranoid` adds `fullfsync`, which on macOS is what actually forces the
/// **drive's own write cache** to disk — a plain `fsync()` there returns once
/// the data reaches the drive, not once the drive has persisted it. It is the
/// only level that genuinely survives losing power, and it costs 180× per
/// event: roughly twenty seconds of pure fsync across a turn that makes a few
/// thousand events. That is a work stoppage, not a trade-off, which is why it
/// is opt-in rather than the default despite being the only complete answer.
///
/// The honest summary for an operator: at the default, a crash loses nothing
/// and a power cut can lose the last few seconds of *raw* events. What it
/// cannot lose is everything derived from them —
/// [`Store::reconcile_interrupted_executions`](crate::Store::reconcile_interrupted_executions)
/// rebuilds the projections from whatever log survived, which is the part
/// that used to be lost permanently and the reason this table is a tail risk
/// rather than the main one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Durability {
    /// `synchronous=NORMAL` — the pre-v18 setting.
    Normal,
    /// `synchronous=FULL`, no `fullfsync`. The default.
    Full,
    /// `synchronous=FULL` + `fullfsync`. Survives power loss; 180× the cost.
    Paranoid,
}

impl Durability {
    /// Read `STELLA_STORE_DURABILITY`.
    pub(crate) fn from_env() -> Self {
        Self::parse(&std::env::var("STELLA_STORE_DURABILITY").unwrap_or_default())
    }

    /// Parse one level name. An empty or unrecognized value is
    /// [`Self::Full`]: a typo in an environment variable must not silently
    /// downgrade a durability guarantee, and the safe direction is the
    /// default anyway.
    ///
    /// Split from [`Self::from_env`] so the mapping is testable without
    /// touching the process environment — `set_var` is a global mutation, and
    /// a test binary runs its cases on many threads at once, so pinning this
    /// contract through the environment would race every other test.
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "normal" => Self::Normal,
            "paranoid" => Self::Paranoid,
            _ => Self::Full,
        }
    }

    /// The pragma pair this level sets.
    fn pragmas(self) -> &'static str {
        match self {
            Self::Normal => "PRAGMA synchronous=NORMAL; PRAGMA fullfsync=0;",
            Self::Full => "PRAGMA synchronous=FULL; PRAGMA fullfsync=0;",
            Self::Paranoid => "PRAGMA synchronous=FULL; PRAGMA fullfsync=1;",
        }
    }
}

pub(crate) fn initialize_store_pragmas(
    conn: &Connection,
) -> std::result::Result<(), rusqlite::Error> {
    const BUSY_ATTEMPTS: u32 = 40;
    const BUSY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);

    let durability = Durability::from_env().pragmas();
    let mut attempts = 0;
    loop {
        // `execute_batch` tolerates the row `PRAGMA journal_mode` returns (a
        // plain `pragma_update` errors on it).
        let result = conn.execute_batch(&format!(
            "PRAGMA busy_timeout=5000;
             PRAGMA journal_mode=WAL;
             {durability}
             PRAGMA foreign_keys=ON;"
        ));
        match result {
            Err(error)
                if attempts < BUSY_ATTEMPTS
                    && error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy) =>
            {
                attempts += 1;
                std::thread::sleep(BUSY_BACKOFF);
            }
            other => return other,
        }
    }
}

/// Run one migration in its own transaction, stamping `user_version` before
/// commit so version and shape can never disagree on disk. The caller has
/// already suspended foreign-key enforcement (a no-op inside a
/// transaction).
///
/// The transaction is `IMMEDIATE` and re-reads `user_version` inside it, so
/// the version the caller decided on is re-confirmed under the write lock.
/// A `DEFERRED` transaction took its read snapshot before acquiring the
/// write lock, which let two processes migrating the same file concurrently
/// both apply the same step — the loser either failing on an already-applied
/// reshape or, for a non-idempotent step, applying it twice (#617 item 8).
/// Losing the race is now the no-op it should always have been.
pub(crate) fn apply_migration(
    conn: &mut Connection,
    migration: Migration,
    target: i64,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Re-read under the write lock: another process may have applied this
    // exact step between the caller's read and this transaction.
    let current: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current >= target {
        tx.commit()?;
        return Ok(());
    }
    migration(&tx)?;
    // lang_altertable §7 requires a full FK audit before committing work
    // done with enforcement off. No store table declares foreign keys
    // today, so this passes trivially — it is what keeps the runner safe
    // for schemas that will.
    let violations: i64 =
        tx.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if violations > 0 {
        return Err(StoreError(format!(
            "migration to schema version {target} would leave {violations} \
             foreign-key violation(s); rolling back"
        )));
    }
    tx.pragma_update(None, "user_version", target)?;
    tx.commit().map_err(StoreError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// #617 item 8: losing the migration race is a no-op, not an error. The
    /// migration body must not run at all when the file is already stamped at
    /// or past `target` — that is what makes a non-idempotent step (an
    /// `ALTER TABLE … ADD COLUMN`) safe to add later.
    #[test]
    fn apply_migration_skips_a_step_another_process_already_stamped() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.pragma_update(None, "user_version", 7).expect("stamp");

        fn must_not_run(_: &rusqlite::Transaction<'_>) -> Result<()> {
            Err(StoreError(
                "the migration body ran even though the version was already stamped".into(),
            ))
        }

        // Target 7 against a file at 7: the step was applied by someone else.
        apply_migration(&mut conn, must_not_run, 7).expect("a stamped step is skipped");
        // And a step already overtaken by a later version is skipped too.
        apply_migration(&mut conn, must_not_run, 5).expect("an overtaken step is skipped");

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read");
        assert_eq!(version, 7, "a skipped step must not move the version");
    }

    /// #617 item 8, the case the issue asked for by name: two processes opening
    /// the same *fresh* workspace at once. Before the bootstrap read moved
    /// inside `BEGIN IMMEDIATE`, both could observe `user_version = 0` with no
    /// tables and both run `create_latest_schema`, and the loser failed with
    /// "table … already exists" instead of no-opping.
    #[test]
    fn concurrent_first_open_of_a_fresh_workspace_both_succeed() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let root = workspace.path().to_path_buf();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let root = root.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    // Line the openers up so the version reads genuinely
                    // interleave rather than serializing by luck of spawn order.
                    barrier.wait();
                    crate::Store::open(&root).map(|_| ())
                })
            })
            .collect();

        for (i, handle) in handles.into_iter().enumerate() {
            handle
                .join()
                .expect("opener thread panicked")
                .unwrap_or_else(|error| {
                    panic!("opener {i} lost the fresh-open race and failed: {error}")
                });
        }

        // The winner's schema is the one on disk, stamped at the current version.
        let store = crate::Store::open(&root).expect("reopen");
        let version: i64 = store
            .lock()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read");
        assert_eq!(version, crate::SCHEMA_VERSION);
    }
}
