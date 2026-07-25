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
//! back to a project store, and a missing/again-openable `usage.db` never
//! blocks a turn — sync is best-effort.
//!
//! Retention: the hub accumulates one telemetry row per model call across every
//! project forever, and rows from deleted checkouts persist. [`UsageStore::prune`]
//! bounds that growth — by age, by a hard row ceiling, and by GC of unregistered
//! projects whose checkout is gone — without ever dropping an org's un-acked
//! cloud-drain rows (see [`UsageStore::prune`] and [`PrunePolicy`]).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, params};

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

/// A stable, dependency-free project identity: FNV-1a/64 of the canonical
/// workspace path, hex-encoded. Deterministic across runs and processes so the
/// same repo always rolls up under one id. (Not cryptographic — it only needs
/// to be stable and collision-resistant for a handful of local paths.)
pub fn project_id_for(root: &Path) -> String {
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let s = canon.to_string_lossy();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
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
CREATE INDEX IF NOT EXISTS execution_rollup_by_model
    ON execution_rollup(model, project_id);
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
        // — skip the additive fold to stay idempotent.
        let already: bool = tx
            .query_row(
                "SELECT 1 FROM execution_rollup WHERE project_id = ?1 AND execution_id = ?2",
                params![r.project_id, r.execution_id],
                |_| Ok(()),
            )
            .is_ok();
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
    pub fn telemetry_cursor(&self, project_id: &str) -> Result<i64> {
        Ok(self
            .lock()
            .query_row(
                "SELECT last_source_rowid FROM telemetry_sync_cursors WHERE project_id = ?1",
                params![project_id],
                |r| r.get(0),
            )
            .unwrap_or(0))
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
    /// `(project_id, source_rowid)` key — see [`Self::vacuum_and_reanchor`].
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
                let registered = tx
                    .query_row(
                        "SELECT 1 FROM telemetry \
                         WHERE project_id = ?1 AND org_id IS NOT NULL LIMIT 1",
                        params![pid],
                        |_| Ok(()),
                    )
                    .is_ok();
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
