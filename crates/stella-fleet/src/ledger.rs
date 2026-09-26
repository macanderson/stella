//! The commit ledger — `fleet.db`, one embedded SQLite file
//! (`rusqlite`, bundled — "one storage engine")
//! recording, for every fleet run: its tasks, each dispatch attempt, the
//! commits an attempt produced, the parent→child lineage, and per-task USD
//! spend.
//!
//! This is the durable audit trail behind the dispatch seam (L-E9): the one
//! place a subagent's commits and cost are stamped, so lineage is never lost
//! and spend is never uncounted. The in-memory [`stella_core::BudgetGuard`]
//! is the *gate*; this ledger is the *record* — both are written on every
//! dispatch (`crate::fleet`).
//!
//! Writes that must be all-or-nothing (an attempt's outcome plus its commits
//! and spend row) go through one transaction
//! ([`Ledger::finish_attempt`]); WAL journaling is enabled at open so a
//! reader is never blocked by an in-flight writer.
//!
//! Schema: `fleet.db` is **versioned** by `SCHEMA_VERSION` and `migrate`,
//! which stamps `PRAGMA user_version` in the same transaction as the DDL it
//! applies. The base DDL is still convergence — every statement is
//! `CREATE … IF NOT EXISTS` and the whole batch replays on every open — so an
//! *additive* table or index reaches an existing ledger the next time it is
//! opened, and adding one needs no migration step. What convergence cannot do
//! is *reshape* an existing table: altering or backfilling a column is
//! silently skipped by the `IF NOT EXISTS` guard on an existing file, so that
//! change must land as a numbered `MIGRATION_V<n>` with a matching
//! `version < n` arm, the way `MIGRATION_V2` rebuilt `lineage` to add its
//! uniqueness constraint.
//!
//! This matters more here than in a rebuildable index: unlike `codegraph.db`,
//! the ledger is *not* a cache. It is the authoritative record of a subagent's
//! commits and of real money spent, and nothing can reconstruct it once
//! written.
//!
//! Beside the audit trail it also holds the **dispatch claims** ([`lease`]):
//! the check-and-set lease that stops two sessions picking up the same unit
//! of work (#1136). It lives here because a claim has to outlive the process
//! that took it, and this is the fleet's only durable file.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::plan::{Isolation, Task, TaskId};

pub mod lease;
mod migrations;
mod query;
#[cfg(test)]
mod tests;

pub use migrations::OrphanRows;
use migrations::{RUN_REFERENCES, migrate};

/// A commit recorded in the ledger — also the shape a [`FleetWorker`] reports
/// back (`crate::fleet::WorkerOutcome::commits`) and the value the emit-shape
/// helper turns into an [`stella_protocol::AgentEvent::Commit`]
/// (`crate::monitor::commit_event`).
///
/// [`FleetWorker`]: crate::fleet::FleetWorker
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRecord {
    pub sha: String,
    pub branch: String,
    pub task_id: TaskId,
    pub message: String,
    pub timestamp_ms: u64,
}

/// A fleet run — the top of the ledger hierarchy (run → task → attempt →
/// commits/spend).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub id: String,
    pub root_task_count: u32,
    pub created_at_ms: u64,
}

/// The opening half of a dispatch attempt, written before the worker runs so
/// a crash mid-attempt still leaves a row naming what was in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptStart {
    /// The fan-out this attempt belongs to — never a `stella-store`
    /// `execution_id` or a session id (see the glossary in `AGENTS.md`).
    pub run_id: String,
    pub task_id: TaskId,
    pub worktree_path: String,
    pub branch: String,
    pub started_at_ms: u64,
}

/// The closing half of a dispatch attempt: its outcome plus everything it
/// produced. Written in one transaction by [`Ledger::finish_attempt`].
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptFinish {
    pub attempt_id: AttemptId,
    pub run_id: String,
    pub task_id: TaskId,
    pub finished_at_ms: u64,
    pub success: bool,
    pub summary: String,
    pub commits: Vec<CommitRecord>,
    pub cost_usd: f64,
    pub spend_at_ms: u64,
}

/// SQLite rowid of an attempt row, returned by [`Ledger::start_attempt`] and
/// referenced by its commits/spend.
pub type AttemptId = i64;

