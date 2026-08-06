//! Read-only queries over the workspace's `.stella` SQLite stores.
//!
//! The observatory deliberately does **not** go through `stella-store`'s
//! `Store::open`: opening the store creates `.stella/` and runs schema
//! migrations — writes — and an observer must never mutate what it observes.
//! Instead every request opens the database file with
//! `SQLITE_OPEN_READ_ONLY` and runs its own SQL, mirroring the semantics of
//! `Store::usage_stats` (resolved = outcome `completed`; division
//! `off-grid` for provider `local`). Missing files and missing tables are
//! states, not errors — each section degrades to an empty payload so a
//! fresh workspace renders an empty dashboard instead of a 500.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

/// Day bucket for rows SQLite's `date()` can't parse — see
/// [`Observatory::activity`]. The dashboard matches on this exact string to
/// keep the bucket off its date axis, so the two must not drift apart.
pub const UNDATED: &str = "undated";

/// Newest executions returned by [`Observatory::executions`] and drawn on
/// [`Observatory::overview`]'s timeline. The dashboard re-fetches both every
/// 5 s and filters client-side by timeframe, so an unbounded listing made
/// every poll pay for the store's whole history. The cap loses nothing the
/// page can show: the headline tiles for the "all" timeframe come from the
/// aggregate queries (which stay exact), and 500 bars is already past
/// sub-pixel in the 640-px-wide timeline chart.
pub(crate) const MAX_LISTED_EXECUTIONS: usize = 500;

/// Newest rows returned by [`Observatory::reflection_ratings`]. Same 5 s-poll
/// reasoning as `MAX_LISTED_EXECUTIONS`, and the same horizon, so the
/// self-improvement tab's ratings line up with the runs the tables list.
pub(crate) const MAX_LISTED_REFLECTIONS: usize = 500;

/// Newest tool calls the [`Observatory::tools`] leaderboard aggregates. This
/// is the store's highest-cardinality table and the p50 needs every duration
/// in the window materialized, so scanning all of history on every 5 s poll
/// grew without bound; the window keeps the leaderboard reading as current
/// behaviour instead.
pub(crate) const TOOL_CALL_SCAN_WINDOW: usize = 10_000;

/// Running tool calls carried inline on the live stream
/// ([`Observatory::live`]).
///
/// A bound rather than a limit anyone should reach: this counts calls
/// *concurrently in flight*, which even a wide fleet fan-out keeps in the
/// dozens. The cap exists so a workspace whose rows were left `running` by a
/// pre-v18 binary — before anything swept them — cannot make every frame of
/// the stream enormous.
pub(crate) const MAX_RUNNING_CALLS: usize = 200;

/// Char cap on one transcript body in the default
/// [`Observatory::execution_journal`] payload. A single `read_file` result
/// can be megabytes; the drawer needs the shape of the turn, not the bytes,
/// and `?full=1` is the escape hatch for the reader who wants them.
pub(crate) const JOURNAL_BODY_CLIP: usize = 4_000;

/// Everything that can go wrong serving observatory data.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The underlying SQLite read failed in a way that isn't "the table
    /// doesn't exist yet" (those degrade to empty results instead).
    #[error("telemetry query failed: {0}")]
    Query(#[from] rusqlite::Error),
}

/// A read-only handle on the workspace's telemetry stores.
#[derive(Debug)]
pub struct Observatory {
    store_db: PathBuf,
    fleet_db: PathBuf,
    workspace_root: PathBuf,
}

impl Observatory {
    /// Point the observatory at a workspace root (the directory that holds
    /// `.stella/`). Nothing is opened or created here.
    #[must_use]
    pub fn new(workspace_root: &Path) -> Self {
        let dot = workspace_root.join(".stella");
        Self {
            store_db: dot.join("private").join("store.db"),
            fleet_db: dot.join("private").join("fleet.db"),
            workspace_root: workspace_root.to_path_buf(),
        }
    }

    /// Open `store.db` read-only, or `None` while it doesn't exist yet.
    fn store(&self) -> Option<Connection> {
        open_read_only(&self.store_db)
    }

    /// Open `fleet.db` read-only, or `None` while it doesn't exist yet.
    fn fleet_conn(&self) -> Option<Connection> {
        open_read_only(&self.fleet_db)
    }

    /// Workspace identity + which stores exist — the dashboard header.
    #[must_use]
    pub fn meta(&self) -> Value {
        json!({
            "workspace": self.workspace_root.display().to_string(),
            "store_db": self.store_db.exists(),
            "fleet_db": self.fleet_db.exists(),
        })
    }

