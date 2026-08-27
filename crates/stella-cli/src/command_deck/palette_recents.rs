// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The command palette's `recent` section, on disk — SPEC 10, #5048.
//!
//! "The commands you ran in this workspace last week" is in no session's event
//! log, so it has to be written down — and the deck is the wrong side to write
//! it, because `stella-tui` owns no store and must not learn where a workspace
//! keeps its files. The driver keeps the list and pushes it in, the same route
//! the command vocabulary already travels ([`Inbound::SlashCommands`]).
//!
//! The store is no new one: [`stella_store::workspace_private_state_path`],
//! the owner-only `<workspace>/.stella/private/` tier that already holds
//! `reflections.jsonl`, the self-tuning ledger and `mcp_oauth.json`. That tier
//! is created 0700 and validated no-follow, is gitignored by construction, and
//! follows `STELLA_WORKSPACE_STATE_ROOT` — so a turn in a throwaway worktree
//! writes to the real repository rather than into a directory built to be
//! deleted (#4394). ADR 0019 records why not the session registry or a new one.
//!
//! Every operation here is best-effort and silent, because the whole feature
//! is the *order of a menu*: a read that fails costs a shortcut and a write
//! that fails costs one next time. A workspace mounted read-only must still
//! open its palette.

use std::path::Path;

use stella_tui::{Inbound, WorkspaceInput};
use tokio::sync::mpsc::UnboundedSender;

/// The file, under the workspace's private state directory.
///
/// JSON rather than one-name-per-line because the list is *replaced* on every
/// write, never appended to — move-to-front is the whole semantic — so the
/// append-friendly `.jsonl` shape its neighbours use would be a lie about how
/// this file is maintained.
pub(super) const FILE: &str = "palette-recents.json";

/// What this workspace has run, newest first, or empty when there is nothing
/// to say (no file yet, an unreadable one, or a corrupt one).
///
/// Anything unparseable is treated as absent rather than repaired: the next
/// [`record`] rewrites the file whole, so a bad one heals on its own the
/// first time a command runs.
pub(super) fn load(workspace_root: &Path) -> Vec<String> {
    let Ok(Some(path)) = stella_store::existing_workspace_private_state_path(workspace_root, FILE)
    else {
        return Vec::new();
    };
    let Ok(raw) = stella_store::read_sensitive_file_to_string(&path) else {
        return Vec::new();
    };
    let mut names: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
    // The cap belongs to the palette, and a file written by an older build
    // (or edited by hand) must not be able to push the domain groups off the
    // popup.
    names.truncate(stella_tui::PALETTE_RECENT_LIMIT);
    names
}

/// Record `name` as the most recently run command and return the new list.
///
/// The move-to-front rule itself is [`stella_tui::palette_remember`], shared
/// with the deck rather than restated here: the deck applies it optimistically
/// on the keystroke so the section reorders at once, and this applies it to
/// the durable copy. One rule, so the two cannot disagree about what "recent"
/// means.
pub(super) fn record(workspace_root: &Path, name: &str) -> Vec<String> {
    let mut names = load(workspace_root);
    stella_tui::palette_remember(&mut names, name);
    if let Ok(path) = stella_store::workspace_private_state_path(workspace_root, FILE)
        && let Ok(json) = serde_json::to_vec(&names)
    {
        // Best-effort by contract — see the module doc.
        let _ = stella_store::write_sensitive_file_atomic(&path, &json);
    }
    names
}

/// The deck's `recent` section as an [`Inbound`], for the driver's startup
/// seed and after every recorded run.
pub(super) fn inbound(names: Vec<String>) -> Inbound {
    Inbound::PaletteRecents(names)
}

