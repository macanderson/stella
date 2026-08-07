//! Retention for `store.db` (#616) — the deletion path the source-of-truth
//! store never had.
//!
//! `usage.db` (the derived, content-free rollup) has had a retention engine
//! since its first release ([`crate::usage::UsageStore::prune`]); `store.db`
//! — which holds the full prompts, the complete event payloads, tool
//! arguments, and context-block preimages — grew forever. This module is the
//! mirror-image engine, with two differences the schema forces:
//!
//! 1. **There are no foreign keys.** `store.db`'s DDL declares no
//!    `REFERENCES`/`ON DELETE CASCADE` anywhere (verify: `rg REFERENCES
//!    stella-store/src/ddl.rs` finds nothing), so `PRAGMA foreign_keys=ON` in
//!    [`Store::init`](crate::Store) enforces exactly zero constraints here.
//!    Deleting an `executions` row therefore cascades **explicitly**, through
//!    [`DEPENDENT_TABLES`]. The `dependent_tables_cover_every_execution_keyed_table`
//!    test below keeps that list honest against the schema. (Named, not
//!    linked: it is `#[cfg(test)]`, so rustdoc cannot resolve it.)
//! 2. **The unit of retention is the execution, not the row.** Every
//!    dependent table keys off `executions.id`, so an execution is the
//!    smallest thing that can be dropped without orphaning rows.
//!
//! # The replication guard
//!
//! `store.db` replicates its `telemetry` rows into `usage.db` with a cursor:
//! the hub stores the highest already-shipped source `telemetry.rowid` in
//! `telemetry_sync_cursors.last_source_rowid`
//! ([`UsageStore::telemetry_cursor`](crate::usage::UsageStore::telemetry_cursor)),
//! and [`Store::replicate_telemetry_to_usage`](crate::Store::replicate_telemetry_to_usage)
//! ships everything above it. Dropping an execution whose telemetry sits
//! *above* that cursor destroys money data that was never rolled up, silently.
//! So [`StorePrunePolicy::telemetry_cursor`] carries the watermark in, and an
//! execution is prunable only when every telemetry row it owns is at or below
//! it — unless [`StorePrunePolicy::force`].
//!
//! # Why the cursor has to move when we delete
//!
//! `store.db`'s `telemetry` has no `INTEGER PRIMARY KEY` (its key is `UNIQUE
//! (execution_id, step)`), so its `rowid` is implicit — and an implicit rowid
//! is not delete-stable:
//!
//! - **A plain `DELETE` frees the top rowid.** Without `AUTOINCREMENT` SQLite
//!   hands `MAX(rowid) + 1` to the next insert, so deleting the highest
//!   telemetry row makes the *next real model call* land at or below a stale
//!   cursor and never replicate. This is not theoretical — it is what
//!   `a_prune_without_vacuum_rewinds_the_cursor_past_reusable_rowids` pins
//!   in this module's tests, with
//!   `a_prune_that_empties_telemetry_leaves_the_next_turn_replicable`
//!   covering it end to end in `stella-cli/tests/stats_prune_cli.rs`.
//! - **`VACUUM` is documented to renumber it.** SQLite's docs say `VACUUM`
//!   "may change the ROWIDs of entries in any tables that do not have an
//!   explicit INTEGER PRIMARY KEY". Empirically it currently does *not*
//!   (3.51 copies the b-tree through the xfer optimization, preserving
//!   rowids), but the contract is the docs, not the observed build.
//!
//! So the corrected watermark is never *computed* from a renumbering rule —
//! it is **measured** against the one key `VACUUM` cannot touch,
//! `(execution_id, step)`: take the surviving telemetry row with the greatest
//! `rowid <= cursor` before the reclaim, then read that same row's `rowid`
//! back afterwards. That is correct whether or not rowids moved, and it
//! degrades to `0` (re-ship the retained backlog, which the hub dedups on
//! `(project_id, source_rowid)`) rather than ever stepping over a row that
//! never shipped. It is the same shape as
//! [`UsageStore::prune`](crate::usage::UsageStore::prune)'s
//! `vacuum_and_reanchor`, with one structural difference: the cursor lives in
//! a *different database file* that `Store` has no handle to. So this module
//! computes the value into [`StorePruneReport::telemetry_cursor_after`] and
//! the caller — which owns both files — writes it back with
//! [`UsageStore::rewind_telemetry_cursor`](crate::usage::UsageStore::rewind_telemetry_cursor).
//! `stella stats prune` does exactly that.
//!
//! `telemetry.rowid` is the only `store.db` rowid anything persists outside
//! the file. The crate's remaining `rowid` references —
//! `memory_citation_stats`, `mcp_usage_stats`, `receipts::inspect`, and the
//! v0→v1 dedup migrations — are all `ORDER BY` tiebreaks, and any renumber
//! preserves relative order, so none of them constrain the reclaim.