    /// Headline aggregates: runs, resolve rate, spend, tokens, cache traffic,
    /// and the per-execution timeline the overview chart draws.
    pub fn overview(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!({ "runs": 0, "timeline": [] }));
        };
        let head = or_empty(conn.query_row(
            "SELECT count(*),
                    count(*) FILTER (WHERE outcome = 'completed'),
                    coalesce(sum(cost_usd), 0)
             FROM executions",
            [],
            |r| {
                Ok(json!({
                    "runs": r.get::<_, i64>(0)?,
                    "resolved": r.get::<_, i64>(1)?,
                    "cost_usd": r.get::<_, f64>(2)?,
                }))
            },
        ))?;
        let tokens = or_empty(conn.query_row(
            "SELECT coalesce(sum(input_tokens), 0),
                    coalesce(sum(output_tokens), 0),
                    coalesce(sum(cache_read_tokens), 0),
                    coalesce(sum(cache_write_tokens), 0),
                    coalesce(sum(cache_miss_tokens), 0),
                    coalesce(sum(duration_ms), 0),
                    count(*)
             FROM telemetry",
            [],
            |r| {
                Ok(json!({
                    "input_tokens": r.get::<_, i64>(0)?,
                    "output_tokens": r.get::<_, i64>(1)?,
                    "cache_read_tokens": r.get::<_, i64>(2)?,
                    "cache_write_tokens": r.get::<_, i64>(3)?,
                    "cache_miss_tokens": r.get::<_, i64>(4)?,
                    "model_ms": r.get::<_, i64>(5)?,
                    "steps": r.get::<_, i64>(6)?,
                }))
            },
        ))?;
        // Newest first so the LIMIT keeps the recent end of history; reversed
        // below because the chart draws left-to-right in id order.
        let mut timeline = collect_rows(
            &conn,
            &format!(
                "SELECT e.id, e.kind, e.provider, e.model, e.outcome, e.cost_usd,
                        e.started_at, e.finished_at,
                        coalesce(t.input_tokens, 0), coalesce(t.output_tokens, 0),
                        coalesce(t.duration_ms, 0)
                 FROM executions e
                 LEFT JOIN (SELECT execution_id,
                                   sum(input_tokens)  AS input_tokens,
                                   sum(output_tokens) AS output_tokens,
                                   sum(duration_ms)   AS duration_ms
                            FROM telemetry GROUP BY execution_id) t
                   ON t.execution_id = e.id
                 ORDER BY e.id DESC LIMIT {MAX_LISTED_EXECUTIONS}"
            ),
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "provider": r.get::<_, String>(2)?,
                    "model": r.get::<_, String>(3)?,
                    "outcome": r.get::<_, Option<String>>(4)?,
                    "cost_usd": r.get::<_, f64>(5)?,
                    "started_at": r.get::<_, String>(6)?,
                    "finished_at": r.get::<_, Option<String>>(7)?,
                    "input_tokens": r.get::<_, i64>(8)?,
                    "output_tokens": r.get::<_, i64>(9)?,
                    "model_ms": r.get::<_, i64>(10)?,
                }))
            },
        )?;
        timeline.reverse();
        let mut out = head;
        merge(&mut out, tokens);
        out["timeline"] = Value::Array(timeline);
        Ok(out)
    }

    /// The executions table: one row per run with its aggregates, newest
    /// first, capped at `MAX_LISTED_EXECUTIONS`.
    pub fn executions(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!([]));
        };
        let rows = collect_rows(
            &conn,
            &format!(
                "SELECT e.id, e.kind, e.prompt, e.provider, e.model, e.outcome,
                        e.cost_usd, e.started_at, e.finished_at,
                        coalesce(t.steps, 0), coalesce(t.input_tokens, 0),
                        coalesce(t.output_tokens, 0), coalesce(t.duration_ms, 0),
                        coalesce(tc.calls, 0), coalesce(ft.files, 0)
                 FROM executions e
                 LEFT JOIN (SELECT execution_id, count(*) AS steps,
                                   sum(input_tokens)  AS input_tokens,
                                   sum(output_tokens) AS output_tokens,
                                   sum(duration_ms)   AS duration_ms
                            FROM telemetry GROUP BY execution_id) t
                   ON t.execution_id = e.id
                 LEFT JOIN (SELECT execution_id, count(*) AS calls
                            FROM tool_calls GROUP BY execution_id) tc
                   ON tc.execution_id = e.id
                 LEFT JOIN (SELECT execution_id, count(*) AS files
                            FROM files_touched GROUP BY execution_id) ft
                   ON ft.execution_id = e.id
                 ORDER BY e.id DESC LIMIT {MAX_LISTED_EXECUTIONS}"
            ),
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "prompt": truncate(r.get::<_, String>(2)?, 160),
                    "provider": r.get::<_, String>(3)?,
                    "model": r.get::<_, String>(4)?,
                    "outcome": r.get::<_, Option<String>>(5)?,
                    "cost_usd": r.get::<_, f64>(6)?,
                    "started_at": r.get::<_, String>(7)?,
                    "finished_at": r.get::<_, Option<String>>(8)?,
                    "steps": r.get::<_, i64>(9)?,
                    "input_tokens": r.get::<_, i64>(10)?,
                    "output_tokens": r.get::<_, i64>(11)?,
                    "model_ms": r.get::<_, i64>(12)?,
                    "tool_calls": r.get::<_, i64>(13)?,
                    "files_touched": r.get::<_, i64>(14)?,
                }))
            },
        )?;
        Ok(Value::Array(rows))
    }

    /// One execution drilled all the way down: step telemetry, tool calls,
    /// files touched, and the post-turn self-reflection when one exists.
    pub fn execution(&self, id: i64) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(Value::Null);
        };
        let head = or_empty(conn.query_row(
            "SELECT id, kind, prompt, provider, model, outcome, cost_usd,
                    started_at, finished_at
             FROM executions WHERE id = ?1",
            [id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "prompt": r.get::<_, String>(2)?,
                    "provider": r.get::<_, String>(3)?,
                    "model": r.get::<_, String>(4)?,
                    "outcome": r.get::<_, Option<String>>(5)?,
                    "cost_usd": r.get::<_, f64>(6)?,
                    "started_at": r.get::<_, String>(7)?,
                    "finished_at": r.get::<_, Option<String>>(8)?,
                }))
            },
        ))?;
        if head == json!({}) {
            return Ok(Value::Null);
        }
        let steps = collect_rows_for(
            &conn,
            id,
            "SELECT step, provider, model, input_tokens, output_tokens,
                    cache_read_tokens, cache_miss_tokens, cache_write_tokens,
                    cost_usd, duration_ms, retries, tool_calls
             FROM telemetry WHERE execution_id = ?1 ORDER BY step ASC",
            |r| {
                Ok(json!({
                    "step": r.get::<_, i64>(0)?,
                    "provider": r.get::<_, String>(1)?,
                    "model": r.get::<_, String>(2)?,
                    "input_tokens": r.get::<_, i64>(3)?,
                    "output_tokens": r.get::<_, i64>(4)?,
                    "cache_read_tokens": r.get::<_, i64>(5)?,
                    "cache_miss_tokens": r.get::<_, i64>(6)?,
                    "cache_write_tokens": r.get::<_, i64>(7)?,
                    "cost_usd": r.get::<_, f64>(8)?,
                    "duration_ms": r.get::<_, i64>(9)?,
                    "retries": r.get::<_, i64>(10)?,
                    "tool_calls": r.get::<_, i64>(11)?,
                }))
            },
        )?;
        let tools = collect_rows_for(
            &conn,
            id,
            "SELECT seq, name, surface, reason, ok, error, bytes_out,
                    duration_ms, ts
             FROM tool_calls WHERE execution_id = ?1 ORDER BY seq ASC",
            |r| {
                Ok(json!({
                    "seq": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "surface": r.get::<_, String>(2)?,
                    "reason": r.get::<_, String>(3)?,
                    "ok": r.get::<_, i64>(4)? != 0,
                    "error": r.get::<_, String>(5)?,
                    "bytes_out": r.get::<_, i64>(6)?,
                    "duration_ms": r.get::<_, i64>(7)?,
                    "ts": r.get::<_, String>(8)?,
                }))
            },
        )?;
        let files = collect_rows_for(
            &conn,
            id,
            "SELECT path, ops, lines_added, lines_removed
             FROM files_touched WHERE execution_id = ?1 ORDER BY path ASC",
            |r| {
                Ok(json!({
                    "path": r.get::<_, String>(0)?,
                    "ops": r.get::<_, String>(1)?,
                    "lines_added": r.get::<_, i64>(2)?,
                    "lines_removed": r.get::<_, i64>(3)?,
                }))
            },
        )?;
        let reflection = or_empty(conn.query_row(
            "SELECT delivered, self_rating, what_went_well, what_to_improve,
                    critique
             FROM execution_reflection WHERE execution_id = ?1",
            [id],
            |r| {
                Ok(json!({
                    "delivered": r.get::<_, Option<i64>>(0)?,
                    "self_rating": r.get::<_, Option<i64>>(1)?,
                    "what_went_well": r.get::<_, String>(2)?,
                    "what_to_improve": r.get::<_, String>(3)?,
                    "critique": r.get::<_, String>(4)?,
                }))
            },
        ))?;
        let mut out = head;
        out["steps"] = Value::Array(steps);
        out["tools"] = Value::Array(tools);
        out["files"] = Value::Array(files);
        out["recall"] = Value::Array(recall_timings(&conn, id)?);
        out["reflection"] = if reflection == json!({}) {
            Value::Null
        } else {
            reflection
        };
        Ok(out)
    }

    /// One execution's transcript, replayed from its slice of the `events`
    /// journal (#1461): the answer text, reasoning and tool traffic the
    /// telemetry projections deliberately do not carry.
    ///
    /// This is the second sanctioned read of `events` (after
    /// `recall_timings`) and it obeys the same bargain: the filter is
    /// `execution_id`-first, which the store's `UNIQUE (execution_id, seq)`
    /// index serves directly. The whole transcript is fetched once on
    /// drawer-open; while the execution is still running, the drawer polls
    /// back with `after_seq` set to the highest `seq` it already has
    /// (#1476), so a long run's transcript is never re-downloaded in full —
    /// the same `(execution_id, seq)` index serves `seq > ?` directly. A
    /// cross-execution transcript view would need a projection, not a wider
    /// `WHERE`.
    ///
    /// `text_delta` rows never appear: the journal's `text` event is the
    /// authoritative full answer and the deltas are a live-preview artifact.
    /// Most stores hold none (the journal drops them at write time), but the
    /// filter is contract, not assumption. Bodies are clipped to
    /// `JOURNAL_BODY_CLIP` chars unless `full`; a clipped entry says so
    /// (`truncated: true`) instead of presenting the elision as the data.
    pub fn execution_journal(
        &self,
        id: i64,
        full: bool,
        after_seq: Option<i64>,
    ) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(Value::Array(Vec::new()));
        };
        // `-1` rather than `0` when unfiltered: `seq` starts at 0
        // (`Store::record_event`'s first call), and a sentinel of 0 would
        // silently drop that opening row from every unfiltered fetch.
        let after = after_seq.unwrap_or(-1);
        let sql = "SELECT seq, ts, event_type, payload
             FROM events
             WHERE execution_id = ?1
               AND seq > ?2
               AND event_type IN ('stage', 'text', 'reasoning', 'tool_start',
                                  'tool_result', 'speculation_discarded')
             ORDER BY seq ASC";
        let mut stmt = match conn.prepare(sql) {
            Ok(stmt) => stmt,
            Err(e) if is_missing_schema(&e) => return Ok(Value::Array(Vec::new())),
            Err(e) => return Err(e.into()),
        };
        let mapped = stmt.query_map([id, after], |r| {
            Ok(json!({
                "seq": r.get::<_, i64>(0)?,
                "ts": r.get::<_, String>(1)?,
                "type": r.get::<_, String>(2)?,
                "payload": r.get::<_, String>(3)?,
            }))
        })?;
        let mut rows = Vec::new();
        for row in mapped {
            rows.push(row?);
        }
        Ok(Value::Array(
            rows.into_iter()
                .map(|row| journal_entry(row, full))
                .collect(),
        ))
    }

    /// One model call's **sent context** (#1475): the messages that call was
    /// given, rebuilt from its persisted receipt and the event journal — what
    /// `stella inspect <execution> --step N` prints in the terminal.
    ///
    /// The transcript route above replays what a run *did*; this one answers
    /// what a call *was sent*, system prompt included, which no other view in
    /// the dashboard carries. Always returns the index of the execution's
    /// recorded calls, because that is what the drawer drills from; `step`
    /// additionally selects one of them to reconstruct (`turn` and `call_seq`
    /// default to the worker call, as they do on the CLI).
    ///
    /// Bodies obey the transcript route's clip policy — `JOURNAL_BODY_CLIP`
    /// chars unless `full`, and a clipped message says so. An execution with no
    /// receipts (a store recorded with `context.lifecycle` off, or one older
    /// than the receipts plane) answers with an explicit no-receipts payload;
    /// missing is a state here as everywhere else, never an error.
    ///
    /// The fold itself lives in `sent_context`, which documents why the
    /// observatory re-implements it instead of linking `stella-store`.
    pub fn execution_context(
        &self,
        id: i64,
        turn: i64,
        step: Option<i64>,
        call_seq: i64,
        full: bool,
    ) -> Result<Value, DbError> {
        match self.store() {
            Some(conn) => crate::sent_context::payload(&conn, id, turn, step, call_seq, full),
            None => Ok(crate::sent_context::no_receipts(id)),
        }
    }

    /// What changed between two model calls (#1511) — the diff between the
    /// addressed call's reconstruction and its resolved baseline, in the JSON
    /// shape `stella inspect --diff --format json` emits. The fold lives in
    /// `context_diff`, which documents the baseline semantics it mirrors.
    pub fn execution_context_diff(
        &self,
        id: i64,
        turn: i64,
        step: i64,
        call_seq: i64,
        base: &str,
        only: &str,
    ) -> Result<Value, DbError> {
        match self.store() {
            Some(conn) => crate::context_diff::payload(&conn, id, turn, step, call_seq, base, only),
            None => Ok(crate::sent_context::no_receipts(id)),
        }
    }

    /// Every session of this workspace: the registry's live/crashed view
    /// joined with the store's per-session rollups. The fold lives in
    /// `sessions`, which documents the two-source merge.
    pub fn sessions(&self) -> Result<Value, DbError> {
        crate::sessions::sessions(self.store().as_ref(), &self.workspace_root)
    }

    /// One session drilled to its turns, plus the session-scoped surfaces the
    /// per-turn view can't show (skills, MCP traffic, task board, PRs,
    /// lessons, memory writes).
    pub fn session(&self, id: &str) -> Result<Value, DbError> {
        crate::sessions::session_detail(self.store().as_ref(), &self.workspace_root, id)
    }

    /// One execution's behavioural tendencies (retries, loop detections,
    /// compactions, policy verdicts), folded from its journal slice — the
    /// fourth sanctioned `events` read; see `sessions::execution_tendencies`.
    pub fn execution_tendencies(&self, id: i64) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(Value::Null);
        };
        crate::sessions::execution_tendencies(&conn, id)
    }

    /// Per-(provider, model) usage — the same rows `stella stats` prints,
    /// same semantics (`resolved` = outcome `completed`, `off-grid` = local).
    pub fn models(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!([]));
        };
        let rows = collect_rows(
            &conn,
            "SELECT e.provider, e.model, count(*) AS runs,
                    count(*) FILTER (WHERE e.outcome = 'completed') AS resolved,
                    coalesce(sum(e.cost_usd), 0) AS total_cost_usd,
                    CAST(coalesce(sum(t.input_tokens), 0) AS INTEGER),
                    CAST(coalesce(sum(t.output_tokens), 0) AS INTEGER),
                    CAST(coalesce(sum(t.cache_read_tokens), 0) AS INTEGER),
                    CAST(coalesce(sum(t.cache_write_tokens), 0) AS INTEGER),
                    CAST(coalesce(sum(t.duration_ms), 0) AS INTEGER)
             FROM executions e
             LEFT JOIN (SELECT execution_id,
                               sum(input_tokens)       AS input_tokens,
                               sum(output_tokens)      AS output_tokens,
                               sum(cache_read_tokens)  AS cache_read_tokens,
                               sum(cache_write_tokens) AS cache_write_tokens,
                               sum(duration_ms)        AS duration_ms
                        FROM telemetry GROUP BY execution_id) t
               ON t.execution_id = e.id
             GROUP BY e.provider, e.model
             ORDER BY total_cost_usd DESC, e.provider ASC, e.model ASC",
            |r| {
                let provider: String = r.get(0)?;
                let runs: i64 = r.get(2)?;
                let resolved: i64 = r.get(3)?;
                let cost: f64 = r.get(4)?;
                let total_ms: i64 = r.get(9)?;
                Ok(json!({
                    "division": if provider == "local" { "off-grid" } else { "-" },
                    "provider": provider,
                    "model": r.get::<_, String>(1)?,
                    "runs": runs,
                    "resolved": resolved,
                    "resolve_rate": resolved as f64 / runs.max(1) as f64,
                    "total_cost_usd": cost,
                    "cost_per_resolved_usd":
                        if resolved > 0 { json!(cost / resolved as f64) } else { Value::Null },
                    "input_tokens": r.get::<_, i64>(5)?,
                    "output_tokens": r.get::<_, i64>(6)?,
                    "cache_read_tokens": r.get::<_, i64>(7)?,
                    "cache_write_tokens": r.get::<_, i64>(8)?,
                    "avg_duration_ms": total_ms as f64 / runs.max(1) as f64,
                }))
            },
        )?;
        Ok(Value::Array(rows))
    }

    /// Tool leaderboard over the newest `TOOL_CALL_SCAN_WINDOW` calls:
    /// calls, failures, latency, bytes returned. The p50 is computed here
    /// (SQLite has no percentile function).
    ///
    /// Accumulates into `ToolAgg` rather than a bare tuple so the fold and
    /// the row-building loop name the same four quantities.
    pub fn tools(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!([]));
        };
        // Folded row by row rather than through `collect_rows`: this is the
        // highest-cardinality scan (one row per tool call, for an exact p50),
        // so materializing it as JSON first allocates per call. The scan is
        // windowed to the newest calls by rowid — every poll re-paying for
        // the whole history was unbounded, and calls old enough to age out of
        // the window no longer describe how the tools behave today anyway.
        let sql = format!(
            "SELECT name, ok, duration_ms, bytes_out FROM tool_calls
             ORDER BY rowid DESC LIMIT {TOOL_CALL_SCAN_WINDOW}"
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(stmt) => stmt,
            Err(e) if is_missing_schema(&e) => return Ok(json!([])),
            Err(e) => return Err(e.into()),
        };
        let scanned = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? != 0,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut by_name: std::collections::BTreeMap<String, ToolAgg> =
            std::collections::BTreeMap::new();
        for call in scanned {
            let (name, ok, duration_ms, bytes_out) = call?;
            let entry = by_name.entry(name).or_default();
            entry.calls += 1;
            if !ok {
                entry.errors += 1;
            }
            entry.durations.push(duration_ms);
            entry.bytes_out += bytes_out;
        }
        let mut rows: Vec<Value> = by_name
            .into_iter()
            .map(|(name, mut agg)| {
                agg.durations.sort_unstable();
                // Nearest-rank over the already-sorted window — the vector is
                // materialized for the p50 anyway, so the tail percentiles
                // cost two more indexed reads, not another scan.
                let rank = |num: usize, den: usize| {
                    agg.durations
                        .get((agg.durations.len() * num / den).min(agg.durations.len() - 1))
                        .copied()
                        .unwrap_or(0)
                };
                let p50 = agg
                    .durations
                    .get(agg.durations.len() / 2)
                    .copied()
                    .unwrap_or(0);
                let (p90, p99) = if agg.durations.is_empty() {
                    (0, 0)
                } else {
                    (rank(9, 10), rank(99, 100))
                };
                let max = agg.durations.last().copied().unwrap_or(0);
                json!({
                    "name": name,
                    "calls": agg.calls,
                    "errors": agg.errors,
                    "p50_ms": p50,
                    "p90_ms": p90,
                    "p99_ms": p99,
                    "max_ms": max,
                    "bytes_out": agg.bytes_out,
                })
            })
            .collect();
        rows.sort_by_key(|r| -r["calls"].as_i64().unwrap_or(0));
        Ok(Value::Array(rows))
    }

    /// Files-touched leaderboard across all executions.
    pub fn files(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!([]));
        };
        let rows = collect_rows(
            &conn,
            "SELECT path, count(*) AS execs,
                    coalesce(sum(lines_added), 0),
                    coalesce(sum(lines_removed), 0)
             FROM files_touched
             GROUP BY path
             ORDER BY execs DESC, path ASC
             LIMIT 100",
            |r| {
                Ok(json!({
                    "path": r.get::<_, String>(0)?,
                    "executions": r.get::<_, i64>(1)?,
                    "lines_added": r.get::<_, i64>(2)?,
                    "lines_removed": r.get::<_, i64>(3)?,
                }))
            },
        )?;
        Ok(Value::Array(rows))
    }

    /// Promoted rules alone — the `rules` slice of [`Self::memory`], split
    /// out so `/api/rules` runs one query instead of running four and
    /// discarding three. Same payload, same missing-table degradation.
    pub(crate) fn rules(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!([]));
        };
        Ok(Value::Array(rules_rows(&conn)?))
    }

    /// The self-improvement plane: memory citations, mined reflections,
    /// promoted rules, and skill usage.
    pub fn memory(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!({
                "citations": [], "reflections": [], "rules": [], "skills": []
            }));
        };
        let citations = collect_rows(
            &conn,
            "SELECT memory_id, count(*) AS cites,
                    avg(useful_score),
                    avg(truthful),
                    max(ts)
             FROM memory_citations
             GROUP BY memory_id ORDER BY cites DESC LIMIT 50",
            |r| {
                Ok(json!({
                    "memory_id": r.get::<_, String>(0)?,
                    "citations": r.get::<_, i64>(1)?,
                    "avg_useful": r.get::<_, f64>(2)?,
                    "truthful_rate": r.get::<_, f64>(3)?,
                    "last_cited": r.get::<_, String>(4)?,
                }))
            },
        )?;
        let reflections = collect_rows(
            &conn,
            "SELECT kind, content, domains, occurred_at
             FROM reflections ORDER BY occurred_at DESC LIMIT 100",
            |r| {
                Ok(json!({
                    "kind": r.get::<_, String>(0)?,
                    "content": r.get::<_, String>(1)?,
                    "domains": r.get::<_, String>(2)?,
                    "occurred_at": r.get::<_, i64>(3)?,
                }))
            },
        )?;
        let rules = rules_rows(&conn)?;
        let skills = collect_rows(
            &conn,
            "SELECT skill, count(*) AS uses, max(version), max(ts)
             FROM skill_usage GROUP BY skill ORDER BY uses DESC LIMIT 50",
            |r| {
                Ok(json!({
                    "skill": r.get::<_, String>(0)?,
                    "uses": r.get::<_, i64>(1)?,
                    "version": r.get::<_, i64>(2)?,
                    "last_used": r.get::<_, String>(3)?,
                }))
            },
        )?;
        Ok(json!({
            "citations": citations,
            "reflections": reflections,
            "rules": rules,
            "skills": skills,
        }))
    }

    /// Daily activity rollup: runs, resolutions, spend, tokens, tool calls
    /// per UTC day — the timeframe charts' raw material. Days are merged
    /// across the three tables in Rust (a day may have tool calls but no
    /// finished executions, or vice versa).
    ///
    /// Rows whose timestamp SQLite cannot parse land in the `UNDATED`
    /// bucket: `date()` returns NULL for those, and reading NULL as `String`
    /// used to fail the whole query and blank the dashboard. They are real
    /// runs, so they keep a named bucket instead of being dropped — the UI
    /// keeps them off the date axis and reports the total separately.
    pub fn activity(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!([]));
        };
        use std::collections::BTreeMap;
        fn day_slot<'a>(days: &'a mut BTreeMap<String, Value>, day: &str) -> &'a mut Value {
            days.entry(day.to_string()).or_insert_with(|| {
                json!({
                    "day": day, "runs": 0, "resolved": 0, "cost_usd": 0.0,
                    "input_tokens": 0, "output_tokens": 0, "model_ms": 0,
                    "tool_calls": 0, "tool_errors": 0,
                })
            })
        }
        // The `coalesce(date(…), '')` in each query below turns an unparseable
        // timestamp into the empty string; name it here so it never reaches
        // the UI blank. `UNDATED` sorts after every ISO day, so it lands last.
        fn day_key(row: &Value) -> String {
            match row["day"].as_str().unwrap_or_default() {
                "" => UNDATED.to_string(),
                day => day.to_string(),
            }
        }
        let mut days: BTreeMap<String, Value> = BTreeMap::new();
        for row in collect_rows(
            &conn,
            "SELECT coalesce(date(started_at), ''), count(*),
                    count(*) FILTER (WHERE outcome = 'completed'),
                    coalesce(sum(cost_usd), 0)
             FROM executions GROUP BY 1",
            |r| {
                Ok(json!({
                    "day": r.get::<_, String>(0)?,
                    "runs": r.get::<_, i64>(1)?,
                    "resolved": r.get::<_, i64>(2)?,
                    "cost_usd": r.get::<_, f64>(3)?,
                }))
            },
        )? {
            let day = day_key(&row);
            let entry = day_slot(&mut days, &day);
            entry["runs"] = row["runs"].clone();
            entry["resolved"] = row["resolved"].clone();
            entry["cost_usd"] = row["cost_usd"].clone();
        }
        for row in collect_rows(
            &conn,
            "SELECT coalesce(date(e.started_at), ''),
                    coalesce(sum(t.input_tokens), 0),
                    coalesce(sum(t.output_tokens), 0),
                    coalesce(sum(t.duration_ms), 0)
             FROM telemetry t JOIN executions e ON e.id = t.execution_id
             GROUP BY 1",
            |r| {
                Ok(json!({
                    "day": r.get::<_, String>(0)?,
                    "input_tokens": r.get::<_, i64>(1)?,
                    "output_tokens": r.get::<_, i64>(2)?,
                    "model_ms": r.get::<_, i64>(3)?,
                }))
            },
        )? {
            let day = day_key(&row);
            let entry = day_slot(&mut days, &day);
            entry["input_tokens"] = row["input_tokens"].clone();
            entry["output_tokens"] = row["output_tokens"].clone();
            entry["model_ms"] = row["model_ms"].clone();
        }
        for row in collect_rows(
            &conn,
            "SELECT coalesce(date(ts), ''), count(*), count(*) FILTER (WHERE ok = 0)
             FROM tool_calls GROUP BY 1",
            |r| {
                Ok(json!({
                    "day": r.get::<_, String>(0)?,
                    "tool_calls": r.get::<_, i64>(1)?,
                    "tool_errors": r.get::<_, i64>(2)?,
                }))
            },
        )? {
            let day = day_key(&row);
            let entry = day_slot(&mut days, &day);
            entry["tool_calls"] = row["tool_calls"].clone();
            entry["tool_errors"] = row["tool_errors"].clone();
        }
        Ok(Value::Array(days.into_values().collect()))
    }

    /// The newest `MAX_LISTED_REFLECTIONS` post-turn self-reflections joined
    /// to their executions: the self-improvement tab's ratings feed.
    pub fn reflection_ratings(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!([]));
        };
        // Newest first for the LIMIT, reversed below: the ratings chart plots
        // ascending execution order left-to-right.
        let mut rows = collect_rows(
            &conn,
            &format!(
                "SELECT er.execution_id, e.kind, e.outcome, e.model,
                        coalesce(nullif(er.prompt, ''), e.prompt),
                        er.delivered, er.self_rating,
                        er.what_went_well, er.what_to_improve, er.critique,
                        er.recorded_at
                 FROM execution_reflection er
                 LEFT JOIN executions e ON e.id = er.execution_id
                 ORDER BY er.execution_id DESC LIMIT {MAX_LISTED_REFLECTIONS}"
            ),
            |r| {
                Ok(json!({
                    "execution_id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, Option<String>>(1)?,
                    "outcome": r.get::<_, Option<String>>(2)?,
                    "model": r.get::<_, Option<String>>(3)?,
                    "prompt": truncate(r.get::<_, Option<String>>(4)?.unwrap_or_default(), 140),
                    "delivered": r.get::<_, Option<i64>>(5)?,
                    "self_rating": r.get::<_, Option<i64>>(6)?,
                    "what_went_well": truncate(r.get::<_, String>(7)?, 280),
                    "what_to_improve": truncate(r.get::<_, String>(8)?, 280),
                    "critique": truncate(r.get::<_, String>(9)?, 280),
                    "recorded_at": r.get::<_, String>(10)?,
                }))
            },
        )?;
        rows.reverse();
        Ok(Value::Array(rows))
    }

    /// MCP tool traffic per (server, tool).
    pub fn mcp(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!([]));
        };
        let rows = collect_rows(
            &conn,
            "SELECT server, tool, count(*) AS calls, max(called_at_ms)
             FROM mcp_usage GROUP BY server, tool ORDER BY calls DESC LIMIT 100",
            |r| {
                Ok(json!({
                    "server": r.get::<_, String>(0)?,
                    "tool": r.get::<_, String>(1)?,
                    "calls": r.get::<_, i64>(2)?,
                    "last_called_ms": r.get::<_, i64>(3)?,
                }))
            },
        )?;
        Ok(Value::Array(rows))
    }

    /// A cheap fingerprint of everything the dashboard draws — the change
    /// detector behind the live stream.
    ///
    /// This is the query the server runs several times a second, so it must
    /// stay cheap: five probes with no join, four answered from index or
    /// table metadata. `count(*) FROM tool_calls` is the one that scans and
    /// grows with history — carried because a tool_result closes a row in
    /// place and moves no maximum. Comparing two fingerprints answers "is there anything
    /// new?" without paying for a single one of the payload queries — which
    /// is the whole reason the browser can stop re-aggregating all of history
    /// on a timer.
    ///
    /// `events` moves on every persisted event, so it alone is a near-total
    /// change signal. The other four are carried because they can move
    /// without it: an execution row opens before its first event, and the
    /// `finished_at` stamp and per-day rollups land after the last one.
    pub fn cursor(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            // Same key set as the store-backed branch below: a cursor that
            // changes shape when store.db first appears reads as a spurious
            // all-keys change, and a client indexing `tool_calls_running`
            // on a fresh workspace would find the key missing.
            return Ok(json!({
                "events": 0,
                "executions": 0,
                "tool_calls": 0,
                "tool_calls_running": 0,
                "telemetry": 0,
            }));
        };
        // `max(rowid)` is monotonic and never reused while rows are only
        // appended; `count(*)` catches the in-place UPDATEs a tool result
        // makes, which do not move any maximum.
        let one = |sql: &str| -> Result<i64, DbError> {
            match conn.query_row(sql, [], |r| r.get::<_, i64>(0)) {
                Ok(v) => Ok(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
                Err(e) if is_missing_schema(&e) => Ok(0),
                Err(e) => Err(e.into()),
            }
        };
        Ok(json!({
            "events": one("SELECT coalesce(max(rowid), 0) FROM events")?,
            "executions": one("SELECT coalesce(max(id), 0) FROM executions")?,
            // Counted, not maxed: a `tool_result` closes an existing row in
            // place. A cursor built only from maxima would miss every
            // completion and the dashboard would show calls that never
            // finish.
            "tool_calls": one("SELECT count(*) FROM tool_calls")?,
            "tool_calls_running": one("SELECT count(*) FROM tool_calls WHERE state = 'running'")?,
            "telemetry": one("SELECT coalesce(max(rowid), 0) FROM telemetry")?,
        }))
    }

    /// The in-flight slice: executions that have not closed out, and the tool
    /// calls currently running under them.
    ///
    /// Small by construction — this is "what is happening right now", not
    /// history — so the live stream can carry it inline on every change
    /// instead of making the client come back for it. Every rollup is scoped
    /// to the open executions (the subqueries filter on
    /// `finished_at IS NULL` before aggregating), so the cost tracks
    /// concurrent work rather than accumulated work. This ran unscoped once:
    /// the inner `GROUP BY`s aggregated the whole `tool_calls` and
    /// `telemetry` tables 4×/second per open stream, and the outer
    /// `WHERE e.finished_at IS NULL` discarded almost all of it.
    ///
    /// `elapsed_ms` is computed here rather than in the browser because the
    /// timestamps are the database's clock, and a client whose clock is
    /// skewed would otherwise render negative durations.
    pub fn live(&self) -> Result<Value, DbError> {
        let Some(conn) = self.store() else {
            return Ok(json!({ "executions": [], "running_calls": [] }));
        };
        let executions = collect_rows(
            &conn,
            "SELECT e.id, e.kind, e.prompt, e.provider, e.model, e.started_at,
                    CAST((julianday('now') - julianday(e.started_at)) * 86400000 AS INTEGER),
                    coalesce(tc.calls, 0), coalesce(tc.running, 0), coalesce(tc.errors, 0),
                    coalesce(t.steps, 0), coalesce(t.input_tokens, 0),
                    coalesce(t.output_tokens, 0), coalesce(t.cost_usd, 0)
             FROM executions e
             LEFT JOIN (SELECT execution_id, count(*) AS calls,
                               count(*) FILTER (WHERE state = 'running') AS running,
                               count(*) FILTER (WHERE state = 'error')   AS errors
                        FROM tool_calls
                        WHERE execution_id IN
                              (SELECT id FROM executions WHERE finished_at IS NULL)
                        GROUP BY execution_id) tc
               ON tc.execution_id = e.id
             LEFT JOIN (SELECT execution_id, count(*) AS steps,
                               sum(input_tokens)  AS input_tokens,
                               sum(output_tokens) AS output_tokens,
                               sum(cost_usd)      AS cost_usd
                        FROM telemetry
                        WHERE execution_id IN
                              (SELECT id FROM executions WHERE finished_at IS NULL)
                        GROUP BY execution_id) t
               ON t.execution_id = e.id
             WHERE e.finished_at IS NULL
             ORDER BY e.id DESC",
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "prompt": truncate(r.get::<_, String>(2)?, 160),
                    "provider": r.get::<_, String>(3)?,
                    "model": r.get::<_, String>(4)?,
                    "started_at": r.get::<_, String>(5)?,
                    "elapsed_ms": r.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0),
                    "tool_calls": r.get::<_, i64>(7)?,
                    "tool_calls_running": r.get::<_, i64>(8)?,
                    "tool_errors": r.get::<_, i64>(9)?,
                    "steps": r.get::<_, i64>(10)?,
                    "input_tokens": r.get::<_, i64>(11)?,
                    "output_tokens": r.get::<_, i64>(12)?,
                    "cost_usd": r.get::<_, f64>(13)?,
                }))
            },
        )?;
        let running_calls = collect_rows(
            &conn,
            &format!(
                "SELECT execution_id, seq, name, surface, ts,
                        CAST((julianday('now') - julianday(ts)) * 86400000 AS INTEGER)
                 FROM tool_calls WHERE state = 'running'
                 ORDER BY execution_id DESC, seq DESC LIMIT {MAX_RUNNING_CALLS}"
            ),
            |r| {
                Ok(json!({
                    "execution_id": r.get::<_, i64>(0)?,
                    "seq": r.get::<_, i64>(1)?,
                    "name": r.get::<_, String>(2)?,
                    "surface": r.get::<_, String>(3)?,
                    "started_at": r.get::<_, String>(4)?,
                    "elapsed_ms": r.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0),
                }))
            },
        )?;
        Ok(json!({ "executions": executions, "running_calls": running_calls }))
    }

    /// Fleet ledger: every fan-out run with its tasks, attempts, commits.
    pub fn fleet(&self) -> Result<Value, DbError> {
        let Some(conn) = self.fleet_conn() else {
            return Ok(json!([]));
        };
        let rows = collect_rows(
            &conn,
            "SELECT r.id, r.root_task_count, r.created_at_ms,
                    (SELECT count(*) FROM attempts a WHERE a.run_id = r.id),
                    (SELECT count(*) FROM attempts a
                      WHERE a.run_id = r.id AND a.success = 1),
                    (SELECT count(*) FROM commits c WHERE c.run_id = r.id)
             FROM runs r ORDER BY r.created_at_ms DESC LIMIT 50",
            |r| {
                Ok(json!({
                    "run_id": r.get::<_, String>(0)?,
                    "tasks": r.get::<_, i64>(1)?,
                    "created_at_ms": r.get::<_, i64>(2)?,
                    "attempts": r.get::<_, i64>(3)?,
                    "succeeded": r.get::<_, i64>(4)?,
                    "commits": r.get::<_, i64>(5)?,
                }))
            },
        )?;
        Ok(Value::Array(rows))
    }
}

