//! The durable half of adoption attribution: `AdoptedChange` → `files_touched`.
//!
//! #2907's stated goal was that "the stream and the durable record are two
//! projections of one adoption". #3413 landed the stream projection; this is
//! the durable one, which had no producer at all — `Store::record_files_touched`
//! sat with no production caller in the workspace and the table was empty for
//! every run, isolated and shared-tree alike (#3419).
//!
//! Lives in its own module rather than in `candidate_ws.rs` because that file
//! is large and this is a separable concern: one mapping, one write, and the
//! decision about what a row means.

use std::sync::Arc;
use stella_pipeline::ports::AdoptedChange;
use stella_protocol::FileChangeKind;
use stella_store::{FileTouchRow, Store};

/// Where one candidate's adopted changes are attributed.
///
/// Cloned into every candidate the session builds, so each one writes against
/// the execution that owns the run — the id is resolved once, when the run's
/// `executions` row is opened, and never re-derived here.
#[derive(Clone)]
pub(crate) struct AdoptionLedger {
    store: Arc<Store>,
    execution_id: i64,
}

impl AdoptionLedger {
    pub(crate) fn new(store: Arc<Store>, execution_id: i64) -> Self {
        Self {
            store,
            execution_id,
        }
    }

    /// Persist one `files_touched` row per adopted path.
    ///
    /// Best-effort by construction: this is **observability, not evidence**
    /// (#2882), exactly as `CandidateWorkspace::attribute_adopted` documents.
    /// A failed write must never fail an adoption that git has already
    /// completed — the user's tree has the bytes either way, and turning a
    /// telemetry error into a delivery error would make the record's absence
    /// destroy the thing it was recording.
    pub(crate) fn record(&self, adopted: &[AdoptedChange]) {
        if adopted.is_empty() {
            return;
        }
        let rows: Vec<FileTouchRow> = adopted.iter().map(file_touch_row).collect();
        if let Err(error) = self.store.record_files_touched(self.execution_id, &rows) {
            // Deliberately stderr and not the event stream: the stream's copy
            // of this fact is the `FileChange` events emitted beside the same
            // rows, which are unaffected by a store failure. Saying it twice
            // there would imply the two projections disagreed.
            eprintln!("  ⚠ could not attribute adopted files: {error}");
        }
    }
}

/// One adopted change as a durable row.
fn file_touch_row(change: &AdoptedChange) -> FileTouchRow {
    FileTouchRow {
        path: change.path.clone(),
        ops: ops_letter(change.kind).to_string(),
        lines_added: u64::from(change.added),
        lines_removed: u64::from(change.removed),
        events_json: events_json(change),
    }
}

/// The `ops` alphabet is CRUD letters, not the change-kind names.
///
/// Fixed by the reader, not by preference: `Store::execution_rollup` counts a
/// run's written files with `ops LIKE '%C%' OR ops LIKE '%U%' OR ops LIKE
/// '%D%'` (`stella-store/src/lib.rs`), so a row spelled "Modified" — or "M" —
/// is a row that query cannot see. `FileTouchRow`'s own doc gives `"RUD"` as
/// its example for the same reason.
fn ops_letter(kind: FileChangeKind) -> &'static str {
    match kind {
        FileChangeKind::Created => "C",
        FileChangeKind::Read => "R",
        FileChangeKind::Modified => "U",
        FileChangeKind::Deleted => "D",
    }
}

/// The row's audit log: the ordered `{event, reason, lines_added,
/// lines_removed}` objects `FileTouchRow::events_json` documents.
///
/// An adoption contributes exactly one entry, because it *is* one event: git
/// applied the candidate's bytes to the user's tree in a single delivery. The
/// per-file diff is deliberately not carried here — `AdoptedChange::diff` can
/// be the whole file, `files_touched` is a per-execution index rather than a
/// content store, and the diff already rides the `FileChange` event beside
/// this row.
fn events_json(change: &AdoptedChange) -> String {
    serde_json::json!([{
        "event": "adopted",
        "reason": "candidate adopted into the session tree",
        "lines_added": change.added,
        "lines_removed": change.removed,
    }])
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &str, kind: FileChangeKind, added: u32, removed: u32) -> AdoptedChange {
        AdoptedChange {
            path: path.into(),
            kind,
            added,
            removed,
            diff: Some("@@ -1 +1 @@".into()),
        }
    }

    /// The letters the digest query greps for. A rename of the kind enum that
    /// silently changed these would make `files_written` count zero while the
    /// table filled up.
    #[test]
    fn ops_are_the_crud_letters_the_digest_query_matches() {
        assert_eq!(ops_letter(FileChangeKind::Created), "C");
        assert_eq!(ops_letter(FileChangeKind::Read), "R");
        assert_eq!(ops_letter(FileChangeKind::Modified), "U");
        assert_eq!(ops_letter(FileChangeKind::Deleted), "D");
    }

    #[test]
    fn a_row_carries_the_line_counts_and_one_audit_entry() {
        let row = file_touch_row(&change("src/lib.rs", FileChangeKind::Modified, 12, 3));
        assert_eq!(row.path, "src/lib.rs");
        assert_eq!(row.ops, "U");
        assert_eq!(row.lines_added, 12);
        assert_eq!(row.lines_removed, 3);

        let events: serde_json::Value = serde_json::from_str(&row.events_json).expect("json");
        let entries = events.as_array().expect("an array");
        assert_eq!(entries.len(), 1, "one adoption is one audit entry");
        assert_eq!(entries[0]["event"], "adopted");
        assert_eq!(entries[0]["lines_added"], 12);
    }

    /// The diff is the event stream's to carry, not the ledger's.
    #[test]
    fn the_row_does_not_carry_the_file_diff() {
        let row = file_touch_row(&change("src/lib.rs", FileChangeKind::Modified, 1, 0));
        assert!(
            !row.events_json.contains("@@"),
            "files_touched indexes an execution's paths; the diff rides the FileChange event"
        );
    }
}
