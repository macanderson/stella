//! Bounding `store.db`'s growth — the engine behind `stella storage prune`.
//!
//! ## Why this exists
//!
//! `store.db` grows one row per event, per model call, per tool call, per
//! context block, forever. `executions.prompt` holds the full prompt text and
//! `events.payload` the full event payload, so it is also the tier that holds
//! the actual content — not a rollup of it.
//!
//! Its sibling [`crate::usage`] — a *derived, content-free* hub — has had an
//! age cutoff, a row ceiling, project GC, `VACUUM`, dry-run, and a CLI verb
//! since #520. `store.db` had none of it. That asymmetry pointed the wrong
//! way: the derived copy could be bounded and the source of truth holding the
//! prompts could not, so the only way a user could reclaim that space (or
//! shed that content) was to delete the file and lose everything.
//!
//! ## The unit of retention is the execution
//!
//! `executions` is the spine every other table keys off. Pruning means
//! choosing a set of execution ids and deleting them together with every row
//! that hangs off them, in one transaction. A partial cascade would leave
//! orphaned events pointing at a missing turn, which reads back as corruption.
//!
//! ## What this will never delete
//!
//! Retention on a source of truth has to be conservative, so three classes of
//! row are excluded by construction rather than by policy:
//!
//! - **`forgotten`** — the tombstones. A tombstone is the *record that
//!   something was forgotten*; dropping one un-forgets that content on the
//!   next re-mine. It is append-only by design (see [`crate::forget`]) and
//!   retention does not touch it, at any age, even under `--force`.
//! - **Unfinished executions** (`finished_at IS NULL`) — an in-flight turn.
//!   Age says nothing useful about a row that is still being written.
//! - **Executions with a pending enterprise export** — the exact analogue of
//!   the hub's cloud-drain guard. An execution the sink has not spooled yet
//!   is dropped only under `force`.
//!
//! Not execution-scoped and so out of scope entirely: `rules`, `graph_nodes`,
//! `graph_edges`, `file_locks`, `pull_requests`. The `enterprise_export_*`
//! bookkeeping tables are left alone too — they have their own compaction
//! ([`crate::Store::compact_enterprise_export_ledger`]), and a ledger row
//! outliving its execution is inert.
//!
//! ## Never automatic
//!
//! There is no sweep on open and no background daemon. Retention runs when a
//! user asks for it, needs at least one explicit knob, and supports
//! `dry_run` so the report can be read before anything is deleted.

use rusqlite::params;

use crate::{Result, Store, StoreError};

/// Every table keyed on a NOT NULL `execution_id`, deleted as part of an
/// execution's cascade.
///
/// `reflections` is deliberately absent: its `execution_id` is nullable
/// (cross-turn reflections carry NULL) so it needs an `IS NOT NULL` guard and
/// is handled separately. `tasks` is present — a task board belongs to the
/// turn that produced it.
const CASCADE_TABLES: &[&str] = &[
    "events",
    "files_touched",
    "telemetry",
    "memory_citations",
    "agent_uses",
    "skill_usage",
    "mcp_usage",
    "tool_calls",
    "execution_reflection",
    "context_blocks",
    "step_manifest",
    "step_receipt",
    "tasks",
];

/// Row count past which a non-empty prune runs `VACUUM` on its own, so a big
/// reclamation actually returns bytes to the filesystem instead of leaving a
/// sparse file. Mirrors the hub's `LARGE_PRUNE_ROWS`.
const LARGE_PRUNE_EXECUTIONS: u64 = 5_000;