/// One execution's context-recall timings, read out of its event stream
/// (#875).
///
/// Recall sits on the **first-token path of every turn**, so a slow one
/// delays everything after it — and it was invisible in the dashboard, the
/// receipt and the log alike. A cold store, a large corpus, or a wedged
/// embedding call looked exactly like a fast recall right until it became a
/// timeout, at which point the operator still could not tell recall was the
/// bottleneck.
///
/// Read from `events` rather than from a projection of its own, and only on
/// the per-execution detail route. The filter is `execution_id`-first, which
/// the `UNIQUE (execution_id, seq)` index serves directly, so this touches
/// one turn's events rather than scanning the table. The same query without
/// that filter — a global recall-latency trend — would scan every event in
/// history, which is not something the polled routes can afford; earning
/// that view means a projection, not a wider `WHERE`.
///
/// `latency_ms` absent (a stream recorded before the field existed) reads as
/// `null`, not `0`: "not measured" and "instant" are different facts and the
/// dashboard must not draw one as the other.
fn recall_timings(conn: &Connection, execution_id: i64) -> Result<Vec<Value>, DbError> {
    collect_rows_for(
        conn,
        execution_id,
        "SELECT ts,
                json_extract(payload, '$.latency_ms'),
                json_extract(payload, '$.used_ann_index'),
                json_extract(payload, '$.tokens'),
                json_array_length(json_extract(payload, '$.frames'))
         FROM events
         WHERE execution_id = ?1 AND event_type = 'context_recall'
         ORDER BY seq ASC",
        |r| {
            Ok(json!({
                "ts": r.get::<_, String>(0)?,
                "latency_ms": r.get::<_, Option<i64>>(1)?,
                "used_ann_index": r.get::<_, Option<i64>>(2)?.map(|v| v != 0),
                "tokens": r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                "frames": r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            }))
        },
    )
}

