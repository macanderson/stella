//! The `/export` telemetry dump and the usage aggregate behind `stella
//! stats` — split out of `lib.rs` to keep that file small, and because *which
//! executions a read covers* is a contract in its own right rather than an
//! incidental detail of either query.
//!
//! The archive `/export` writes is meant to leave the machine: its own
//! documentation invites you to email it or attach it to a pull request as
//! evidence, and #817 hardened it on exactly that premise — masking every
//! credential that reached the telemetry, and tightening the archive to
//! `0600`. Both of those defences assume the blast radius is **one session**.
//!
//! Until #2558 it was the whole workspace store. [`Store::export_all_json`]
//! and [`Store::usage_stats`] carried no `WHERE` clause at all, so someone
//! attaching an export to a public PR to show one run was disclosing every
//! other run in that project: their prompts, their tool arguments, their
//! touched files' contents. The data model already supported the fix —
//! `executions.session_id` (schema v8) is stamped by
//! [`Store::set_execution_session`] and indexed by `executions_by_session` —
//! so what was missing was only a predicate and a way to ask for it.
//!
//! `ExportScope` is that way, and it is why the scoped and unscoped reads
//! share one SQL body rather than owning one each: two copies of this SQL
//! would drift, and the drifted copy is the one that leaks.
//!
//! It stays **private**, and the choice is surfaced as named methods —
//! [`Store::export_all_json`] against [`Store::export_session_json`],
//! [`Store::usage_stats`] against [`Store::usage_stats_for_session`]. A caller
//! that never holds a scope value cannot pair one table's scope with
//! another's, and a scope nothing accepts would be public API in name only.

use base64::Engine as _;
use rusqlite::ToSql;
use serde::Serialize;

use crate::{Result, Store, UsageStatsRow};

/// Which executions an analytics read covers.
///
/// The two variants need *different* predicates, not one parameterized
/// predicate: `executions` carries `session_id` itself, while the eight child
/// tables reach it through `execution_id`. Both are served from here so a
/// caller cannot pair one table's scope with another's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportScope<'a> {
    /// Every execution recorded in this workspace's store.
    ///
    /// This is the right scope for `stella stats`, which is explicitly a
    /// whole-workspace question. It is the **wrong** scope for anything that
    /// produces an artifact meant to be shared — see the module docs.
    Workspace,
    /// Only executions stamped with this session registry id
    /// (`SessionRecord::id`).
    Session(&'a str),
}

impl ExportScope<'_> {
    /// The `WHERE` clause restricting the `executions` table to this scope, or
    /// an empty string for the whole workspace. `qualifier` is the table alias
    /// plus its dot (`"e."`), or empty when the table is unaliased.
    ///
    /// The session id is **bound**, never interpolated — the only text this
    /// builds is the `?1` placeholder and a static column reference.
    fn executions_where(&self, qualifier: &str) -> String {
        match self {
            Self::Workspace => String::new(),
            Self::Session(_) => format!("WHERE {qualifier}session_id = ?1"),
        }
    }

    /// The `WHERE` clause restricting a child table keyed by `execution_id`.
    ///
    /// A row whose `execution_id` is NULL (only `reflections` permits one)
    /// fails `IN` and is excluded, which is correct — it belongs to no session
    /// — and is why [`Store::export_exclusions`] counts those rows rather than
    /// letting them vanish.
    fn child_where(&self) -> &'static str {
        match self {
            Self::Workspace => "",
            Self::Session(_) => {
                "WHERE execution_id IN (SELECT id FROM executions WHERE session_id = ?1)"
            }
        }
    }

    /// The bound parameters both clauses above expect.
    fn bindings(&self) -> Vec<&dyn ToSql> {
        match self {
            Self::Workspace => Vec::new(),
            Self::Session(id) => vec![id],
        }
    }
}

/// What a session-scoped export deliberately left out of the archive, so the
/// manifest can say so.
///
/// Silently dropping rows is its own quiet wrong answer: a reader who cannot
/// tell "this session did nothing else" from "the exporter hid the rest" has
/// no more evidence than before. Counting the exclusions is what makes the
/// archive's scope a claim rather than an assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct ExportExclusions {
    /// Executions belonging to some *other* session — the disclosure this
    /// scoping exists to prevent.
    pub other_session_executions: i64,
    /// Executions carrying no session stamp at all: recorded before schema v8
    /// added the column, or by a path that had no session to name.
    pub unattributed_executions: i64,
    /// Reflections with no `execution_id` — the one exported table whose rows
    /// can hang off nothing, so they belong to no session and cannot be
    /// attributed to one later. Reflections that *do* name an execution are
    /// accounted for by the two execution counts above.
    pub unattributed_reflections: i64,
}

