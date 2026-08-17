//! Schema versioning for `.stella/private/store.db`: the ordered [`MIGRATIONS`]
//! list, the fresh-file bootstrap ([`create_latest_schema`]), and the
//! transactional runner ([`apply_migration`]) that stamps
//! `PRAGMA user_version` inside the same transaction as the reshape — a
//! crash mid-migration rolls the file back to the old version and old
//! shape, never a mix. The DDL itself lives in [`crate::ddl`]; the
//! fresh-vs-legacy disambiguation and the step loop that drives this
//! module live in `Store::migrate` (see the crate docs' "Schema
//! versioning" section).
//!
//! # Where a step lives
//!
//! This file holds the **ladder and nothing that can grow**: the index-ordered
//! [`MIGRATIONS`] array with its append point, [`SCHEMA_VERSION`],
//! [`create_latest_schema`], the shared probes, and the runner. Every step's
//! body lives in a submodule, grouped by what it does to the schema — so the
//! one file every migration PR must edit stays a table of contents rather
//! than a 1500-line accumulation (#3397).
//!
//! | Submodule | Steps | What they have in common |
//! |---|---|---|
//! | [`legacy_rebuild`] | v0 → v2 | §7 rebuilds retrofitting UNIQUE keys onto pre-versioning files |
//! | [`additive_tables`] | v2 → v7, v13 → v14, v19 → v21 | a new table appears; nothing existing changes shape |
//! | [`execution_plane`] | v7 → v10, v21 → v23 | new columns on `executions` and its per-execution facts |
//! | [`receipts`] | v10 → v13, v14 → v16 | the `context_blocks`/`step_manifest`/`step_receipt` plane |
//! | [`schema_removal`] | v16 → v17 | the one pure removal |
//! | [`live_tool_calls`], [`abandoned_state`], [`error_class`] | v17 → v18, v23 → v25 | `tool_calls` as a live projection and its `state`/`error_class` columns |
//! | [`token_unit`] | v18 → v19 | the one-token-rule reconciliation |
//! | [`pipeline_variant`] | v25 → v26 | the door/wrapper split |
//! | [`execution_role`] | v26 → v27 | the door/role split |
//!
//! A new step gets a new module (or joins the group whose shape it shares),
//! declares itself here, and takes the next slot in the ladder. It does not
//! get an inline function: an inline body is how this file reached the
//! ratchet the first time.

use rusqlite::{Connection, TransactionBehavior, params};

use crate::ddl::{
    AGENT_USES_DDL, CONTEXT_BLOCKS_DDL, EXECUTION_REFLECTION_DDL, EXECUTIONS_DDL, FORGOTTEN_DDL,
    FOUNDRY_TOOLS_DDL, MCP_USAGE_DDL, MEMORY_CITATIONS_DDL, PULL_REQUESTS_DDL, REFLECTIONS_DDL,
    RULES_TABLE, SESSION_TURN_DIFFS_DDL, SKILL_USAGE_DDL, STEP_MANIFEST_DDL, STEP_RECEIPT_DDL,
    TABLES, TASKS_DDL, TELEMETRY_INDEX, TOOL_CALLS_INDEXES, UNCHANGED_TABLES, events_ddl,
    files_touched_ddl, telemetry_ddl, tool_calls_ddl,
};
use crate::{Result, StoreError};

mod abandoned_state;
mod additive_tables;
mod error_class;
mod execution_plane;
mod execution_role;
mod legacy_rebuild;
mod live_tool_calls;
mod pipeline_variant;
pub(crate) mod pragmas;
mod receipts;
mod schema_removal;
mod token_unit;

// `Durability` is deliberately NOT re-exported alongside the runner: its only
// non-test reader is `initialize_store_pragmas` itself, so a re-export here is
// unused outside `cfg(test)` and fails the `-D warnings` gate. Its one test
// names it through the module path instead.
pub(crate) use pragmas::initialize_store_pragmas;

