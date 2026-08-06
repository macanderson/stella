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

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::gc::WorktreeActivity;
use crate::plan::{Isolation, Task, TaskId};

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
    /// file); existing files were deliberately left unconstrained because
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
    /// version alone cannot answer it: both variants are v2.
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
    pub fn finish_attempt(&self, finish: &AttemptFinish) -> Result<(), LedgerError> {
        // `unchecked_transaction` (rather than `&mut self` + `transaction()`)
        // keeps every ledger method on `&self`, so the fleet can hold the whole
        // ledger behind one `Mutex`. It is sound precisely because of that
        // mutex: the borrow rusqlite would otherwise enforce is enforced by the
        // lock, and this is the only place that opens a transaction — so a
        // nested/interleaved transaction on this connection cannot arise.
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE attempts SET finished_at_ms = ?2, success = ?3, summary = ?4 WHERE id = ?1",
            params![
                finish.attempt_id,
                finish.finished_at_ms as i64,
                finish.success as i64,
                finish.summary,
            ],
        )?;
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
             VALUES (?1, ?2, ?3, ?4, ?5)",
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

    /// Total USD spend recorded against a run (sum over all its tasks'
    /// attempts).
    pub fn total_spend(&self, run_id: &str) -> Result<f64, LedgerError> {
        let total = self.conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM spend WHERE run_id = ?1",
            params![run_id],
            |row| row.get::<_, f64>(0),
        )?;
        Ok(total)
    }

    /// USD spend recorded against a single task within a run.
    pub fn task_spend(&self, run_id: &str, task_id: &str) -> Result<f64, LedgerError> {
        let total = self.conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM spend WHERE run_id = ?1 AND task_id = ?2",
            params![run_id, task_id],
            |row| row.get::<_, f64>(0),
        )?;
        Ok(total)
    }

    /// Child task ids recorded as lineage under a parent run, sorted.
    pub fn lineage_children(&self, parent_run_id: &str) -> Result<Vec<String>, LedgerError> {
        let mut stmt = self.conn.prepare(
            "SELECT child_task_id FROM lineage WHERE parent_run_id = ?1 ORDER BY child_task_id",
        )?;
        let rows = stmt.query_map(params![parent_run_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// How many attempts a task has had (retries show up as extra rows).
    pub fn attempt_count(&self, run_id: &str, task_id: &str) -> Result<u32, LedgerError> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM attempts WHERE run_id = ?1 AND task_id = ?2",
            params![run_id, task_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count as u32)
    }

    /// Whether an attempt row's outcome has been stamped yet (`false` while a
    /// worker is still in flight or if it crashed before finishing).
    pub fn attempt_is_finished(&self, attempt_id: AttemptId) -> Result<bool, LedgerError> {
        let finished: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT finished_at_ms FROM attempts WHERE id = ?1",
                params![attempt_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?;
        Ok(matches!(finished, Some(Some(_))))
    }

    /// One row per worktree path this ledger has ever dispatched into: how
    /// many of its attempts are still unfinished, and when its last attempt
    /// finished — the ledger half of the GC decision (issue #1217).
    ///
    /// The unfinished count is what makes a sweep safe: an attempt row is
    /// opened *before* its worker runs, so a worktree with an unfinished
    /// attempt may still be in use and [`crate::gc::Gc`] keeps it
    /// unconditionally. `NULL` finish times are excluded from the `MAX`, so an
    /// in-flight attempt never dates a worktree.
    pub fn worktree_activity(&self) -> Result<Vec<WorktreeActivity>, LedgerError> {
        let mut stmt = self.conn.prepare(
            "SELECT worktree_path,
                    SUM(CASE WHEN finished_at_ms IS NULL THEN 1 ELSE 0 END),
                    MAX(finished_at_ms)
             FROM attempts
             GROUP BY worktree_path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(WorktreeActivity {
                worktree_path: row.get(0)?,
                unfinished_attempts: row.get::<_, i64>(1)?.max(0) as u32,
                last_finished_ms: row.get::<_, Option<i64>>(2)?.map(|ms| ms as u64),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(LedgerError::from)
    }

    /// When a task last finished an attempt, across **every** run in this
    /// ledger — the per-task half of the prompt-cache warmth signal
    /// (issue #1222): on a re-run of a plan the task ids repeat, so this is
    /// the timestamp the caller projects to "seconds until this task's
    /// prefix expires". Unfinished attempts (in flight, or crashed before
    /// stamping) carry no signal — the last provider call of a finished
    /// attempt is ~its finish time; an unfinished row's start time would
    /// only mis-date it. `None` when the task has never finished one.
    ///
    /// The value is whatever clock the writing fleet stamped. The CLI stamps
    /// wall-clock (Unix-epoch ms); rows written by older builds carry
    /// process-relative ms, which read as decades-stale against a wall
    /// clock — i.e. cold, the conservative direction for a warmth signal.
    pub fn last_attempt_finish_ms(&self, task_id: &str) -> Result<Option<u64>, LedgerError> {
        let last: Option<i64> = self.conn.query_row(
            "SELECT MAX(finished_at_ms) FROM attempts WHERE task_id = ?1",
            params![task_id],
            |row| row.get(0),
        )?;
        Ok(last.map(|ms| ms as u64))
    }

    /// When **any** task in this workspace last finished an attempt — the
    /// shared-prefix half of the warmth signal (issue #1222): within one run
    /// every worker shares the same byte-stable workspace prefix, so a task
    /// with no history of its own inherits the prefix's last touch. Uniform
    /// across a ready set of first-time tasks, which makes it an honest
    /// no-op reorder there; it only separates tasks once per-task history
    /// exists. Same timestamp caveat as
    /// [`last_attempt_finish_ms`](Self::last_attempt_finish_ms).
    pub fn latest_attempt_finish_ms(&self) -> Result<Option<u64>, LedgerError> {
        let last: Option<i64> =
            self.conn
                .query_row("SELECT MAX(finished_at_ms) FROM attempts", [], |row| {
                    row.get(0)
                })?;
        Ok(last.map(|ms| ms as u64))
    }
}

/// The schema version `migrate` brings a `fleet.db` up to. Bump it in the
/// same commit that adds a `MIGRATION_V<n>` step and its `version < n` arm.
const SCHEMA_VERSION: i64 = 2;

/// Apply pending migration steps inside ONE transaction that stamps
/// `user_version` atomically with the DDL — the same shape `stella-store`'s
/// `migrations.rs` and `stella-context`'s store use.
///
/// Before this existed the schema was a bare `CREATE TABLE IF NOT EXISTS`
/// batch, so a `fleet.db` already on a user's disk froze at whatever shape it
/// was created with: a later release adding a column or a constraint was a
/// silent no-op on that file and its INSERTs failed at runtime. `MIGRATION_V1`
/// is that original batch verbatim, which is exactly why retrofitting works —
/// re-running it against an unversioned (`user_version = 0`) file is a no-op,
/// so an existing file is stamped v1 and then takes v2 like any other.
///
/// **Downgrades are not guarded**, matching `stella-context`'s documented
/// behavior: a file stamped by a newer binary takes the early return and is
/// opened as-is. Rejecting it is arguably right, but it turns `open` into an
/// error for anyone who downgrades and belongs in a deliberate change with a
/// migration story, not here.
///
/// **The version is read inside an IMMEDIATE transaction**, so two processes
/// opening the same un-migrated `fleet.db` at once cannot both observe the
/// pre-migration version and both apply the ladder. That race previously
/// surfaced as `SQLITE_BUSY_SNAPSHOT` on one of the two opens — which
/// `busy_timeout` does not retry — and it is also what made appending a
/// non-replay-safe step (an `ALTER TABLE … ADD COLUMN`) unsafe, because the
/// loser applied it twice. Both are closed (#617 item 8).
fn migrate(conn: &Connection) -> Result<(), LedgerError> {
    // IMMEDIATE, and the version is read inside it: a DEFERRED transaction
    // snapshots before it locks, so two processes opening the same
    // un-migrated `fleet.db` could both read the same version and both apply
    // the ladder — in WAL the loser hits SQLITE_BUSY_SNAPSHOT, which
    // `busy_timeout` does not retry, so one `open` failed outright. This is
    // also what makes a non-replay-safe step (an `ALTER TABLE … ADD COLUMN`)
    // safe to append below; before, it would have been applied twice
    // (#617 item 8).
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    // A genuinely fresh file gets the referential integrity an existing one
    // cannot be given — see [`FRESH_SCHEMA`].
    if version == 0 && !any_fleet_table_exists(&tx)? {
        tx.execute_batch(FRESH_SCHEMA)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        return Ok(());
    }
    if version < 1 {
        tx.execute_batch(MIGRATION_V1)?;
    }
    if version < 2 {
        tx.execute_batch(MIGRATION_V2)?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// Whether any ledger table is already present — the fresh-vs-legacy probe,
/// since `user_version = 0` means both "brand new file" and "created before
/// versioning existed".
fn any_fleet_table_exists(conn: &Connection) -> Result<bool, LedgerError> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table'
         AND name IN ('runs', 'tasks', 'attempts', 'commits', 'lineage', 'spend')",
        [],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// One orphan class: rows in `table` whose `column` names a run that is not in
/// `runs`. Produced by [`Ledger::orphan_rows`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanRows {
    pub table: &'static str,
    pub column: &'static str,
    pub count: i64,
}

/// Every column that names a run, and so can hold an orphan: the four a
/// fresh file constrains and an existing one does not, plus `commits.run_id`,
/// which is denormalized beside its real `attempt_id` foreign key and
/// unconstrained on every file. Kept beside `FRESH_SCHEMA` so the report and
/// the schema cannot drift apart.
const RUN_REFERENCES: [(&str, &str); 5] = [
    ("tasks", "run_id"),
    ("attempts", "run_id"),
    ("commits", "run_id"),
    ("lineage", "parent_run_id"),
    ("spend", "run_id"),
];

/// The schema a **brand-new** `fleet.db` is created with: the current shape
/// (post-v2 `lineage`) plus the `REFERENCES runs (id)` constraints that
/// `tasks`, `attempts`, `lineage` and `spend` have always been missing.
///
/// **Why fresh files only** (#617 item 5). Retrofitting these constraints onto
/// a deployed `fleet.db` is a table rebuild, and `apply_migration`-style
/// runners abort on `pragma_foreign_key_check` — so on any file that already
/// holds a row naming a deleted run, the migration fails and the ledger stops
/// opening. The only way through is deleting those rows, which is deleting a
/// user's fleet history: which tasks ran, what was attempted, what it cost.
/// The issue filed this as a routine schema tidy-up and did not say that.
/// So new files get enforcement, existing files are left exactly as they are,
/// and [`Ledger::orphan_rows`] reports what an unconstrained file holds
/// (surfaced by `stella doctor`).
///
/// **Consequence a future migration author must know:** `SCHEMA_VERSION` no
/// longer determines shape by itself. Two files can both be at v2 — one with
/// these constraints and one without. A step that rebuilds any of these four
/// tables has to reproduce the right variant, or read the existing DDL from
/// `sqlite_master` rather than assuming. The alternative was worse: silently
/// deleting history, or leaving new databases unconstrained forever.
const FRESH_SCHEMA: &str = "\
CREATE TABLE runs (
    id              TEXT PRIMARY KEY,
    root_task_count INTEGER NOT NULL,
    created_at_ms   INTEGER NOT NULL
);
CREATE TABLE tasks (
    run_id    TEXT NOT NULL REFERENCES runs (id),
    task_id   TEXT NOT NULL,
    title     TEXT NOT NULL,
    isolation TEXT NOT NULL,
    PRIMARY KEY (run_id, task_id)
);
CREATE TABLE attempts (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id         TEXT NOT NULL REFERENCES runs (id),
    task_id        TEXT NOT NULL,
    worktree_path  TEXT NOT NULL,
    branch         TEXT NOT NULL,
    started_at_ms  INTEGER NOT NULL,
    finished_at_ms INTEGER,
    success        INTEGER,
    summary        TEXT
);
CREATE INDEX attempts_by_task ON attempts (run_id, task_id);
CREATE TABLE commits (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id   INTEGER NOT NULL REFERENCES attempts (id),
    run_id       TEXT NOT NULL,
    task_id      TEXT NOT NULL,
    sha          TEXT NOT NULL,
    branch       TEXT NOT NULL,
    message      TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL
);
CREATE INDEX commits_by_task ON commits (run_id, task_id);
CREATE TABLE lineage (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_run_id  TEXT NOT NULL REFERENCES runs (id),
    child_task_id  TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    UNIQUE (parent_run_id, child_task_id)
);
CREATE INDEX lineage_by_parent ON lineage (parent_run_id);
CREATE TABLE spend (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id         TEXT NOT NULL REFERENCES runs (id),
    task_id        TEXT NOT NULL,
    attempt_id     INTEGER NOT NULL REFERENCES attempts (id),
    cost_usd       REAL NOT NULL,
    recorded_at_ms INTEGER NOT NULL
);
CREATE INDEX spend_by_run ON spend (run_id);
";

/// v1 — the schema as it originally shipped (unversioned). Every statement is
/// `IF NOT EXISTS`, so this doubles as the retrofit step for files created
/// before `user_version` was stamped.
const MIGRATION_V1: &str = "\
CREATE TABLE IF NOT EXISTS runs (
    id              TEXT PRIMARY KEY,
    root_task_count INTEGER NOT NULL,
    created_at_ms   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS tasks (
    run_id    TEXT NOT NULL,
    task_id   TEXT NOT NULL,
    title     TEXT NOT NULL,
    isolation TEXT NOT NULL,
    PRIMARY KEY (run_id, task_id)
);
CREATE TABLE IF NOT EXISTS attempts (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id         TEXT NOT NULL,
    task_id        TEXT NOT NULL,
    worktree_path  TEXT NOT NULL,
    branch         TEXT NOT NULL,
    started_at_ms  INTEGER NOT NULL,
    finished_at_ms INTEGER,
    success        INTEGER,
    summary        TEXT
);
CREATE INDEX IF NOT EXISTS attempts_by_task ON attempts (run_id, task_id);
CREATE TABLE IF NOT EXISTS commits (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    attempt_id   INTEGER NOT NULL REFERENCES attempts (id),
    run_id       TEXT NOT NULL,
    task_id      TEXT NOT NULL,
    sha          TEXT NOT NULL,
    branch       TEXT NOT NULL,
    message      TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS commits_by_task ON commits (run_id, task_id);
CREATE TABLE IF NOT EXISTS lineage (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_run_id  TEXT NOT NULL,
    child_task_id  TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS lineage_by_parent ON lineage (parent_run_id);
CREATE TABLE IF NOT EXISTS spend (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id         TEXT NOT NULL,
    task_id        TEXT NOT NULL,
    attempt_id     INTEGER NOT NULL REFERENCES attempts (id),
    cost_usd       REAL NOT NULL,
    recorded_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS spend_by_run ON spend (run_id);
";

/// v2 — one lineage edge per (parent run, child task).
///
/// `lineage` shipped without a uniqueness constraint, so every re-dispatch of
/// a task appended a duplicate edge and `lineage_children` returned that child
/// once per attempt. Adding a constraint to an existing table is a rebuild;
/// the `GROUP BY` collapses duplicates already on disk, keeping each edge's
/// earliest `recorded_at_ms`. Nothing references `lineage` by foreign key, so
/// the drop/rename is safe with `foreign_keys=ON`. Dropping the old table also
/// drops its index, hence the recreate at the end.
const MIGRATION_V2: &str = "\
CREATE TABLE lineage_v2 (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_run_id  TEXT NOT NULL,
    child_task_id  TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    UNIQUE (parent_run_id, child_task_id)
);
INSERT INTO lineage_v2 (parent_run_id, child_task_id, recorded_at_ms)
    SELECT parent_run_id, child_task_id, MIN(recorded_at_ms)
    FROM lineage
    GROUP BY parent_run_id, child_task_id;
DROP TABLE lineage;
ALTER TABLE lineage_v2 RENAME TO lineage;
CREATE INDEX IF NOT EXISTS lineage_by_parent ON lineage (parent_run_id);
";

#[cfg(test)]
mod tests {
    use super::*;

    /// #617 item 5: a brand-new ledger enforces the run references, so no new
    /// orphan can ever be created.
    #[test]
    fn a_fresh_ledger_enforces_run_references_and_rejects_an_unknown_run() {
        let ledger = Ledger::open_in_memory().expect("open");
        assert!(ledger.enforces_run_references().expect("ddl"));
        assert!(ledger.orphan_rows().expect("scan").is_empty());

        // Inserting a task for a run that does not exist must now fail.
        let err = ledger.conn.execute(
            "INSERT INTO tasks (run_id, task_id, title, isolation)
             VALUES ('ghost', 't1', 'title', 'shared')",
            [],
        );
        assert!(
            err.is_err(),
            "a fresh ledger must reject a task naming an unknown run"
        );
    }

    /// The other half of the ruling: a ledger that came up the legacy ladder is
    /// left unconstrained — opening it must NOT fail, and must not delete the
    /// orphan history it already holds. It is reported instead.
    #[test]
    fn a_legacy_ledger_keeps_its_orphans_and_reports_them() {
        // Build a v0 file the old way, with an orphan row already in it.
        let conn = Connection::open_in_memory().expect("conn");
        conn.execute_batch(MIGRATION_V1).expect("legacy schema");
        conn.execute(
            "INSERT INTO tasks (run_id, task_id, title, isolation)
             VALUES ('deleted-run', 't1', 'orphaned', 'shared')",
            [],
        )
        .expect("orphan row is accepted by the legacy shape");
        // `spend.attempt_id` has always been a real foreign key, so the attempt
        // has to exist — its own `run_id` is the orphaned part.
        conn.execute(
            "INSERT INTO attempts (run_id, task_id, worktree_path, branch, started_at_ms)
             VALUES ('deleted-run', 't1', '/w', 'fleet/t1', 0)",
            [],
        )
        .expect("orphan attempt row");
        let attempt_id: i64 = conn.last_insert_rowid();
        // `commits.attempt_id` has always been a real foreign key too; its
        // denormalized `run_id` is the orphaned part.
        conn.execute(
            "INSERT INTO commits (attempt_id, run_id, task_id, sha, branch, message, timestamp_ms)
             VALUES (?1, 'deleted-run', 't1', 'abc', 'fleet/t1', 'work', 0)",
            params![attempt_id],
        )
        .expect("orphan commit row");
        conn.execute(
            "INSERT INTO spend (run_id, task_id, attempt_id, cost_usd, recorded_at_ms)
             VALUES ('deleted-run', 't1', ?1, 1.0, 0)",
            params![attempt_id],
        )
        .expect("orphan spend row");

        // Now let the ledger take it through migrate() as a deployed file.
        let ledger = Ledger::init(conn).expect("a legacy file with orphans still opens");

        assert!(
            !ledger.enforces_run_references().expect("ddl"),
            "an existing file is left unconstrained on purpose"
        );

        let orphans = ledger.orphan_rows().expect("scan");
        assert_eq!(
            orphans,
            vec![
                OrphanRows {
                    table: "tasks",
                    column: "run_id",
                    count: 1
                },
                OrphanRows {
                    table: "attempts",
                    column: "run_id",
                    count: 1
                },
                OrphanRows {
                    table: "commits",
                    column: "run_id",
                    count: 1
                },
                OrphanRows {
                    table: "spend",
                    column: "run_id",
                    count: 1
                },
            ],
            "every orphan class is reported, none deleted"
        );

        // And the history is still there.
        let surviving: i64 = ledger
            .conn
            .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
            .expect("count");
        assert_eq!(surviving, 1, "the orphan row must not have been deleted");
    }

    fn task(id: &str) -> Task {
        Task::new(id, format!("title {id}"), "prompt")
    }

    fn commit(task_id: &str, sha: &str) -> CommitRecord {
        CommitRecord {
            sha: sha.into(),
            branch: format!("fleet/{task_id}"),
            task_id: task_id.into(),
            message: format!("work on {task_id}"),
            timestamp_ms: 1_000,
        }
    }

    fn seed_run(ledger: &Ledger, run_id: &str) {
        ledger
            .record_run(&RunRecord {
                id: run_id.into(),
                root_task_count: 1,
                created_at_ms: 1,
            })
            .unwrap();
        ledger.record_task(run_id, &task("t1")).unwrap();
    }

    #[test]
    fn open_in_memory_applies_schema_and_is_empty() {
        let ledger = Ledger::open_in_memory().unwrap();
        assert_eq!(ledger.total_spend("run").unwrap(), 0.0);
        assert!(ledger.commits_for_task("run", "t1").unwrap().is_empty());
        assert!(ledger.lineage_children("run").unwrap().is_empty());
    }

    #[test]
    fn attempt_round_trips_commits_and_spend_atomically() {
        let ledger = Ledger::open_in_memory().unwrap();
        seed_run(&ledger, "run1");

        let attempt_id = ledger
            .start_attempt(&AttemptStart {
                run_id: "run1".into(),
                task_id: "t1".into(),
                worktree_path: "/tmp/wt/t1".into(),
                branch: "fleet/t1".into(),
                started_at_ms: 10,
            })
            .unwrap();
        assert!(!ledger.attempt_is_finished(attempt_id).unwrap());

        ledger
            .finish_attempt(&AttemptFinish {
                attempt_id,
                run_id: "run1".into(),
                task_id: "t1".into(),
                finished_at_ms: 20,
                success: true,
                summary: "done".into(),
                commits: vec![commit("t1", "aaa"), commit("t1", "bbb")],
                cost_usd: 0.25,
                spend_at_ms: 21,
            })
            .unwrap();

        assert!(ledger.attempt_is_finished(attempt_id).unwrap());
        let commits = ledger.commits_for_task("run1", "t1").unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sha, "aaa");
        assert_eq!(commits[1].sha, "bbb");
        assert!((ledger.total_spend("run1").unwrap() - 0.25).abs() < 1e-9);
        assert!((ledger.task_spend("run1", "t1").unwrap() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn spend_sums_across_multiple_attempts_of_a_run() {
        let ledger = Ledger::open_in_memory().unwrap();
        seed_run(&ledger, "run1");
        ledger.record_task("run1", &task("t2")).unwrap();

        for (task_id, cost) in [("t1", 0.1), ("t2", 0.4)] {
            let attempt_id = ledger
                .start_attempt(&AttemptStart {
                    run_id: "run1".into(),
                    task_id: task_id.into(),
                    worktree_path: format!("/tmp/{task_id}"),
                    branch: format!("fleet/{task_id}"),
                    started_at_ms: 1,
                })
                .unwrap();
            ledger
                .finish_attempt(&AttemptFinish {
                    attempt_id,
                    run_id: "run1".into(),
                    task_id: task_id.into(),
                    finished_at_ms: 2,
                    success: true,
                    summary: "ok".into(),
                    commits: vec![],
                    cost_usd: cost,
                    spend_at_ms: 3,
                })
                .unwrap();
        }
        assert!((ledger.total_spend("run1").unwrap() - 0.5).abs() < 1e-9);
        assert!((ledger.task_spend("run1", "t1").unwrap() - 0.1).abs() < 1e-9);
        assert!((ledger.task_spend("run1", "t2").unwrap() - 0.4).abs() < 1e-9);
    }

    #[test]
    fn retries_of_a_task_show_up_as_multiple_attempts() {
        let ledger = Ledger::open_in_memory().unwrap();
        seed_run(&ledger, "run1");
        for started in [1, 5] {
            ledger
                .start_attempt(&AttemptStart {
                    run_id: "run1".into(),
                    task_id: "t1".into(),
                    worktree_path: "/tmp/t1".into(),
                    branch: "fleet/t1".into(),
                    started_at_ms: started,
                })
                .unwrap();
        }
        assert_eq!(ledger.attempt_count("run1", "t1").unwrap(), 2);
    }

    #[test]
    fn lineage_records_parent_run_to_child_tasks() {
        let ledger = Ledger::open_in_memory().unwrap();
        seed_run(&ledger, "parent-run");
        ledger.record_lineage("parent-run", "t-child-b", 1).unwrap();
        ledger.record_lineage("parent-run", "t-child-a", 2).unwrap();
        assert_eq!(
            ledger.lineage_children("parent-run").unwrap(),
            vec!["t-child-a".to_string(), "t-child-b".to_string()]
        );
        assert!(ledger.lineage_children("other-run").unwrap().is_empty());
    }

    #[test]
    fn re_dispatching_a_task_does_not_duplicate_its_lineage_edge() {
        // Restart is "the caller re-dispatching the same Task", and dispatch
        // stamps lineage once per attempt — but an edge is a fact about the
        // graph, not an attempt count (`attempts` already records retries).
        let ledger = Ledger::open_in_memory().unwrap();
        seed_run(&ledger, "run1");
        ledger.record_lineage("run1", "t1", 10).unwrap();
        ledger.record_lineage("run1", "t1", 40).unwrap();

        assert_eq!(
            ledger.lineage_children("run1").unwrap(),
            vec!["t1".to_string()],
            "a re-dispatch does not return the child twice"
        );
        let recorded: i64 = ledger
            .conn
            .query_row("SELECT recorded_at_ms FROM lineage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(recorded, 10, "the first dispatch's timestamp is kept");
    }

    #[test]
    fn record_run_refreshes_the_task_count_but_never_the_creation_time() {
        // `Fleet::new` stamps the creation time with a 0 task count; the
        // later `run_plan` fills the count in. That second write must not
        // rewrite the run's recorded creation time.
        let ledger = Ledger::open_in_memory().unwrap();
        for (root_task_count, created_at_ms) in [(0, 100), (3, 900)] {
            ledger
                .record_run(&RunRecord {
                    id: "run1".into(),
                    root_task_count,
                    created_at_ms,
                })
                .unwrap();
        }
        let (count, created): (u32, i64) = ledger
            .conn
            .query_row(
                "SELECT root_task_count, created_at_ms FROM runs WHERE id = ?1",
                params!["run1"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 3, "the root task count is refreshed");
        assert_eq!(created, 100, "creation time is write-once");
    }

    // schema versioning

    fn user_version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_fresh_ledger_is_stamped_at_the_current_schema_version() {
        let ledger = Ledger::open_in_memory().unwrap();
        assert_eq!(user_version(&ledger.conn), SCHEMA_VERSION);
    }

    #[test]
    fn an_unversioned_ledger_migrates_in_place_without_losing_data() {
        // A `fleet.db` written before `user_version` was stamped: the schema
        // as it originally shipped, real rows, and the duplicate lineage edge
        // a re-dispatch left behind.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-fleet.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            assert_eq!(user_version(&conn), 0, "the legacy file is unversioned");
            conn.execute(
                "INSERT INTO runs (id, root_task_count, created_at_ms) VALUES ('run1', 2, 7)",
                [],
            )
            .unwrap();
            for at in [10_i64, 40] {
                conn.execute(
                    "INSERT INTO lineage (parent_run_id, child_task_id, recorded_at_ms) \
                     VALUES ('run1', 't1', ?1)",
                    params![at],
                )
                .unwrap();
            }
        }

        let ledger = Ledger::open(&path).unwrap();
        assert_eq!(user_version(&ledger.conn), SCHEMA_VERSION);
        // Pre-existing rows survived, and the duplicate edge collapsed onto
        // the earliest timestamp.
        let created: i64 = ledger
            .conn
            .query_row(
                "SELECT created_at_ms FROM runs WHERE id = 'run1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(created, 7);
        assert_eq!(
            ledger.lineage_children("run1").unwrap(),
            vec!["t1".to_string()]
        );
        let recorded: i64 = ledger
            .conn
            .query_row("SELECT recorded_at_ms FROM lineage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(recorded, 10);

        // And the migrated file still takes writes on the new schema.
        ledger.record_lineage("run1", "t1", 90).unwrap();
        assert_eq!(ledger.lineage_children("run1").unwrap().len(), 1);
    }

    #[test]
    fn a_migrated_ledger_is_not_re_migrated_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet.db");
        {
            let ledger = Ledger::open(&path).unwrap();
            seed_run(&ledger, "run1");
            ledger.record_lineage("run1", "t1", 5).unwrap();
        }
        let reopened = Ledger::open(&path).unwrap();
        assert_eq!(user_version(&reopened.conn), SCHEMA_VERSION);
        assert_eq!(
            reopened.lineage_children("run1").unwrap(),
            vec!["t1".to_string()]
        );
    }

    // the warmth-signal reads (issue #1222)

    /// Record one finished attempt for `task_id` in `run_id`, minimal shape.
    fn finished_attempt(ledger: &Ledger, run_id: &str, task_id: &str, finished_at_ms: u64) {
        let attempt_id = ledger
            .start_attempt(&AttemptStart {
                run_id: run_id.into(),
                task_id: task_id.into(),
                worktree_path: format!("/tmp/{task_id}"),
                branch: format!("fleet/{task_id}"),
                started_at_ms: finished_at_ms.saturating_sub(1_000),
            })
            .unwrap();
        ledger
            .finish_attempt(&AttemptFinish {
                attempt_id,
                run_id: run_id.into(),
                task_id: task_id.into(),
                finished_at_ms,
                success: true,
                summary: "ok".into(),
                commits: vec![],
                cost_usd: 0.0,
                spend_at_ms: finished_at_ms,
            })
            .unwrap();
    }

    #[test]
    fn last_attempt_finish_is_per_task_and_spans_runs() {
        let ledger = Ledger::open_in_memory().unwrap();
        seed_run(&ledger, "run1");
        seed_run(&ledger, "run2");
        // The same task id across two runs — a plan re-run — plus a sibling.
        finished_attempt(&ledger, "run1", "t1", 10_000);
        finished_attempt(&ledger, "run2", "t1", 50_000);
        finished_attempt(&ledger, "run1", "t2", 30_000);

        assert_eq!(
            ledger.last_attempt_finish_ms("t1").unwrap(),
            Some(50_000),
            "the LATEST finish across runs wins"
        );
        assert_eq!(ledger.last_attempt_finish_ms("t2").unwrap(), Some(30_000));
        assert_eq!(
            ledger.last_attempt_finish_ms("never-ran").unwrap(),
            None,
            "a task with no history carries no per-task signal"
        );
        // The shared-prefix timestamp: the newest finish over ALL tasks.
        assert_eq!(ledger.latest_attempt_finish_ms().unwrap(), Some(50_000));
    }

    #[test]
    fn unfinished_attempts_carry_no_warmth_signal() {
        let ledger = Ledger::open_in_memory().unwrap();
        seed_run(&ledger, "run1");
        // Opened but never stamped — a crash, or a worker still in flight.
        ledger
            .start_attempt(&AttemptStart {
                run_id: "run1".into(),
                task_id: "t1".into(),
                worktree_path: "/tmp/t1".into(),
                branch: "fleet/t1".into(),
                started_at_ms: 5,
            })
            .unwrap();
        assert_eq!(ledger.last_attempt_finish_ms("t1").unwrap(), None);
        assert_eq!(ledger.latest_attempt_finish_ms().unwrap(), None);
    }

    #[test]
    fn commit_record_json_roundtrips() {
        let c = commit("t1", "deadbeef");
        let back: CommitRecord = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn ledger_persists_to_a_file_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.db");
        {
            let ledger = Ledger::open(&path).unwrap();
            seed_run(&ledger, "run1");
            let attempt_id = ledger
                .start_attempt(&AttemptStart {
                    run_id: "run1".into(),
                    task_id: "t1".into(),
                    worktree_path: "/tmp/t1".into(),
                    branch: "fleet/t1".into(),
                    started_at_ms: 1,
                })
                .unwrap();
            ledger
                .finish_attempt(&AttemptFinish {
                    attempt_id,
                    run_id: "run1".into(),
                    task_id: "t1".into(),
                    finished_at_ms: 2,
                    success: true,
                    summary: "ok".into(),
                    commits: vec![commit("t1", "abc")],
                    cost_usd: 0.5,
                    spend_at_ms: 3,
                })
                .unwrap();
        }
        // Reopen the same file: the schema and data survive.
        let reopened = Ledger::open(&path).unwrap();
        assert_eq!(reopened.commits_for_task("run1", "t1").unwrap().len(), 1);
        assert!((reopened.total_spend("run1").unwrap() - 0.5).abs() < 1e-9);
    }
}
