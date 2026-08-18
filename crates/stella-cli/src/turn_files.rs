// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `AgentEvent::FileChange` for a turn the pipeline never delivered (#3413).
//!
//! # The hole this closes
//!
//! Until this existed, `FileChange` had exactly one producer in the workspace:
//! `Pipeline::deliver_winner`, emitting one event per adopted change when a
//! winning candidate lands in the real tree. Every other turn — plain `stella`
//! chat, `stella run` on the engine path, the deck's lead turn, a fleet
//! worker, a subsession — reported that it had changed nothing, because
//! adoption is the only place anything measured. The TUI Files tab,
//! `stella export` and the audit log were empty for all of them, which reads
//! as "this turn touched no files" rather than "this surface never reports".
//!
//! It was not a regression anyone introduced. The 12-tool purge (#3244)
//! deleted the file-writing built-ins and the file-CRUD ledger that emitted
//! these; the docs simply kept naming a producer that no longer existed.
//!
//! # Why the answer is a measurement and not a tool hook
//!
//! The file built-ins are back — `ToolRegistry::new` registers `write_file`,
//! `edit_file` and `delete_file` alongside `bash` — so the obvious repair,
//! having the tools emit, is now *available*. It is still the wrong answer,
//! for the reason it was always the wrong answer rather than merely an
//! impossible one:
//!
//! - **Inferring from tool inputs is the known defect.** A wrapper that
//!   synthesized these events from the arguments of four hard-coded tool names
//!   is exactly what #2290 was: files edited in bulk, or by a worker lane,
//!   rendered as `+0 -0`. The counts contract exists because of it — `added`
//!   and `removed` come from a real measurement or they are not sent, and a
//!   tool's arguments are a request, not a measurement of what landed.
//! - **A tool hook could never be complete anyway.** `bash` mutates the tree
//!   without naming a path in any schema the engine reads, and so do MCP
//!   servers and custom script tools. A hook on the four file tools would
//!   report a *subset* of the turn's changes while looking exhaustive, which
//!   is worse than the empty ledger this module replaced.
//!
//! So the producer is the work journal's turn-boundary tree snapshot, which
//! measures with git's own `--numstat`. One reading, at the turn boundary,
//! after the model has answered.
//!
//! # What these events do and do not claim
//!
//! **Observability, never evidence** — the same contract `FileChange` has
//! always carried, and it has teeth here. A tree measurement answers *what
//! changed during this turn*, not *what the agent changed*: a user editing a
//! file in another window mid-turn appears in this stream indistinguishable
//! from the agent's own writes. Nothing may found a claim on it, and #2873
//! already removed every decision that read a count of these events. The
//! authority on what the agent did is the pipeline's adoption, which measures
//! a candidate against a sealed baseline and can tell the two apart.
//!
//! It lives beside `agent.rs` rather than inside it because `agent.rs` is a
//! grandfathered god file, closed to growth (AGENTS.md § "God files — plan
//! around them, never into them").

use std::sync::Arc;

use stella_core::EventSender;
use stella_protocol::event::{AgentEvent, FileChangeKind};
use stella_store::work_journal::{JournalChange, JournalChangeKind};
use stella_store::{FileTouchRow, Store};

use crate::config::Config;

/// Measure the shared work tree once, then write both of this turn's file
/// projections from that one reading: the [`AgentEvent::FileChange`] stream
/// and the durable `files_touched` rows.
///
/// Call **before** the turn's event channel closes — these are the turn's
/// events, and a consumer folding the stream must see them inside the turn
/// they describe.
///
/// The measurement is taken exactly once and both projections are derived from
/// it. Snapshotting twice would not merely cost a second `--numstat`: the
/// journal's snapshot advances its own baseline, so the second reading would
/// report an unchanged tree and the two projections would disagree about the
/// same turn.
///
/// Best-effort and silent, like every other turn-boundary write: a turn that
/// ended is not made less ended by an unmeasurable tree. A send failure means
/// the renderer is already gone, which is not this function's problem to
/// report.
pub(crate) fn emit_shared_tree_changes(
    cfg: &Config,
    tx: &EventSender,
    execution: Option<&(Arc<Store>, i64)>,
) {
    let measured = cfg.durability.snapshot_worktree();
    if measured.is_empty() {
        return;
    }
    if let Some((store, execution_id)) = execution {
        let rows: Vec<FileTouchRow> = measured.iter().map(file_touch_row).collect();
        // Best-effort for the same reason `AdoptionLedger::record` is: this is
        // observability, not evidence (#2882). A telemetry write must never
        // fail a turn whose bytes are already on disk.
        let _ = store.record_files_touched(*execution_id, &rows);
    }
    for change in measured {
        let _ = tx.send(file_change(change));
    }
}

