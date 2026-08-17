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
//! The obvious repair — have the tools emit — is unavailable, and would be
//! wrong if it were available:
//!
//! - **Nothing left names a path.** `ToolRegistry::new` registers the
//!   task-board, scratch-state, sub-agent and `get_environment` tools. A turn
//!   that mutates the tree does it through an MCP server or a custom script
//!   tool, and neither describes the paths it writes in any schema the engine
//!   reads.
//! - **Inferring from tool inputs is the known defect.** A wrapper that
//!   synthesized these events from the arguments of four hard-coded tool names
//!   is exactly what #2290 was: files edited in bulk, or by a worker lane,
//!   rendered as `+0 -0`. The counts contract exists because of it — `added`
//!   and `removed` come from a real measurement or they are not sent.
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

use stella_core::EventSender;
use stella_protocol::event::{AgentEvent, FileChangeKind};
use stella_store::work_journal::{JournalChange, JournalChangeKind};

use crate::config::Config;

/// Measure the shared work tree and emit one [`AgentEvent::FileChange`] per
/// path this turn changed.
///
/// Call **before** the turn's event channel closes — these are the turn's
/// events, and a consumer folding the stream must see them inside the turn
/// they describe.
///
/// Best-effort and silent, like every other turn-boundary write: a turn that
/// ended is not made less ended by an unmeasurable tree. A send failure means
/// the renderer is already gone, which is not this function's problem to
/// report.
pub(crate) fn emit_shared_tree_changes(cfg: &Config, tx: &EventSender) {
    for change in cfg.durability.snapshot_worktree() {
        let _ = tx.send(file_change(change));
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
