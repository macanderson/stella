// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `mcp_usage` call log. One row per MCP tool call under an execution,
//! drained once per execution from the ledger `stella-mcp` writes to.
//!
//! It sits beside [`crate::agent_uses`] for the reason that module gives.
//! `lib.rs` is a god file closed to growth (AGENTS.md § *God files*), so a
//! table's rows, its writer and its reader live in the table's own module.
//!
//! Nothing is summed on the way in. Each call is its own row, because the
//! thing being counted is "this tool ran under this execution at this time".
//! [`fold_mcp_usage_stats`] sums them on the way out.

use rusqlite::params;

use crate::{Result, Store};

/// One MCP tool call, ready to store: the server, the tool, a reason if the
/// call carried one, and the time in epoch millis. Outside tools rarely give
/// a reason. Each call is its own row, so two calls to one tool are two rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpUsageRow {
    pub server: String,
    pub tool: String,
    pub reason: String,
    pub called_at_ms: i64,
}

/// One server and tool, with its call count. This is what the MCP tab's
/// "N calls" column shows. Most used first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpUsageStat {
    pub server: String,
    pub tool: String,
    /// How many times this tool was called, across all executions.
    pub calls: i64,
    /// The last reason given, or empty if no call gave one.
    pub last_reason: String,
    /// The time of the last call, in epoch millis.
    pub last_called_at_ms: i64,
}

impl Store {
    /// Store one execution's MCP tool calls, one row per call, in drain
    /// order. `seq` is the index in the batch, and the table holds it unique
    /// per execution. So writing the same drained batch twice is an error
    /// rather than a quiet double count. It is one transaction, like
    /// [`Store::record_files_touched`]. A clash part way through leaves no
    /// half batch behind, so a fixed batch can be written again.
    pub fn record_mcp_usage(&self, execution_id: i64, calls: &[McpUsageRow]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for (seq, row) in calls.iter().enumerate() {
            tx.execute(
                "INSERT INTO mcp_usage \
                 (execution_id, seq, server, tool, reason, called_at_ms) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    execution_id,
                    seq as i64,
                    row.server,
                    row.tool,
                    row.reason,
                    row.called_at_ms,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Call counts per server and tool ([`McpUsageStat`]). This is what the
    /// MCP tab and `stella mcp usage` show. Most used first, with ties broken
    /// by server then tool, so the order never wobbles.
    pub fn mcp_usage_stats(&self) -> Result<Vec<McpUsageStat>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT server, tool, reason, called_at_ms FROM mcp_usage \
             ORDER BY called_at_ms ASC, rowid ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(McpUsageRow {
                server: row.get(0)?,
                tool: row.get(1)?,
                reason: row.get(2)?,
                called_at_ms: row.get(3)?,
            })
        })?;
        let mut calls = Vec::new();
        for row in rows {
            calls.push(row?);
        }
        Ok(fold_mcp_usage_stats(&calls))
    }
}

/// Sum rows in time order into one row per server and tool: how many calls,
/// the last reason given, and the last call time. Rows have to arrive oldest
/// first, which is the order [`Store::mcp_usage_stats`] reads them in. The
/// result is most used first, with ties broken by server then tool.
pub fn fold_mcp_usage_stats(rows: &[McpUsageRow]) -> Vec<McpUsageStat> {
    use std::collections::BTreeMap;
    let mut by_key: BTreeMap<(String, String), McpUsageStat> = BTreeMap::new();
    for row in rows {
        let entry = by_key
            .entry((row.server.clone(), row.tool.clone()))
            .or_insert_with(|| McpUsageStat {
                server: row.server.clone(),
                tool: row.tool.clone(),
                calls: 0,
                last_reason: String::new(),
                last_called_at_ms: 0,
            });
        entry.calls += 1;
        // Rows arrive oldest first, so the last reason seen is the newest.
        if !row.reason.is_empty() {
            entry.last_reason = row.reason.clone();
        }
        entry.last_called_at_ms = entry.last_called_at_ms.max(row.called_at_ms);
    }
    let mut stats: Vec<McpUsageStat> = by_key.into_values().collect();
    stats.sort_by(|a, b| {
        b.calls
            .cmp(&a.calls)
            .then_with(|| a.server.cmp(&b.server))
            .then_with(|| a.tool.cmp(&b.tool))
    });
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(server: &str, tool: &str, reason: &str, at: i64) -> McpUsageRow {
        McpUsageRow {
            server: server.to_string(),
            tool: tool.to_string(),
            reason: reason.to_string(),
            called_at_ms: at,
        }
    }

    /// Writing a drained batch twice is refused, not counted twice. The
    /// closeout leans on that when it asks again.
    #[test]
    fn the_same_batch_written_twice_is_refused() {
        let store = Store::in_memory().expect("store");
        let id = store
            .begin_execution("run", "p", "anthropic", "claude")
            .expect("execution");
        let calls = [call("github", "search_issues", "", 10)];

        store.record_mcp_usage(id, &calls).expect("first write");
        let again = store
            .record_mcp_usage(id, &calls)
            .expect_err("the seq is already taken");
        assert!(again.sqlite_code().is_some(), "SQLite names it: {again}");
    }

    #[test]
    fn folding_orders_by_calls_and_keeps_the_last_reason() {
        let stats = fold_mcp_usage_stats(&[
            call("github", "search_issues", "find the bug", 10),
            call("fs", "read", "", 20),
            call("github", "search_issues", "", 30),
        ]);

        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].tool, "search_issues");
        assert_eq!(stats[0].calls, 2);
        assert_eq!(stats[0].last_reason, "find the bug");
        assert_eq!(stats[0].last_called_at_ms, 30);
    }
}
