// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The cross-project `(org, provider, model)` spend report — what
//! `stella usage report` and the Observatory's usage view render.
//!
//! It reads the hub's `telemetry` replica, which holds one row per paid call
//! from every project that has synced, and joins the `execution_rollup` row
//! beside it for the one fact a per-call table cannot carry: whether the
//! execution behind those calls accounted for all of them (#4171).

use rusqlite::params;

use super::{Result, UsageStore};

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
    /// Cache writes are the cost side of the cache ledger: without them a
    /// report can show a read count but never net savings (writes bill at a
    /// premium on opt-in providers). The column was always in the hub table;
    /// only this rollup dropped it.
    pub cache_write_tokens: i64,
    pub cost_usd: f64,
    pub projects: i64,
    /// How many of the executions behind this line are **floors** — a turn
    /// finished with at least one paid call whose usage envelope never landed,
    /// so its receipts prove a lower bound and not the total (#4171).
    ///
    /// Non-zero makes `cost_usd` an "at least" figure. It is deliberately a
    /// count of executions rather than of calls: the flag is per-execution, and
    /// "3 of 210 executions are floors" is the sentence a user checking a
    /// provider bill can act on.
    pub floor_executions: i64,
    /// How many of the executions behind this line have **no verdict at
    /// all** — their `telemetry` rows survive but the `execution_rollup` row
    /// that would say complete-or-floor was pruned first (#4485). `prune`
    /// ages `execution_rollup` and `telemetry` out on predicates they do not
    /// share (`crates/stella-store/src/usage.rs`'s age-cutoff branch; the
    /// same asymmetry `super::tool_fold` documents for #3411), so this is
    /// not a hypothetical: an org-scoped, un-acked telemetry row survives an
    /// age cutoff that its rollup row does not.
    ///
    /// Distinct from [`Self::floor_executions`] on purpose: a floor is a
    /// **known** lower bound, this is an **unknown** verdict. Folding it into
    /// the floor count would under-state the floor count's own meaning
    /// (`floor_executions` used to do exactly that, by reading a missing
    /// rollup row as complete via `COALESCE(r.usage_complete, 1)`).
    pub unknown_executions: i64,
}

impl UsageStore {
    /// The global report: per (org, provider, model) call counts, token and
    /// cache totals, cost, and how many projects contributed. `org` filters to
    /// one org id; `None` reports everything (NULL-org rows group as
    /// unregistered/local).
    ///
    /// # Why the `execution_rollup` join
    ///
    /// [`GlobalTelemetryRow::floor_executions`] cannot come from `telemetry`
    /// alone, and #4171 says why: a call whose usage envelope never landed
    /// wrote **no row**, so no sum over `telemetry` can tell "complete" from
    /// "missing a row". `telemetry.usage_complete` is a per-*call* flag and
    /// answers a different question. The per-execution verdict lives on
    /// `executions.usage_complete` in the project store and reaches the hub on
    /// the rollup row, so that is what the count reads.
    ///
    /// A telemetry row whose rollup `prune` has already deleted has no
    /// verdict left to read: it counts toward
    /// [`GlobalTelemetryRow::unknown_executions`], never silently toward
    /// "complete" (#4485; before this it read as complete via
    /// `COALESCE(r.usage_complete, 1)`, under-reporting how many figures were
    /// floors). That gap is the pre-existing shape of hub pruning rather than
    /// a new one — the rollup and the replica are pruned on predicates they
    /// do not share — and it costs only a *marking*, never a figure: the
    /// spend is summed from `telemetry` either way.
    pub fn global_telemetry_totals(&self, org: Option<&str>) -> Result<Vec<GlobalTelemetryRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT t.org_id, t.provider, t.model, COUNT(*), SUM(t.input_tokens), \
                    SUM(t.output_tokens), SUM(t.cache_read_tokens), \
                    SUM(t.cache_write_tokens), SUM(t.cost_usd), \
                    COUNT(DISTINCT t.project_id), \
                    COUNT(DISTINCT CASE WHEN r.usage_complete = 0 \
                                        THEN t.project_id || ':' || t.execution_id END), \
                    COUNT(DISTINCT CASE WHEN r.usage_complete IS NULL \
                                        THEN t.project_id || ':' || t.execution_id END) \
             FROM telemetry t \
             LEFT JOIN execution_rollup r \
               ON r.project_id = t.project_id AND r.execution_id = t.execution_id \
             WHERE (?1 IS NULL OR t.org_id = ?1) \
             GROUP BY t.org_id, t.provider, t.model \
             ORDER BY SUM(t.cost_usd) DESC, t.provider, t.model",
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
                cache_write_tokens: r.get(7)?,
                cost_usd: r.get(8)?,
                projects: r.get(9)?,
                floor_executions: r.get(10)?,
                unknown_executions: r.get(11)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}
