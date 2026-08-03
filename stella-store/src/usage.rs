//! `usage.db` — the **user-tier** telemetry hub, one database per
//! developer (not per project), living at `~/.stella/usage.db`. It is a
//! *derived* store: every project's
//! `.stella/private/store.db` is the source of truth, and each finished turn is rolled
//! up here so a future cross-project dashboard can answer "how do I actually
//! use Stella, across all my repos?" without opening every project database.
//!
//! Privacy: this tier stores **metadata and rollups**, never source code or
//! tool outputs. Prompts are reduced to a digest plus a short preview.
//!
//! Direction of flow is one-way: `store.db` → `usage.db`. Nothing here writes
//! back to a project store, and a missing/un-openable `usage.db` never
//! blocks a turn — sync is best-effort.
//!
//! Retention: the hub accumulates one telemetry row per model call across every
//! project forever, and rows from deleted checkouts persist. [`UsageStore::prune`]
//! bounds that growth — by age, by a hard row ceiling, and by GC of unregistered
//! projects whose checkout is gone — without ever dropping an org's un-acked
//! cloud-drain rows (see [`UsageStore::prune`] and [`PrunePolicy`]).
//!
//! Cloud drain: [`UsageStore::cloud_pending`] stages an org's un-acked rows and
//! [`UsageStore::ack_cloud_synced`] advances a monotonic per-org cursor that
//! never rewinds. Because it advances only on a confirmed ack, one row the
//! intake rejects *permanently* would wedge every newer row for that org — so
//! [`UsageStore::quarantine_cloud_row`] dead-letters that row (with its
//! rejection reason, retained for inspection) and steps the cursor over it in a
//! single transaction (#467). The drain loop that pinpoints such a row lives in
//! [`crate::drain`].
//!
//! Schema: `usage.db` is versioned by *convergence*, not by a `user_version`
//! migration list (unlike `.stella/private/store.db`, see `crate::migrations`).
//! Every table in `USAGE_SCHEMA` is `CREATE ... IF NOT EXISTS` and the whole
//! batch replays on every open, so an additive table or index reaches an
//! existing hub the next time it is opened. Adding a table here is the
//! migration; a table that ever needs a *reshape* would need the versioned
//! machinery introduced first.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// The user-tier stella data dir (usage rollup, session registry,
/// notifications, enterprise spool). `STELLA_DATA_DIR` overrides; otherwise
/// `~/.stella` on every platform (see [`crate::home::stella_home`]) — no
/// platform-specific guessing.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("STELLA_DATA_DIR") {
        return PathBuf::from(dir);
    }
    crate::home::stella_home().unwrap_or_else(|| PathBuf::from("."))
}

/// Where the user-tier aggregate lives: `data_dir()/usage.db`.
pub fn usage_db_path() -> PathBuf {
    data_dir().join("usage.db")
}

/// A dependency-free project identity: FNV-1a/64 (`crate::fnv_hex`) of the
/// canonical workspace path, hex-encoded. Deterministic across runs and
/// processes — but NOT across a `mv` of the checkout, which is why a
/// registered workspace replicates under `identity::replication_project_id`
/// instead (#408).
pub fn project_id_for(root: &Path) -> String {
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    crate::fnv_hex(&canon.to_string_lossy())
}

/// One per-tool bucket for the usage histogram (the "you grep symbols a lot but
/// never call graph_query" signal). `calls`/`errors` for one execution; the
/// aggregate is accumulated per (project, tool, surface, day).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBucket {
    pub tool: String,
    pub surface: String,
    pub calls: i64,
    pub errors: i64,
}

/// Everything the user tier records for one finished turn. Assembled from a
/// project `Store` (see `Store::execution_rollup`) and handed to
/// [`UsageStore::sync_execution`]. Carries no source content — only metadata,
/// a prompt digest + short preview, and rolled-up counts.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionRollupRow {
    pub project_id: String,
    pub project_name: String,
    pub project_root: String,
    pub execution_id: i64,
    pub kind: String,
    pub prompt_digest: String,
    pub prompt_preview: String,
    pub model: String,
    pub provider: String,
    pub outcome: String,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub duration_ms: i64,
    pub tool_calls: i64,
    pub files_written: i64,
    pub produced_output: bool,
    /// False when any paid-call envelope or persistence boundary is unknown.
    pub usage_complete: bool,
    pub self_rating: Option<i64>,
    pub started_at: String,
    /// The turn's day bucket (`YYYY-MM-DD`, from `started_at`) for the rollups.
    pub day: String,
    /// Per-tool buckets for this turn, folded into `tool_usage_rollup`.
    pub tool_histogram: Vec<ToolBucket>,
}

const USAGE_SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS projects (
    project_id    TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    root_path     TEXT NOT NULL,
    first_seen_at TEXT NOT NULL,
    last_seen_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS execution_rollup (
    project_id      TEXT NOT NULL,
    execution_id    INTEGER NOT NULL,
    kind            TEXT NOT NULL,
    prompt_digest   TEXT NOT NULL,
    prompt_preview  TEXT NOT NULL DEFAULT '',
    model           TEXT NOT NULL,
    provider        TEXT NOT NULL,
    outcome         TEXT NOT NULL,
    cost_usd        REAL NOT NULL,
    input_tokens    INTEGER NOT NULL,
    output_tokens   INTEGER NOT NULL,
    duration_ms     INTEGER NOT NULL,
    tool_calls      INTEGER NOT NULL,
    files_written   INTEGER NOT NULL,
    produced_output INTEGER NOT NULL,
    self_rating     INTEGER,
    started_at      TEXT NOT NULL,
    PRIMARY KEY (project_id, execution_id)
);
-- Convergence works for removals too: the batch replays on every open, so a
-- DROP here reaches existing hubs the same way a new table would. The
-- by-model index served no query (every execution_rollup reader keys on
-- project_id, which the primary key already covers).
DROP INDEX IF EXISTS execution_rollup_by_model;
CREATE TABLE IF NOT EXISTS tool_usage_rollup (
    project_id TEXT NOT NULL,
    tool       TEXT NOT NULL,
    surface    TEXT NOT NULL,
    day        TEXT NOT NULL,
    calls      INTEGER NOT NULL DEFAULT 0,
    errors     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, tool, surface, day)
);
CREATE TABLE IF NOT EXISTS telemetry (
    project_id     TEXT NOT NULL,
    source_rowid   INTEGER NOT NULL,
    org_id         TEXT,
    workspace_id   TEXT,
    repo_id        TEXT NOT NULL DEFAULT '',
    execution_id   INTEGER NOT NULL,
    step           INTEGER NOT NULL,
    recorded_at    TEXT NOT NULL DEFAULT '',
    provider       TEXT NOT NULL,
    call_role      TEXT NOT NULL,
    model          TEXT NOT NULL,
    input_tokens   INTEGER NOT NULL,
    estimated_input_tokens INTEGER NOT NULL,
    output_tokens  INTEGER NOT NULL,
    cache_read_tokens  INTEGER NOT NULL,
    cache_miss_tokens  INTEGER NOT NULL,
    cache_write_tokens INTEGER NOT NULL,
    cost_usd       REAL NOT NULL,
    duration_ms    INTEGER NOT NULL,
    retries        INTEGER NOT NULL,
    tool_calls     INTEGER NOT NULL,
    usage_complete INTEGER NOT NULL,
    PRIMARY KEY (project_id, source_rowid)
);
CREATE INDEX IF NOT EXISTS telemetry_by_org
    ON telemetry(org_id, recorded_at);
-- The observatory's per-(provider, model) hub rollup groups this table by
-- exactly these columns; the index is what lets that GROUP BY scan in index
-- order instead of sorting the whole hub.
CREATE INDEX IF NOT EXISTS telemetry_by_model
    ON telemetry(provider, model);
