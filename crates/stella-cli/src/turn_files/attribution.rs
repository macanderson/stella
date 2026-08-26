// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Who a measured tree change may be claimed for (#4386).
//!
//! # The defect
//!
//! [`super`]'s module doc already stated the hazard: a tree measurement
//! answers *what changed during this turn*, not *what the agent changed*. What
//! it treated as an edge case is the maintainer's normal way of working —
//! several `stella` sessions in one checkout — and there the two are routinely
//! different. `WorkJournal` gives each session its own snapshot ref and its own
//! git index, but `git add -A` sweeps the one **shared** work tree, so every
//! byte another session wrote since this session's last snapshot lands in this
//! session's diff. In the reported export a read-only research turn is recorded
//! as creating `scripts/prose_score.py`; that file is another session's work,
//! and the agent later mined a repository "convention" out of the contamination.
//!
//! # Why the answer is a label rather than a filter
//!
//! Both sessions snapshot the same bytes, so no content the tree carries can
//! tell their writes apart — the trees converge, and only the snapshot *times*
//! differ. Two signals do survive:
//!
//! - the paths a mutating call read for itself before and after it wrote
//!   ([`stella_tools::own_change`]), which is evidence about this session
//!   whatever else is in the tree, and
//! - whether any other session was live in this work tree at all, which the
//!   session registry knows.
//!
//! Neither can be turned into "B wrote this". Together they are enough to stop
//! the stream *claiming* what it cannot know, which is what [`Provenance`]
//! records. Dropping the unattributable changes instead would be worse: `bash`,
//! MCP servers and script tools mutate the tree without naming a path, and the
//! reading is the only trace those leave. So every measured change is still
//! reported; a change the reading cannot attribute now says so.

use std::path::Path;

use stella_store::SessionRecord;
use stella_tools::own_change::OwnChange;

/// What one measured tree change may be claimed as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provenance {
    /// The mutating call that took this reading also read the bytes itself, so
    /// this path is this session's work however many sessions share the tree.
    OwnReading,
    /// The tree changed and nothing else live was writing to it, so this
    /// session is the only thing that can have changed it.
    SoleWriter,
    /// The tree changed while at least one other live session shared it. The
    /// change is measured; its author is not knowable from the measurement.
    Unattributed,
}

impl Provenance {
    /// The sentence that rides into `files_touched.events_json`, which is what
    /// `stella export` and the dashboard read.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::OwnReading => "the call's own before/after reading",
            Self::SoleWriter => "turn-boundary work-tree measurement",
            Self::Unattributed => {
                "measured, not attributed — another session shared this work tree"
            }
        }
    }

    /// Whether this session may be named as the author.
    pub(crate) fn attributed(self) -> bool {
        !matches!(self, Self::Unattributed)
    }
}

/// Classify one measured path.
///
/// `own` is what the call that triggered this reading reported about its own
/// writes, empty at the turn boundary where no single call owns the sweep.
/// `sharers` is [`live_sharers`]'s answer.
pub(crate) fn provenance(path: &str, own: &[OwnChange], sharers: &[String]) -> Provenance {
    if own.iter().any(|change| change.path == path) {
        return Provenance::OwnReading;
    }
    if sharers.is_empty() {
        return Provenance::SoleWriter;
    }
    Provenance::Unattributed
}

/// The other live sessions bound to `workspace`, from a registry listing.
///
/// Split from the read so the rule is testable without a registry directory:
/// `SessionRegistry::list` already drops a record whose process is gone, so
/// liveness here is `SessionStatus::is_live` over what it returned and nothing
/// more.
///
/// Paths are compared after [`std::fs::canonicalize`] where it succeeds,
/// because a registry record holds whatever absolute path that session was
/// launched with — `/tmp/x` and `/private/tmp/x` are the same tree on macOS,
/// and treating them as different would silently restore the defect on the
/// platform it was reported from. A path that cannot be canonicalized at all —
/// the session's workspace has since been deleted — falls back to a literal
/// comparison, which is the only answer left when the tree it named is gone.
pub(crate) fn sharers_of(
    records: &[SessionRecord],
    workspace: &Path,
    own_session: &str,
) -> Vec<String> {
    let ours = real_path(workspace);
    let mut sharers: Vec<String> = records
        .iter()
        .filter(|record| record.id != own_session)
        .filter(|record| record.status.is_live())
        .filter(|record| real_path(Path::new(&record.workspace)) == ours)
        .map(|record| record.id.clone())
        .collect();
    sharers.sort();
    sharers.dedup();
    sharers
}