use rusqlite::{OptionalExtension as _, Transaction, params};

use crate::{Result, Store};

/// Every table that keys off `executions.id` and dies with it. The list is
/// explicit because the schema declares no foreign keys — see the module docs.
///
/// Order matters only in that `executions` is deleted *last* (below), so the
/// prunable predicate still sees the spine while dependents are going away.
///
/// Deliberately **not** here: `reflections`. It is the durable home for
/// lessons and self-critiques and is meant to outlive the turn that produced
/// it — its `execution_id` is nullable precisely because a reflection need not
/// have a live execution (cross-turn lessons already carry NULL). Pruning a
/// 90-day-old turn should reclaim its transcript, not un-learn its lesson.
/// `enterprise_export_ledger`/`_skips` are likewise excluded: they are the
/// export audit trail keyed by `(sink_fingerprint, execution_id)`, and
/// `executions.id` is `AUTOINCREMENT` (ids are never reused), so a retained
/// ledger row can never be mistaken for a different execution.
pub const DEPENDENT_TABLES: [&str; 14] = [
    "events",
    "telemetry",
    "files_touched",
    "memory_citations",
    "mcp_usage",
    "agent_uses",
    "skill_usage",
    "tool_calls",
    "execution_reflection",
    "tasks",
    "context_blocks",
    "step_manifest",
    "step_receipt",
    // Turn diffs are replay material, the same class as `events`: pruning a
    // turn's transcript reclaims its diff with it. Rows whose `execution_id`
    // is NULL (a turn that ended without opening an execution) are outside
    // the execution-keyed cascade and survive, which is honest — nothing
    // proves which execution they belonged to.
    "session_turn_diffs",
];

/// A prune that deletes at least this many rows triggers an automatic
/// `VACUUM` to hand the reclaimed pages back to the filesystem (a small prune
/// leaves the freed pages in the file's freelist for reuse). `--vacuum`
/// forces it for any non-empty prune. Matches `usage.rs`'s threshold — the
/// two stores should not have surprisingly different reclaim behaviour.
const LARGE_PRUNE_ROWS: u64 = 10_000;

/// Scratch table holding one prune phase's target execution ids. The target
/// set MUST be materialized before any dependent row is deleted: the prunable
/// predicate reads `telemetry`, so deleting telemetry first would make the
/// remaining tables see a *different*, larger prunable set.
const TARGETS_TABLE: &str = "temp.store_prune_targets";

/// Retention knobs for [`Store::prune`]. Every field is opt-in; a policy with
/// neither an age window nor a ceiling is a no-op that deletes nothing.
#[derive(Debug, Clone, Default)]
pub struct StorePrunePolicy {
    /// A SQLite datetime modifier (e.g. `"-90 days"`); executions whose
    /// `started_at` predates `now + modifier` are dropped, with their
    /// dependent rows. `None` disables age pruning. The CLI builds this from
    /// `--older-than 90d` with the same vocabulary `stella usage prune` uses.
    pub older_than: Option<String>,
    /// Hard ceiling on retained `executions`; the oldest prunable executions
    /// are evicted until at/under it. `None` disables the ceiling.
    pub max_rows: Option<i64>,
    /// The hub's replication watermark for this workspace — the highest
    /// `telemetry.rowid` already shipped into `usage.db`
    /// ([`UsageStore::telemetry_cursor`](crate::usage::UsageStore::telemetry_cursor)).
    /// An execution owning telemetry above it is never dropped without
    /// `force`. `0` (the default) means "nothing has replicated", which is the
    /// safe reading: every execution that produced a model call is protected.
    pub telemetry_cursor: i64,
    /// Prune even executions with un-replicated telemetry, permanently losing
    /// that usage/cost data. Off by default; the guard above holds unless set.
    pub force: bool,
    /// `VACUUM` after pruning to reclaim file bytes (also happens
    /// automatically for a large prune).
    pub vacuum: bool,
    /// Compute the report without deleting anything (rolls the transaction
    /// back and skips `VACUUM`).
    pub dry_run: bool,
}

