// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The cross-project per-tool reliability report (#3147).
//!
//! `tool_usage_rollup` has been written on every synced turn since the hub
//! existed — per `(project, tool, surface, day)` call and error counts — but
//! its `errors` column had no reader at all: the one consumer summed `calls`
//! and dropped the rest. That made a per-tool reliability ceiling ("no
//! shipped tool errors more than N% of the time") unenforceable in any
//! script, CI job, or bench postmortem: the only surface showing tool errors
//! was the Observatory's browser dashboard, loopback-only and windowed.
//!
//! This module is the hub-side half of that surface: one reader returning
//! the unwindowed cross-project totals, consumed by `stella usage report
//! --by-tool`. Rates are left to the caller — the hub hands out exact
//! numerators and denominators, not derived percentages.
//!
//! Honesty caveat, named rather than silent: rows written by pre-v24 stores
//! counted *abandoned* calls (turn ended mid-flight) as errors, and those
//! historic buckets are already merged beyond splitting. Writers at v24+
//! exclude abandonment (#3146), so the report converges on the true defect
//! rate going forward.

use super::{Result, UsageStore};

/// One tool's cross-project totals, straight off `tool_usage_rollup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolReportRow {
    /// Tool name as the projection recorded it (raw, including `mcp__`
    /// namespacing — the hub never renames).
    pub tool: String,
    /// `"native"` | `"mcp"` — the surfaces the store-side producer derives.
    pub surface: String,
    /// Total calls across every project and day the hub has seen.
    pub calls: i64,
    /// Total errored calls (`state = 'error'` at the producer; see the
    /// module doc for the pre-v24 caveat).
    pub errors: i64,
}

impl UsageStore {
    /// Cross-project per-tool call and error totals, most-used first.
    ///
    /// Supersedes the calls-only `tool_totals` histogram, which read the
    /// same table and discarded the `errors` column — the exact number a
    /// reliability ceiling needs (#3147).
    pub fn tool_report(&self) -> Result<Vec<ToolReportRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tool, surface, SUM(calls) AS c, SUM(errors) \
             FROM tool_usage_rollup \
             GROUP BY tool, surface ORDER BY c DESC, tool ASC, surface ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ToolReportRow {
                tool: row.get(0)?,
                surface: row.get(1)?,
                calls: row.get(2)?,
                errors: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::super::UsageStore;

    /// The witness for #3147: the `errors` column actually reaches a reader.
    /// On the old code this fails at compile time — no reader existed, and
    /// the one adjacent histogram summed `calls` alone.
    #[test]
    fn the_report_reads_the_errors_column_not_just_calls() {
        let usage = UsageStore::in_memory().unwrap();
        usage
            .lock()
            .execute_batch(
                "INSERT INTO tool_usage_rollup (project_id, tool, surface, day, calls, errors)
                 VALUES ('p1', 'bash', 'native', '2026-08-11', 10, 2),
                        ('p2', 'bash', 'native', '2026-08-12', 5, 1),
                        ('p1', 'read_file', 'native', '2026-08-12', 3, 0)",
            )
            .unwrap();

        let report = usage.tool_report().unwrap();
        assert_eq!(report.len(), 2);
        assert_eq!(
            (report[0].tool.as_str(), report[0].calls, report[0].errors),
            ("bash", 15, 3),
            "cross-project, cross-day sums with the error numerator intact"
        );
        assert_eq!(
            (report[1].tool.as_str(), report[1].errors),
            ("read_file", 0)
        );
        // Distinct surfaces stay distinct rows: an MCP tool's reliability is
        // not folded into a native tool that happens to share a name.
        usage
            .lock()
            .execute(
                "INSERT INTO tool_usage_rollup (project_id, tool, surface, day, calls, errors)
                 VALUES ('p1', 'bash', 'mcp', '2026-08-12', 1, 1)",
                params![],
            )
            .unwrap();
        assert_eq!(usage.tool_report().unwrap().len(), 3);
    }
}
