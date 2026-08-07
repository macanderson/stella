// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Per-turn workspace diffs, precomputed at turn end (#1870).
//!
//! # The boundary ruling, on the record (the #615 pattern)
//!
//! The observatory reads `.stella`-rooted artifacts and spawns nothing — that
//! posture is load-bearing, not incidental. Serving real per-turn diffs from
//! the work journal (`~/.stella/work/<workspace-id>.git`) therefore cannot
//! mean the dashboard reading the bare repo: not by subprocess (`git` would be
//! a whole new execution surface on a loopback server), and not by linking an
//! object reader (gitoxide is heavy; a hand-rolled loose-object+pack reader is
//! its own project). Instead **the session that owns the journal precomputes
//! the diff at the moment it marks the turn** — one tree-to-tree read in the
//! process already shelling to git on every step — shapes the hunks with
//! `stella-diff`, and persists them in `store.db`'s `session_turn_diffs`
//! projection, which the observatory already reads read-only. The dashboard
//! stays a pure artifact reader; the cost is one bounded diff per turn, off
//! the model's critical path.
//!
//! Best-effort like the turn mark itself: a turn that ended is not made less
//! ended by an unrecordable diff, so every failure here degrades to "that
//! turn has no replayable diff" rather than surfacing.

use stella_store::{Store, work_journal::WorkJournal};

/// Files carried per turn. A turn that rewrites more than this many files
/// still records the first `MAX_DIFF_FILES` (deterministically — the paths
/// arrive in git's sorted tree order) and says the listing was truncated,
/// instead of writing an unbounded row on every turn end.
const MAX_DIFF_FILES: usize = 50;

/// Per-side byte ceiling above which a file's hunks are not computed. The
/// diff is O(old × new) in the worst case and the row is re-read by a
/// dashboard poll; a generated megabyte artifact gets a named entry with
/// `skipped: true` rather than a megabyte of hunks.
const MAX_FILE_BYTES: usize = 512 * 1024;

/// Context lines per hunk — the same width `stella inspect --diff` and the
/// observatory's context-diff route use.
const DIFF_CONTEXT: usize = 3;

/// Compute and persist the diff for `turn`, best-effort.
///
/// `execution_id` is the execution the turn ran as (the store's turn
/// identity) — the join key the Sessions view needs, because the journal's
/// turn ordinal has no other persisted correspondence to `executions.id`.
pub(crate) fn record_turn_diff(
    journal: &WorkJournal,
    store: &Store,
    session_id: &str,
    execution_id: Option<i64>,
    turn: u32,
) {
    let Ok(paths) = journal.changed_paths_at_turn(turn) else {
        return;
    };
    if paths.is_empty() {
        // A turn that touched nothing records nothing: "no row" and "no
        // changes" read the same on the dashboard, and writing empty rows
        // would grow the table on every conversational turn.
        return;
    }
    let files_truncated = paths.len() > MAX_DIFF_FILES;
    let files: Vec<serde_json::Value> = paths
        .iter()
        .take(MAX_DIFF_FILES)
        .map(|path| file_entry(journal, turn, path))
        .collect();
    let payload = serde_json::json!({
        "files": files,
        "files_truncated": files_truncated,
    });
    let _ = store.record_session_turn_diff(session_id, turn, execution_id, &payload.to_string());
}