impl StorePrunePolicy {
    /// Whether this policy would do nothing at all — the CLI turns this into
    /// a helpful error rather than a silent success.
    pub fn is_noop(&self) -> bool {
        self.older_than.is_none() && self.max_rows.is_none()
    }
}

/// What [`Store::prune`] did (or, under `dry_run`, would do).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorePruneReport {
    /// `executions` rows removed by the age cutoff.
    pub aged_out: u64,
    /// `executions` rows evicted to satisfy the row ceiling.
    pub ceiling_evicted: u64,
    /// Rows removed from the [`DEPENDENT_TABLES`] cascade (all phases).
    pub dependent_rows: u64,
    /// `telemetry` rows removed — the subset of `dependent_rows` that carried
    /// per-call token and cost data. Called out separately because it is the
    /// only class of row here that another database was tracking.
    pub telemetry_pruned: u64,
    /// Executions inside the age window that were kept because they still
    /// own un-replicated telemetry (the row-ceiling phase reports its own
    /// residual as `still_over_ceiling` instead). Non-zero means
    /// "run `stella usage sync`, then re-run" — or `--force` to accept the
    /// loss. Only counted without `force`.
    pub protected_unreplicated: u64,
    /// Executions still above the ceiling after eviction, for the same
    /// reason.
    pub still_over_ceiling: u64,
    /// Whether `VACUUM` ran.
    pub vacuumed: bool,
    /// The replication watermark the caller must write back to `usage.db`,
    /// when pruning invalidated the one it passed in — see the module docs.
    /// `None` when nothing needs to change (no telemetry deleted, or no
    /// cursor to correct).
    pub telemetry_cursor_after: Option<i64>,
}

impl StorePruneReport {
    /// Total rows removed across every table.
    pub fn total_rows(&self) -> u64 {
        self.aged_out + self.ceiling_evicted + self.dependent_rows
    }
}

impl Store {
    /// Bound `store.db`'s growth per a [`StorePrunePolicy`] — the engine
    /// behind `stella stats prune`. Runs, in order: age cutoff → row ceiling,
    /// both in one transaction, then (optionally) `VACUUM`.
    ///
    /// Deleting an execution explicitly deletes its rows in all 13
    /// [`DEPENDENT_TABLES`] — the schema declares no foreign keys, so nothing
    /// cascades on its own.
    ///
    /// Telemetry that has not yet replicated into `usage.db` is never
    /// destroyed without `force`; when a prune *does* invalidate the hub's
    /// cursor, [`StorePruneReport::telemetry_cursor_after`] carries the
    /// corrected value for the caller to persist. See the module docs for
    /// both mechanisms.
    ///
    /// `dry_run` computes the same report but rolls the transaction back and
    /// skips `VACUUM`, so nothing is deleted.
    pub fn prune(&self, policy: &StorePrunePolicy) -> Result<StorePruneReport> {
        let mut report = StorePruneReport::default();
        if policy.is_noop() {
            return Ok(report);
        }
        let mut conn = self.lock();
        // The surviving telemetry row that still marks the replication
        // watermark, addressed by the `VACUUM`-stable `(execution_id, step)`
        // key rather than by its rowid. Captured inside the transaction (once
        // the deletes are done, before any reclaim) and resolved back to a
        // rowid afterwards — see the module docs. `None` means nothing at or
        // below the cursor survived, i.e. the watermark resets to 0.
        let mut watermark_key: Option<(i64, i64)> = None;

        // The "safe to drop" predicate, correlated to the outer `executions`
        // row: an execution is prunable only when it owns no telemetry row
        // above the hub's replication cursor. `force` collapses it to
        // "everything".
        let prunable = if policy.force {
            "1".to_string()
        } else {
            format!(
                "NOT EXISTS (SELECT 1 FROM telemetry t \
                   WHERE t.execution_id = executions.id AND t.rowid > {})",
                policy.telemetry_cursor
            )
        };

        {
            let tx = conn.transaction()?;
            tx.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS {TARGETS_TABLE} (id INTEGER PRIMARY KEY);
                 DELETE FROM {TARGETS_TABLE};"
            ))?;

