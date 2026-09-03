// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What happened when a turn could not write its audit record.
//!
//! Three writes close an execution: agent uses, MCP usage, and the outcome
//! row. All three are best effort. A turn that ran is not less run because
//! the store would not take a row. Saying why is not optional.
//!
//! A failed write keeps its [`StoreError`]. So the warning can name the
//! SQLite result code, the database file, and whether a retry ran. A write
//! turned away for a held lock is asked for again first, through
//! [`stella_store::retry_busy`]. A write that is still lost is counted, so a
//! run can say how much of its record is missing.
//!
//! The count is one [`AtomicU32`] for the whole process, because that is the
//! shape of the thing it counts. A session's turns share one store across
//! threads. A child turn of the self-driving loop reports its own count in
//! the turn summary, and the loop adds it to this one.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use colored::Colorize;
use stella_store::StoreError;

/// Audit writes given up on since this process started.
static DROPPED: AtomicU32 = AtomicU32::new(0);

/// One audit write that never reached the store.
#[derive(Debug)]
pub(crate) struct DroppedWrite {
    /// The record that was lost, named the way the warning shows it.
    what: &'static str,
    /// What the last attempt was refused with.
    error: StoreError,
    /// Attempts spent, retries included.
    attempts: u32,
}

impl DroppedWrite {
    /// One line to act on: the result code, whether the write was asked for
    /// again, and what SQLite said.
    ///
    /// The code leads because it says where to look. `DatabaseBusy` points at
    /// whatever else is writing the file. `ReadOnly` points at its file mode.
    /// `Full` points at the disk.
    fn diagnosis(&self) -> String {
        let code = self
            .error
            .sqlite_code()
            .map_or_else(|| "no SQLite code".to_owned(), |code| format!("{code:?}"));
        let tries = if self.attempts > 1 {
            format!("after {} attempts", self.attempts)
        } else {
            "not retried".to_owned()
        };
        format!(
            "{what}: {code}, {tries} — {error}",
            what = self.what,
            error = self.error
        )
    }
}

/// Run one audit write. Ask again while the database is busy, and count the
/// write if it is still turned away.
///
/// The count is kept here, not at the call sites, so the number and the
/// warning cannot drift apart. A write is counted once, when it is given up
/// on, whoever is closing the execution.
pub(crate) fn audit_write(
    what: &'static str,
    write: impl FnMut() -> Result<(), StoreError>,
) -> Option<DroppedWrite> {
    let retry = stella_store::retry_busy(write);
    match retry.outcome {
        Ok(()) => None,
        Err(error) => {
            add(1);
            Some(DroppedWrite {
                what,
                error,
                attempts: retry.attempts,
            })
        }
    }
}

/// Audit writes this process has given up on.
pub(crate) fn dropped_audit_writes() -> u32 {
    DROPPED.load(Ordering::Relaxed)
}

/// Read the count and set it back to zero. A caller adding it to a durable
/// tally can then never count the same dropped write twice.
pub(crate) fn take_dropped_audit_writes() -> u32 {
    DROPPED.swap(0, Ordering::Relaxed)
}

/// Add what a child turn said it dropped.
///
/// The self-driving loop runs its turns in child processes. The drops it
/// cares about happen where its own count cannot see them. The child prints
/// its count in the turn summary, and the loop adds it here. That keeps one
/// number true for both.
pub(crate) fn note_child_dropped_audit_writes(count: u32) {
    add(count);
}

fn add(count: u32) {
    let _ = DROPPED.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(count))
    });
}

/// The plain warning: a store write failed and `what` is now incomplete.
///
/// It is used for writes that have no error left to report, such as the
/// prompt receipt. It is also the first line of
/// [`warn_dropped_audit_writes`].
pub(crate) fn warn_store_write_failed(what: &str) {
    eprintln!(
        "  {} store write failed — {what} for this execution is incomplete",
        "⚠".yellow()
    );
}

/// Warn about audit writes that were lost, naming each one and the file.
pub(crate) fn warn_dropped_audit_writes(db_path: Option<&Path>, dropped: &[DroppedWrite]) {
    if dropped.is_empty() {
        return;
    }
    warn_store_write_failed("the audit record (agent uses / MCP usage / outcome)");
    eprint!("{}", dropped_write_report(db_path, dropped));
}