/// Retention knobs for [`Store::prune`]. Every field is opt-in; a policy with
/// neither `older_than` nor `max_executions` is rejected rather than silently
/// doing nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// A SQLite datetime modifier (e.g. `"-90 days"`); executions that
    /// *finished* before `now + modifier` are dropped. `None` disables the age
    /// cutoff. The CLI builds this from `--older-than 90d`.
    pub older_than: Option<String>,
    /// Hard ceiling on retained executions; the oldest prunable ones are
    /// evicted until at/under it. `None` disables the ceiling.
    pub max_executions: Option<i64>,
    /// Prune executions with a pending enterprise export too, breaking that
    /// drain. Off by default. Never overrides the `forgotten`-table or
    /// unfinished-execution exclusions — those are structural, not policy.
    pub force: bool,
    /// `VACUUM` after pruning to return bytes to the filesystem (also happens
    /// automatically past [`LARGE_PRUNE_EXECUTIONS`]).
    pub vacuum: bool,
    /// Compute the report without deleting anything: the transaction is rolled
    /// back and `VACUUM` is skipped.
    pub dry_run: bool,
}

impl RetentionPolicy {
    /// Whether this policy asks for anything at all.
    pub fn is_noop(&self) -> bool {
        self.older_than.is_none() && self.max_executions.is_none()
    }
}

/// What [`Store::prune`] did (or, under `dry_run`, would have done).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionReport {
    /// Executions removed by the age cutoff.
    pub aged_out: u64,
    /// Executions removed to satisfy the row ceiling.
    pub ceiling_evicted: u64,
    /// Rows removed from the cascade tables across every pruned execution.
    pub child_rows_removed: u64,
    /// Executions that matched the age cutoff or ceiling but were kept because
    /// an enterprise export is still pending and `force` was not set.
    pub protected_pending_export: u64,
    /// Executions kept because they had not finished yet.
    pub protected_unfinished: u64,
    /// Executions still above the ceiling after eviction, because the rest are
    /// protected. Non-zero means "drain first, or re-run with `--force`".
    pub still_over_ceiling: u64,
    /// Whether `VACUUM` ran.
    pub vacuumed: bool,
}

impl RetentionReport {
    /// Total executions removed.
    pub fn executions_removed(&self) -> u64 {
        self.aged_out + self.ceiling_evicted
    }

    /// Whether anything was (or would be) removed.
    pub fn is_empty(&self) -> bool {
        self.executions_removed() == 0 && self.child_rows_removed == 0
    }
}

/// The `executions`-row predicate for "safe to drop", correlated to the outer
/// `executions` row.
///
/// An unfinished execution is never prunable — that exclusion is structural
/// and `force` does not lift it. `force` only lifts the pending-export guard.
fn prunable_predicate(force: bool) -> &'static str {
    if force {
        "executions.finished_at IS NOT NULL"
    } else {
        "executions.finished_at IS NOT NULL \
         AND NOT EXISTS ( \
           SELECT 1 FROM enterprise_export_ledger l \
           WHERE l.execution_id = executions.id AND l.status = 'pending')"
    }
}