            // 1) Age cutoff over `executions.started_at` (the row's only
            //    non-null clock — `finished_at` is NULL for a turn that never
            //    landed, which is exactly the kind of debris retention exists
            //    to reclaim).
            if let Some(modifier) = &policy.older_than {
                let aged = "julianday(executions.started_at) < julianday('now', ?1)";
                if !policy.force {
                    report.protected_unreplicated += tx.query_row(
                        &format!(
                            "SELECT COUNT(*) FROM executions WHERE {aged} AND NOT ({prunable})"
                        ),
                        params![modifier],
                        |r| r.get::<_, i64>(0),
                    )? as u64;
                }
                tx.execute(
                    &format!(
                        "INSERT OR IGNORE INTO {TARGETS_TABLE} (id) \
                         SELECT id FROM executions WHERE {aged} AND ({prunable})"
                    ),
                    params![modifier],
                )?;
                let phase = delete_targets(&tx)?;
                report.aged_out += phase.executions;
                report.dependent_rows += phase.dependents;
                report.telemetry_pruned += phase.telemetry;
            }

            // 2) Hard ceiling on `executions` — evict the oldest prunable ones
            //    until at/under it. `id` is AUTOINCREMENT, so ascending id is
            //    oldest-first and is stable even if two turns share a
            //    `started_at` second.
            if let Some(max_rows) = policy.max_rows {
                let total: i64 =
                    tx.query_row("SELECT COUNT(*) FROM executions", [], |r| r.get(0))?;
                if total > max_rows {
                    let excess = total - max_rows;
                    tx.execute(
                        &format!(
                            "INSERT OR IGNORE INTO {TARGETS_TABLE} (id) \
                             SELECT id FROM executions WHERE {prunable} \
                             ORDER BY id ASC LIMIT ?1"
                        ),
                        params![excess],
                    )?;
                    let phase = delete_targets(&tx)?;
                    report.ceiling_evicted += phase.executions;
                    report.dependent_rows += phase.dependents;
                    report.telemetry_pruned += phase.telemetry;
                    let after: i64 =
                        tx.query_row("SELECT COUNT(*) FROM executions", [], |r| r.get(0))?;
                    if after > max_rows {
                        report.still_over_ceiling = (after - max_rows) as u64;
                    }
                }
            }

            if policy.dry_run {
                // Roll back: `tx` drops un-committed, so nothing is deleted
                // and the caller sees only what *would* have happened. No
                // cursor correction either — the cursor is only wrong once
                // rows are actually gone.
                return Ok(report);
            }

            // Address the surviving watermark row by its VACUUM-stable key
            // while the transaction still holds the post-delete state. Only
            // deleting telemetry can invalidate the caller's cursor, so a
            // prune that touched none leaves it alone.
            if policy.telemetry_cursor > 0 && report.telemetry_pruned > 0 {
                watermark_key = tx
                    .query_row(
                        "SELECT execution_id, step FROM telemetry \
                         WHERE rowid <= ?1 ORDER BY rowid DESC LIMIT 1",
                        params![policy.telemetry_cursor],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                // `None` from that query is a real answer — nothing at or
                // below the cursor survived, so the watermark resets to 0 —
                // and not "no correction needed". Seed the report here so the
                // two cases stay distinguishable; step 4 refines it.
                report.telemetry_cursor_after = Some(0);
            }

            tx.execute_batch(&format!("DROP TABLE IF EXISTS {TARGETS_TABLE};"))?;
            tx.commit()?;
        }

        // 3) Reclaim file bytes on a large (or explicitly requested) prune.
        let deleted = report.aged_out + report.ceiling_evicted + report.dependent_rows;
        if deleted > 0 && (policy.vacuum || deleted >= LARGE_PRUNE_ROWS) {
            conn.execute_batch("VACUUM")?;
            report.vacuumed = true;
        }

        // 4) Read the watermark row's rowid back — AFTER any VACUUM, so the
        //    answer reflects whatever the reclaim did to the rowid space. A
        //    watermark row that did not survive resolves to 0, which re-ships
        //    the retained backlog instead of skipping anything.
        if let Some((execution_id, step)) = watermark_key {
            report.telemetry_cursor_after = Some(
                conn.query_row(
                    "SELECT rowid FROM telemetry WHERE execution_id = ?1 AND step = ?2",
                    params![execution_id, step],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0),
            );
        }
        Ok(report)
    }
}