CREATE TABLE IF NOT EXISTS telemetry_sync_cursors (
    project_id        TEXT PRIMARY KEY,
    last_source_rowid INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS cloud_sync_cursors (
    org_id         TEXT PRIMARY KEY,
    last_hub_rowid INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS cloud_quarantine (
    org_id         TEXT NOT NULL,
    project_id     TEXT NOT NULL,
    source_rowid   INTEGER NOT NULL,
    workspace_id   TEXT,
    repo_id        TEXT NOT NULL DEFAULT '',
    hub_rowid      INTEGER NOT NULL,
    recorded_at    TEXT NOT NULL DEFAULT '',
    provider       TEXT NOT NULL DEFAULT '',
    model          TEXT NOT NULL DEFAULT '',
    input_tokens   INTEGER NOT NULL DEFAULT 0,
    output_tokens  INTEGER NOT NULL DEFAULT 0,
    cost_usd       REAL NOT NULL DEFAULT 0,
    reason         TEXT NOT NULL,
    http_status    INTEGER,
    quarantined_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (org_id, project_id, source_rowid)
);
CREATE INDEX IF NOT EXISTS cloud_quarantine_by_org
    ON cloud_quarantine(org_id, quarantined_at);
";

/// The user-tier aggregate store. Read/write, loopback-local, no server.
pub struct UsageStore {
    conn: Mutex<Connection>,
}

impl UsageStore {
    /// Open (creating dirs + schema) the per-user `usage.db` at the default
    /// location, migrating the legacy split layout into `~/.stella` first.
    /// Best-effort callers treat an `Err` as "no cross-project aggregation
    /// this run".
    pub fn open_default() -> Result<Self> {
        crate::home::migrate_legacy_global_dirs();
        Self::open_at(&usage_db_path())
    }

    /// Open (creating parent dirs + schema) at an explicit path.
    pub fn open_at(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            crate::ensure_private_dir(parent)?;
        }
        Self::init(crate::open_private_sqlite(path)?)
    }

    /// In-memory aggregate — tests and ephemeral runs.
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(USAGE_SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Roll one finished turn up into the aggregate: upsert its project, insert
    /// (or replace) the execution rollup, and fold the tool histogram into the
    /// per-day counts. One transaction; idempotent on (project_id,
    /// execution_id) so a re-sync (e.g. `stella usage sync`) never double-counts
    /// the execution rollup. Tool-day counts are additive, so a backfill must
    /// run against a fresh aggregate (documented on the sync command).
    pub fn sync_execution(&self, r: &ExecutionRollupRow) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        // Upsert the project (first_seen sticks; last_seen advances).
        tx.execute(
            "INSERT INTO projects (project_id, name, root_path, first_seen_at, last_seen_at) \
             VALUES (?1, ?2, ?3, ?4, ?4) \
             ON CONFLICT(project_id) DO UPDATE SET \
               name = excluded.name, \
               root_path = excluded.root_path, \
               last_seen_at = excluded.last_seen_at",
            params![r.project_id, r.project_name, r.project_root, r.started_at],
        )?;
        // Was this execution already rolled up? If so, its tool counts were too
        // — skip the additive fold to stay idempotent. NoRows is the only
        // "not yet" answer: `.is_ok()` read a genuine query error as "not
        // rolled up" and re-ran the additive tool-histogram fold, breaking
        // the documented never-double-counts contract.
        let already: bool = match tx.query_row(
            "SELECT 1 FROM execution_rollup WHERE project_id = ?1 AND execution_id = ?2",
            params![r.project_id, r.execution_id],
            |_| Ok(()),
        ) {
            Ok(()) => true,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(e) => return Err(e.into()),
        };
        tx.execute(
            "INSERT OR REPLACE INTO execution_rollup \
             (project_id, execution_id, kind, prompt_digest, prompt_preview, model, provider, \
              outcome, cost_usd, input_tokens, output_tokens, duration_ms, tool_calls, \
              files_written, produced_output, self_rating, started_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                r.project_id,
                r.execution_id,
                r.kind,
                r.prompt_digest,
                r.prompt_preview,
                r.model,
                r.provider,
                r.outcome,
                r.cost_usd,
                r.input_tokens,
                r.output_tokens,
                r.duration_ms,
                r.tool_calls,
                r.files_written,
                r.produced_output as i64,
                r.self_rating,
                r.started_at,
            ],
        )?;
        if !already {
            for b in &r.tool_histogram {
                tx.execute(
                    "INSERT INTO tool_usage_rollup (project_id, tool, surface, day, calls, errors) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT(project_id, tool, surface, day) DO UPDATE SET \
                       calls = calls + excluded.calls, \
                       errors = errors + excluded.errors",
                    params![r.project_id, b.tool, b.surface, r.day, b.calls, b.errors],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Number of projects known to the aggregate.
    pub fn project_count(&self) -> Result<i64> {
        Ok(self
            .lock()
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))?)
    }

    /// Cross-project per-tool call totals, most-used first — the histogram a
    /// dashboard/recommender reads to spot "grep a lot, graph_query never".
    pub fn tool_totals(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tool, SUM(calls) AS c FROM tool_usage_rollup \
             GROUP BY tool ORDER BY c DESC, tool ASC",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Count of rolled-up executions for a project.
    pub fn execution_count(&self, project_id: &str) -> Result<i64> {
        Ok(self.lock().query_row(
            "SELECT COUNT(*) FROM execution_rollup WHERE project_id = ?1",
            params![project_id],
            |r| r.get(0),
        )?)
    }

    /// The replication watermark for one project: the highest source-store
    /// `telemetry.rowid` already in the hub. 0 for a never-synced project.
    ///
    /// Only "no row" means 0. A genuine read failure (SQLITE_BUSY past the
    /// timeout, a corrupt page) propagates, because swallowing it as 0 is
    /// indistinguishable from never-synced and makes
    /// [`crate::Store::replicate_telemetry_to_usage`] re-ship the project's
    /// whole telemetry history, every turn, with nothing surfaced. The caller
    /// discards the result (`let _ =`), so a real error stays best-effort
    /// rather than failing a turn.
    pub fn telemetry_cursor(&self, project_id: &str) -> Result<i64> {
        Ok(self
            .lock()
            .query_row(
                "SELECT last_source_rowid FROM telemetry_sync_cursors WHERE project_id = ?1",
                params![project_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// Move one project's replication watermark to `last_source_rowid`,
    /// **downward included** — the repair path for a source `store.db` whose
    /// `telemetry.rowid`s a prune invalidated.
    ///
    /// [`Self::replicate_telemetry`] advances the cursor with `MAX(...)` so a
    /// late or out-of-order batch can never rewind it. That is right for
    /// replication and wrong here: `store.db`'s `telemetry.rowid` is implicit
    /// (its key is `UNIQUE (execution_id, step)`), so deleting rows or
    /// `VACUUM`ing that file renumbers the surviving ones *down*, stranding
    /// this cursor above rows that have never shipped. `stella stats prune`
    /// computes the corrected value
    /// ([`StorePruneReport::telemetry_cursor_after`](crate::StorePruneReport::telemetry_cursor_after))
    /// and writes it back through here. Erring low only re-ships rows the hub
    /// dedups on `(project_id, source_rowid)`; erring high loses them.
    pub fn rewind_telemetry_cursor(&self, project_id: &str, last_source_rowid: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO telemetry_sync_cursors (project_id, last_source_rowid) \
             VALUES (?1, ?2) \
             ON CONFLICT(project_id) DO UPDATE SET last_source_rowid = excluded.last_source_rowid",
            params![project_id, last_source_rowid],
        )?;
        Ok(())
    }

    /// Replicate a batch of source telemetry rows into the hub and advance
    /// the project's cursor, in one transaction. Idempotent on
    /// (project_id, source_rowid): a re-replicated row overwrites itself, so
    /// a crash between commit and the caller observing it never double-counts.
    pub fn replicate_telemetry(
        &self,
        scope: &crate::identity::TelemetryScope,
        rows: &[crate::SourceTelemetryRow],
    ) -> Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let mut max_rowid: i64 = 0;
        for row in rows {
            let t = &row.telemetry;
            tx.execute(
                "INSERT OR REPLACE INTO telemetry \
                 (project_id, source_rowid, org_id, workspace_id, repo_id, execution_id, step, \
                  recorded_at, provider, call_role, model, input_tokens, estimated_input_tokens, \
                  output_tokens, cache_read_tokens, cache_miss_tokens, cache_write_tokens, \
                  cost_usd, duration_ms, retries, tool_calls, usage_complete) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    scope.project_id,
                    row.source_rowid,
                    scope.org_id,
                    scope.workspace_id,
                    scope.repo_id,
                    row.execution_id,
                    t.step as i64,
                    row.recorded_at,
                    t.provider,
                    t.call_role,
                    t.model,
                    t.input_tokens as i64,
                    t.estimated_input_tokens as i64,
                    t.output_tokens as i64,
                    t.cache_read_tokens as i64,
                    t.cache_miss_tokens as i64,
                    t.cache_write_tokens as i64,
                    t.cost_usd,
                    t.duration_ms as i64,
                    t.retries,
                    t.tool_calls as i64,
                    t.usage_complete,
                ],
            )?;
            max_rowid = max_rowid.max(row.source_rowid);
        }
        tx.execute(
            "INSERT INTO telemetry_sync_cursors (project_id, last_source_rowid) \
             VALUES (?1, ?2) \
             ON CONFLICT(project_id) DO UPDATE SET \
               last_source_rowid = MAX(last_source_rowid, excluded.last_source_rowid)",
            params![scope.project_id, max_rowid],
        )?;
        tx.commit()?;
        Ok(rows.len() as u64)
    }

    /// The global report: per (org, provider, model) call counts, token and
    /// cache totals, cost, and how many projects contributed — the query a
    /// cross-project dashboard or `stella usage` renders. `org` filters to
    /// one org id; `None` reports everything (NULL-org rows group as
    /// unregistered/local).
    pub fn global_telemetry_totals(&self, org: Option<&str>) -> Result<Vec<GlobalTelemetryRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT org_id, provider, model, COUNT(*), SUM(input_tokens), SUM(output_tokens), \
                    SUM(cache_read_tokens), SUM(cost_usd), COUNT(DISTINCT project_id) \
             FROM telemetry \
             WHERE (?1 IS NULL OR org_id = ?1) \
             GROUP BY org_id, provider, model \
             ORDER BY SUM(cost_usd) DESC, provider, model",
        )?;
        let rows = stmt.query_map(params![org], |r| {
            Ok(GlobalTelemetryRow {
                org_id: r.get(0)?,
                provider: r.get(1)?,
                model: r.get(2)?,
                calls: r.get(3)?,
                input_tokens: r.get(4)?,
                output_tokens: r.get(5)?,
                cache_read_tokens: r.get(6)?,
                cost_usd: r.get(7)?,
                projects: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Hub rows for one org not yet acknowledged by the cloud, oldest first
    /// — the drain a cloud syncer walks before [`Self::ack_cloud_synced`].
    pub fn cloud_pending(&self, org_id: &str, limit: usize) -> Result<Vec<CloudTelemetryEvent>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT t.rowid, t.org_id, t.workspace_id, t.repo_id, t.project_id, t.execution_id, \
                    t.step, t.recorded_at, t.provider, t.call_role, t.model, t.input_tokens, \
                    t.estimated_input_tokens, t.output_tokens, t.cache_read_tokens, \
                    t.cache_miss_tokens, t.cache_write_tokens, t.cost_usd, t.duration_ms, \
                    t.retries, t.tool_calls, t.usage_complete, t.source_rowid \
             FROM telemetry t \
             WHERE t.org_id = ?1 \
               AND t.rowid > COALESCE((SELECT last_hub_rowid FROM cloud_sync_cursors \
                                       WHERE org_id = ?1), 0) \
             ORDER BY t.rowid ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![org_id, limit as i64], |r| {
            Ok(CloudTelemetryEvent {
                hub_rowid: r.get(0)?,
                org_id: r.get(1)?,
                workspace_id: r.get(2)?,
                repo_id: r.get(3)?,
                project_id: r.get(4)?,
                execution_id: r.get(5)?,
                source_rowid: r.get(22)?,
                recorded_at: r.get(7)?,
                telemetry: crate::TelemetryRow {
                    step: r.get::<_, i64>(6)? as u64,
                    provider: r.get(8)?,
                    call_role: r.get(9)?,
                    model: r.get(10)?,
                    input_tokens: r.get::<_, i64>(11)? as u64,
                    estimated_input_tokens: r.get::<_, i64>(12)? as u64,
                    output_tokens: r.get::<_, i64>(13)? as u64,
                    cache_read_tokens: r.get::<_, i64>(14)? as u64,
                    cache_miss_tokens: r.get::<_, i64>(15)? as u64,
                    cache_write_tokens: r.get::<_, i64>(16)? as u64,
                    cost_usd: r.get(17)?,
                    duration_ms: r.get::<_, i64>(18)? as u64,
                    retries: r.get(19)?,
                    tool_calls: r.get::<_, i64>(20)? as u64,
                    usage_complete: r.get(21)?,
                },
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Acknowledge cloud receipt of every hub row up to `up_to_hub_rowid`
    /// for one org. Monotonic — an out-of-order ack never rewinds.
    pub fn ack_cloud_synced(&self, org_id: &str, up_to_hub_rowid: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO cloud_sync_cursors (org_id, last_hub_rowid) VALUES (?1, ?2) \
             ON CONFLICT(org_id) DO UPDATE SET \
               last_hub_rowid = MAX(last_hub_rowid, excluded.last_hub_rowid)",
            params![org_id, up_to_hub_rowid],
        )?;
        Ok(())
    }

    /// Dead-letter one permanently-rejected hub row and advance the org cursor
    /// past it — the head-of-line-blocking escape hatch (#467).
    ///
    /// The cursor never rewinds and never advances on an un-acked row, so a
    /// single row the intake refuses *forever* would otherwise wedge every
    /// newer row for that org. Once the drain has pinpointed that row
    /// ([`crate::drain::drain_org`] bisects the batch), this records it with its
    /// rejection reason and moves the cursor past it in **one transaction**:
    /// quarantine-then-ack can only ever err toward re-quarantining the same
    /// row (idempotent on `(org_id, project_id, source_rowid)`), never toward
    /// skipping one without a record. A crash between the two halves is
    /// impossible; a crash before the commit replays the whole step.
    ///
    /// The row is **never silently dropped**: the quarantine record keeps its
    /// identity, its content-free telemetry, and why the intake refused it, and
    /// it survives the retention prune that will eventually reclaim the acked
    /// `telemetry` row itself (which is why the fields are copied, not joined).
    ///
    /// Content-free by construction (#466): the copied columns are identity +
    /// addressing + per-call telemetry only. No prompt, completion, path, or
    /// tool payload exists on a hub row to copy, and `reason` is the intake's
    /// own diagnostic, truncated to [`MAX_QUARANTINE_REASON_BYTES`] so a
    /// misbehaving intake cannot turn the dead-letter table into a content
    /// channel.
    ///
    /// `advance_to_hub_rowid` is the highest row the caller has *settled* —
    /// normally `event.hub_rowid`, or a later row when a delivered prefix is
    /// being acked in the same step. It is clamped to at least the quarantined
    /// row so the poison row can never be quarantined without being stepped
    /// over.
    pub fn quarantine_cloud_row(
        &self,
        event: &CloudTelemetryEvent,
        reason: &QuarantineReason,
        advance_to_hub_rowid: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let t = &event.telemetry;
        tx.execute(
            "INSERT INTO cloud_quarantine \
             (org_id, project_id, source_rowid, workspace_id, repo_id, hub_rowid, recorded_at, \
              provider, model, input_tokens, output_tokens, cost_usd, reason, http_status) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(org_id, project_id, source_rowid) DO UPDATE SET \
               hub_rowid = excluded.hub_rowid, \
               reason = excluded.reason, \
               http_status = excluded.http_status",
            params![
                event.org_id,
                event.project_id,
                event.source_rowid,
                event.workspace_id,
                event.repo_id,
                event.hub_rowid,
                event.recorded_at,
                t.provider,
                t.model,
                t.input_tokens as i64,
                t.output_tokens as i64,
                t.cost_usd,
                reason.detail(),
                reason.http_status,
            ],
        )?;
        tx.execute(
            "INSERT INTO cloud_sync_cursors (org_id, last_hub_rowid) VALUES (?1, ?2) \
             ON CONFLICT(org_id) DO UPDATE SET \
               last_hub_rowid = MAX(last_hub_rowid, excluded.last_hub_rowid)",
            params![event.org_id, advance_to_hub_rowid.max(event.hub_rowid)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// How many rows the cloud drain has dead-lettered for one org — the count
    /// half of what `stella cloud status` surfaces. `None` counts every org.
    pub fn cloud_quarantine_count(&self, org_id: Option<&str>) -> Result<i64> {
        Ok(self.lock().query_row(
            "SELECT COUNT(*) FROM cloud_quarantine WHERE (?1 IS NULL OR org_id = ?1)",
            params![org_id],
            |r| r.get(0),
        )?)
    }

    /// Dead-lettered rows for one org, newest first — the inspection view
    /// behind `stella cloud status`. `None` reports every org.
    pub fn cloud_quarantined(
        &self,
        org_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<QuarantinedRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT org_id, project_id, source_rowid, workspace_id, repo_id, hub_rowid, \
                    recorded_at, provider, model, cost_usd, reason, http_status, quarantined_at \
             FROM cloud_quarantine \
             WHERE (?1 IS NULL OR org_id = ?1) \
             ORDER BY quarantined_at DESC, hub_rowid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![org_id, limit as i64], |r| {
            Ok(QuarantinedRow {
                org_id: r.get(0)?,
                project_id: r.get(1)?,
                source_rowid: r.get(2)?,
                workspace_id: r.get(3)?,
                repo_id: r.get(4)?,
                hub_rowid: r.get(5)?,
                recorded_at: r.get(6)?,
                provider: r.get(7)?,
                model: r.get(8)?,
                cost_usd: r.get(9)?,
                reason: r.get(10)?,
                http_status: r.get(11)?,
                quarantined_at: r.get(12)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Dead-letter counts grouped by rejection reason, largest group first —
    /// the "count + reason" rollup `stella cloud status` prints without dumping
    /// every row. `None` reports every org.
    pub fn cloud_quarantine_reasons(
        &self,
        org_id: Option<&str>,
    ) -> Result<Vec<(String, Option<i64>, i64)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT reason, http_status, COUNT(*) AS n FROM cloud_quarantine \
             WHERE (?1 IS NULL OR org_id = ?1) \
             GROUP BY reason, http_status ORDER BY n DESC, reason ASC",
        )?;
        let rows = stmt.query_map(params![org_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Every project the hub knows: (project_id, name, root_path) — the
    /// registry `stella usage sync --all` walks for backfill.
    pub fn registered_projects(&self) -> Result<Vec<(String, String, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT project_id, name, root_path FROM projects ORDER BY last_seen_at DESC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Bound the hub's growth per a [`PrunePolicy`] — the engine behind
    /// `stella usage prune`. Runs, in order: project GC → age cutoff → row
    /// ceiling, all in one transaction, then (optionally) `VACUUM`.
    ///
    /// The cloud drain is never broken. Age and ceiling pruning only touch a
    /// row when it is safe to drop — NULL-org (never shipped), already acked
    /// (`rowid <= cloud_sync_cursors.last_hub_rowid`), or `force`. GC only
    /// removes *unregistered* projects (no org-scoped rows), so their rows
    /// never drain either. And because `VACUUM` renumbers `telemetry.rowid`
    /// (the table's PK is `(project_id, source_rowid)`, so its `rowid` is
    /// implicit and not VACUUM-stable) while the cloud cursor is stored as a
    /// `rowid`, every cursor is re-anchored after `VACUUM` against the stable
    /// `(project_id, source_rowid)` key — see `Self::vacuum_and_reanchor`.
    ///
    /// `dry_run` computes the same report but rolls the transaction back and
    /// skips `VACUUM`, so nothing is deleted.
    pub fn prune(&self, policy: &PrunePolicy) -> Result<PruneReport> {
        let mut conn = self.lock();
        let mut report = PruneReport::default();
        // The "safe to drop" predicate, correlated to the `telemetry` row.
        // `force` collapses it to "everything"; otherwise a row is prunable
        // only when it is NULL-org or already acked by the cloud drain.
        let prunable = prunable_predicate(policy.force);

        {
            let tx = conn.transaction()?;

            // 1) GC unregistered, gone-root projects. The caller supplies the
            //    set whose checkout is missing; we drop only those with no
            //    org-scoped rows (all-NULL-org → never drained → safe).
            for pid in &policy.gc_project_ids {
                // NoRows is the only "not registered" answer. `.is_ok()`
                // collapsed every other error (I/O, corrupt page, interrupt)
                // into `false` too — and `false` is the branch that DELETEs
                // an org's un-drained rows, so a transient read fault became
                // permanent data loss the prunable-predicate guard exists to
                // prevent.
                let registered = match tx.query_row(
                    "SELECT 1 FROM telemetry \
                     WHERE project_id = ?1 AND org_id IS NOT NULL LIMIT 1",
                    params![pid],
                    |_| Ok(()),
                ) {
                    Ok(()) => true,
                    Err(rusqlite::Error::QueryReturnedNoRows) => false,
                    Err(e) => return Err(e.into()),
                };
                if registered {
                    continue;
                }
                report.gc_rows +=
                    tx.execute("DELETE FROM telemetry WHERE project_id = ?1", params![pid])? as u64;
                tx.execute(
                    "DELETE FROM execution_rollup WHERE project_id = ?1",
                    params![pid],
                )?;
                tx.execute(
                    "DELETE FROM tool_usage_rollup WHERE project_id = ?1",
                    params![pid],
                )?;
                tx.execute(
                    "DELETE FROM telemetry_sync_cursors WHERE project_id = ?1",
                    params![pid],
                )?;
                tx.execute("DELETE FROM projects WHERE project_id = ?1", params![pid])?;
                report.gc_projects += 1;
            }

            // 2) Age cutoff. Rows whose `recorded_at` predates `now + modifier`
            //    are dropped when prunable; org rows that would age out but are
            //    still un-acked are counted as protected (kept for the drain).
            //    Rollup tables never drain, so they age out unconditionally.
            if let Some(modifier) = &policy.older_than {
                if !policy.force {
                    report.protected_unacked += tx.query_row(
                        &format!(
                            "SELECT COUNT(*) FROM telemetry \
                             WHERE julianday(recorded_at) < julianday('now', ?1) \
                               AND NOT {prunable}"
                        ),
                        params![modifier],
                        |r| r.get::<_, i64>(0),
                    )? as u64;
                }
                report.aged_out += tx.execute(
                    &format!(
                        "DELETE FROM telemetry \
                         WHERE julianday(recorded_at) < julianday('now', ?1) \
                           AND {prunable}"
                    ),
                    params![modifier],
                )? as u64;
                report.rollups_aged_out += tx.execute(
                    "DELETE FROM execution_rollup \
                     WHERE julianday(started_at) < julianday('now', ?1)",
                    params![modifier],
                )? as u64;
                report.rollups_aged_out += tx.execute(
                    "DELETE FROM tool_usage_rollup WHERE day < date('now', ?1)",
                    params![modifier],
                )? as u64;
            }

            // 3) Hard row ceiling on `telemetry` — evict the oldest prunable
            //    rows until at/under it. Un-acked org rows can't be evicted
            //    without `force`, so record any residual overage for the caller.
            if let Some(max_rows) = policy.max_rows {
                let total: i64 =
                    tx.query_row("SELECT COUNT(*) FROM telemetry", [], |r| r.get(0))?;
                if total > max_rows {
                    let excess = total - max_rows;
                    report.ceiling_evicted += tx.execute(
                        &format!(
                            "DELETE FROM telemetry WHERE rowid IN (\
                               SELECT rowid FROM telemetry WHERE {prunable} \
                               ORDER BY rowid ASC LIMIT ?1)"
                        ),
                        params![excess],
                    )? as u64;
                    let after: i64 =
                        tx.query_row("SELECT COUNT(*) FROM telemetry", [], |r| r.get(0))?;
                    if after > max_rows {
                        report.still_over_ceiling = (after - max_rows) as u64;
                    }
                }
            }

            if policy.dry_run {
                // Roll back: `tx` drops un-committed, so nothing is deleted.
                return Ok(report);
            }

            // Clamp every cloud cursor to the surviving max `rowid`. Deleting the
            // table's max rowid frees it, and SQLite hands that freed (lower)
            // rowid to the next replicated row — which would then land at/below a
            // cursor left pointing past it and never be surfaced by
            // `cloud_pending` (`rowid > cursor`). A cursor never legitimately
            // exceeds `MAX(rowid)` (every acked row survived acking), so this
            // only lowers a cursor stranded by a delete, never skips an un-acked
            // row. The `VACUUM` path re-anchors on the stable key below and
            // supersedes this; it runs here so the non-`VACUUM` path is safe too.
            tx.execute(
                "UPDATE cloud_sync_cursors SET last_hub_rowid = \
                   MIN(last_hub_rowid, COALESCE((SELECT MAX(rowid) FROM telemetry), 0))",
                [],
            )?;

            tx.commit()?;
        }

        // 4) Reclaim file bytes on a large (or explicitly requested) prune, and
        //    re-anchor the cloud cursors that `VACUUM` would otherwise strand.
        let deleted =
            report.aged_out + report.ceiling_evicted + report.gc_rows + report.rollups_aged_out;
        if deleted > 0 && (policy.vacuum || deleted >= LARGE_PRUNE_ROWS) {
            Self::vacuum_and_reanchor(&conn)?;
            report.vacuumed = true;
        }
        Ok(report)
    }

    /// `VACUUM` the hub, keeping every org's cloud cursor pointing at the same
    /// logical row. `telemetry.rowid` is implicit (composite PK) so `VACUUM`
    /// renumbers it, but `(project_id, source_rowid)` is stable — so we snapshot
    /// each org's highest *surviving acked* row by that key before `VACUUM`,
    /// then set the cursor to that row's new `rowid` afterward. We err low: if
    /// the boundary row didn't survive the prune, the cursor resets to 0, which
    /// re-ships the retained acked backlog (idempotent server-side on
    /// `(workspace_id, source_rowid)`) rather than skipping an un-acked row.
    fn vacuum_and_reanchor(conn: &Connection) -> Result<()> {
        // (org_id, boundary key) captured post-delete, pre-VACUUM.
        let mut boundaries: Vec<(String, Option<(String, i64)>)> = Vec::new();
        {
            let mut stmt = conn.prepare("SELECT org_id, last_hub_rowid FROM cloud_sync_cursors")?;
            let cursors: Vec<(String, i64)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            for (org_id, cursor) in cursors {
                let key = conn
                    .query_row(
                        "SELECT project_id, source_rowid FROM telemetry \
                         WHERE org_id = ?1 AND rowid <= ?2 ORDER BY rowid DESC LIMIT 1",
                        params![org_id, cursor],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                    )
                    .ok();
                boundaries.push((org_id, key));
            }
        }

        conn.execute_batch("VACUUM")?;

        // Re-anchor every cursor atomically: a partial update (an error or crash
        // mid-loop) would otherwise leave some orgs pointing at stale pre-VACUUM
        // rowids while others are correct.
        let tx = conn.unchecked_transaction()?;
        for (org_id, key) in boundaries {
            let new_cursor: i64 = match key {
                Some((project_id, source_rowid)) => tx
                    .query_row(
                        "SELECT rowid FROM telemetry \
                         WHERE project_id = ?1 AND source_rowid = ?2",
                        params![project_id, source_rowid],
                        |r| r.get(0),
                    )
                    .unwrap_or(0),
                None => 0,
            };
            tx.execute(
                "UPDATE cloud_sync_cursors SET last_hub_rowid = ?1 WHERE org_id = ?2",
                params![new_cursor, org_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// A prune that deletes at least this many rows triggers an automatic `VACUUM`
/// to hand the reclaimed pages back to the filesystem (a small prune leaves the
/// freed pages in the file's freelist for reuse). `--vacuum` forces it for any
/// non-empty prune.
const LARGE_PRUNE_ROWS: u64 = 10_000;

/// The `telemetry`-row predicate for "safe to drop without breaking the cloud
/// drain", correlated to the outer `telemetry` row. `force` makes every row
/// prunable; otherwise a row qualifies only when it is NULL-org (never shipped)
/// or already acked (`rowid <= cloud_sync_cursors.last_hub_rowid`).
fn prunable_predicate(force: bool) -> &'static str {
    if force {
        "1"
    } else {
        "(telemetry.org_id IS NULL \
          OR telemetry.rowid <= COALESCE( \
             (SELECT last_hub_rowid FROM cloud_sync_cursors c \
              WHERE c.org_id = telemetry.org_id), 0))"
    }
}

/// Retention knobs for [`UsageStore::prune`]. Every field is opt-in; a policy
/// with no age, ceiling, or GC set is a no-op.
#[derive(Debug, Clone, Default)]
pub struct PrunePolicy {
    /// A SQLite datetime modifier (e.g. `"-90 days"`); rows older than
    /// `now + modifier` are dropped. `None` disables age pruning. The CLI
    /// builds this from `--older-than 90d`.
    pub older_than: Option<String>,
    /// Hard ceiling on retained `telemetry` rows; the oldest prunable rows are
    /// evicted until at/under it. `None` disables the ceiling.
    pub max_rows: Option<i64>,
    /// Project ids whose checkout the caller found missing on disk. Each is
    /// GC'd only if it is *unregistered* (no org-scoped rows in the hub).
    pub gc_project_ids: Vec<String>,
    /// Prune even un-acked org rows (breaks a pending cloud drain). Off by
    /// default; the safety guard above holds unless this is set.
    pub force: bool,
    /// `VACUUM` after pruning to reclaim file bytes (also happens automatically
    /// for a large prune). Cloud cursors are re-anchored across the `VACUUM`.
    pub vacuum: bool,
    /// Compute the report without deleting anything (rolls the transaction back
    /// and skips `VACUUM`).
    pub dry_run: bool,
}

/// What [`UsageStore::prune`] did (or, under `dry_run`, would do).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// `telemetry` rows removed by the age cutoff.
    pub aged_out: u64,
    /// Rollup rows (`execution_rollup` + `tool_usage_rollup`) removed by the
    /// age cutoff. These never drain, so the cursor guard does not apply.
    pub rollups_aged_out: u64,
    /// `telemetry` rows evicted to satisfy the row ceiling.
    pub ceiling_evicted: u64,
    /// Unregistered, gone-root projects removed.
    pub gc_projects: u64,
    /// `telemetry` rows removed by project GC.
    pub gc_rows: u64,
    /// Un-acked org rows that would have aged out but were kept for the cloud
    /// drain (only counted without `force`).
    pub protected_unacked: u64,
    /// `telemetry` rows still above the ceiling after eviction because they are
    /// un-acked and `force` was not set. Non-zero means "run with `--force` to
    /// go lower, or drain first".
    pub still_over_ceiling: u64,
    /// Whether `VACUUM` ran (and cloud cursors were re-anchored).
    pub vacuumed: bool,
}

/// One line of the global telemetry report: per (org, provider, model)
/// totals across every replicated project. A `None` org is
/// unregistered/local usage.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalTelemetryRow {
    pub org_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cost_usd: f64,
    pub projects: i64,
}

/// One org-scoped hub row awaiting cloud acknowledgement.
///
/// `hub_rowid` addresses the row for the monotonic cloud cursor
/// ([`UsageStore::ack_cloud_synced`]); `source_rowid` is the per-project id the
/// wire contract ships as half of the intake's `(workspace_id, source_rowid)`
/// dedup key (see [`crate::drain`]). They are distinct: the cursor is a hub
/// concern, the dedup key a wire concern.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudTelemetryEvent {
    pub hub_rowid: i64,
    pub org_id: String,
    pub workspace_id: Option<String>,
    pub repo_id: String,
    pub project_id: String,
    pub execution_id: i64,
    pub source_rowid: i64,
    pub recorded_at: String,
    pub telemetry: crate::TelemetryRow,
}

/// Hard cap on the bytes of intake diagnostic text a quarantine record keeps.
///
/// The reason is the *remote* intake's own words. A hub row is content-free by
/// construction (#466), so a well-behaved intake has nothing sensitive to quote
/// back — but the dead-letter table must not become an unbounded channel for
/// whatever a misbehaving or compromised intake decides to echo, so the store
/// truncates rather than trusting the peer.
pub const MAX_QUARANTINE_REASON_BYTES: usize = 512;

/// Why the cloud intake permanently refused one row, as persisted by
/// [`UsageStore::quarantine_cloud_row`].
///
/// Deliberately distinct from the drain's [`crate::drain::DrainRejection`]: only
/// a **terminal, row-attributable** rejection may ever become a quarantine
/// record, and the named constructor says so. A transient failure must retry,
/// and a terminal *batch* failure (bad auth, unsupported schema version) is not
/// attributable to any one row — dead-lettering either would be silent data
/// loss. [`crate::drain::DrainRejection::quarantine_reason`] is the only path
/// the drain loop takes to build one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineReason {
    /// The intake's HTTP status, when the rejection came over HTTP.
    pub http_status: Option<u16>,
    detail: String,
}

impl QuarantineReason {
    /// Build a reason for a rejection the caller has already established is
    /// terminal **and** attributable to this single row. `detail` is truncated
    /// to [`MAX_QUARANTINE_REASON_BYTES`] on a char boundary.
    pub fn terminal(http_status: Option<u16>, detail: &str) -> Self {
        Self {
            http_status,
            detail: truncate_on_char_boundary(detail.trim(), MAX_QUARANTINE_REASON_BYTES)
                .to_string(),
        }
    }

    /// The (truncated) intake diagnostic.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// Longest prefix of `s` that fits in `max` bytes without splitting a `char`.
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// One dead-lettered hub row, retained for inspection after the cloud drain
/// stepped the org cursor past it (#467).
///
/// Content-free by construction (#466): identity, addressing, the per-call
/// telemetry a `stella cloud status` reader needs to recognize the row, and the
/// intake's rejection reason. No prompt, completion, path, or tool payload.
#[derive(Debug, Clone, PartialEq)]
pub struct QuarantinedRow {
    pub org_id: String,
    pub project_id: String,
    /// Half of the intake's `(workspace_id, source_rowid)` identity — what an
    /// operator quotes when asking the intake why it refused the row.
    pub source_rowid: i64,
    pub workspace_id: Option<String>,
    pub repo_id: String,
    /// The hub cursor address the drain advanced past.
    pub hub_rowid: i64,
    pub recorded_at: String,
    pub provider: String,
    pub model: String,
    pub cost_usd: f64,
    /// The intake's diagnostic, truncated to [`MAX_QUARANTINE_REASON_BYTES`].
    pub reason: String,
    pub http_status: Option<u16>,
    pub quarantined_at: String,
}

// The registration-time scope backfill (#406), in its own module.
mod backfill;
// Drain observability: last-attempt record + cursor/backlog readers (#464).
pub mod drain_state;
// Project re-key: merge a forked path-derived identity into the stable one (#408).
mod rekey;

#[cfg(test)]
mod tests {
    use super::*;

    fn rollup(execution_id: i64, tools: Vec<ToolBucket>) -> ExecutionRollupRow {
        ExecutionRollupRow {
            usage_complete: true,
            project_id: "proj_a".into(),
            project_name: "stella".into(),
            project_root: "/w/stella".into(),
            execution_id,
            kind: "deck".into(),
            prompt_digest: "digest".into(),
            prompt_preview: "build the feature".into(),
            model: "glm-5.2".into(),
            provider: "zai".into(),
            outcome: "completed".into(),
            cost_usd: 0.05,
            input_tokens: 61_000,
            output_tokens: 8_192,
            duration_ms: 133_700,
            tool_calls: 3,
            files_written: 0,
            produced_output: false,
            self_rating: None,
            started_at: "2026-07-17T13:00:00Z".into(),
            day: "2026-07-17".into(),
            tool_histogram: tools,
        }
    }

    #[test]
    fn sync_records_project_execution_and_tool_histogram() {
        let usage = UsageStore::in_memory().unwrap();
        usage
            .sync_execution(&rollup(
                1,
                vec![
                    ToolBucket {
                        tool: "grep".into(),
                        surface: "native".into(),
                        calls: 2,
                        errors: 0,
                    },
                    ToolBucket {
                        tool: "read_file".into(),
                        surface: "native".into(),
                        calls: 1,
                        errors: 1,
                    },
                ],
            ))
            .unwrap();
        assert_eq!(usage.project_count().unwrap(), 1);
        assert_eq!(usage.execution_count("proj_a").unwrap(), 1);
        let totals = usage.tool_totals().unwrap();
        assert_eq!(totals[0], ("grep".to_string(), 2));
    }

    #[test]
    fn re_syncing_the_same_execution_is_idempotent() {
        let usage = UsageStore::in_memory().unwrap();
        let r = rollup(
            7,
            vec![ToolBucket {
                tool: "grep".into(),
                surface: "native".into(),
                calls: 2,
                errors: 0,
            }],
        );
        usage.sync_execution(&r).unwrap();
        usage.sync_execution(&r).unwrap(); // re-sync must not double-count
        assert_eq!(usage.execution_count("proj_a").unwrap(), 1);
        assert_eq!(usage.tool_totals().unwrap(), vec![("grep".to_string(), 2)]);
    }

    #[test]
    fn two_projects_aggregate_independently_but_share_tool_totals() {
        let usage = UsageStore::in_memory().unwrap();
        let mut a = rollup(
            1,
            vec![ToolBucket {
                tool: "grep".into(),
                surface: "native".into(),
                calls: 3,
                errors: 0,
            }],
        );
        usage.sync_execution(&a).unwrap();
        a.project_id = "proj_b".into();
        a.project_name = "arena".into();
        a.execution_id = 1;
        usage.sync_execution(&a).unwrap();
        assert_eq!(usage.project_count().unwrap(), 2);
        assert_eq!(usage.tool_totals().unwrap(), vec![("grep".to_string(), 6)]);
    }

    fn scope(org: Option<&str>, workspace: Option<&str>) -> crate::identity::TelemetryScope {
        crate::identity::TelemetryScope {
            org_id: org.map(String::from),
            workspace_id: workspace.map(String::from),
            repo_id: "repo01".into(),
            project_id: "proj_a".into(),
        }
    }

    fn source_row(source_rowid: i64, cost: f64) -> crate::SourceTelemetryRow {
        crate::SourceTelemetryRow {
            source_rowid,
            execution_id: 1,
            recorded_at: "2026-07-23T10:00:00Z".into(),
            telemetry: crate::TelemetryRow {
                step: source_rowid as u64,
                provider: "zai".into(),
                call_role: "engine".into(),
                model: "glm-5.2".into(),
                input_tokens: 1000,
                estimated_input_tokens: 900,
                output_tokens: 100,
                cache_read_tokens: 500,
                cache_miss_tokens: 500,
                cache_write_tokens: 0,
                cost_usd: cost,
                duration_ms: 1200,
                retries: 0,
                tool_calls: 2,
                usage_complete: true,
            },
        }
    }

    #[test]
    fn replication_advances_the_cursor_and_is_idempotent() {
        let hub = UsageStore::in_memory().unwrap();
        let s = scope(None, None);
        assert_eq!(hub.telemetry_cursor("proj_a").unwrap(), 0);
        hub.replicate_telemetry(&s, &[source_row(1, 0.01), source_row(2, 0.02)])
            .unwrap();
        assert_eq!(hub.telemetry_cursor("proj_a").unwrap(), 2);
        // Re-replicating the same rows overwrites, never duplicates.
        hub.replicate_telemetry(&s, &[source_row(1, 0.01), source_row(2, 0.02)])
            .unwrap();
        let totals = hub.global_telemetry_totals(None).unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].calls, 2);
        assert_eq!(totals[0].org_id, None, "unregistered rows carry NULL org");
        assert!((totals[0].cost_usd - 0.03).abs() < 1e-9);
    }

    #[test]
    fn org_scoping_filters_the_report_and_the_cloud_drain() {
        let hub = UsageStore::in_memory().unwrap();
        hub.replicate_telemetry(&scope(None, None), &[source_row(1, 0.01)])
            .unwrap();
        let acme = scope(Some("acme"), Some("ws-1"));
        let mut acme_scope = acme.clone();
        acme_scope.project_id = "proj_b".into();
        hub.replicate_telemetry(&acme_scope, &[source_row(1, 0.05), source_row(2, 0.05)])
            .unwrap();

        // The org filter sees only acme's rows; None sees both groups.
        assert_eq!(
            hub.global_telemetry_totals(Some("acme")).unwrap()[0].calls,
            2
        );
        assert_eq!(hub.global_telemetry_totals(None).unwrap().len(), 2);

        // The cloud drain is org-scoped: NULL-org (unregistered) rows are
        // never shipped.
        let pending = hub.cloud_pending("acme", 10).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].workspace_id.as_deref(), Some("ws-1"));
        assert_eq!(pending[0].repo_id, "repo01");

        // Ack advances monotonically and drains the backlog.
        let last = pending.last().unwrap().hub_rowid;
        hub.ack_cloud_synced("acme", last).unwrap();
        assert!(hub.cloud_pending("acme", 10).unwrap().is_empty());
        hub.ack_cloud_synced("acme", last - 1).unwrap(); // out-of-order ack
        assert!(
            hub.cloud_pending("acme", 10).unwrap().is_empty(),
            "an out-of-order ack never rewinds the cursor"
        );
    }

    // ---- poison-row quarantine (#467) --------------------------------------

    /// Seed one org row and hand back the hub plus its pending event.
    fn hub_with_one_org_row() -> (UsageStore, CloudTelemetryEvent) {
        let hub = UsageStore::in_memory().unwrap();
        hub.replicate_telemetry(
            &scope(Some("acme"), Some("ws-1")),
            &[source_row(1, 0.05), source_row(2, 0.05)],
        )
        .unwrap();
        let event = hub.cloud_pending("acme", 10).unwrap().remove(0);
        (hub, event)
    }

    #[test]
    fn quarantine_advances_the_cursor_past_the_poison_row_in_one_step() {
        let (hub, poison) = hub_with_one_org_row();
        assert_eq!(hub.cloud_pending("acme", 10).unwrap().len(), 2);

        hub.quarantine_cloud_row(
            &poison,
            &QuarantineReason::terminal(Some(400), "unknown provider"),
            poison.hub_rowid,
        )
        .unwrap();

        // The org's newer row is now the only pending one — head-of-line
        // blocking is gone.
        let still_pending: Vec<i64> = hub
            .cloud_pending("acme", 10)
            .unwrap()
            .iter()
            .map(|e| e.source_rowid)
            .collect();
        assert_eq!(still_pending, vec![2]);
        assert_eq!(hub.cloud_quarantine_count(Some("acme")).unwrap(), 1);
    }

    #[test]
    fn quarantine_never_rewinds_the_monotonic_cursor() {
        let (hub, first) = hub_with_one_org_row();
        // Ack everything, then quarantine the *older* row: the cursor must not
        // walk backwards and re-ship the row already confirmed.
        let last = hub.cloud_pending("acme", 10).unwrap().pop().unwrap();
        hub.ack_cloud_synced("acme", last.hub_rowid).unwrap();
        assert!(hub.cloud_pending("acme", 10).unwrap().is_empty());

        hub.quarantine_cloud_row(
            &first,
            &QuarantineReason::terminal(Some(422), "late reject"),
            first.hub_rowid,
        )
        .unwrap();

        assert!(
            hub.cloud_pending("acme", 10).unwrap().is_empty(),
            "quarantining an already-acked row must never rewind the cursor"
        );
    }

    #[test]
    fn quarantining_the_same_row_twice_is_idempotent_and_keeps_the_latest_reason() {
        let (hub, poison) = hub_with_one_org_row();
        hub.quarantine_cloud_row(
            &poison,
            &QuarantineReason::terminal(Some(400), "first diagnosis"),
            poison.hub_rowid,
        )
        .unwrap();
        hub.quarantine_cloud_row(
            &poison,
            &QuarantineReason::terminal(Some(422), "second diagnosis"),
            poison.hub_rowid,
        )
        .unwrap();

        assert_eq!(
            hub.cloud_quarantine_count(Some("acme")).unwrap(),
            1,
            "a replayed quarantine must not double-count"
        );
        let rows = hub.cloud_quarantined(Some("acme"), 10).unwrap();
        assert_eq!(rows[0].reason, "second diagnosis");
        assert_eq!(rows[0].http_status, Some(422));
    }

    #[test]
    fn quarantine_is_org_scoped_and_reports_counts_by_reason() {
        let hub = UsageStore::in_memory().unwrap();
        for (org, project) in [("acme", "proj_a"), ("globex", "proj_b")] {
            let mut s = scope(Some(org), Some("ws-1"));
            s.project_id = project.into();
            hub.replicate_telemetry(&s, &[source_row(1, 0.05), source_row(2, 0.05)])
                .unwrap();
        }
        for org in ["acme", "acme", "globex"] {
            let e = hub.cloud_pending(org, 10).unwrap().remove(0);
            hub.quarantine_cloud_row(
                &e,
                &QuarantineReason::terminal(Some(400), "unknown provider"),
                e.hub_rowid,
            )
            .unwrap();
        }

        assert_eq!(hub.cloud_quarantine_count(Some("acme")).unwrap(), 2);
        assert_eq!(hub.cloud_quarantine_count(Some("globex")).unwrap(), 1);
        assert_eq!(hub.cloud_quarantine_count(None).unwrap(), 3, "all orgs");
        let reasons = hub.cloud_quarantine_reasons(Some("acme")).unwrap();
        assert_eq!(
            reasons,
            vec![("unknown provider".to_string(), Some(400), 2)]
        );
        assert!(
            hub.cloud_quarantined(Some("globex"), 10)
                .unwrap()
                .iter()
                .all(|q| q.org_id == "globex"),
            "the inspection view is org-scoped"
        );
    }

    /// The hard invariant from #466 / AGENTS.md #3: the dead-letter store keeps
    /// a row for inspection, so it must not become a content leak. The column
    /// set is pinned to identity + addressing + content-free telemetry + the
    /// intake's diagnostic — adding a prompt/completion/path column here fails
    /// this test.
    #[test]
    fn the_quarantine_table_is_content_free_by_construction() {
        let hub = UsageStore::in_memory().unwrap();
        let conn = hub.lock();
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('cloud_quarantine')")
            .unwrap();
        let mut columns: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        columns.sort();
        let mut allowed: Vec<String> = [
            // identity + addressing
            "org_id",
            "project_id",
            "source_rowid",
            "workspace_id",
            "repo_id",
            "hub_rowid",
            "recorded_at",
            // content-free telemetry
            "provider",
            "model",
            "input_tokens",
            "output_tokens",
            "cost_usd",
            // why it was refused
            "reason",
            "http_status",
            "quarantined_at",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        allowed.sort();
        assert_eq!(
            columns, allowed,
            "cloud_quarantine columns drifted — a dead-letter record is \
             identity + telemetry + rejection reason ONLY, never content"
        );
    }

    #[test]
    fn a_quarantine_reason_cannot_grow_without_bound() {
        let (hub, poison) = hub_with_one_org_row();
        // A hostile/buggy intake echoing megabytes back must not turn the
        // dead-letter table into a channel.
        // The leading ASCII byte puts every multi-byte char on odd offsets, so
        // the cap lands mid-char and the boundary walk is genuinely exercised.
        let flood = format!("x{}", "é".repeat(MAX_QUARANTINE_REASON_BYTES));
        let reason = QuarantineReason::terminal(None, &flood);
        assert!(reason.detail().len() < MAX_QUARANTINE_REASON_BYTES);
        assert!(
            flood.starts_with(reason.detail()),
            "truncation keeps a valid prefix on a char boundary"
        );

        hub.quarantine_cloud_row(&poison, &reason, poison.hub_rowid)
            .unwrap();
        let stored = hub.cloud_quarantined(Some("acme"), 1).unwrap();
        assert!(stored[0].reason.len() <= MAX_QUARANTINE_REASON_BYTES);
    }

    #[test]
    fn quarantine_reason_trims_and_preserves_short_diagnostics() {
        let r = QuarantineReason::terminal(Some(400), "  model id not recognized\n");
        assert_eq!(r.detail(), "model id not recognized");
        assert_eq!(r.http_status, Some(400));
    }

    // ---- retention / prune -------------------------------------------------

    fn source_row_at(source_rowid: i64, cost: f64, recorded_at: &str) -> crate::SourceTelemetryRow {
        let mut r = source_row(source_rowid, cost);
        r.recorded_at = recorded_at.into();
        r
    }

    fn telemetry_rows(hub: &UsageStore) -> i64 {
        hub.lock()
            .query_row("SELECT COUNT(*) FROM telemetry", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn age_prune_spares_unacked_org_rows_unless_forced() {
        let hub = UsageStore::in_memory().unwrap();
        hub.replicate_telemetry(
            &scope(Some("acme"), Some("ws-1")),
            &[
                source_row_at(1, 0.01, "2000-01-01T00:00:00Z"),
                source_row_at(2, 0.01, "2000-01-01T00:00:00Z"),
            ],
        )
        .unwrap();

        // Nothing acked → both rows are un-acked. Age-prune matches them but
        // protects them for the drain.
        let report = hub
            .prune(&PrunePolicy {
                older_than: Some("-1 days".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 0, "un-acked org rows are never dropped");
        assert_eq!(report.protected_unacked, 2);
        assert_eq!(telemetry_rows(&hub), 2);

        // --force overrides the guard.
        let report = hub
            .prune(&PrunePolicy {
                older_than: Some("-1 days".into()),
                force: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 2);
        assert_eq!(telemetry_rows(&hub), 0);
    }

    #[test]
    fn age_prune_drops_acked_and_null_org_rows() {
        let hub = UsageStore::in_memory().unwrap();
        // One acked org row + one NULL-org row, both ancient.
        hub.replicate_telemetry(
            &scope(Some("acme"), Some("ws-1")),
            &[source_row_at(1, 0.01, "2000-01-01T00:00:00Z")],
        )
        .unwrap();
        let mut local = scope(None, None);
        local.project_id = "proj_local".into();
        hub.replicate_telemetry(&local, &[source_row_at(1, 0.01, "2000-01-01T00:00:00Z")])
            .unwrap();
        // Ack the org row (its hub rowid is 1).
        hub.ack_cloud_synced("acme", 1).unwrap();

        let report = hub
            .prune(&PrunePolicy {
                older_than: Some("-1 days".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 2, "acked org + NULL-org rows both drop");
        assert_eq!(report.protected_unacked, 0);
        assert_eq!(telemetry_rows(&hub), 0);
    }

    #[test]
    fn gc_removes_unregistered_projects_only() {
        let hub = UsageStore::in_memory().unwrap();
        let mut local = scope(None, None);
        local.project_id = "proj_local".into();
        hub.replicate_telemetry(&local, &[source_row(1, 0.01), source_row(2, 0.01)])
            .unwrap();
        let mut acme = scope(Some("acme"), Some("ws-1"));
        acme.project_id = "proj_acme".into();
        hub.replicate_telemetry(&acme, &[source_row(1, 0.05)])
            .unwrap();

        // The caller marks BOTH as gone-root; only the unregistered one is GC'd.
        let report = hub
            .prune(&PrunePolicy {
                gc_project_ids: vec!["proj_local".into(), "proj_acme".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.gc_projects, 1);
        assert_eq!(report.gc_rows, 2);
        assert_eq!(
            telemetry_rows(&hub),
            1,
            "the registered project's row stays"
        );
        assert_eq!(
            hub.cloud_pending("acme", 10).unwrap().len(),
            1,
            "GC never touches a project that can still drain"
        );
    }

    #[test]
    fn ceiling_evicts_oldest_prunable_and_reports_residual() {
        let hub = UsageStore::in_memory().unwrap();
        // 3 NULL-org rows (prunable) then 2 un-acked org rows (protected).
        hub.replicate_telemetry(
            &scope(None, None),
            &[
                source_row(1, 0.01),
                source_row(2, 0.01),
                source_row(3, 0.01),
            ],
        )
        .unwrap();
        let mut acme = scope(Some("acme"), Some("ws-1"));
        acme.project_id = "proj_acme".into();
        hub.replicate_telemetry(&acme, &[source_row(1, 0.05), source_row(2, 0.05)])
            .unwrap();

        // Cap at 1: the 3 NULL-org rows evict, but the 2 un-acked org rows can't.
        let report = hub
            .prune(&PrunePolicy {
                max_rows: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.ceiling_evicted, 3);
        assert_eq!(report.still_over_ceiling, 1, "un-acked rows block the cap");
        assert_eq!(telemetry_rows(&hub), 2);

        // --force reaches the cap.
        let report = hub
            .prune(&PrunePolicy {
                max_rows: Some(1),
                force: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.ceiling_evicted, 1);
        assert_eq!(report.still_over_ceiling, 0);
        assert_eq!(telemetry_rows(&hub), 1);
    }

    #[test]
    fn dry_run_reports_without_deleting() {
        let hub = UsageStore::in_memory().unwrap();
        hub.replicate_telemetry(
            &scope(None, None),
            &[
                source_row_at(1, 0.01, "2000-01-01T00:00:00Z"),
                source_row_at(2, 0.01, "2000-01-01T00:00:00Z"),
            ],
        )
        .unwrap();
        let report = hub
            .prune(&PrunePolicy {
                older_than: Some("-1 days".into()),
                vacuum: true, // ignored under dry_run
                dry_run: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 2, "dry run reports what it would drop");
        assert!(!report.vacuumed, "dry run never vacuums");
        assert_eq!(telemetry_rows(&hub), 2, "dry run deletes nothing");
    }

    #[test]
    fn vacuum_reanchors_cloud_cursor_across_rowid_renumber() {
        // A real file-backed hub so VACUUM performs its rowid renumber.
        let tmp = tempfile::tempdir().unwrap();
        let hub = UsageStore::open_at(&tmp.path().join("usage.db")).unwrap();

        // Rows 1–2 ancient (age out); 3–5 far-future (retained).
        hub.replicate_telemetry(
            &scope(Some("acme"), Some("ws-1")),
            &[
                source_row_at(1, 0.01, "2000-01-01T00:00:00Z"),
                source_row_at(2, 0.01, "2000-01-01T00:00:00Z"),
                source_row_at(3, 0.01, "2999-01-01T00:00:00Z"),
                source_row_at(4, 0.01, "2999-01-01T00:00:00Z"),
                source_row_at(5, 0.01, "2999-01-01T00:00:00Z"),
            ],
        )
        .unwrap();
        // Ack the first three (cursor = hub rowid of the 3rd pending row).
        let pending = hub.cloud_pending("acme", 10).unwrap();
        let third = pending[2].hub_rowid;
        hub.ack_cloud_synced("acme", third).unwrap();

        // Drop the two ancient (acked → prunable) rows, then VACUUM — which
        // renumbers the surviving rowids 3,4,5 down to 1,2,3.
        let report = hub
            .prune(&PrunePolicy {
                older_than: Some("-1 days".into()),
                vacuum: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 2);
        assert!(report.vacuumed);

        // The un-acked rows (source_rowid 4 and 5) must still be surfaced. A
        // naive VACUUM leaves the cursor at the stale rowid 3, so this would be
        // empty — the re-anchor is what keeps the drain whole.
        let pending = hub.cloud_pending("acme", 10).unwrap();
        let ids: Vec<i64> = pending.iter().map(|e| e.source_rowid).collect();
        assert_eq!(ids, vec![4, 5], "un-acked rows survive the rowid renumber");
    }

    #[test]
    fn small_prune_without_vacuum_does_not_strand_replayed_rows() {
        // A prune below the auto-VACUUM threshold and without `--vacuum` still
        // frees the deleted rowids; SQLite reuses them for the next insert. The
        // cursor clamp must stop a replayed row landing at/below a stale cursor
        // and never draining. (Regression: before the clamp this returned [].)
        let tmp = tempfile::tempdir().unwrap();
        let hub = UsageStore::open_at(&tmp.path().join("usage.db")).unwrap();
        hub.replicate_telemetry(
            &scope(Some("acme"), Some("ws-1")),
            &[
                source_row_at(1, 0.01, "2000-01-01T00:00:00Z"),
                source_row_at(2, 0.01, "2000-01-01T00:00:00Z"),
                source_row_at(3, 0.01, "2000-01-01T00:00:00Z"),
            ],
        )
        .unwrap();
        let last = hub
            .cloud_pending("acme", 10)
            .unwrap()
            .last()
            .unwrap()
            .hub_rowid;
        hub.ack_cloud_synced("acme", last).unwrap();

        let report = hub
            .prune(&PrunePolicy {
                older_than: Some("-1 days".into()),
                vacuum: false,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 3);
        assert!(!report.vacuumed, "small prune must not vacuum");

        hub.replicate_telemetry(
            &scope(Some("acme"), Some("ws-1")),
            &[source_row_at(4, 0.01, "2999-01-01T00:00:00Z")],
        )
        .unwrap();
        let ids: Vec<i64> = hub
            .cloud_pending("acme", 10)
            .unwrap()
            .iter()
            .map(|e| e.source_rowid)
            .collect();
        assert_eq!(
            ids,
            vec![4],
            "a row replicated after a non-vacuum prune must still be drainable"
        );
    }

    /// `replicate_telemetry` advances the source cursor with `MAX(...)`, so it
    /// structurally cannot undo itself. `stella stats prune` needs exactly
    /// that: pruning `store.db` frees and (per SQLite's docs) may renumber the
    /// `telemetry.rowid`s this watermark addresses, leaving it pointing past
    /// rows that never shipped.
    #[test]
    fn rewind_telemetry_cursor_can_lower_a_watermark_replication_cannot() {
        let hub = UsageStore::in_memory().unwrap();
        let scope = scope(None, None);
        hub.replicate_telemetry(&scope, &[source_row(7, 0.01)])
            .unwrap();
        assert_eq!(hub.telemetry_cursor(&scope.project_id).unwrap(), 7);

        // Replication alone can never walk it back…
        hub.replicate_telemetry(&scope, &[source_row(2, 0.01)])
            .unwrap();
        assert_eq!(
            hub.telemetry_cursor(&scope.project_id).unwrap(),
            7,
            "MAX() keeps an out-of-order batch from rewinding the cursor"
        );

        // …the prune repair path can.
        hub.rewind_telemetry_cursor(&scope.project_id, 2).unwrap();
        assert_eq!(hub.telemetry_cursor(&scope.project_id).unwrap(), 2);

        // And it works for a project the hub has never seen (cursor row
        // absent), which is the never-synced workspace case.
        hub.rewind_telemetry_cursor("proj_unknown", 0).unwrap();
        assert_eq!(hub.telemetry_cursor("proj_unknown").unwrap(), 0);
    }

    #[test]
    fn project_id_is_stable_and_path_derived() {
        let a = project_id_for(Path::new("/tmp"));
        let b = project_id_for(Path::new("/tmp"));
        assert_eq!(a, b, "same path → same id");
        assert_eq!(a.len(), 16, "16 hex chars");
    }

    #[test]
    fn usage_db_path_honors_the_data_dir_override() {
        // SAFETY: single-threaded test; we set and read one process env var.
        unsafe {
            std::env::set_var("STELLA_DATA_DIR", "/tmp/stella-usage-test");
        }
        assert_eq!(
            usage_db_path(),
            PathBuf::from("/tmp/stella-usage-test/usage.db")
        );
        unsafe {
            std::env::remove_var("STELLA_DATA_DIR");
        }
    }

    #[cfg(unix)]
    #[test]
    fn usage_store_repairs_private_dir_and_file_modes() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let private = tmp.path().join("stella-data");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o777)).unwrap();
        let db = private.join("usage.db");

        drop(UsageStore::open_at(&db).unwrap());
        let mode = |path: &Path| {
            std::fs::symlink_metadata(path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(&private), 0o700);
        assert_eq!(mode(&db), 0o600);

        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o666)).unwrap();
        drop(UsageStore::open_at(&db).unwrap());
        assert_eq!(mode(&db), 0o600, "existing private DB is repaired");
    }

    #[cfg(unix)]
    #[test]
    fn usage_store_rejects_a_symlink_database() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let private = tmp.path().join("stella-data");
        std::fs::create_dir_all(&private).unwrap();
        let target = tmp.path().join("outside.db");
        std::fs::write(&target, b"outside").unwrap();
        symlink(&target, private.join("usage.db")).unwrap();
        assert!(UsageStore::open_at(&private.join("usage.db")).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"outside");
    }
}