impl ExportExclusions {
    /// Whether anything at all was left out — the dashboard says "the only
    /// session recorded in this workspace" rather than listing three zeroes.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The eight telemetry tables that key off `execution_id`, as
/// `(name, select_and_from, order_by)`.
///
/// Split at the seam the scope predicate goes into: a `WHERE` clause has to
/// land between the `FROM` and the `ORDER BY`, and splitting the strings here
/// is what lets one loop serve both scopes. `executions` is not in this list —
/// it is the spine, filters on `session_id` directly, and builds its JSON by
/// hand rather than through [`Store::query_to_json`].
const CHILD_TABLES: [(&str, &str, &str); 8] = [
    (
        "telemetry",
        "SELECT execution_id, step, ts, provider, call_role, model, input_tokens, \
         estimated_input_tokens, output_tokens, cache_read_tokens, cache_miss_tokens, \
         cache_write_tokens, cost_usd, duration_ms, retries, tool_calls, usage_complete, \
         sub_agent_id FROM telemetry",
        "ORDER BY execution_id ASC, step ASC",
    ),
    (
        "tool_calls",
        "SELECT execution_id, seq, call_id, name, surface, args_json, args_digest, reason, ok, \
         error, bytes_out, duration_ms FROM tool_calls",
        "ORDER BY execution_id ASC, seq ASC",
    ),
    (
        "files_touched",
        "SELECT execution_id, path, ops, lines_added, lines_removed, events FROM files_touched",
        "ORDER BY execution_id ASC, path ASC",
    ),
    (
        "mcp_usage",
        "SELECT execution_id, server, tool, reason, called_at_ms FROM mcp_usage",
        "ORDER BY called_at_ms ASC",
    ),
    (
        "agent_uses",
        "SELECT execution_id, agent, version, reason, ts, kind FROM agent_uses",
        "ORDER BY execution_id ASC, ts ASC",
    ),
    (
        "skill_usage",
        "SELECT execution_id, skill, version, reason, ts FROM skill_usage",
        "ORDER BY execution_id ASC, ts ASC",
    ),
    (
        "execution_reflection",
        "SELECT execution_id, prompt, delivered, self_rating, what_went_well, what_to_improve, \
         critique, produced_output, wrote_files, truncated, partial_run \
         FROM execution_reflection",
        "ORDER BY execution_id ASC",
    ),
    (
        "reflections",
        "SELECT id, execution_id, kind, content, domains, occurred_at FROM reflections",
        "ORDER BY id ASC",
    ),
];

impl Store {
    /// Aggregate usage/cost analytics per (provider, model) across the **whole
    /// workspace** — the data behind `stella stats` and every
    /// "$-per-resolved-task" receipt.
    ///
    /// Semantics:
    /// - One output row per distinct `executions.(provider, model)` pair;
    ///   telemetry is attributed to its execution's provider/model.
    /// - `runs` counts every execution; `resolved` counts
    ///   `outcome = 'completed'` (aborted and still-open runs are not
    ///   resolved).
    /// - Cost comes from `executions.cost_usd` — the per-run total, kept at
    ///   or above what that run's receipts prove as each call settles and
    ///   floored again at finish ([`Store::finish_execution_accounted`]), so
    ///   a run that was cancelled mid-flight is not summed here at a
    ///   truncated prefix of the tokens it is summed at below (#2570). Token
    ///   and duration sums come from `telemetry`, pre-aggregated per
    ///   execution before the join so a multi-step run can never fan out the
    ///   executions side.
    /// - `cost_per_resolved_usd` is `None` when `resolved = 0`.
    /// - Rows are ordered by total cost descending (ties broken by
    ///   provider, then model, so output is deterministic).
    ///
    /// Division mapping: only provider `local` maps to the off-grid cost
    /// tier (`off-grid`); all other rows carry `"-"` — see [`UsageStatsRow`].
    ///
    /// For anything that renders into a **shareable artifact**, use
    /// [`Store::usage_stats_for_session`] instead: totals computed over the
    /// whole workspace describe runs the artifact does not contain.
    pub fn usage_stats(&self) -> Result<Vec<UsageStatsRow>> {
        self.usage_stats_scoped(ExportScope::Workspace)
    }