use additive_tables::{
    migrate_v2_to_v3, migrate_v3_to_v4, migrate_v4_to_v5, migrate_v5_to_v6, migrate_v6_to_v7,
    migrate_v13_to_v14, migrate_v19_to_v20, migrate_v20_to_v21,
};
use error_class::migrate_v24_to_v25;
use execution_plane::{
    migrate_v7_to_v8, migrate_v8_to_v9, migrate_v9_to_v10, migrate_v21_to_v22, migrate_v22_to_v23,
};
use execution_role::migrate_v26_to_v27;
pub(crate) use execution_role::{ROLE_KINDS, SYSTEM_NON_DOOR};
use legacy_rebuild::{migrate_v0_to_v1, migrate_v1_to_v2};
use live_tool_calls::migrate_v17_to_v18;
use pipeline_variant::migrate_v25_to_v26;
use receipts::{
    migrate_v10_to_v11, migrate_v11_to_v12, migrate_v12_to_v13, migrate_v14_to_v15,
    migrate_v15_to_v16,
};
use schema_removal::migrate_v16_to_v17;
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
pub(crate) const MIGRATIONS: [Migration; 27] = [
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
    // v19 → v20: the additive `foundry_tools` table — the tool-foundry
    // adoption ledger (#830). Purely additive; no existing table is touched,
    // so no rebuild, no dedupe, no backfill. A file that predates it simply
    // has no adoptions, which is the correct starting state: every
    // self-authored tool already on disk becomes unadopted, and therefore
    // withheld, until someone runs `stella tools --adopt` on it.
    migrate_v19_to_v20,
    // v20 → v21: the additive `session_turn_diffs` table — per-turn workspace
    // diffs precomputed from the work journal (#1870). Purely additive; a
    // file that predates it simply has no replayable turn diffs, which is the
    // correct starting state: the marks those diffs are computed from were
    // only ever written going forward too.
    migrate_v20_to_v21,
    // v21 → v22: `executions` grows `journal_era` — which compaction-journaling
    // era wrote this row's events (#1981). Additive, column-guarded ADD COLUMN;
    // every existing row backfills to era 0, which is exactly what it is: a
    // journal written before compaction recorded its replacement bytes.
    migrate_v21_to_v22,
    // v22 → v23: `execution_reflection` grows `parse_error` — the excerpt of a
    // reflection response the lesson parser could not read (#2175). Additive,
    // column-guarded ADD COLUMN, nullable: NULL means "no parse failure
    // recorded", which is exactly what every pre-existing row means. There is
    // no backfill to do — the fact was only ever printed to stderr before, so
    // it does not exist anywhere to recover.
    migrate_v22_to_v23,
    // v23 → v24: `tool_calls.state` gains `'abandoned'` — a call whose turn
    // ended before it returned stops masquerading as a tool error (#3146). A
    // §7 rebuild (the CHECK constraint cannot be altered in place, and fresh
    // v18..=v23 files carry one that would reject the new value), with the
    // copy doubling as the backfill from the stable abandonment sentinel.
    abandoned_state::migrate_v23_to_v24,
    // v24 → v25: `tool_calls` grows `error_class` — which kind of failure a
    // failed call was (#3145), projected from `ToolOutput::Error.class`.
    // Additive, column-guarded ADD COLUMN, defaulting to `''` = unclassified,
    // which is what every pre-existing row is. No backfill: the class of an
    // older failure exists nowhere but its prose, and classifying by string
    // match is the practice this column replaces.
    migrate_v24_to_v25,
    // v25 → v26: `executions.kind` becomes the door and only the door, and
    // which wrapper ran moves to `executions.pipeline_variant` (#3388).
    // Additive column plus a narrow backfill; see the module's own doc.
    migrate_v25_to_v26,
    // v26 → v27: `executions.kind` stops carrying the five standalone
    // system-call ROLE values, which move to `executions.role` (#3395). The
    // same defect as v25 → v26 from the third direction; see the module doc.
    migrate_v26_to_v27,
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
    //   v19 → v20: CLAIMED above by the tool-foundry adoption ledger (#830).
    //
    //   v20 → v21: CLAIMED above by the per-turn workspace diffs (#1870).
    //
    //   v21 → v22: CLAIMED above by the journal-era stamp (#1981).
    //
    //   v22 → v23: CLAIMED above by the reflection parse-failure record.
    //
    //   v23 → v24: CLAIMED above by the abandoned-call state (#3146).
    //
    //   v24 → v25: CLAIMED above by `tool_calls.error_class` (#3145).
    //
    //   v25 → v26: CLAIMED above by the door/wrapper split (#3388).
    //
    //   v26 → v27: CLAIMED above by the door/role split (#3395).
    // Nothing is reserved now: take v27 → v28 and add your own line here.
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
    tx.execute_batch(&tool_calls_ddl("tool_calls"))?;
    tx.execute_batch(TOOL_CALLS_INDEXES)?;
    tx.execute_batch(EXECUTION_REFLECTION_DDL)?;
    tx.execute_batch(REFLECTIONS_DDL)?;
    tx.execute_batch(TASKS_DDL)?;
    tx.execute_batch(PULL_REQUESTS_DDL)?;
    tx.execute_batch(CONTEXT_BLOCKS_DDL)?;
    tx.execute_batch(STEP_MANIFEST_DDL)?;
    tx.execute_batch(STEP_RECEIPT_DDL)?;
    tx.execute_batch(FORGOTTEN_DDL)?;
    tx.execute_batch(FOUNDRY_TOOLS_DDL)?;
    tx.execute_batch(SESSION_TURN_DIFFS_DDL)?;
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