/// One measured delta as a durable row.
///
/// The durable half of #3413. That change gave the engine path a `FileChange`
/// producer but no `files_touched` producer, leaving adoption as the table's
/// only writer — so every direct-edit turn recorded zero touched files however
/// many it wrote. `Store::finalize_execution_reflection` reads exactly this
/// table for its `wrote_files` flag, so the empty table also told the
/// reflection loop that a turn which edited dozens of files had written none.
fn file_touch_row(change: &JournalChange) -> FileTouchRow {
    FileTouchRow {
        path: change.path.clone(),
        ops: ops_letter(change.kind).to_string(),
        lines_added: u64::from(change.added),
        lines_removed: u64::from(change.removed),
        events_json: serde_json::json!([{
            "event": "measured",
            "reason": "turn-boundary work-tree measurement",
            "lines_added": change.added,
            "lines_removed": change.removed,
        }])
        .to_string(),
    }
}

/// The `ops` alphabet is CRUD letters, fixed by the reader rather than by
/// preference — `Store::execution_rollup` and `finalize_execution_reflection`
/// both match `ops LIKE '%C%' OR '%U%' OR '%D%'`, so a row spelled any other
/// way is a row those queries cannot see. Same alphabet as
/// [`crate::candidate_ws::adoption_ledger`], deliberately.
fn ops_letter(kind: JournalChangeKind) -> &'static str {
    match kind {
        JournalChangeKind::Created => "C",
        JournalChangeKind::Modified => "U",
        JournalChangeKind::Deleted => "D",
    }
}

/// Map one measured delta onto the wire event.
///
/// [`FileChangeKind::Read`] is deliberately unreachable here: a tree snapshot
/// cannot observe a read, and the variant is replay-only (see its doc).
fn file_change(change: JournalChange) -> AgentEvent {
    AgentEvent::FileChange {
        path: change.path,
        kind: match change.kind {
            JournalChangeKind::Created => FileChangeKind::Created,
            JournalChangeKind::Modified => FileChangeKind::Modified,
            JournalChangeKind::Deleted => FileChangeKind::Deleted,
        },
        added: change.added,
        removed: change.removed,
        diff: change.diff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured(kind: JournalChangeKind) -> JournalChange {
        JournalChange {
            path: "src/lib.rs".into(),
            kind,
            added: 12,
            removed: 3,
            diff: Some("@@ -1 +1,2 @@\n+new\n".into()),
        }
    }

    /// The letters the two readers grep for. `execution_rollup`'s
    /// `files_written` and `finalize_execution_reflection`'s `wrote_files`
    /// both match `ops LIKE '%C%' OR '%U%' OR '%D%'`, so a row spelled any
    /// other way fills the table while both queries answer zero.
    #[test]
    fn durable_rows_use_the_crud_letters_both_reader_queries_match() {
        for (kind, expected) in [
            (JournalChangeKind::Created, "C"),
            (JournalChangeKind::Modified, "U"),
            (JournalChangeKind::Deleted, "D"),
        ] {
            assert_eq!(ops_letter(kind), expected);
        }
    }

    /// The durable projection carries the same measurement as the stream one,
    /// from the same reading. Before this existed, a direct-edit turn emitted
    /// `FileChange` events and wrote no `files_touched` row at all.
    #[test]
    fn a_measured_change_becomes_a_durable_row_carrying_the_same_counts() {
        let row = file_touch_row(&measured(JournalChangeKind::Modified));
        assert_eq!(row.path, "src/lib.rs");
        assert_eq!(row.ops, "U");
        assert_eq!((row.lines_added, row.lines_removed), (12, 3));

        let events: serde_json::Value = serde_json::from_str(&row.events_json).expect("json");
        let entries = events.as_array().expect("an array");
        assert_eq!(entries.len(), 1, "one measurement is one audit entry");
        assert_eq!(entries[0]["event"], "measured");
        assert_eq!(entries[0]["lines_added"], 12);
        assert!(
            !row.events_json.contains("@@"),
            "files_touched indexes paths; the diff rides the FileChange event"
        );
    }

    #[test]
    fn every_measured_kind_maps_onto_a_mutating_wire_kind() {
        for (measured_kind, expected) in [
            (JournalChangeKind::Created, FileChangeKind::Created),
            (JournalChangeKind::Modified, FileChangeKind::Modified),
            (JournalChangeKind::Deleted, FileChangeKind::Deleted),
        ] {
            let AgentEvent::FileChange { kind, .. } = file_change(measured(measured_kind)) else {
                panic!("mapped to the wrong event");
            };
            assert_eq!(kind, expected);
            assert!(
                kind.is_mutation(),
                "a tree snapshot can only ever observe a mutation"
            );
        }
    }

    #[test]
    fn the_measured_counts_reach_the_wire_unchanged() {
        // The #2290 contract at the seam it would break: whatever git measured
        // is what ships, never a recount of the diff text.
        let AgentEvent::FileChange {
            added,
            removed,
            diff,
            path,
            ..
        } = file_change(measured(JournalChangeKind::Modified))
        else {
            panic!("mapped to the wrong event");
        };
        assert_eq!((added, removed), (12, 3));
        assert_eq!(path, "src/lib.rs");
        assert!(
            diff.is_some_and(|d| !d.contains("12")),
            "the counts are carried, not re-derivable from the rendering"
        );
    }
}