/// Service a [`WorkspaceInput::PaletteRan`] from the deck: record it and push
/// the refreshed list back. Returns `true` when `input` was one, so a caller
/// can chain this beside the other `service_*` helpers.
///
/// Serviced identically idle or mid-turn, like the session-registry verbs it
/// sits beside: it is one small local file write, and a user who ran a
/// command while an agent worked should not have to wait for the turn to end
/// before their palette remembers it.
pub(super) fn service(
    input: &WorkspaceInput,
    workspace_root: &Path,
    in_tx: &UnboundedSender<Inbound>,
) -> bool {
    let WorkspaceInput::PaletteRan { name } = input else {
        return false;
    };
    let _ = in_tx.send(inbound(record(workspace_root, name)));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace with no history yet answers with an empty list rather than
    /// creating anything — opening a palette must not scaffold a file.
    #[test]
    fn an_untouched_workspace_has_no_history_and_is_not_written_to() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load(dir.path()).is_empty());
        assert!(
            !dir.path()
                .join(".stella")
                .join("private")
                .join(FILE)
                .exists(),
            "a read created the file"
        );
    }

    /// **The across-sessions witness (#5048).** What one session records, the
    /// next session reads back — newest first, deduplicated, capped.
    ///
    /// `load` and `record` are the whole persistence surface, so two calls
    /// with no shared state between them *is* the second session.
    #[test]
    fn what_one_session_records_the_next_one_reads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["/plan", "/diff", "/plan"] {
            record(dir.path(), name);
        }
        assert_eq!(
            load(dir.path()),
            vec!["/plan".to_string(), "/diff".to_string()],
            "the re-run command moved to the front instead of duplicating"
        );

        // Past the cap the oldest falls off, and the file never grows.
        for i in 0..stella_tui::PALETTE_RECENT_LIMIT {
            record(dir.path(), &format!("/c{i}"));
        }
        let names = load(dir.path());
        assert_eq!(names.len(), stella_tui::PALETTE_RECENT_LIMIT);
        assert!(!names.contains(&"/diff".to_string()), "{names:?}");
    }

    /// The history lands in the owner-only `private/` tier — gitignored by
    /// construction — rather than loose in `.stella/`, where a `git add -A`
    /// could sweep it into the user's repository.
    #[test]
    fn the_history_lives_in_the_private_tier() {
        let dir = tempfile::tempdir().expect("tempdir");
        record(dir.path(), "/plan");
        assert!(
            dir.path()
                .join(".stella")
                .join("private")
                .join(FILE)
                .exists(),
            "not written where it was expected"
        );
        assert!(
            !dir.path().join(".stella").join(FILE).exists(),
            "written loose in .stella, outside the private tier"
        );
    }

    /// A corrupt file is absent, not fatal — and the next run heals it.
    #[test]
    fn a_corrupt_history_reads_as_empty_and_heals_on_the_next_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = stella_store::workspace_private_state_path(dir.path(), FILE).expect("path");
        std::fs::write(&path, b"{ not json").expect("write");
        assert!(load(dir.path()).is_empty());
        assert_eq!(record(dir.path(), "/plan"), vec!["/plan".to_string()]);
        assert_eq!(load(dir.path()), vec!["/plan".to_string()]);
    }

    /// A file left behind by a build with a bigger cap is trimmed on read, so
    /// the popup's shape is decided by this build and not by history.
    #[test]
    fn an_over_long_history_is_trimmed_on_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = stella_store::workspace_private_state_path(dir.path(), FILE).expect("path");
        let names: Vec<String> = (0..40).map(|i| format!("/c{i}")).collect();
        std::fs::write(&path, serde_json::to_vec(&names).expect("json")).expect("write");
        assert_eq!(load(dir.path()).len(), stella_tui::PALETTE_RECENT_LIMIT);
    }

    /// Anything that is not a `PaletteRan` is left for the next handler in
    /// the chain.
    #[test]
    fn service_claims_only_its_own_input() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(!service(&WorkspaceInput::QueueClear, dir.path(), &tx));
        assert!(rx.try_recv().is_err(), "nothing sent for a foreign input");

        assert!(service(
            &WorkspaceInput::PaletteRan {
                name: "/plan".into()
            },
            dir.path(),
            &tx
        ));
        match rx.try_recv() {
            Ok(Inbound::PaletteRecents(names)) => assert_eq!(names, vec!["/plan".to_string()]),
            other => panic!("expected the refreshed list, got {other:?}"),
        }
    }
}