/// One changed file's entry: `stella-diff` hunks between the two sides of the
/// turn boundary, in the exact JSON shape the observatory's context-diff
/// route already serves — so the dashboard renders both with one renderer.
fn file_entry(journal: &WorkJournal, turn: u32, path: &str) -> serde_json::Value {
    // A side that does not exist — the file was created this turn, deleted
    // this turn, or there is no previous mark at all — is empty, which is
    // exactly what a diff against nothing means.
    let old = turn
        .checked_sub(1)
        .filter(|&prev| prev > 0)
        .and_then(|prev| journal.read_at_turn(prev, path).ok())
        .unwrap_or_default();
    let new = journal.read_at_turn(turn, path).unwrap_or_default();
    if old.len() > MAX_FILE_BYTES || new.len() > MAX_FILE_BYTES {
        return serde_json::json!({
            "path": path,
            "added": 0,
            "removed": 0,
            "hunks": [],
            "skipped": true,
        });
    }
    let diff = stella_diff::unified_diff(&old, &new, DIFF_CONTEXT);
    serde_json::json!({
        "path": path,
        "added": diff.added,
        "removed": diff.removed,
        "hunks": diff.hunks.iter().map(|h| serde_json::json!({
            "old_start": h.old_start,
            "old_count": h.old_count,
            "new_start": h.new_start,
            "new_count": h.new_count,
            "lines": h.lines.iter().map(|l| serde_json::json!({
                "op": l.op.tag(),
                "text": l.text,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "skipped": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The #1870 witness, end to end on the CLI side: a work journal with two
    /// turn refs where one file changed between them produces a persisted
    /// diff whose hunks name the changed lines. Fails on main — nothing
    /// computes or persists a turn diff there.
    #[test]
    fn a_marked_turn_persists_hunks_naming_the_changed_lines() {
        let guard = tempfile::tempdir().unwrap();
        let ws = guard.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let journal_root = guard.path().join("work");
        let journal = WorkJournal::open_in(&journal_root, &ws, "ses-witness").unwrap();
        let store = Store::open(&ws).unwrap();

        std::fs::write(ws.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let c1 = journal.record(&["a.txt".into()], &[], "turn 1").unwrap();
        journal.mark_turn(1, &c1).unwrap();

        std::fs::write(ws.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        let c2 = journal.record(&["a.txt".into()], &[], "turn 2").unwrap();
        journal.mark_turn(2, &c2).unwrap();

        record_turn_diff(&journal, &store, "ses-witness", Some(41), 2);

        let row = store
            .session_turn_diff("ses-witness", 2)
            .expect("read")
            .expect("the turn recorded a diff");
        assert_eq!(row.execution_id, Some(41));
        let payload: serde_json::Value = serde_json::from_str(&row.files).expect("json");
        assert_eq!(payload["files"][0]["path"], "a.txt", "{payload}");
        assert_eq!(payload["files_truncated"], false, "{payload}");
        let lines = &payload["files"][0]["hunks"][0]["lines"];
        let removed: Vec<&str> = lines
            .as_array()
            .expect("lines")
            .iter()
            .filter(|l| l["op"] == "remove")
            .filter_map(|l| l["text"].as_str())
            .collect();
        let added: Vec<&str> = lines
            .as_array()
            .expect("lines")
            .iter()
            .filter(|l| l["op"] == "add")
            .filter_map(|l| l["text"].as_str())
            .collect();
        assert_eq!(removed, vec!["two"], "the old line is named: {payload}");
        assert_eq!(added, vec!["TWO"], "and the new one: {payload}");
    }

    /// A turn that touched nothing writes nothing — absence is the honest
    /// record, and the table must not grow on conversational turns.
    #[test]
    fn a_turn_with_no_changes_records_no_row() {
        let guard = tempfile::tempdir().unwrap();
        let ws = guard.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let journal = WorkJournal::open_in(&guard.path().join("work"), &ws, "ses-idle").unwrap();
        let store = Store::open(&ws).unwrap();

        std::fs::write(ws.join("a.txt"), "same\n").unwrap();
        let c1 = journal.record(&["a.txt".into()], &[], "turn 1").unwrap();
        journal.mark_turn(1, &c1).unwrap();
        // Turn 2 commits nothing new; its mark names the same tree.
        journal.mark_turn(2, &c1).unwrap();

        record_turn_diff(&journal, &store, "ses-idle", None, 2);
        assert_eq!(store.session_turn_diff("ses-idle", 2).unwrap(), None);
    }

    /// An oversized side degrades to a named, skipped entry — the row stays
    /// bounded and the file is still listed as changed.
    #[test]
    fn an_oversized_file_is_listed_but_skipped() {
        let guard = tempfile::tempdir().unwrap();
        let ws = guard.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let journal = WorkJournal::open_in(&guard.path().join("work"), &ws, "ses-big").unwrap();
        let store = Store::open(&ws).unwrap();

        std::fs::write(ws.join("big.txt"), "x".repeat(MAX_FILE_BYTES + 1)).unwrap();
        let c1 = journal.record(&["big.txt".into()], &[], "turn 1").unwrap();
        journal.mark_turn(1, &c1).unwrap();

        record_turn_diff(&journal, &store, "ses-big", None, 1);
        let row = store
            .session_turn_diff("ses-big", 1)
            .expect("read")
            .expect("recorded");
        let payload: serde_json::Value = serde_json::from_str(&row.files).expect("json");
        assert_eq!(payload["files"][0]["path"], "big.txt", "{payload}");
        assert_eq!(payload["files"][0]["skipped"], true, "{payload}");
        assert_eq!(payload["files"][0]["hunks"], serde_json::json!([]));
    }
}
