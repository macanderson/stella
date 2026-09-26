//! Schema shape for `fleet.db`, split out of `ledger.rs`: the version
//! ladder [`migrate`] steps through, the DDL each step applies, and the
//! orphan report that reads a ledger's shape back out.

use rusqlite::Connection;

use super::LedgerError;

/// The schema version `migrate` brings a `fleet.db` up to. Bump it in the
/// same commit that adds a `MIGRATION_V<n>` step and its `version < n` arm.
pub(crate) const SCHEMA_VERSION: i64 = 4;

/// Apply pending migration steps inside ONE transaction that stamps
/// `user_version` atomically with the DDL — the same shape `stella-store`'s
/// `migrations.rs` and `stella-context`'s store use.
///
/// Before this existed the schema was a bare `CREATE TABLE IF NOT EXISTS`
/// batch, so a `fleet.db` already on a user's disk froze at whatever shape it
/// was created with. A later release adding a column or a constraint was a
/// silent no-op on that file and its INSERTs failed at runtime. `MIGRATION_V1`
/// is that original batch verbatim, so retrofitting works: re-running it
/// against an unversioned (`user_version = 0`) file is a no-op, so an
/// existing file is stamped v1 and then takes v2 like any other.
///
/// **Downgrades are not guarded**, matching `stella-context`'s documented
/// behavior: a file stamped by a newer binary takes the early return and is
/// opened as-is. Rejecting it is arguably right, but it turns `open` into an
/// error for anyone who downgrades and belongs in a planned change with a
/// migration story, not here.
///
/// **The version is read inside an IMMEDIATE transaction**, so two processes
/// opening the same un-migrated `fleet.db` at once cannot both observe the
/// pre-migration version and both apply the ladder. That race previously
/// surfaced as `SQLITE_BUSY_SNAPSHOT` on one of the two opens, which
/// `busy_timeout` does not retry. It is also what made appending a
/// non-replay-safe step (an `ALTER TABLE … ADD COLUMN`) unsafe, because the
/// loser applied it twice. Both are closed (#617 item 8).
pub(crate) fn migrate(conn: &Connection) -> Result<(), LedgerError> {
    // IMMEDIATE, and the version is read inside it. A DEFERRED transaction
    // snapshots before it locks, so two processes opening the same
    // un-migrated `fleet.db` could both read the same version and both apply
    // the ladder. In WAL the loser hits SQLITE_BUSY_SNAPSHOT, which
    // `busy_timeout` does not retry, so one `open` failed outright. That is
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
    if version < 3 {
        tx.execute_batch(MIGRATION_V3)?;
    }
    if version < 4 {
        tx.execute_batch(MIGRATION_V4)?;
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
/// `runs`. Produced by [`Ledger::orphan_rows`](super::Ledger::orphan_rows).
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
pub(crate) const RUN_REFERENCES: [(&str, &str); 5] = [
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
/// and [`Ledger::orphan_rows`](super::Ledger::orphan_rows) reports what an unconstrained file holds
/// (surfaced by `stella doctor`).
///
/// **Consequence a future migration author must know:** `SCHEMA_VERSION` no
/// longer determines shape by itself. Two files can both be at v2 — one with
/// these constraints and one without. A step that rebuilds any of these four
/// tables has to reproduce the right shape, or read the existing DDL from
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
    cost_usd       REAL NOT NULL CHECK (cost_usd >= 0),
    recorded_at_ms INTEGER NOT NULL,
    UNIQUE (attempt_id)
);
CREATE INDEX spend_by_run ON spend (run_id);
CREATE TABLE dispatch_claims (
    claim_key      TEXT PRIMARY KEY,
    owner          TEXT NOT NULL,
    fence          INTEGER NOT NULL,
    acquired_at_ms INTEGER NOT NULL,
    renewed_at_ms  INTEGER NOT NULL,
    expires_at_ms  INTEGER NOT NULL
);
CREATE INDEX dispatch_claims_by_expiry ON dispatch_claims (expires_at_ms);
";

/// v1 — the schema as it originally shipped (unversioned). Every statement is
/// `IF NOT EXISTS`, so this doubles as the retrofit step for files created
/// before `user_version` was stamped.
pub(crate) const MIGRATION_V1: &str = "\
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

/// v3 — the dispatch claims table (#1136): one row per unit of dispatch,
/// holding the check-and-set lease in [`lease`](super::lease).
///
/// Purely additive, so it is `IF NOT EXISTS` and replay-safe. `claim_key` is
/// the primary key precisely because the claim is an upsert on it — the
/// uniqueness constraint is what makes "exactly one holder" a property of the
/// storage engine rather than of the calling code.
///
/// `owner` carries no `REFERENCES runs (id)`: a claimant need not be a fleet
/// run (a bare agent session working an issue is the case #1136 was filed
/// for), and a claim must survive being read after its run row is gone.
const MIGRATION_V3: &str = "\
CREATE TABLE IF NOT EXISTS dispatch_claims (
    claim_key      TEXT PRIMARY KEY,
    owner          TEXT NOT NULL,
    fence          INTEGER NOT NULL,
    acquired_at_ms INTEGER NOT NULL,
    renewed_at_ms  INTEGER NOT NULL,
    expires_at_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS dispatch_claims_by_expiry ON dispatch_claims (expires_at_ms);
";

/// v4 — a validated `spend` table. `CHECK (cost_usd >= 0)` stops a negative
/// cost from reaching storage, even if a future caller skips the Rust-side
/// guard in `crate::fleet::Fleet::dispatch_claimed`. A NaN report is caught
/// by `cost_usd REAL NOT NULL` rather than by this `CHECK`, since SQLite
/// binds a NaN as NULL before a CHECK ever sees it. `UNIQUE (attempt_id)`
/// is what makes `Ledger::finish_attempt`'s `ON CONFLICT (attempt_id) DO
/// NOTHING` an idempotent no-op on a retried close, not a second spend row.
///
/// Both constraints need a table rebuild — the same as `MIGRATION_V2`'s
/// `lineage` fix, since SQLite has no `ALTER TABLE ADD CONSTRAINT`. An old
/// row with a negative `cost_usd`, or `NULL` (what a bound NaN becomes on
/// disk), is clamped to zero rather than left to fail the rebuild. That
/// money was already wrong, and refusing to open the ledger over it would
/// only trade a bad number for a locked-out one. A duplicate `attempt_id` from
/// before this fix collapses to its earliest row (lowest `id`), matching
/// `finish_attempt`'s "first close wins" rule.
const MIGRATION_V4: &str = "\
CREATE TABLE spend_v4 (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id         TEXT NOT NULL,
    task_id        TEXT NOT NULL,
    attempt_id     INTEGER NOT NULL REFERENCES attempts (id),
    cost_usd       REAL NOT NULL CHECK (cost_usd >= 0),
    recorded_at_ms INTEGER NOT NULL,
    UNIQUE (attempt_id)
);
INSERT INTO spend_v4 (run_id, task_id, attempt_id, cost_usd, recorded_at_ms)
    SELECT run_id, task_id, attempt_id, MAX(COALESCE(cost_usd, 0.0), 0.0), recorded_at_ms
    FROM spend
    WHERE id IN (SELECT MIN(id) FROM spend GROUP BY attempt_id);
DROP TABLE spend;
ALTER TABLE spend_v4 RENAME TO spend;
CREATE INDEX IF NOT EXISTS spend_by_run ON spend (run_id);
";