/// Shape one raw `events` row into the transcript entry the drawer renders.
///
/// The payload is the internally-tagged `AgentEvent` JSON the store
/// persisted (`{"type":"text","delta":…}`). Only the fields the transcript
/// needs are lifted out, keyed by the row's own `event_type` column. A
/// payload that no longer parses (a hand-edited store, a variant this binary
/// predates) keeps its seq/type header with no body rather than erroring —
/// one unreadable row must not blank the transcript around it.
fn journal_entry(row: Value, full: bool) -> Value {
    let ty = row["type"].as_str().unwrap_or("").to_owned();
    let payload: Value = row["payload"]
        .as_str()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or(Value::Null);
    let mut out = json!({
        "seq": row["seq"],
        "ts": row["ts"],
        "type": ty,
    });
    if payload.is_null() {
        return out;
    }
    match ty.as_str() {
        "stage" => {
            out["label"] = payload["name"].clone();
        }
        "text" | "reasoning" => {
            set_journal_body(&mut out, payload["delta"].as_str().unwrap_or(""), full);
        }
        "tool_start" => {
            out["call_id"] = payload["call"]["call_id"].clone();
            out["name"] = payload["call"]["name"].clone();
            let args = serde_json::to_string_pretty(&payload["call"]["input"]).unwrap_or_default();
            set_journal_body(&mut out, &args, full);
        }
        "tool_result" => {
            out["call_id"] = payload["call_id"].clone();
            out["duration_ms"] = payload["duration_ms"].clone();
            out["speculated"] = json!(payload["speculated"].as_bool().unwrap_or(false));
            // `ToolOutput` is externally tagged: `{"ok":{"content":…}}` or
            // `{"error":{"message":…}}` — the tag is the ok/failed verdict.
            out["ok"] = json!(!payload["output"]["ok"].is_null());
            let body = payload["output"]["ok"]["content"]
                .as_str()
                .or_else(|| payload["output"]["error"]["message"].as_str())
                .unwrap_or("");
            set_journal_body(&mut out, body, full);
        }
        "speculation_discarded" => {
            out["call_id"] = payload["call_id"].clone();
            out["name"] = payload["name"].clone();
            out["reason"] = payload["reason"].clone();
        }
        _ => {}
    }
    out
}