/// `canonicalize`, or the path as given when it no longer resolves.
fn real_path(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// [`sharers_of`] over the default session registry.
///
/// The one impure half, and the reason this is a function rather than a method
/// on the durability handle: reading the registry is a directory walk, so it is
/// taken at a turn's bookends and cached, never per tool call.
///
/// **The residual.** `SessionRegistry::list` answers an unreadable registry
/// directory and an empty one identically, so a workspace whose `~/.stella`
/// is broken reports no sharers and its rows claim authorship as they did
/// before this existed. That is the status quo on a path where the home
/// itself is unusable, rather than a new way to be wrong — and a session
/// bound to a journal key that is not a registry id (a fleet attempt, whose
/// key is `{run}/{task}`) is likewise reported as alone, correctly: those run
/// in their own worktrees, so no other session shares the tree they measure.
pub(crate) fn live_sharers(workspace: &Path, own_session: &str) -> Vec<String> {
    sharers_of(
        &stella_store::SessionRegistry::open_default().list(),
        workspace,
        own_session,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_store::SessionStatus;
    use stella_tools::own_change::OwnChangeKind;

    fn record(id: &str, workspace: &str, status: SessionStatus) -> SessionRecord {
        SessionRecord {
            id: id.into(),
            pid: std::process::id(),
            workspace: workspace.into(),
            title: String::new(),
            summary: String::new(),
            description: None,
            status,
            started_at_ms: 0,
            updated_at_ms: 0,
            supervisor: None,
        }
    }

    fn own(path: &str) -> OwnChange {
        OwnChange {
            path: path.into(),
            kind: OwnChangeKind::Modified,
            added: 1,
            removed: 0,
            diff: String::new(),
            minimal: true,
        }
    }

    #[test]
    fn a_live_session_in_the_same_tree_is_a_sharer() {
        let guard = tempfile::tempdir().unwrap();
        let ws = guard.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let ws_str = ws.to_string_lossy().to_string();
        let records = vec![
            record("ses-a", &ws_str, SessionStatus::InProgress),
            record("ses-b", &ws_str, SessionStatus::InProgress),
        ];
        assert_eq!(
            sharers_of(&records, &ws, "ses-a"),
            vec!["ses-b".to_string()]
        );
    }

    #[test]
    fn a_finished_session_and_another_workspace_are_not_sharers() {
        let guard = tempfile::tempdir().unwrap();
        let ws = guard.path().join("ws");
        let other = guard.path().join("other");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let records = vec![
            record("ses-done", &ws.to_string_lossy(), SessionStatus::Complete),
            record(
                "ses-else",
                &other.to_string_lossy(),
                SessionStatus::InProgress,
            ),
        ];
        assert!(sharers_of(&records, &ws, "ses-a").is_empty());
    }

    /// The macOS shape: `/tmp` is a symlink to `/private/tmp`, so a registry
    /// record written by a session launched through one spelling must still
    /// match a workspace named by the other. Without this the guard would be
    /// inert on the platform #4386 was reported from.
    #[test]
    fn two_spellings_of_one_tree_are_the_same_tree() {
        let guard = tempfile::tempdir().unwrap();
        let ws = guard.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let link = guard.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&ws, &link).unwrap();
        #[cfg(not(unix))]
        let link = ws.clone();
        let records = vec![record(
            "ses-b",
            &link.to_string_lossy(),
            SessionStatus::InProgress,
        )];
        assert_eq!(
            sharers_of(&records, &ws, "ses-a"),
            vec!["ses-b".to_string()]
        );
    }

    #[test]
    fn a_lone_session_owns_every_change_it_measures() {
        assert_eq!(
            provenance("src/lib.rs", &[], &[]),
            Provenance::SoleWriter,
            "with nobody else in the tree the measurement is this session's"
        );
    }

    #[test]
    fn a_shared_tree_makes_a_measured_change_unattributable() {
        let sharers = vec!["ses-b".to_string()];
        assert_eq!(
            provenance("scripts/prose_score.py", &[], &sharers),
            Provenance::Unattributed
        );
    }

    /// The precision half: a path the call read for itself is this session's
    /// work whoever else is in the tree, because that reading came from the
    /// call and not from the shared snapshot.
    #[test]
    fn a_path_the_call_read_itself_survives_a_shared_tree() {
        let sharers = vec!["ses-b".to_string()];
        assert_eq!(
            provenance("src/lib.rs", &[own("src/lib.rs")], &sharers),
            Provenance::OwnReading
        );
        assert_eq!(
            provenance("other.rs", &[own("src/lib.rs")], &sharers),
            Provenance::Unattributed,
            "one call's own reading must not vouch for a path it never named"
        );
    }

    #[test]
    fn only_the_unattributed_reason_disclaims_authorship() {
        assert!(Provenance::OwnReading.attributed());
        assert!(Provenance::SoleWriter.attributed());
        assert!(!Provenance::Unattributed.attributed());
        assert!(
            Provenance::Unattributed
                .reason()
                .contains("another session"),
            "the reason string is what the export renders, so it has to say why"
        );
    }
}