/// Row counts from deleting one phase's materialized target set.
#[derive(Default)]
struct PhaseCounts {
    executions: u64,
    dependents: u64,
    telemetry: u64,
}

/// Delete every execution currently in [`TARGETS_TABLE`] plus all of its
/// dependent rows, then empty the target table for the next phase.
///
/// `telemetry` is counted out separately as well as into the dependent total:
/// it is the only class of row here another database was tracking, so the
/// caller needs to know whether this prune touched any.
fn delete_targets(tx: &Transaction<'_>) -> Result<PhaseCounts> {
    let mut counts = PhaseCounts::default();
    for table in DEPENDENT_TABLES {
        let removed = tx.execute(
            &format!(
                "DELETE FROM {table} \
                 WHERE execution_id IN (SELECT id FROM {TARGETS_TABLE})"
            ),
            [],
        )? as u64;
        counts.dependents += removed;
        if table == "telemetry" {
            counts.telemetry = removed;
        }
    }
    // The spine goes last: the prunable predicate above reads through it.
    counts.executions = tx.execute(
        &format!("DELETE FROM executions WHERE id IN (SELECT id FROM {TARGETS_TABLE})"),
        [],
    )? as u64;
    tx.execute(&format!("DELETE FROM {TARGETS_TABLE}"), [])?;
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TelemetryRow;

    /// A backdated execution carrying one row in EVERY dependent table, so a
    /// missed table in the cascade shows up as a survivor.
    fn seeded_execution(store: &Store, prompt: &str, started_at: &str) -> i64 {
        let id = store
            .begin_execution("goal", prompt, "zai", "glm-5.2")
            .unwrap();
        seed_dependent_rows(store, id);
        store
            .lock()
            .execute(
                "UPDATE executions SET started_at = ?1 WHERE id = ?2",
                params![started_at, id],
            )
            .unwrap();
        id
    }

    /// Put one row in each [`DEPENDENT_TABLES`] entry for `execution_id`,
    /// driving the column list off `PRAGMA table_info` rather than a
    /// hand-written INSERT per table: a column (or a whole table) added later
    /// is covered automatically, which is the point of the cascade test.
    fn seed_dependent_rows(store: &Store, execution_id: i64) {
        let conn = store.lock();
        for table in DEPENDENT_TABLES {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let cols: Vec<(String, String, bool, Option<String>)> = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)? != 0,
                        r.get::<_, Option<String>>(4)?,
                    ))
                })
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();

            let mut names = vec!["execution_id".to_string()];
            let mut values = vec![execution_id.to_string()];
            for (name, ty, notnull, default) in cols {
                // Everything else either has a default or is nullable; only
                // NOT NULL-without-default columns need a value invented.
                if name == "execution_id" || !notnull || default.is_some() {
                    continue;
                }
                names.push(name);
                values.push(match ty.to_ascii_uppercase().as_str() {
                    "TEXT" => "'x'".to_string(),
                    "REAL" => "0.0".to_string(),
                    // The execution id, not a constant: a table with a
                    // UNIQUE key over invented columns (`session_turn_diffs`'
                    // `(session_id, turn)`) must seed distinct rows per
                    // execution, or the second seed collides.
                    _ => execution_id.to_string(),
                });
            }
            conn.execute(
                &format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    names.join(", "),
                    values.join(", ")
                ),
                [],
            )
            .unwrap_or_else(|e| panic!("seeding {table} failed: {e}"));
        }
    }

    fn telemetry_row(step: u64) -> TelemetryRow {
        TelemetryRow {
            step,
            provider: "zai".into(),
            call_role: "worker".into(),
            model: "glm-5.2".into(),
            input_tokens: 100,
            estimated_input_tokens: 90,
            output_tokens: 10,
            cache_read_tokens: 0,
            cache_miss_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: 0.01,
            duration_ms: 10,
            retries: 0,
            tool_calls: 0,
            usage_complete: true,
        }
    }

    fn rows_in(store: &Store, table: &str) -> i64 {
        store
            .lock()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    /// The premise the whole module rests on: `store.db` declares no foreign
    /// keys, so `PRAGMA foreign_keys=ON` in `Store::init` buys nothing here and
    /// the cascade in [`delete_targets`] has to be explicit. If a migration
    /// ever adds real `ON DELETE CASCADE` constraints, this fails and the
    /// explicit deletes can be reconsidered.
    #[test]
    fn store_declares_no_foreign_key_cascades() {
        let store = Store::in_memory().unwrap();
        let conn = store.lock();
        let on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(on, 1, "enforcement is on — it just has nothing to enforce");
        for table in DEPENDENT_TABLES {
            let mut stmt = conn
                .prepare(&format!("PRAGMA foreign_key_list({table})"))
                .unwrap();
            let fks = stmt.query_map([], |_| Ok(())).unwrap().count();
            assert_eq!(
                fks, 0,
                "{table} declares a foreign key — the cascade story changed"
            );
        }
    }

    /// Drift guard: every table in the store that carries an `execution_id`
    /// column must either be in the cascade or be a documented carve-out. A
    /// new execution-keyed table added without touching this list would
    /// otherwise silently outlive its executions forever.
    #[test]
    fn dependent_tables_cover_every_execution_keyed_table() {
        /// See [`DEPENDENT_TABLES`] — the durable lessons store deliberately
        /// outlives the turn that produced it.
        const CARVE_OUTS: [&str; 1] = ["reflections"];

        let store = Store::in_memory().unwrap();
        let conn = store.lock();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        for table in tables {
            if table.starts_with("sqlite_") || table.starts_with("enterprise_export_") {
                continue;
            }
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let keyed = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .any(|c| c.as_deref() == Ok("execution_id"));
            if !keyed {
                continue;
            }
            assert!(
                DEPENDENT_TABLES.contains(&table.as_str()) || CARVE_OUTS.contains(&table.as_str()),
                "`{table}` keys off execution_id but is not in DEPENDENT_TABLES — \
                 add it to the cascade, or to CARVE_OUTS with a reason"
            );
        }
    }

    #[test]
    fn age_prune_drops_old_executions_and_every_dependent_row() {
        let store = Store::in_memory().unwrap();
        let old = seeded_execution(&store, "ancient", "2000-01-01 00:00:00");
        let fresh = seeded_execution(&store, "today", "2999-01-01 00:00:00");

        for table in DEPENDENT_TABLES {
            assert_eq!(rows_in(&store, table), 2, "{table} seeded for both");
        }

        let report = store
            .prune(&StorePrunePolicy {
                older_than: Some("-1 days".into()),
                // The seeded telemetry rows are rowid 1 (old) and 2 (fresh);
                // both are "already replicated" so the guard is satisfied.
                telemetry_cursor: 2,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(report.aged_out, 1, "only the ancient execution ages out");
        assert_eq!(
            report.dependent_rows,
            DEPENDENT_TABLES.len() as u64,
            "one row per dependent table went with it"
        );
        assert_eq!(report.telemetry_pruned, 1);
        assert_eq!(rows_in(&store, "executions"), 1);

        // The cascade assertion: nothing keyed to the dropped execution may
        // survive, and nothing keyed to the surviving one may be touched.
        for table in DEPENDENT_TABLES {
            let orphans: i64 = store
                .lock()
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE execution_id = ?1"),
                    params![old],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(orphans, 0, "{table} kept rows for the pruned execution");
            let kept: i64 = store
                .lock()
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE execution_id = ?1"),
                    params![fresh],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(kept, 1, "{table} lost the retained execution's row");
        }
    }

    /// The carve-out, pinned: pruning a turn reclaims its transcript but must
    /// not un-learn the lesson it produced.
    #[test]
    fn age_prune_keeps_reflections() {
        let store = Store::in_memory().unwrap();
        let old = seeded_execution(&store, "ancient", "2000-01-01 00:00:00");
        store
            .lock()
            .execute(
                "INSERT INTO reflections (execution_id, kind, content, occurred_at) \
                 VALUES (?1, 'lesson', 'always read the test first', 0)",
                params![old],
            )
            .unwrap();

        store
            .prune(&StorePrunePolicy {
                older_than: Some("-1 days".into()),
                telemetry_cursor: 1,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(rows_in(&store, "executions"), 0);
        assert_eq!(rows_in(&store, "reflections"), 1, "the lesson outlives it");
    }

    #[test]
    fn unreplicated_telemetry_is_spared_unless_forced() {
        let store = Store::in_memory().unwrap();
        let old = seeded_execution(&store, "ancient", "2000-01-01 00:00:00");
        // A second telemetry row for the same ancient execution, recorded
        // after the hub last synced — rowid 2, above the cursor.
        store.record_telemetry(old, &telemetry_row(2)).unwrap();
        assert_eq!(rows_in(&store, "telemetry"), 2);

        let report = store
            .prune(&StorePrunePolicy {
                older_than: Some("-1 days".into()),
                telemetry_cursor: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            report.aged_out, 0,
            "un-replicated telemetry blocks the drop"
        );
        assert_eq!(report.protected_unreplicated, 1);
        assert_eq!(rows_in(&store, "executions"), 1);
        assert_eq!(rows_in(&store, "events"), 1, "dependents untouched too");

        // --force accepts the loss.
        let report = store
            .prune(&StorePrunePolicy {
                older_than: Some("-1 days".into()),
                telemetry_cursor: 1,
                force: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 1);
        assert_eq!(report.telemetry_pruned, 2);
        assert_eq!(rows_in(&store, "executions"), 0);
        assert_eq!(rows_in(&store, "telemetry"), 0);
    }

    /// A never-synced workspace (cursor 0) is the maximally conservative
    /// reading: every execution that produced a model call is protected.
    #[test]
    fn a_zero_cursor_protects_every_execution_with_telemetry() {
        let store = Store::in_memory().unwrap();
        seeded_execution(&store, "ancient", "2000-01-01 00:00:00");
        let report = store
            .prune(&StorePrunePolicy {
                older_than: Some("-1 days".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 0);
        assert_eq!(report.protected_unreplicated, 1);
        assert_eq!(rows_in(&store, "executions"), 1);
    }

    #[test]
    fn dry_run_reports_without_deleting() {
        let store = Store::in_memory().unwrap();
        seeded_execution(&store, "ancient", "2000-01-01 00:00:00");
        seeded_execution(&store, "also ancient", "2000-01-02 00:00:00");

        let report = store
            .prune(&StorePrunePolicy {
                older_than: Some("-1 days".into()),
                telemetry_cursor: 2,
                vacuum: true, // ignored under dry_run
                dry_run: true,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(report.aged_out, 2, "dry run reports what it would drop");
        assert_eq!(report.dependent_rows, 2 * DEPENDENT_TABLES.len() as u64);
        assert!(!report.vacuumed, "dry run never vacuums");
        assert_eq!(
            report.telemetry_cursor_after, None,
            "nothing was deleted, so the hub cursor is still correct"
        );
        assert_eq!(rows_in(&store, "executions"), 2, "dry run deletes nothing");
        for table in DEPENDENT_TABLES {
            assert_eq!(rows_in(&store, table), 2, "{table} untouched by a dry run");
        }
    }

    #[test]
    fn ceiling_evicts_oldest_executions_first() {
        let store = Store::in_memory().unwrap();
        // Four executions, all recent — only the ceiling can reach them.
        let ids: Vec<i64> = (0..4)
            .map(|n| seeded_execution(&store, &format!("turn {n}"), "2999-01-01 00:00:00"))
            .collect();

        let report = store
            .prune(&StorePrunePolicy {
                max_rows: Some(2),
                telemetry_cursor: 4,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(report.ceiling_evicted, 2);
        assert_eq!(report.still_over_ceiling, 0);
        assert_eq!(report.dependent_rows, 2 * DEPENDENT_TABLES.len() as u64);

        let survivors: Vec<i64> = store
            .lock()
            .prepare("SELECT id FROM executions ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(survivors, vec![ids[2], ids[3]], "the OLDEST two went");
    }

    #[test]
    fn ceiling_reports_residual_when_the_guard_blocks_eviction() {
        let store = Store::in_memory().unwrap();
        for n in 0..3 {
            seeded_execution(&store, &format!("turn {n}"), "2999-01-01 00:00:00");
        }
        // Only the first execution's telemetry has replicated.
        let report = store
            .prune(&StorePrunePolicy {
                max_rows: Some(1),
                telemetry_cursor: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.ceiling_evicted, 1, "only the replicated one evicts");
        assert_eq!(report.still_over_ceiling, 1);
        assert_eq!(rows_in(&store, "executions"), 2);
    }

    #[test]
    fn an_empty_policy_is_a_no_op() {
        let store = Store::in_memory().unwrap();
        seeded_execution(&store, "ancient", "2000-01-01 00:00:00");
        let report = store.prune(&StorePrunePolicy::default()).unwrap();
        assert_eq!(report, StorePruneReport::default());
        assert_eq!(report.total_rows(), 0);
        assert_eq!(rows_in(&store, "executions"), 1);
    }

    /// The `VACUUM` path's contract: the watermark the report hands back must
    /// surface EXACTLY the rows that never replicated — none stranded (silent
    /// loss of cost data), none needlessly re-shipped. It is measured against
    /// the `(execution_id, step)` key rather than derived from a renumbering
    /// rule, so it holds whether or not this SQLite build moves rowids.
    ///
    /// File-backed: `VACUUM` on `:memory:` has nothing to compact.
    #[test]
    fn vacuum_reanchors_the_replication_cursor_onto_the_surviving_watermark_row() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();

        // Replicated (rowids 1, 2): one ancient — pruned — and one recent that
        // survives and therefore still carries the watermark.
        seeded_execution(&store, "ancient", "2000-01-01 00:00:00");
        seeded_execution(&store, "replicated but recent", "2999-01-01 00:00:00");
        // Never replicated (rowids 3, 4).
        let fresh_a = seeded_execution(&store, "fresh a", "2999-01-02 00:00:00");
        let fresh_b = seeded_execution(&store, "fresh b", "2999-01-03 00:00:00");
        assert_eq!(rows_in(&store, "telemetry"), 4);

        let report = store
            .prune(&StorePrunePolicy {
                older_than: Some("-1 days".into()),
                telemetry_cursor: 2,
                vacuum: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 1);
        assert!(report.vacuumed);

        let cursor = report
            .telemetry_cursor_after
            .expect("pruning telemetry invalidates the caller's cursor");
        let owners: Vec<i64> = store
            .telemetry_rows_after(cursor, 10)
            .unwrap()
            .iter()
            .map(|r| r.execution_id)
            .collect();
        assert_eq!(
            owners,
            vec![fresh_a, fresh_b],
            "the re-anchored cursor surfaces every un-replicated row and nothing else"
        );
    }

    /// When nothing at or below the cursor survives the prune, the watermark
    /// has no anchor left and must reset to 0 — re-shipping the (empty)
    /// backlog rather than stepping over rows that never shipped.
    #[test]
    fn a_vanished_watermark_row_resets_the_cursor_to_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        seeded_execution(&store, "ancient a", "2000-01-01 00:00:00");
        seeded_execution(&store, "ancient b", "2000-01-02 00:00:00");
        let fresh = seeded_execution(&store, "fresh", "2999-01-01 00:00:00");

        let report = store
            .prune(&StorePrunePolicy {
                older_than: Some("-1 days".into()),
                telemetry_cursor: 2,
                vacuum: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 2);
        assert_eq!(report.telemetry_cursor_after, Some(0));

        let owners: Vec<i64> = store
            .telemetry_rows_after(0, 10)
            .unwrap()
            .iter()
            .map(|r| r.execution_id)
            .collect();
        assert_eq!(owners, vec![fresh]);
    }

    /// The hazard that needs no `VACUUM` at all: deleting the top `telemetry`
    /// rowid frees it, and SQLite (no `AUTOINCREMENT` on that table) hands it
    /// straight to the next model call — which would land at or below a stale
    /// cursor and never replicate. The rewound cursor is what keeps it visible.
    #[test]
    fn a_prune_without_vacuum_rewinds_the_cursor_past_reusable_rowids() {
        let store = Store::in_memory().unwrap();
        seeded_execution(&store, "ancient a", "2000-01-01 00:00:00");
        seeded_execution(&store, "ancient b", "2000-01-02 00:00:00");

        let report = store
            .prune(&StorePrunePolicy {
                older_than: Some("-1 days".into()),
                telemetry_cursor: 2,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 2);
        assert!(!report.vacuumed, "a small prune does not vacuum");
        assert_eq!(
            report.telemetry_cursor_after,
            Some(0),
            "the table is empty, so no cursor above 0 can be honoured"
        );

        // A new turn's telemetry reuses rowid 1; at the clamped cursor it is
        // still visible to the replicator. At the stale cursor it would not be.
        let next = store
            .begin_execution("goal", "next turn", "zai", "glm-5.2")
            .unwrap();
        store.record_telemetry(next, &telemetry_row(0)).unwrap();
        assert!(
            store.telemetry_rows_after(2, 10).unwrap().is_empty(),
            "the stale cursor hides the replayed rowid"
        );
        assert_eq!(store.telemetry_rows_after(0, 10).unwrap().len(), 1);
    }
}
