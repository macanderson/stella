// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `--output-format json` envelope for a raw (`--no-pipeline`) run.
//!
//! Split out of `agent.rs`, which is a grandfathered god file closed to growth
//! (AGENTS.md § "God files — plan around them, never into them").
//!
//! Its `files_touched` key used to be a hardcoded empty list, with a comment
//! saying why: "the CLI keeps no per-file recorder". That stopped being true
//! when [`crate::turn_files`] gave the engine path a measured producer
//! (#3413), so the envelope is now filled from the turn's own stream rather
//! than declared empty.

use stella_protocol::event::AgentEvent;

use super::RawRunSummary;
use crate::config::Config;
use stella_core::TurnOutcome;

/// Print the one-object JSON summary for a finished raw turn and record that
/// the machine-readable contract has been satisfied, so `main`'s catch-all
/// does not follow a returned `Err` with a second envelope for the same
/// failure.
///
/// `events` is the turn's full journal — the same objects `stream-json` would
/// have emitted line by line — and is consumed rather than borrowed because it
/// is both the envelope's `events` array and the source of `files_touched`.
pub(super) fn print_json_summary(cfg: &Config, outcome: &TurnOutcome, events: Vec<AgentEvent>) {
    let (status, text, cost_usd, reason) = match outcome {
        TurnOutcome::Completed { text, cost_usd } => {
            ("completed", Some(text.clone()), Some(*cost_usd), None)
        }
        TurnOutcome::Aborted {
            reason, cost_usd, ..
        } => ("aborted", None, Some(*cost_usd), Some(reason.clone())),
    };
    let summary = RawRunSummary {
        schema_version: crate::SUMMARY_SCHEMA_VERSION,
        status,
        text,
        cost_usd,
        reason,
        model: format!("{}/{}", cfg.provider.id, cfg.model_id),
        files_touched: files_touched(&events),
        events,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_else(|e| format!(
            "{{\"status\":\"error\",\"reason\":\"serialize: {e}\"}}"
        ))
    );
    crate::note_json_summary_emitted();
}

/// The file-touch envelope, folded from this turn's `FileChange` events.
///
/// One row per path, keyed the way `store.db`'s `files_touched` table spells
/// it so a reader of both sees one vocabulary. Counts are summed from the
/// events' own measured deltas and never re-derived from `diff` (#2290); reads
/// carry no delta and are folded in for their `ops` alone.
fn files_touched(events: &[AgentEvent]) -> serde_json::Value {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut seen: Vec<(String, u64, u64, Vec<String>)> = Vec::new();
    for event in events {
        let AgentEvent::FileChange {
            path,
            kind,
            added,
            removed,
            ..
        } = event
        else {
            continue;
        };
        let op = format!("{kind:?}").to_lowercase();
        match seen.iter_mut().find(|(seen, ..)| seen == path) {
            Some((_, a, r, ops)) => {
                *a += u64::from(*added);
                *r += u64::from(*removed);
                if !ops.contains(&op) {
                    ops.push(op);
                }
            }
            None => seen.push((
                path.clone(),
                u64::from(*added),
                u64::from(*removed),
                vec![op],
            )),
        }
    }
    for (path, added, removed, ops) in seen {
        rows.push(serde_json::json!({
            "path": path,
            "ops": ops.join(","),
            "lines_added": added,
            "lines_removed": removed,
        }));
    }
    serde_json::json!({ "files_touched": rows })
}

#[cfg(test)]
mod tests {
    use stella_protocol::event::FileChangeKind;

    use super::*;

    fn change(path: &str, kind: FileChangeKind, added: u32, removed: u32) -> AgentEvent {
        AgentEvent::FileChange {
            path: path.into(),
            kind,
            added,
            removed,
            diff: None,
        }
    }

    #[test]
    fn the_envelope_carries_the_turns_measured_changes_not_an_empty_list() {
        // The witness for the half of #3413 the JSON surface showed: this key
        // was a hardcoded `[]` for every raw run, however much the turn wrote.
        let rows = files_touched(&[
            change("a.rs", FileChangeKind::Modified, 12, 3),
            change("b.rs", FileChangeKind::Created, 5, 0),
        ]);
        let rows = rows["files_touched"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["path"], "a.rs");
        assert_eq!(rows[0]["lines_added"], 12);
        assert_eq!(rows[0]["lines_removed"], 3);
        assert_eq!(rows[0]["ops"], "modified");
    }

    #[test]
    fn a_path_touched_twice_folds_into_one_row_with_summed_counts() {
        let rows = files_touched(&[
            change("a.rs", FileChangeKind::Created, 5, 0),
            change("a.rs", FileChangeKind::Modified, 2, 1),
        ]);
        let rows = rows["files_touched"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "one row per path, not per event");
        assert_eq!(rows[0]["lines_added"], 7);
        assert_eq!(rows[0]["lines_removed"], 1);
        assert_eq!(rows[0]["ops"], "created,modified");
    }

    #[test]
    fn a_turn_with_no_file_changes_still_carries_the_key() {
        // The envelope's key set is the versioned contract: a quiet turn
        // reports an empty list, never a missing key.
        let rows = files_touched(&[]);
        assert!(rows["files_touched"].as_array().unwrap().is_empty());
    }
}