impl Store {
    /// Bound `store.db`'s growth per a [`RetentionPolicy`] — the engine behind
    /// `stella storage prune`.
    ///
    /// Runs, in order: age cutoff → row ceiling, both cascading to every
    /// execution-scoped table, all in one transaction, then (optionally)
    /// `VACUUM`. See the [module docs](self) for what is excluded by
    /// construction and why.
    ///
    /// `dry_run` computes the same report but rolls the transaction back and
    /// skips `VACUUM`, so nothing is deleted.
    pub fn prune(&self, policy: &RetentionPolicy) -> Result<RetentionReport> {
        if policy.is_noop() {
            return Err(StoreError(
                "nothing to prune — pass at least one of --older-than or --max-executions"
                    .to_string(),
            ));
        }

        let mut conn = self.lock();
        let mut report = RetentionReport::default();
        let prunable = prunable_predicate(policy.force);

        {
            let tx = conn.transaction()?;

            // 1) Age cutoff. `finished_at` rather than `started_at`: a turn's
            //    age is when it ended, and the NOT NULL check in `prunable`
            //    already excludes in-flight rows.
            if let Some(modifier) = &policy.older_than {
                let doomed: Vec<i64> = {
                    let sql = format!(
                        "SELECT id FROM executions \
                         WHERE finished_at < datetime('now', ?1) AND {prunable}"
                    );
                    let mut stmt = tx.prepare(&sql)?;
                    let rows = stmt.query_map(params![modifier], |r| r.get::<_, i64>(0))?;
                    rows.collect::<std::result::Result<_, _>>()?
                };
                report.child_rows_removed += cascade_delete(&tx, &doomed)?;
                report.aged_out = doomed.len() as u64;

                // Count what the guards held back, so the report explains a
                // smaller-than-expected prune instead of leaving the user to
                // guess.
                report.protected_pending_export += tx.query_row(
                    "SELECT COUNT(*) FROM executions \
                     WHERE finished_at < datetime('now', ?1) \
                       AND finished_at IS NOT NULL \
                       AND EXISTS (SELECT 1 FROM enterprise_export_ledger l \
                                   WHERE l.execution_id = executions.id \
                                     AND l.status = 'pending')",
                    params![modifier],
                    |r| r.get::<_, i64>(0),
                )? as u64;
                report.protected_unfinished += tx.query_row(
                    "SELECT COUNT(*) FROM executions \
                     WHERE started_at < datetime('now', ?1) AND finished_at IS NULL",
                    params![modifier],
                    |r| r.get::<_, i64>(0),
                )? as u64;
            }

            // 2) Row ceiling: evict oldest-first until at/under it.
            if let Some(ceiling) = policy.max_executions {
                let ceiling = ceiling.max(0);
                let total: i64 =
                    tx.query_row("SELECT COUNT(*) FROM executions", [], |r| r.get(0))?;
                let excess = total - ceiling;
                if excess > 0 {
                    let doomed: Vec<i64> = {
                        let sql = format!(
                            "SELECT id FROM executions WHERE {prunable} \
                             ORDER BY id ASC LIMIT ?1"
                        );
                        let mut stmt = tx.prepare(&sql)?;
                        let rows = stmt.query_map(params![excess], |r| r.get::<_, i64>(0))?;
                        rows.collect::<std::result::Result<_, _>>()?
                    };
                    report.child_rows_removed += cascade_delete(&tx, &doomed)?;
                    report.ceiling_evicted = doomed.len() as u64;
                    // Whatever we could not evict is protected; say so.
                    report.still_over_ceiling = (excess - doomed.len() as i64).max(0) as u64;
                }
            }

            if policy.dry_run {
                tx.rollback()?;
            } else {
                tx.commit()?;
            }
        }

        let removed = report.executions_removed();
        let should_vacuum =
            !policy.dry_run && removed > 0 && (policy.vacuum || removed >= LARGE_PRUNE_EXECUTIONS);
        if should_vacuum {
            // store.db has no rowid-anchored external cursor (the enterprise
            // ledger keys on `execution_id`, which VACUUM preserves), so unlike
            // the hub there is nothing to re-anchor afterwards.
            conn.execute_batch("VACUUM")?;
            report.vacuumed = true;
        }

        Ok(report)
    }
}

/// Delete `ids` from `executions` and every table keyed on them, returning the
/// number of child rows removed.
///
/// Chunked so a large prune cannot blow SQLite's variable limit
/// (`SQLITE_MAX_VARIABLE_NUMBER`, 999 on older builds).
fn cascade_delete(tx: &rusqlite::Transaction<'_>, ids: &[i64]) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let mut child_rows = 0u64;
    for chunk in ids.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let bound: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|id| id as &dyn rusqlite::ToSql).collect();

        for table in CASCADE_TABLES {
            let sql = format!("DELETE FROM {table} WHERE execution_id IN ({placeholders})");
            child_rows += tx.execute(&sql, bound.as_slice())? as u64;
        }
        // `reflections.execution_id` is nullable — cross-turn reflections carry
        // NULL and belong to no execution, so they must survive.
        let sql = format!(
            "DELETE FROM reflections \
             WHERE execution_id IS NOT NULL AND execution_id IN ({placeholders})"
        );
        child_rows += tx.execute(&sql, bound.as_slice())? as u64;

        let sql = format!("DELETE FROM executions WHERE id IN ({placeholders})");
        tx.execute(&sql, bound.as_slice())?;
    }
    Ok(child_rows)
}