    /// [`Store::usage_stats`] restricted to one session's executions — the
    /// totals an export dashboard may honestly display beside that session's
    /// rows.
    pub fn usage_stats_for_session(&self, session_id: &str) -> Result<Vec<UsageStatsRow>> {
        self.usage_stats_scoped(ExportScope::Session(session_id))
    }

    /// One SQL body, both scopes. See [`ExportScope`] for why this is not two
    /// functions.
    fn usage_stats_scoped(&self, scope: ExportScope<'_>) -> Result<Vec<UsageStatsRow>> {
        let conn = self.lock();
        // The inner aggregate stays unfiltered on purpose: it is joined by
        // `execution_id` to an already-scoped `executions`, so a telemetry row
        // from another session has no surviving row to attach to.
        let sql = format!(
            "SELECT e.provider,
                    e.model,
                    count(*) AS runs,
                    count(*) FILTER (WHERE e.outcome = 'completed') AS resolved,
                    coalesce(sum(e.cost_usd), 0) AS total_cost_usd,
                    CAST(coalesce(sum(t.input_tokens), 0) AS INTEGER) AS input_tokens,
                    CAST(coalesce(sum(t.output_tokens), 0) AS INTEGER) AS output_tokens,
                    CAST(coalesce(sum(t.cache_read_tokens), 0) AS INTEGER) AS cache_read_tokens,
                    CAST(coalesce(sum(t.cache_write_tokens), 0) AS INTEGER) AS cache_write_tokens,
                    CAST(coalesce(sum(t.duration_ms), 0) AS INTEGER) AS total_duration_ms
             FROM executions e
             LEFT JOIN (
               SELECT execution_id,
                      sum(input_tokens) AS input_tokens,
                      sum(output_tokens) AS output_tokens,
                      sum(cache_read_tokens) AS cache_read_tokens,
                      sum(cache_write_tokens) AS cache_write_tokens,
                      sum(duration_ms) AS duration_ms
               FROM telemetry
               GROUP BY execution_id
             ) t ON t.execution_id = e.id
             {}
             GROUP BY e.provider, e.model
             ORDER BY total_cost_usd DESC, e.provider ASC, e.model ASC",
            scope.executions_where("e.")
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(scope.bindings().as_slice(), |row| {
            let provider: String = row.get(0)?;
            let model: String = row.get(1)?;
            let runs: i64 = row.get(2)?;
            let resolved: i64 = row.get(3)?;
            let total_cost_usd: f64 = row.get(4)?;
            let input_tokens: i64 = row.get(5)?;
            let output_tokens: i64 = row.get(6)?;
            let cache_read_tokens: i64 = row.get(7)?;
            let cache_write_tokens: i64 = row.get(8)?;
            let total_duration_ms: i64 = row.get(9)?;
            let division = UsageStatsRow::division_for_provider(&provider).to_string();
            Ok(UsageStatsRow {
                provider,
                model,
                division,
                runs,
                resolved,
                // A GROUP BY group always holds ≥ 1 execution row.
                resolve_rate: resolved as f64 / runs as f64,
                total_cost_usd,
                cost_per_resolved_usd: (resolved > 0).then(|| total_cost_usd / resolved as f64),
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                avg_duration_ms: total_duration_ms as f64 / runs as f64,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Dump the telemetry tables for the **whole workspace** as JSON arrays.
    /// Each entry is `(table_name, json_array_string)`.
    ///
    /// This is the analytics export, not a database dump: it covers exactly
    /// the nine telemetry tables the export archive bundles — `executions`,
    /// `telemetry`, `tool_calls`, `files_touched`, `mcp_usage`, `agent_uses`,
    /// `skill_usage`, `execution_reflection`, and `reflections`. The rest of
    /// the store stays out deliberately: `events` and the receipts plane
    /// (`context_blocks` / `step_manifest` / `step_receipt`) carry full
    /// payloads and reconstruction preimages the archive must not ship, and
    /// the state tables (`rules`, `file_locks`, `memory_citations`,
    /// `forgotten`, `tasks`, `pull_requests`) are live workspace state, not
    /// session telemetry.
    ///
    /// The JSON is constructed in Rust (not by SQLite's json1 extension,
    /// which may be absent from a bundled build), so the shape is stable
    /// across platforms. Rows are ordered by the table's natural key; the
    /// exact order per table is documented in the module-level schema
    /// comments.
    ///
    /// **This dump is not shareable.** It is every session ever run in the
    /// workspace. [`Store::export_session_json`] is what `/export` writes;
    /// this remains for in-process introspection whose reader is the same
    /// process that produced the rows.
    pub fn export_all_json(&self) -> Result<Vec<(&'static str, String)>> {
        self.export_json_scoped(ExportScope::Workspace)
    }

    /// [`Store::export_all_json`] restricted to one session's executions — the
    /// dump `/export` archives (#2558).
    ///
    /// Rows belonging to another session, and rows belonging to no session at
    /// all, are excluded; [`Store::export_exclusions`] counts both so the
    /// archive's manifest can name what it left out.
    pub fn export_session_json(&self, session_id: &str) -> Result<Vec<(&'static str, String)>> {
        self.export_json_scoped(ExportScope::Session(session_id))
    }

    /// One SQL body, both scopes. See [`ExportScope`].
    fn export_json_scoped(&self, scope: ExportScope<'_>) -> Result<Vec<(&'static str, String)>> {
        let conn = self.lock();
        let bindings = scope.bindings();
        let mut out: Vec<(&'static str, String)> = Vec::with_capacity(CHILD_TABLES.len() + 1);

        // Executions — the spine everything else keys off.
        {
            let sql = format!(
                "SELECT id, kind, prompt, provider, model, started_at, finished_at, outcome, \
                 cost_usd, usage_complete FROM executions {} ORDER BY id ASC",
                scope.executions_where("")
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(bindings.as_slice(), |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, i64>(0)?,
                    "kind": row.get::<_, String>(1)?,
                    "prompt": row.get::<_, String>(2)?,
                    "provider": row.get::<_, String>(3)?,
                    "model": row.get::<_, String>(4)?,
                    "started_at": row.get::<_, Option<String>>(5)?,
                    "finished_at": row.get::<_, Option<String>>(6)?,
                    "outcome": row.get::<_, Option<String>>(7)?,
                    "cost_usd": row.get::<_, f64>(8)?,
                    "usage_complete": row.get::<_, bool>(9)?,
                }))
            })?;
            let mut arr = Vec::new();
            for r in rows {
                arr.push(r?);
            }
            // `[]`, not `""`, on the (unreachable) serialization failure: every
            // entry of this vec is consumed as a JSON document, and an empty
            // string is not one — it would turn an export hiccup into a parse
            // error downstream. Matches `query_to_json`'s fallback.
            out.push((
                "executions",
                serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into()),
            ));
        }

        for (name, select_from, order_by) in CHILD_TABLES {
            let sql = format!("{select_from} {} {order_by}", scope.child_where());
            out.push((name, self.query_to_json(&conn, &sql, bindings.as_slice())?));
        }

        Ok(out)
    }

    /// Count the rows a session-scoped export leaves behind, so its manifest
    /// can state them. See [`ExportExclusions`] for why this is counted at all
    /// rather than simply not exported.
    pub fn export_exclusions(&self, session_id: &str) -> Result<ExportExclusions> {
        let conn = self.lock();
        let other_session_executions = conn.query_row(
            "SELECT count(*) FROM executions WHERE session_id IS NOT NULL AND session_id <> ?1",
            [session_id],
            |row| row.get(0),
        )?;
        let unattributed_executions = conn.query_row(
            "SELECT count(*) FROM executions WHERE session_id IS NULL",
            [],
            |row| row.get(0),
        )?;
        let unattributed_reflections = conn.query_row(
            "SELECT count(*) FROM reflections WHERE execution_id IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(ExportExclusions {
            other_session_executions,
            unattributed_executions,
            unattributed_reflections,
        })
    }

    /// Execute a `SELECT *`-style query and return a JSON array string, one
    /// object per row. Column names come from the query cursor. Used by
    /// [`export_all_json`](Store::export_all_json) for the uniform tables.
    fn query_to_json(
        &self,
        conn: &rusqlite::Connection,
        sql: &str,
        bindings: &[&dyn ToSql],
    ) -> Result<String> {
        let mut stmt = conn.prepare(sql)?;
        let col_count = stmt.column_count();
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt.query_map(bindings, |row| {
            let mut obj = serde_json::Map::with_capacity(col_count);
            for (i, name) in col_names.iter().enumerate() {
                // sqlite_type → JSON type: text→string, integer→number,
                // real→number, null/blob→null-or-string. rusqlite's
                // value_ref covers all of them.
                use rusqlite::types::ValueRef;
                let val = match row.get_ref(i)? {
                    ValueRef::Null => serde_json::Value::Null,
                    ValueRef::Integer(n) => serde_json::Value::Number(n.into()),
                    ValueRef::Real(f) => serde_json::Number::from_f64(f)
                        .map(serde_json::Value::Number)
                        .unwrap_or(serde_json::Value::Null),
                    ValueRef::Text(bytes) => {
                        serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned())
                    }
                    ValueRef::Blob(bytes) => serde_json::Value::String(
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ),
                };
                obj.insert(name.clone(), val);
            }
            Ok(serde_json::Value::Object(obj))
        })?;
        let mut arr = Vec::new();
        for r in rows {
            arr.push(r?);
        }
        Ok(serde_json::to_string(&arr).unwrap_or_else(|_| "[]".into()))
    }

    /// The timestamp of the most recent log entry across all tables — the
    /// "as-of" watermark for the export. Returns the max of: executions
    /// `finished_at`/`started_at`, telemetry `ts`, reflections `occurred_at`,
    /// and mcp_usage `called_at_ms`. Returns `None` when the store is empty.
    pub fn last_log_timestamp(&self) -> Result<Option<String>> {
        self.last_log_timestamp_scoped(ExportScope::Workspace)
    }

    /// [`Store::last_log_timestamp`] restricted to one session's executions.
    ///
    /// The archive is *named* for the last log entry it contains, so scoping
    /// the rows without scoping the watermark would leave the filename
    /// asserting a time no row in the archive reaches — a small lie, in the
    /// artifact whose whole purpose is being evidence.
    pub fn last_log_timestamp_for_session(&self, session_id: &str) -> Result<Option<String>> {
        self.last_log_timestamp_scoped(ExportScope::Session(session_id))
    }

    fn last_log_timestamp_scoped(&self, scope: ExportScope<'_>) -> Result<Option<String>> {
        let conn = self.lock();
        let bindings = scope.bindings();
        let executions_where = scope.executions_where("");
        let child_where = scope.child_where();
        // The most reliable chronological markers, in priority order. The old
        // `WHERE finished_at IS NOT NULL` on the third is gone because `MAX`
        // already ignores NULLs — keeping it would have forced that one
        // candidate to compose its predicate differently from the other three.
        let candidates = [
            format!("SELECT MAX(ts) FROM telemetry {child_where}"),
            format!("SELECT MAX(started_at) FROM executions {executions_where}"),
            format!("SELECT MAX(finished_at) FROM executions {executions_where}"),
            format!(
                "SELECT datetime(MAX(called_at_ms) / 1000, 'unixepoch') FROM mcp_usage \
                 {child_where}"
            ),
        ];
        let mut latest: Option<String> = None;
        for sql in candidates {
            let row: rusqlite::Result<Option<String>> =
                conn.query_row(&sql, bindings.as_slice(), |row| row.get(0));
            if let Ok(Some(ts)) = row
                && !ts.is_empty()
            {
                latest = Some(match latest {
                    Some(prev) if prev >= ts => prev,
                    _ => ts,
                });
            }
        }
        Ok(latest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileTouchRow, TelemetryRow};

    /// Seed two sessions plus one unattributed execution, and return
    /// `(store, session_a_id)`. Every table the export covers gets a row on
    /// both sides so a leak in any one of them is visible.
    fn two_session_store() -> (Store, &'static str) {
        let store = Store::in_memory().expect("store");
        for (session, provider, path) in [
            (Some("ses-a"), "anthropic", "a/only.rs"),
            (Some("ses-b"), "openai", "b/secret.rs"),
            (None, "local", "unattributed.rs"),
        ] {
            let id = store
                .begin_execution("deck", &format!("prompt for {provider}"), provider, "m")
                .expect("execution");
            if let Some(session) = session {
                store.set_execution_session(id, session).expect("stamp");
            }
            store
                .record_telemetry(
                    id,
                    &TelemetryRow {
                        step: 0,
                        call_role: "worker".into(),
                        provider: provider.into(),
                        model: "m".into(),
                        input_tokens: 100,
                        estimated_input_tokens: 100,
                        output_tokens: 10,
                        cache_read_tokens: 0,
                        cache_miss_tokens: 0,
                        cache_write_tokens: 0,
                        cost_usd: 1.0,
                        duration_ms: 5,
                        retries: 0,
                        tool_calls: 0,
                        usage_complete: true,
                        sub_agent_id: None,
                    },
                )
                .expect("telemetry");
            store
                .record_files_touched(
                    id,
                    &[FileTouchRow {
                        path: path.into(),
                        ops: "M".into(),
                        lines_added: 1,
                        lines_removed: 0,
                        events_json: "[]".into(),
                    }],
                )
                .expect("files");
        }
        (store, "ses-a")
    }

    /// The witness for #2558. `/export` is documented as an artifact you email
    /// or attach to a PR; before the fix its dump was every execution the
    /// workspace had ever recorded, so one session's evidence disclosed every
    /// other session's prompts, tool arguments, and touched-file contents.
    ///
    /// Asserted over EVERY exported table, not just `executions`: the child
    /// tables reach `session_id` through a different predicate, so a table
    /// left unscoped is exactly the leak that would survive an
    /// executions-only assertion.
    #[test]
    fn a_session_export_contains_no_other_session_rows() {
        let (store, session) = two_session_store();

        let dumps = store.export_session_json(session).expect("scoped dump");
        assert_eq!(dumps.len(), CHILD_TABLES.len() + 1, "every table present");

        for (table, json) in &dumps {
            assert!(
                !json.contains("openai") && !json.contains("b/secret.rs"),
                "`{table}` leaked the other session: {json}"
            );
            assert!(
                !json.contains("local") && !json.contains("unattributed.rs"),
                "`{table}` leaked an unattributed execution: {json}"
            );
        }

        // …and the exported session's own rows all survived the filter.
        for (table, needle) in [
            ("executions", "anthropic"),
            ("telemetry", "anthropic"),
            ("files_touched", "a/only.rs"),
        ] {
            let json = dumps
                .iter()
                .find(|(name, _)| *name == table)
                .map(|(_, json)| json.as_str())
                .expect("table present");
            assert!(
                json.contains(needle),
                "`{table}` dropped its own rows: {json}"
            );
        }

        // The unscoped dump is the shape the fix moved away from — it still
        // exists for in-process introspection, and it still sees everything.
        let all = store.export_all_json().expect("workspace dump");
        let executions = all
            .iter()
            .find(|(name, _)| *name == "executions")
            .map(|(_, json)| json.as_str())
            .expect("executions present");
        assert!(
            executions.contains("openai") && executions.contains("local"),
            "the workspace dump is unchanged: {executions}"
        );
    }

    /// The dashboard's KPI tiles and every chart are computed from
    /// `usage_stats`, so scoping the rows without scoping the totals would
    /// print another session's spend over this session's data.
    #[test]
    fn session_usage_stats_total_only_the_exported_session() {
        let (store, session) = two_session_store();

        let scoped = store.usage_stats_for_session(session).expect("scoped");
        assert_eq!(scoped.len(), 1, "one (provider, model) pair: {scoped:?}");
        assert_eq!(scoped[0].provider, "anthropic");
        assert_eq!(scoped[0].runs, 1);
        assert_eq!(scoped[0].input_tokens, 100, "telemetry joined, once");

        let workspace = store.usage_stats().expect("workspace");
        assert_eq!(workspace.len(), 3, "all three executions: {workspace:?}");
    }

    /// A session with no executions of its own must export nothing — not fall
    /// back to the workspace. The empty case is where a "scope it if you can"
    /// implementation quietly leaks everything.
    #[test]
    fn an_unknown_session_exports_no_rows_rather_than_all_of_them() {
        let (store, _) = two_session_store();
        let dumps = store
            .export_session_json("ses-does-not-exist")
            .expect("dump");
        for (table, json) in &dumps {
            assert_eq!(json, "[]", "`{table}` should be empty: {json}");
        }
        assert!(
            store
                .usage_stats_for_session("ses-does-not-exist")
                .expect("stats")
                .is_empty()
        );
    }

    /// Rows the scope drops are counted, not vanished — the manifest's claim
    /// that this archive covers one session is only checkable if it says what
    /// it left out.
    #[test]
    fn exclusions_count_other_sessions_and_unattributed_rows() {
        let (store, session) = two_session_store();

        let excluded = store.export_exclusions(session).expect("census");
        assert_eq!(excluded.other_session_executions, 1, "ses-b");
        assert_eq!(excluded.unattributed_executions, 1, "the unstamped one");
        assert_eq!(excluded.unattributed_reflections, 0);
        assert!(!excluded.is_empty());

        // A store holding only the exported session has nothing to declare.
        let solo = Store::in_memory().expect("store");
        let id = solo
            .begin_execution("deck", "p", "anthropic", "m")
            .expect("exec");
        solo.set_execution_session(id, "ses-solo").expect("stamp");
        assert!(
            solo.export_exclusions("ses-solo")
                .expect("census")
                .is_empty()
        );
    }

    /// The archive is named for the last log entry it contains. A watermark
    /// read across the whole workspace would name a moment from a session the
    /// archive does not hold.
    #[test]
    fn the_watermark_comes_from_the_exported_session_only() {
        let store = Store::in_memory().expect("store");
        let mut ids = Vec::new();
        for session in ["ses-old", "ses-new"] {
            let id = store
                .begin_execution("deck", "p", "anthropic", "m")
                .expect("exec");
            store.set_execution_session(id, session).expect("stamp");
            ids.push(id);
        }
        // Hand-stamp distinguishable clocks. Both the `executions` and the
        // `telemetry` candidate have to move: the watermark is the MAX across
        // all four candidates, so leaving `started_at` at its
        // `CURRENT_TIMESTAMP` default would let today's date win for both
        // sessions and the assertion would pass for the wrong reason.
        for (id, ts) in [
            (ids[0], "2020-01-01 00:00:00"),
            (ids[1], "2030-01-01 00:00:00"),
        ] {
            store
                .record_telemetry(
                    id,
                    &TelemetryRow {
                        step: 0,
                        call_role: "worker".into(),
                        provider: "anthropic".into(),
                        model: "m".into(),
                        input_tokens: 1,
                        estimated_input_tokens: 1,
                        output_tokens: 1,
                        cache_read_tokens: 0,
                        cache_miss_tokens: 0,
                        cache_write_tokens: 0,
                        cost_usd: 0.0,
                        duration_ms: 1,
                        retries: 0,
                        tool_calls: 0,
                        usage_complete: true,
                        sub_agent_id: None,
                    },
                )
                .expect("telemetry");
            let conn = store.lock();
            conn.execute(
                "UPDATE telemetry SET ts = ?1 WHERE execution_id = ?2",
                rusqlite::params![ts, id],
            )
            .expect("restamp telemetry");
            conn.execute(
                "UPDATE executions SET started_at = ?1, finished_at = ?1 WHERE id = ?2",
                rusqlite::params![ts, id],
            )
            .expect("restamp execution");
        }

        let old = store
            .last_log_timestamp_for_session("ses-old")
            .expect("watermark")
            .expect("some");
        assert_eq!(old, "2020-01-01 00:00:00", "not the newer session's clock");
        let workspace = store
            .last_log_timestamp()
            .expect("watermark")
            .expect("some");
        assert_eq!(workspace, "2030-01-01 00:00:00");
    }

    /// A session id is bound, never interpolated. The predicate builder emits
    /// a placeholder and a static column reference and nothing else, so a
    /// hostile id cannot reach the SQL text.
    #[test]
    fn a_session_id_is_bound_not_interpolated() {
        let hostile = "x' OR 1=1 --";
        let scope = ExportScope::Session(hostile);
        assert_eq!(scope.executions_where("e."), "WHERE e.session_id = ?1");
        assert!(!scope.child_where().contains(hostile));

        let (store, _) = two_session_store();
        let dumps = store.export_session_json(hostile).expect("dump");
        for (table, json) in &dumps {
            assert_eq!(
                json, "[]",
                "`{table}` answered an injected predicate: {json}"
            );
        }
    }

    /// The workspace scope binds nothing and adds no clause — the unscoped
    /// readers (`stella stats`, in-process introspection) must be byte-for-byte
    /// the query they always were.
    #[test]
    fn the_workspace_scope_adds_no_predicate() {
        let scope = ExportScope::Workspace;
        assert_eq!(scope.executions_where("e."), "");
        assert_eq!(scope.child_where(), "");
        assert!(scope.bindings().is_empty());
    }
}