/// Failures interacting with the ledger — always typed, never a panic.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("ledger sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// The fleet commit ledger over one SQLite connection. Not `Sync` (a
/// `rusqlite::Connection` isn't), so the fleet holds it behind a `Mutex` and
/// serializes its (fast, synchronous) writes — see `crate::fleet`.
pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    /// Open (creating if absent) the ledger at `path` — the CLI opens
    /// `<workspace>/.stella/private/fleet.db`. Enables WAL
    /// and foreign keys, then applies the schema.
    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// An in-memory ledger — for tests and ephemeral runs. Same schema; WAL
    /// is a no-op for `:memory:`.
    pub fn open_in_memory() -> Result<Self, LedgerError> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    /// Rows whose `run_id` names a run that is no longer in `runs`, per table.
    ///
    /// Scans every column in `RUN_REFERENCES`. `FRESH_SCHEMA` constrains four
    /// of them (`commits.run_id` is denormalized and unconstrained on every
    /// file); existing files were left unconstrained because
    /// retrofitting them can only be done by deleting this history (#617
    /// item 5). Reporting is therefore the whole remedy: nothing reads a row
    /// by orphaned run today, so the rows are inert, but an operator should
    /// be able to see them rather than have them silently removed. Surfaced
    /// by `stella doctor`.
    ///
    /// Classes with a zero count are omitted, so an empty result means clean.
    pub fn orphan_rows(&self) -> Result<Vec<OrphanRows>, LedgerError> {
        let mut found = Vec::new();
        for (table, column) in RUN_REFERENCES {
            // The table list is a compile-time constant, never user input.
            let count: i64 = self.conn.query_row(
                &format!(
                    "SELECT count(*) FROM {table}
                     WHERE {column} NOT IN (SELECT id FROM runs)"
                ),
                [],
                |row| row.get(0),
            )?;
            if count > 0 {
                found.push(OrphanRows {
                    table,
                    column,
                    count,
                });
            }
        }
        Ok(found)
    }

    /// Whether this ledger's `tasks.run_id` carries the `REFERENCES runs (id)`
    /// constraint — i.e. whether it was created by `FRESH_SCHEMA` rather than the
    /// legacy migration ladder.
    ///
    /// Read from `sqlite_master` rather than tracked in `user_version`, because
    /// version alone cannot answer it: both shapes are v2.
    pub fn enforces_run_references(&self) -> Result<bool, LedgerError> {
        let ddl: Option<String> = self
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'tasks'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(ddl
            .map(|sql| sql.contains("REFERENCES runs"))
            .unwrap_or(false))
    }

    fn init(conn: Connection) -> Result<Self, LedgerError> {
        // execute_batch tolerates the row PRAGMA journal_mode returns (a
        // plain pragma_update errors on it).
        //
        // `busy_timeout` matters as much as WAL here: two `stella fleet` runs in
        // one workspace open the SAME `fleet.db`, and SQLite's default busy
        // handler returns SQLITE_BUSY *immediately*. Without the timeout a
        // second writer's `finish_attempt` fails after its worker already spent
        // real money — the attempt's commits and spend row would be lost from
        // the audit trail. WAL is a file-level setting; `foreign_keys` and
        // `busy_timeout` are per-connection and must be re-set on every open
        // (the same idiom as `stella-graph`/`stella-context`).
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )?;
        migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Record a run (idempotent on its id).
    ///
    /// `created_at_ms` is **write-once**: the conflict branch updates only
    /// `root_task_count`, so the run's recorded creation time stays the one
    /// `Fleet::new` stamped rather than being rewritten by the later
    /// `run_plan` call that fills the task count in.
    pub fn record_run(&self, run: &RunRecord) -> Result<(), LedgerError> {
        self.conn.execute(
            "INSERT INTO runs (id, root_task_count, created_at_ms) VALUES (?1, ?2, ?3) \
             ON CONFLICT(id) DO UPDATE SET root_task_count = excluded.root_task_count",
            params![run.id, run.root_task_count, run.created_at_ms as i64],
        )?;
        Ok(())
    }

    /// Record a task belonging to a run (idempotent on (run_id, task_id)).
    pub fn record_task(&self, run_id: &str, task: &Task) -> Result<(), LedgerError> {
        let isolation = match task.isolation {
            Isolation::Isolated => "isolated",
            Isolation::SharedTree => "shared_tree",
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO tasks (run_id, task_id, title, isolation) \
             VALUES (?1, ?2, ?3, ?4)",
            params![run_id, task.id, task.title, isolation],
        )?;
        Ok(())
    }

    /// Open an attempt row and return its id. Written before the worker runs
    /// (see [`AttemptStart`]).
    pub fn start_attempt(&self, start: &AttemptStart) -> Result<AttemptId, LedgerError> {
        self.conn.execute(
            "INSERT INTO attempts \
             (run_id, task_id, worktree_path, branch, started_at_ms, finished_at_ms, success, summary) \
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL)",
            params![
                start.run_id,
                start.task_id,
                start.worktree_path,
                start.branch,
                start.started_at_ms as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Close an attempt and stamp everything it produced — its commits and
    /// its spend row — in a single transaction (all-or-nothing). This is the
    /// durable half of the dispatch seam (`crate::fleet::Fleet::dispatch`).
    ///
    /// **Idempotent**, like [`record_lineage`](Self::record_lineage). A
    /// second call for an attempt already closed is a no-op, not a second
    /// commit/spend row: the `WHERE finished_at_ms IS NULL` guard on the
    /// `attempts` update tells a first close from a retry by its
    /// affected-row count, and a retry returns before touching `commits` or
    /// `spend`. `spend.attempt_id`'s `UNIQUE` constraint plus
    /// `ON CONFLICT (attempt_id) DO NOTHING` back that up for any other
    /// caller of the insert — a worker's stop/timeout race can settle one
    /// attempt twice, and neither close may double the recorded spend.
    /// `INSERT OR IGNORE` would ignore *every* constraint on the row: it
    /// swallows the `CHECK` a negative cost raises and the `NOT NULL` a
    /// bound `NaN` becomes, dropping the spend row and returning `Ok`.
    /// Hence the named conflict target.
    pub fn finish_attempt(&self, finish: &AttemptFinish) -> Result<(), LedgerError> {
        // `unchecked_transaction` (rather than `&mut self` + `transaction()`)
        // keeps every ledger method on `&self`, so the fleet can hold the whole
        // ledger behind one `Mutex`. It is sound precisely because of that
        // mutex: the borrow rusqlite would otherwise enforce is enforced by the
        // lock, and this is the only place that opens a transaction — so a
        // nested/interleaved transaction on this connection cannot arise.
        let tx = self.conn.unchecked_transaction()?;
        let closed = tx.execute(
            "UPDATE attempts SET finished_at_ms = ?2, success = ?3, summary = ?4 \
             WHERE id = ?1 AND finished_at_ms IS NULL",
            params![
                finish.attempt_id,
                finish.finished_at_ms as i64,
                finish.success as i64,
                finish.summary,
            ],
        )?;
        if closed == 0 {
            // Already closed by an earlier call — commit the (no-op) update
            // and stop, rather than appending a second set of commits and a
            // second spend row for the same attempt.
            tx.commit()?;
            return Ok(());
        }
        for commit in &finish.commits {
            tx.execute(
                "INSERT INTO commits \
                 (attempt_id, run_id, task_id, sha, branch, message, timestamp_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    finish.attempt_id,
                    finish.run_id,
                    commit.task_id,
                    commit.sha,
                    commit.branch,
                    commit.message,
                    commit.timestamp_ms as i64,
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO spend (run_id, task_id, attempt_id, cost_usd, recorded_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (attempt_id) DO NOTHING",
            params![
                finish.run_id,
                finish.task_id,
                finish.attempt_id,
                finish.cost_usd,
                finish.spend_at_ms as i64,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Record a parent-run → child-task lineage edge (L-E9: the dispatch seam
    /// stamps lineage so a subagent's work is always traceable to its
    /// parent).
    ///
    /// Idempotent per edge (`UNIQUE (parent_run_id, child_task_id)` +
    /// `INSERT OR IGNORE`): an edge is a fact about the graph, not about the
    /// attempt count, so re-dispatching the same task — the documented
    /// restart mechanism — must not append a second edge and make
    /// [`lineage_children`](Self::lineage_children) return that child twice.
    /// Retries are already counted by the `attempts` table. The kept row's
    /// `recorded_at_ms` is therefore the FIRST dispatch's.
    pub fn record_lineage(
        &self,
        parent_run_id: &str,
        child_task_id: &str,
        recorded_at_ms: u64,
    ) -> Result<(), LedgerError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO lineage (parent_run_id, child_task_id, recorded_at_ms) \
             VALUES (?1, ?2, ?3)",
            params![parent_run_id, child_task_id, recorded_at_ms as i64],
        )?;
        Ok(())
    }

    /// Every commit recorded for a task, oldest first.
    pub fn commits_for_task(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Vec<CommitRecord>, LedgerError> {
        let mut stmt = self.conn.prepare(
            "SELECT sha, branch, task_id, message, timestamp_ms FROM commits \
             WHERE run_id = ?1 AND task_id = ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![run_id, task_id], |row| {
            Ok(CommitRecord {
                sha: row.get(0)?,
                branch: row.get(1)?,
                task_id: row.get(2)?,
                message: row.get(3)?,
                timestamp_ms: row.get::<_, i64>(4)? as u64,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}
