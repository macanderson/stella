// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What becomes of a run's candidate workspaces when it ends.
//!
//! Three questions with one subject, so they live together rather than among
//! the driver and the host assembly in `wrapper_plugin.rs` (which sits under
//! the 1500-line ratchet): what earlier runs left behind, what this run must
//! write out because nothing scored it, and what it may discard.
//!
//! The split between the last two is the whole decision, and
//! [`ended_abnormally`] is where it is made: on a clean ending a candidate
//! still in the table is one the plugin looked at and passed over, and
//! discarding it is the point of best-of-N; on an abort nothing ever scored
//! them, and discarding is the silent loss #2651 measured as a solved task
//! scoring zero.
//!
//! A child module of `wrapper_plugin`, so `BoundWrapper`'s private planes are
//! in scope here exactly as they are in the parent.

use super::{BoundWrapper, CliFailure};

impl BoundWrapper {
    /// What earlier runs in this workspace left behind, one line each.
    ///
    /// Read before this run mints anything, so every record it sees belongs to
    /// somebody else — and a *live* sibling run's records are skipped, so what
    /// is left is residue from a process that is gone. It names a reclaim
    /// command and deletes nothing; see
    /// [`crate::candidate_workspaces::SessionCandidateWorkspaces::orphaned_candidates`].
    /// **One plane's answer, not every member's.** This reads the workspace's
    /// own candidate records
    /// (`SessionCandidateWorkspaces::orphaned_candidates` scans
    /// `<root>/.stella/candidates`), which is a fact about the tree rather
    /// than about the plugin asking — so a composed selection asking once per
    /// member would print every orphan N times (#4094).
    pub(crate) fn orphaned_candidates(&self) -> Vec<String> {
        self.candidate_fanout
            .first()
            .map_or_else(Vec::new, |plane| plane.workspaces().orphaned_candidates())
    }

    /// Write out every candidate this run still holds, before the sweep takes
    /// it, and return where each landed (#2651).
    ///
    /// Called only on an **abnormal** ending, because on a clean one a
    /// candidate still in the table is one the plugin looked at and did not
    /// choose — discarding that is the whole point of best-of-N. An abort is
    /// the ending where nothing ever scored them.
    /// Every member's plane, unlike [`Self::orphaned_candidates`]: a plane
    /// owns the candidates *its own* plugin minted, so a composed selection
    /// that read only the first would silently discard the second's unscored
    /// work — the exact loss #2651 measured.
    pub(crate) async fn preserve_candidates(&self) -> Vec<String> {
        let mut preserved = Vec::new();
        for plane in &self.candidate_fanout {
            preserved.extend(plane.workspaces().preserve_unscored().await);
        }
        preserved
    }

    /// Discard every candidate workspace this run still holds, and return what
    /// would not go.
    ///
    /// Called once at the end of a wrapped run. The failures come back rather
    /// than being swallowed because an un-removed worktree is disk left behind
    /// under a name only the plane's table knew — see
    /// [`stella_runtime::wrapper::CandidateFanouts::discard_all`], whose
    /// contract this is the caller half of.
    pub(crate) async fn sweep_candidates(&self) -> Vec<String> {
        let mut failures = Vec::new();
        for plane in &self.candidate_fanout {
            failures.extend(plane.discard_all().await.iter().map(ToString::to_string));
        }
        failures
    }
}

/// Did this run end in a way that left its candidates unscored?
///
/// A plugin scores what a fan-out produced and then adopts one; the end-of-run
/// sweep deletes the rest, which is correct — a candidate the plugin looked at
/// and passed over is meant to go. An **abort** is the other ending: the turn
/// budget stopped the run, or a turn failed, and no plugin ever got as far as
/// scoring anything. Everything still live then is work nobody judged, and
/// deleting it is the silent discard #2651 measured as a solved task scoring
/// zero.
///
/// Pure, and separate from the loop it governs, because it is the whole
/// decision: read it wrong in the safe direction and a run writes a patch
/// nobody needed; read it wrong in the other and finished work is destroyed.
pub(super) fn ended_abnormally(rounds: &[Result<bool, CliFailure>]) -> bool {
    rounds.iter().any(|round| !matches!(round, Ok(true)))
}
