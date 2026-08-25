// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `--output-format json` envelope for a raw (`--no-pipeline`) run.
//!
//! Split out of `agent.rs` when that file was a grandfathered god file closed
//! to growth (AGENTS.md § "God files — plan around them, never into them"). It
//! has since fallen back under the ratchet and its baseline entry was removed
//! with #3852, so the split is no longer forced — it is kept because the
//! envelope's folds belong together.
//!
//! Its `files_touched` key used to be a hardcoded empty list, with a comment
//! saying why: "the CLI keeps no per-file recorder". That stopped being true
//! when [`crate::turn_files`] gave the engine path a measured producer
//! (#3413), so the envelope is now filled from the turn's own stream rather
//! than declared empty.
//!
//! [`withheld`] is folded the same way, and for a stronger reason: #3616 put
//! the withheld-steering counts on `stream-json` as an event and left the
//! `json` summary with no answer at all (#4465). Reading them back off this
//! turn's own journal rather than re-surveying the workspace is what makes the
//! two machine channels incapable of disagreeing — they are one event, read
//! twice. `crates/stella-cli/src/settings/withheld.rs` states the same rule
//! for its own two carriers.

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
        withheld: withheld(&events),
        events,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&summary)
            .unwrap_or_else(|e| format!("{{\"status\":\"error\",\"reason\":\"serialize: {e}\"}}"))
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

/// What this checkout's trust gate held back, or `null` when it held nothing.
///
/// Read off the turn's own `SteeringWithheld` event — the one
/// `crates/stella-cli/src/agent/output.rs` opens every raw run with — rather
/// than re-surveying the workspace. The counts are already resolved by then,
/// and a second survey could report a number the session did not run under.
///
/// `null` covers both silent arms and they are not the same fact: a trusted
/// checkout loaded its steering, and an untrusted one with nothing on disk had
/// nothing to lose. Neither is a suppression, and the envelope does not invent
/// a distinction the event itself declines to carry.
///
/// Counts and the authority, never a path, a filename or a body — the rule the
/// stderr line and the event are both held to, for the reason
/// `settings::withheld`'s module doc gives: a refusal that echoed
/// repository-controlled strings would be the exfiltration channel it exists
/// to prevent.
fn withheld(events: &[AgentEvent]) -> serde_json::Value {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::SteeringWithheld {
                withheld_by,
                memories,
                records,
                skills,
                commands,
                agents,
            } => Some(serde_json::json!({
                "withheld_by": withheld_by,
                "memories": memories,
                "records": records,
                "skills": skills,
                "commands": commands,
                "agents": agents,
            })),
            _ => None,
        })
        .unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use stella_protocol::Withholder;
    use stella_protocol::event::FileChangeKind;

    use super::*;

    fn change(path: &str, kind: FileChangeKind, added: u32, removed: u32) -> AgentEvent {
        AgentEvent::FileChange {
            path: path.into(),
            kind,
            added,
            removed,
            diff: None,
            minimal: true,
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

    /// **Witness (#4465).** The `json` summary answers what `stream-json` has
    /// answered since #3616: a harness reading the summary alone can tell an
    /// untrusted checkout from a trusted one. Before this the key did not
    /// exist and the only machine answer was the event stream.
    #[test]
    fn the_envelope_reports_what_the_trust_gate_withheld() {
        let value = withheld(&[AgentEvent::SteeringWithheld {
            withheld_by: Withholder::ManagedCeiling,
            memories: 3,
            records: 2,
            skills: 1,
            commands: 0,
            agents: 0,
        }]);
        assert_eq!(value["withheld_by"], "managed_ceiling");
        assert_eq!(value["memories"], 3);
        assert_eq!(value["records"], 2);
        assert_eq!(value["skills"], 1);
        assert_eq!(value["commands"], 0);
        assert_eq!(value["agents"], 0);
    }

    /// A run whose steering was loaded — or which had none to lose — reports
    /// `null`, not a zeroed object: an object would read as a suppression that
    /// cost nothing, which is a different claim from "nothing was suppressed".
    #[test]
    fn a_run_with_nothing_withheld_reports_null() {
        assert!(withheld(&[]).is_null());
    }

    #[test]
    fn a_turn_with_no_file_changes_still_carries_the_key() {
        // The envelope's key set is the versioned contract: a quiet turn
        // reports an empty list, never a missing key.
        let rows = files_touched(&[]);
        assert!(rows["files_touched"].as_array().unwrap().is_empty());
    }
}