/// Attach `body` to a transcript entry, clipped to [`JOURNAL_BODY_CLIP`]
/// unless `full`, and record which of the two happened.
///
/// Shared with the sent-context messages (`sent_context`), so the transcript
/// and the reconstruction can never disagree about what "clipped" means or
/// where the cut falls.
pub(crate) fn set_journal_body(entry: &mut Value, body: &str, full: bool) {
    let clipped = !full && body.chars().count() > JOURNAL_BODY_CLIP;
    entry["body"] = if clipped {
        json!(truncate(body.to_owned(), JOURNAL_BODY_CLIP))
    } else {
        json!(body)
    };
    entry["truncated"] = json!(clipped);
}

/// The promoted-rules query, shared by [`Observatory::rules`] and
/// [`Observatory::memory`] so the two can never drift apart.
fn rules_rows(conn: &Connection) -> Result<Vec<Value>, DbError> {
    collect_rows(
        conn,
        "SELECT rule_id, contents, source, created_at
         FROM rules ORDER BY created_at DESC LIMIT 50",
        |r| {
            Ok(json!({
                "rule_id": r.get::<_, String>(0)?,
                "contents": truncate(r.get::<_, String>(1)?, 240),
                "source": r.get::<_, String>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        },
    )
}

/// Per-tool accumulator for [`Observatory::tools`]. Named fields rather than
/// a `(i64, i64, Vec<i64>, i64)` tuple so the fold and the row builder can't
/// disagree about which slot is which.
#[derive(Default)]
struct ToolAgg {
    calls: i64,
    errors: i64,
    /// Every call's duration, sorted in place when the p50/max are taken.
    durations: Vec<i64>,
    bytes_out: i64,
}

/// Open a SQLite file strictly read-only; `None` when it doesn't exist.
fn open_read_only(path: &Path) -> Option<Connection> {
    if !path.exists() {
        return None;
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    // WAL means a writing session never blocks this reader, but a checkpoint
    // or a migration's exclusive lock still can. Wait it out (the same 5 s the
    // writing stores use) instead of 500-ing a dashboard poll on SQLITE_BUSY.
    let _ = conn.busy_timeout(std::time::Duration::from_millis(5_000));
    Some(conn)
}

/// Run a query collecting every row; a missing table degrades to `[]` so a
/// dashboard section renders empty instead of failing the whole page.
fn collect_rows<F>(conn: &Connection, sql: &str, map: F) -> Result<Vec<Value>, DbError>
where
    F: Fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
{
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let rows = stmt.query_map([], map)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// [`collect_rows`] scoped to one execution id, bound as a parameter.
fn collect_rows_for<F>(conn: &Connection, id: i64, sql: &str, map: F) -> Result<Vec<Value>, DbError>
where
    F: Fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
{
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let rows = stmt.query_map([id], map)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Same degradation for single-row lookups: missing table or no row → `{}`.
fn or_empty(result: rusqlite::Result<Value>) -> Result<Value, DbError> {
    match result {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(json!({})),
        Err(e) if is_missing_schema(&e) => Ok(json!({})),
        Err(e) => Err(e.into()),
    }
}

/// Whether an error is "this file is older than this query", rather than a
/// real failure.
///
/// The observatory opens every database `SQLITE_OPEN_READ_ONLY` and therefore
/// **never runs migrations** — that is the point, an observer must not mutate
/// what it observes. The direct consequence is that it can be pointed at a
/// store several schema versions behind the binary reading it: upgrade
/// `stella`, open the dashboard before running a single turn, and nothing has
/// yet had a reason to open that file read-write.
///
/// So a missing table has always degraded to an empty payload here. A missing
/// **column** is the same state and was not covered, which is a sharper edge:
/// a new query against an older file 500s the whole route rather than
/// blanking one panel. That is exactly how the v18 `tool_calls.state` column
/// took down `/api/v1/cursor` on any workspace that had not been migrated
/// yet — found by pointing this crate at an untouched copy of a real store.
///
/// Both are answered the same way: report what the file can support and stay
/// up. A dashboard that renders an older store with one section empty is
/// strictly better than one that refuses to render at all, and the section
/// fills in the moment a turn migrates the file.
/// Both `rusqlite` variants are matched, and that is not defensive padding —
/// they carry these two failures **differently**, verified against the
/// version in `Cargo.lock`:
///
/// ```text
/// SELECT count(*) FROM no_such_table -> SqliteFailure  { msg: "no such table: …" }
/// SELECT bogus FROM sqlite_master    -> SqlInputError  { msg: "no such column: …" }
/// ```
///
/// Matching only `SqliteFailure` — which is what the missing-table check did,
/// correctly, for the case it was written for — silently fails to catch a
/// missing column, so the degradation never fires and the route 500s anyway.
/// That is precisely how this bug got here.
pub(crate) fn is_missing_schema(e: &rusqlite::Error) -> bool {
    let message = match e {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.as_str(),
        rusqlite::Error::SqlInputError { msg, .. } => msg.as_str(),
        _ => return false,
    };
    message.contains("no such table") || message.contains("no such column")
}

/// Shallow-merge `extra`'s fields into `base` (both must be objects).
pub(crate) fn merge(base: &mut Value, extra: Value) {
    if let (Some(base_map), Value::Object(extra_map)) = (base.as_object_mut(), extra) {
        for (k, v) in extra_map {
            base_map.insert(k, v);
        }
    }
}

/// Cap a string at `max` chars on a char boundary, appending an ellipsis.
fn truncate(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}
