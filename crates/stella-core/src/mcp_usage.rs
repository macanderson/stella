//! Shared MCP tool-usage telemetry: the record shape and the session-scoped
//! ledger handle.
//!
//! External MCP servers' tools are called through `stella-mcp`'s `McpToolSet`,
//! which bypasses `stella-tools::ToolRegistry` entirely (an `mcp__…` name never
//! falls through to the native executor). So the native file-touch ledger can
//! never observe an MCP call. This module is the shared seam that lets it be
//! observed anyway: `McpToolSet` appends an [`McpUsageRecord`] on every
//! successful call, and the CLI drains the same [`McpUsageLedger`] handle into
//! `store.db` once per execution — the same "written by one object, drained via
//! another" shape the memory-citation ledger uses.
//!
//! Both `stella-mcp` and `stella-tools` depend on `stella-core`, so the type
//! lives here to avoid a dependency cycle between them.

use std::sync::{Arc, Mutex};

/// One recorded MCP tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpUsageRecord {
    /// The MCP server the tool belongs to (the config alias / namespace
    /// segment), e.g. `github`.
    pub server: String,
    /// The un-namespaced tool name the model invoked, e.g. `search_issues`.
    pub tool: String,
    /// Why the call was made, taken from a `reason` key in the tool input when
    /// present. External MCP tools rarely define one, so this is usually empty
    /// — the count and tool identity are always reliable, the reason is
    /// best-effort.
    pub reason: String,
    /// Call time in milliseconds since the Unix epoch (captured at call time,
    /// not at drain time, so the timestamp reflects when the tool actually ran).
    pub called_at_ms: u64,
}

impl McpUsageRecord {
    /// Build a record. `called_at_ms` is the call time, in milliseconds
    /// since the Unix epoch, chosen by the caller.
    ///
    /// This crate holds no clock of its own — [`crate::ports::Clock`]'s own
    /// doc comment says so. The caller reads the real clock (`stella-mcp`'s
    /// `McpToolSet`, right when a call finishes) and passes the value in. A
    /// plain argument, not a `&dyn Clock`, because this constructor has no
    /// other reason to hold a clock. It also lets a test pick any instant it
    /// wants.
    pub fn new(
        server: impl Into<String>,
        tool: impl Into<String>,
        reason: impl Into<String>,
        called_at_ms: u64,
    ) -> Self {
        Self {
            server: server.into(),
            tool: tool.into(),
            reason: reason.into(),
            called_at_ms,
        }
    }
}

/// A session-scoped, drain-once ledger of MCP tool calls, shared by two owners:
/// the `McpToolSet` clones one handle to *append*, and `ToolRegistry` holds
/// another to *drain* in `record_execution_end`. Draining (rather than a
/// cumulative snapshot) is what lets each call be persisted under exactly one
/// execution id — re-persisting under later executions would inflate the count.
pub type McpUsageLedger = Arc<Mutex<Vec<McpUsageRecord>>>;

/// Append a record to a ledger, tolerating a poisoned lock (telemetry must
/// never take down a tool call).
pub fn push_usage(ledger: &McpUsageLedger, record: McpUsageRecord) {
    let mut guard = ledger.lock().unwrap_or_else(|p| p.into_inner());
    guard.push(record);
}

/// Drain a ledger, returning everything recorded so far and leaving it empty.
pub fn drain_usage(ledger: &McpUsageLedger) -> Vec<McpUsageRecord> {
    let mut guard = ledger.lock().unwrap_or_else(|p| p.into_inner());
    std::mem::take(&mut *guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_then_drain_moves_records_out_and_leaves_empty() {
        let ledger: McpUsageLedger = Arc::default();
        push_usage(
            &ledger,
            McpUsageRecord::new("github", "search_issues", "", 1),
        );
        push_usage(
            &ledger,
            McpUsageRecord::new("fs", "read", "inspect config", 2),
        );

        let drained = drain_usage(&ledger);
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].server, "github");
        assert_eq!(drained[1].reason, "inspect config");

        // Drained once — a second drain is empty (no double-persist).
        assert!(drain_usage(&ledger).is_empty());
    }

    #[test]
    fn a_shared_clone_sees_appends_from_the_other_owner() {
        let writer: McpUsageLedger = Arc::default();
        let draining = writer.clone();
        push_usage(&writer, McpUsageRecord::new("s", "t", "", 1));
        assert_eq!(drain_usage(&draining).len(), 1);
    }

    /// **Witness.** The old constructor, `now()`, read the real clock inside
    /// this crate. That is the exact thing [`crate::ports::Clock`]'s doc
    /// comment says this crate must never do. `now()` also took no
    /// timestamp argument, so no test could place a record at a chosen
    /// instant.
    ///
    /// `stella-store`'s `mcp_usage` table orders rows by `called_at_ms`. A
    /// test built on `now()` could only see two real clock reads, taken
    /// milliseconds apart. `new` takes the timestamp as a plain argument, so
    /// this test places one record far in the past and one far in the
    /// future, with no wait at all. `now()` had no way to do that.
    #[test]
    fn new_stamps_the_record_at_the_caller_supplied_instant_not_the_wall_clock() {
        let past = McpUsageRecord::new("github", "search_issues", "", 1_000);
        let future = McpUsageRecord::new("github", "search_issues", "", 9_999_999_999_999);
        assert_eq!(past.called_at_ms, 1_000);
        assert_eq!(future.called_at_ms, 9_999_999_999_999);
    }
}
