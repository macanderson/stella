// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One line at deck start when another live session already holds this tree.
//!
//! Two sessions in one working tree are one branch switch away from losing
//! each other's work. The switch puts the tracked files back and leaves the
//! untracked ones alone. What is left is a tree holding a new module that
//! nothing declares, and no error anywhere saying why.
//!
//! Stella's own dispatchers do not make that state. A fleet task gets a
//! `git worktree` of its own unless the plan named the shared tree
//! (ADR 0027), and so does every unit of `stella self-driving work`. A
//! person, or an outside harness, can still start two decks in one tree.
//! This is what that person sees.
//!
//! It never refuses. A session told to stop has nowhere to go, because the
//! working directory is picked at launch and cannot be taken later. So the
//! line names the peer and the branch, and the reader decides.
//!
//! The peers come from the machine-wide session registry, which already
//! holds each session's pid and tree and already reads a dead pid as a
//! crash. A second file with the same three facts would be a second answer
//! to one question.

use std::path::Path;
use std::process::Command;

use stella_store::{SessionRecord, SessionRegistry};

/// A live session that already holds this checkout.
pub(super) struct Peer {
    /// The registry id, so the reader can find it in the SESSIONS view.
    id: String,
    /// The process that owns it.
    pid: u32,
}

/// Every live session in `record`'s tree other than `record` itself.
pub(super) fn peers(records: &[SessionRecord], record: &SessionRecord) -> Vec<Peer> {
    records
        .iter()
        .filter(|other| {
            other.id != record.id
                && other.workspace == record.workspace
                && SessionRegistry::presented_status(other).is_live()
        })
        .map(|other| Peer {
            id: other.id.clone(),
            pid: other.pid,
        })
        .collect()
}

/// The line to print, or `None` when this session has the tree to itself.
///
/// One peer is named in full and the rest are counted. One name is enough to
/// find the others in the SESSIONS view, and naming them all would push the
/// warning off the end of the line.
pub(super) fn notice(peers: &[Peer], branch: Option<&str>) -> Option<String> {
    let first = peers.first()?;
    let rest = match peers.len() {
        1 => String::new(),
        n => format!(", and {} more", n - 1),
    };
    let on_branch = match branch {
        Some(name) => format!(" on branch {name}"),
        None => String::new(),
    };
    Some(format!(
        "another stella session is already running in this checkout{on_branch}: \
         {} (pid {}){rest}. If either of you switches branch, the other loses its \
         uncommitted work — give each session its own `git worktree`.",
        first.id, first.pid
    ))
}

/// The branch this checkout has open, when it has one.
///
/// `None` for a detached head, and for anything that stops git answering —
/// a tree that is not a repository at all, or no `git` on the path.
pub(super) fn current_branch(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() || name == "HEAD" {
        None
    } else {
        Some(name)
    }
}

/// Print the line, when there is one.
///
/// Best effort from end to end: a registry that cannot be read and a git that
/// cannot answer both leave the session alone.
pub(super) fn announce(registry: &SessionRegistry, record: &SessionRecord, root: &Path) {
    let peers = peers(&registry.list(), record);
    if let Some(line) = notice(&peers, current_branch(root).as_deref()) {
        eprintln!("  ! {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_store::SessionStatus;

    /// A pid this process can prove is dead. It does not fit `pid_t`, which
    /// `stella_store::pid_alive` reads as dead rather than wrapping it
    /// negative.
    const DEAD_PID: u32 = u32::MAX;

    fn record(id: &str, pid: u32, workspace: &str, status: SessionStatus) -> SessionRecord {
        let mut r = SessionRecord::new(workspace.to_string(), "fixture".to_string());
        r.id = id.to_string();
        r.pid = pid;
        r.status = status;
        r
    }

    fn live(id: &str, workspace: &str) -> SessionRecord {
        record(id, std::process::id(), workspace, SessionStatus::InProgress)
    }

    #[test]
    fn a_session_alone_in_its_tree_gets_no_line() {
        let mine = live("ses-1", "/w/one");
        let found = peers(std::slice::from_ref(&mine), &mine);
        assert!(found.is_empty());
        assert!(notice(&found, Some("main")).is_none());
    }

    #[test]
    fn a_live_peer_in_the_same_tree_is_named_with_the_branch() {
        let mine = live("ses-1", "/w/one");
        let theirs = live("ses-2", "/w/one");
        let found = peers(&[mine.clone(), theirs], &mine);
        assert_eq!(found.len(), 1);
        let line = notice(&found, Some("fix/thing")).expect("a peer means a line");
        assert!(line.contains("ses-2"), "{line}");
        assert!(line.contains("on branch fix/thing"), "{line}");
        assert!(!line.contains("and 0 more"), "{line}");
    }

    #[test]
    fn a_session_in_another_tree_is_not_a_peer() {
        let mine = live("ses-1", "/w/one");
        let elsewhere = live("ses-2", "/w/two");
        assert!(peers(&[mine.clone(), elsewhere], &mine).is_empty());
    }

    #[test]
    fn a_crashed_peer_is_not_named() {
        let mine = live("ses-1", "/w/one");
        let gone = record("ses-2", DEAD_PID, "/w/one", SessionStatus::InProgress);
        assert!(peers(&[mine.clone(), gone], &mine).is_empty());
    }

    #[test]
    fn a_finished_peer_is_not_named() {
        let mine = live("ses-1", "/w/one");
        let done = record(
            "ses-2",
            std::process::id(),
            "/w/one",
            SessionStatus::Complete,
        );
        assert!(peers(&[mine.clone(), done], &mine).is_empty());
    }

    #[test]
    fn several_peers_name_one_and_count_the_rest() {
        let mine = live("ses-1", "/w/one");
        let all = vec![
            mine.clone(),
            live("ses-2", "/w/one"),
            live("ses-3", "/w/one"),
            live("ses-4", "/w/one"),
        ];
        let found = peers(&all, &mine);
        assert_eq!(found.len(), 3);
        let line = notice(&found, None).expect("three peers mean a line");
        assert!(line.contains("and 2 more"), "{line}");
        assert!(!line.contains("on branch"), "{line}");
    }

    #[test]
    fn a_directory_that_is_not_a_repository_has_no_branch() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(current_branch(dir.path()), None);
    }
}