/// The indented lines under the warning: one per lost write, then the file
/// they were meant for.
///
/// It reads only what was lost, so both answers to "which file" can be tested
/// without a database.
fn dropped_write_report(db_path: Option<&Path>, dropped: &[DroppedWrite]) -> String {
    let mut report = String::new();
    for write in dropped {
        report.push_str(&format!("      {}\n", write.diagnosis()));
    }
    let store = db_path.map_or_else(
        || "in memory, so there is no file to inspect".to_owned(),
        |path| path.display().to_string(),
    );
    report.push_str(&format!("      store: {store}\n"));
    report
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{DroppedWrite, dropped_write_report};
    use stella_store::{Store, StoreError};

    /// A real turned-away write, so the code in the report is SQLite's own
    /// and not one a test made up.
    fn refused_write() -> StoreError {
        let store = Store::in_memory().expect("store");
        let id = store
            .begin_execution("run", "p", "anthropic", "claude")
            .expect("execution");
        let calls = [stella_store::McpUsageRow {
            server: "github".into(),
            tool: "search_issues".into(),
            reason: String::new(),
            called_at_ms: 1,
        }];
        store.record_mcp_usage(id, &calls).expect("first write");
        store
            .record_mcp_usage(id, &calls)
            .expect_err("the seq is already taken")
    }

    /// **The witness.** A turned-away audit write is kept, counted, and
    /// reported with the SQLite result code.
    ///
    /// The failure is a real one. The MCP usage table refuses a second row at
    /// a `seq` it already holds, which is the clash `record_mcp_usage` names.
    #[test]
    fn a_refused_audit_write_is_counted_and_carries_its_code() {
        let store = Store::in_memory().expect("store");
        let id = store
            .begin_execution("run", "p", "anthropic", "claude")
            .expect("execution");
        let root = tempfile::tempdir().expect("root");
        let registry = stella_tools::ToolRegistry::new(root.path().to_path_buf());
        stella_core::mcp_usage::push_usage(
            &registry.mcp_usage_ledger(),
            stella_core::mcp_usage::McpUsageRecord::now("github", "search_issues", ""),
        );
        store
            .record_mcp_usage(
                id,
                &[stella_store::McpUsageRow {
                    server: "github".into(),
                    tool: "search_issues".into(),
                    reason: String::new(),
                    called_at_ms: 1,
                }],
            )
            .expect("occupy the seq the closeout will write");

        let before = super::dropped_audit_writes();
        let end = crate::agent::record_execution_end(&store, id, &registry, "completed", 0.0, true);

        assert_eq!(super::dropped_audit_writes(), before + 1);
        assert!(!end.write_ok, "the closeout knows the write was refused");
        let report = dropped_write_report(store.db_path(), &end.dropped);
        assert!(report.contains("MCP usage"), "{report}");
        assert!(report.contains("ConstraintViolation"), "{report}");
    }

    #[test]
    fn the_report_names_the_code_the_file_and_whether_a_retry_ran() {
        let dropped = [DroppedWrite {
            what: "MCP usage",
            error: refused_write(),
            attempts: 3,
        }];

        let report = dropped_write_report(Some(Path::new("/w/.stella/private/store.db")), &dropped);

        assert!(report.contains("MCP usage"), "{report}");
        assert!(report.contains("ConstraintViolation"), "{report}");
        assert!(report.contains("after 3 attempts"), "{report}");
        assert!(
            report.contains("store: /w/.stella/private/store.db"),
            "{report}"
        );
    }

    /// A write that was tried once says so, so nobody reads one try as a
    /// lock that was waited out.
    #[test]
    fn a_write_that_was_not_retried_says_so() {
        let dropped = [DroppedWrite {
            what: "the outcome",
            error: refused_write(),
            attempts: 1,
        }];

        let report = dropped_write_report(None, &dropped);

        assert!(report.contains("not retried"), "{report}");
        assert!(report.contains("in memory"), "{report}");
    }
}
