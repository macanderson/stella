// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Candidate isolation, as a port: the shadow workspace a candidate runs in,
//! the failures creating or adopting one can have, and what an adoption
//! actually moved.
//!
//! Split out of [`super`] when that file crossed the 1500-line guard. The cut
//! follows the concern rather than the line count: every other port in
//! `ports.rs` is something the pipeline *asks a question of* — a provider, a
//! test runner, a probe — while these four items describe a place the
//! pipeline *puts work*, with a lifecycle (create → seed → dispatch → seal →
//! adopt → remove) that none of the others have.
//!
//! Re-exported from [`super`], so every existing `ports::CandidateWorkspace`
//! path still resolves.

use async_trait::async_trait;
use stella_protocol::FileChangeKind;

use super::{DiagnosticRunner, RepoStatusPort, TestRunner};

/// One file the winning best-of-N candidate changed, as applied to the real
/// tree by [`CandidateWorkspace::adopt`]. Paths are repo-relative.
///
/// This is the whole `FileChange` the pipeline emits for adopted work, because
/// nothing in the session observed the winner's edits — they happened inside
/// a shadow worktree, against a registry deliberately left unattached (a
/// losing candidate's edits must never be announced as the user's). So the
/// delta has to come from the adoption itself: `git`'s own `--numstat` for the
/// counts, and its patch text, sliced per file, for `diff`.
///
/// `added`/`removed` default to `0` and `diff` to `None` only for an
/// implementation that cannot measure them — which reads in the Files tab as
/// "changed, extent unknown", not as "nothing changed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub added: u32,
    pub removed: u32,
    pub diff: Option<String>,
}

/// A typed candidate-isolation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceError {
    /// The current tree state could not be snapshotted (not a git repository,
    /// no commits yet, git unavailable, worktree creation failed). The
    /// pipeline scores the affected candidate as aborted — it never falls
    /// back to running that candidate in the shared tree.
    #[error("could not snapshot the working tree: {reason}")]
    Snapshot { reason: String },
    /// Candidate state could not be committed into the immutable tree that
    /// final verification and adoption must share.
    #[error("could not seal candidate workspace `{workspace}`: {reason}")]
    Seal { reason: String, workspace: String },
    /// Applying the winning candidate's changes to the real tree failed —
    /// typically because the user edited the same files mid-run. Adoption is
    /// all-or-nothing: NOTHING was applied, `paths` names the conflicts, and
    /// the candidate workspace is preserved at `workspace` so the winning
    /// work is recoverable by hand.
    #[error(
        "adopting the winning candidate's changes failed ({reason}); conflicting paths: {}; \
         nothing was applied — the candidate's work is preserved at `{workspace}`",
        paths.join(", ")
    )]
    Adopt {
        reason: String,
        paths: Vec<String>,
        workspace: String,
    },
    /// An accepted witness artifact could not be copied out of the pristine
    /// authoring snapshot into the candidate that executed. The candidate is
    /// left exactly as the worker made it and the run degrades to the
    /// unauthored ladder: a witness that cannot be placed proves nothing, but
    /// the work it would have pinned is real and already done.
    #[error("could not graft witness artifact `{path}` into `{workspace}`: {reason}")]
    Graft {
        reason: String,
        path: String,
        workspace: String,
    },
}

/// One isolated best-of-N candidate workspace: a snapshot of the working
/// tree's *current* state (HEAD plus uncommitted and untracked files) that a
/// candidate executes and verifies inside, so sibling candidates never see
/// each other's edits and losers leave no residue in the real tree. The
/// bundled ports are rooted at the snapshot — the pipeline threads them (not
/// the session ports) through the candidate's engine turns and verification
/// runs.
#[async_trait]
pub trait CandidateWorkspace: Send + Sync {
    /// Absolute workspace root used to bind engine hook payloads and hook
    /// process execution to this candidate rather than the session tree.
    fn root(&self) -> &str;
    /// The tool executor rooted at this workspace: every engine-turn tool
    /// call (read/edit/shell) lands in the snapshot, never in the real tree.
    fn tools(&self) -> &dyn stella_core::ToolExecutor;
    /// Capability-minimal witness executor, constructed with the candidate:
    /// candidate-root reads plus one atomic, create-only witness test action.
    /// It must never expose general writes, edits, processes, hooks, MCP,
    /// custom tools, credentials, or external adapters.
    fn witness_tools(&self) -> &dyn stella_core::ToolExecutor;
    /// Runs closed diagnostic invocations inside the snapshot.
    fn diagnostics(&self) -> &dyn DiagnosticRunner;
    /// Typed test-process runner rooted at this workspace.
    fn tests(&self) -> &dyn TestRunner;
    /// Untracked-file fingerprints of the snapshot, mirroring the real
    /// tree's semantics (the tamper watchlist and zero-diff guard must keep
    /// working unchanged inside a candidate).
    fn repo_status(&self) -> &dyn RepoStatusPort;
    /// Workspace-relative paths that exist in the real tree but NOT in this
    /// snapshot, as a display list for the candidate's own context.
    ///
    /// A git-shaped snapshot carries tracked content and untracked-but-not-
    /// ignored files; everything in `.gitignore` is absent by construction.
    /// That is defensible — ignored bytes are derived state, and copying them
    /// per candidate can cost more than the fan-out — but it is invisible from
    /// inside the snapshot, where a missing `node_modules/` surfaces only as a
    /// test command failing for no stated reason.
    ///
    /// Empty by default: a substrate that copies whole trees omits nothing and
    /// has nothing to declare. Only a snapshot that *elides* part of the tree
    /// owes the candidate this list.
    fn omitted_paths(&self) -> &[String] {
        &[]
    }
    /// Hand this candidate the approved plan's steps for its own private
    /// task board, and whether that board's activity may be announced on the
    /// turn's event channel.
    ///
    /// The pipeline calls this once per workspace, right after `create` and
    /// before any dispatch (#1719). `steps` are the approval gate's rendered
    /// plan steps in gate order, so the ordinal ids the worker was shown
    /// (`task_start "3"`) resolve against this candidate's board exactly as
    /// they do against the session's; empty when the run planned nothing.
    /// `announce` is true only when this candidate is alone in the fan-out —
    /// a full-board `TaskUpdate` snapshot carries no candidate tag, so
    /// several private boards reporting onto one channel would splice into a
    /// checklist that is nobody's (the same reasoning that mutes `TextDelta`
    /// on a shared event lane). Boards stay per-candidate at every width:
    /// a step is completable exactly once per board, and the second sibling
    /// to finish it must not be told its work already happened.
    ///
    /// Default: no-op — a substrate with no task board has nothing to seed.
    fn seed_task_board(&self, _steps: &[String], _announce: bool) {}
    /// Whether this candidate's **mutations** may ride the turn's event
    /// channel as they happen, rather than only being re-emitted by adoption.
    ///
    /// Called once per workspace beside [`Self::seed_task_board`], with the
    /// same `announce` answer and for the same reason: the caller who creates
    /// the workspaces is the only one who knows the fan-out width.
    ///
    /// A candidate's edits live in a shadow the user's checkout has not
    /// received, so announcing them is a claim — which is why the default
    /// posture withholds them. But a **lone** candidate is the one case where
    /// the claim is true in advance: there is exactly one tree of edits, and
    /// it is the tree that becomes the user's if the run verifies. Withholding
    /// there cost the transcript its single most useful row — an inline diff
    /// is bound to a tool result at fold time from the `FileChange` emitted
    /// *during* that call, so a suppressed change is not a diff shown later,
    /// it is a diff shown never. On the deck's default path (one candidate,
    /// always isolated) that meant no edit in any session ever rendered its
    /// diff.
    ///
    /// A wider fan-out still withholds: several candidates' trees spliced into
    /// one Files tab is nobody's tree, exactly as several private boards
    /// reporting onto one channel is nobody's checklist.
    ///
    /// Default: no-op — a substrate whose edits are already the user's has
    /// nothing to gate.
    fn announce_changes(&self, _announce: bool) {}
    /// Move one seeded plan step to `status` and announce the board.
    ///
    /// **The pipeline drives the board; the worker is not asked to.** #1719
    /// made a candidate's `task_start "3"` resolve, which was necessary and
    /// not sufficient: nothing ever *called* it. The execute stage walks the
    /// plan one engine turn per step and each step's prompt says only what to
    /// do — so on a real six-step run the deck's PLAN rail sat at `0/6` from
    /// the first turn to the last while the transcript beneath it said the
    /// work was being finished. The rail was not broken; it was never told.
    ///
    /// Deterministic, for the same reason `ladder_decision` is: the pipeline
    /// *knows* which step is in flight — it wrote the prompt — so asking a
    /// model to report it would buy a round trip per step for an answer
    /// already in hand, and would then be wrong whenever the model forgot.
    /// The board tools stay registered for work the worker adds itself; this
    /// is the floor beneath them.
    ///
    /// Best-effort by contract: an id the board does not carry (a resumed run
    /// past its seeded plan), or a step already terminal (a worker that
    /// completed it through the tools first), is not an error worth failing a
    /// turn over. Implementations swallow it.
    ///
    /// Default: no-op — a substrate with no task board has nothing to move.
    fn mark_plan_step(&self, _id: &str, _status: stella_protocol::TaskStatus) {}
    /// Commit the current candidate bytes into its private immutable history
    /// immediately before a final verification observation.
    async fn seal(&self) -> Result<(), WorkspaceError>;
    /// Whether the live worktree and HEAD still exactly match the last seal.
    async fn sealed_is_unchanged(&self) -> Result<bool, WorkspaceError>;
    /// Workspace-relative paths where the REAL tree already holds this
    /// candidate's own sealed bytes at a path whose baseline state differed —
    /// the blob-level signature of the candidate having written *through* its
    /// isolation (#1538). Nothing else on the machine produces the exact
    /// sealed blob, so a non-empty answer is attributable to this candidate,
    /// never to a user's mid-run edit.
    ///
    /// Consulted by the pipeline once per verification round, immediately
    /// after [`Self::seal`] (the seal is what pins the bytes being matched).
    /// Best-effort by contract: a substrate that cannot measure — or a probe
    /// that fails — answers empty rather than failing a candidate on broken
    /// forensics, so the check can only ever *add* a detection. The default
    /// is empty for the same reason.
    async fn escaped_paths(&self) -> Vec<String> {
        Vec::new()
    }
    /// Seal the workspace's current state and apply everything produced
    /// since the last delivery (the starting snapshot, on the first call) to
    /// the real tree — the same all-or-nothing contract as [`Self::adopt`],
    /// run at execute-stage step boundaries so a process killed before the
    /// run ever reaches a verdict still has that step's work in the graded
    /// tree (#2941). [`Self::adopt`]'s final delivery only has to send
    /// whatever a checkpoint has not already sent.
    ///
    /// Deliberately gated on the same fact [`Self::announce_changes`]
    /// already gates on, not on a caller-supplied flag: several candidates'
    /// trees delivered piecemeal into one real tree would be nobody's tree,
    /// so a substrate that is not the sole candidate in its fan-out answers
    /// `Ok(empty)` without touching git. A substrate that cannot checkpoint
    /// at all shares that answer by construction — the default below.
    async fn deliver_checkpoint(&self) -> Result<Vec<AdoptedChange>, WorkspaceError> {
        Ok(Vec::new())
    }
    /// Apply this workspace's changes — relative to its starting snapshot —
    /// to the real tree. All-or-nothing: on conflict the real tree is left
    /// byte-identical and the error names the conflicting paths
    /// ([`WorkspaceError::Adopt`]); the user's index and stash are never
    /// touched on any path.
    ///
    /// `withhold` names workspace-relative paths to leave behind — they are
    /// excluded from both the returned change list and the applied patch, so
    /// the two can never disagree. This is how an authored witness stays
    /// ephemeral: it must exist inside the candidate (it *is* the test the
    /// flip oracle runs), but it is scaffolding for the run, not a change the
    /// user asked for, so by default it dies with the workspace instead of
    /// being copied into the real tree. Empty means adopt everything.
    async fn adopt(&self, withhold: &[String]) -> Result<Vec<AdoptedChange>, WorkspaceError>;
    /// Attribute changes this workspace just delivered into the real tree to
    /// whatever durable "what did this session change" record the host keeps
    /// (#2907).
    ///
    /// Called by the pipeline immediately after every successful delivery —
    /// [`Self::adopt`] and [`Self::deliver_checkpoint`] alike — with exactly
    /// the rows that delivery returned, beside the `FileChange` events those
    /// same rows produce. Both surfaces then describe one reality, which is
    /// the whole point: before this, an isolated run's durable record said the
    /// session touched nothing while the event stream said it had rewritten
    /// four files.
    ///
    /// # Why here and nowhere else
    ///
    /// A candidate's edits land in a shadow worktree that is discarded with
    /// it; the session never sees them. Adoption is the first and only moment
    /// those edits become the user's tree's, and it is also the only seam
    /// where the isolated case is distinguishable from the shared-tree one —
    /// a shared-tree run's mutations happen in the user's own tree as they
    /// happen and never reach this method, so nothing is counted twice.
    /// Folding at the event forwarder instead would double exactly those.
    ///
    /// This is **observability, not evidence** (#2882). What it records
    /// changes no decision — not the verdict, not the ladder, not delivery. It
    /// is a report of what git already did.
    ///
    /// # The durable ledger is adoption-only, deliberately (#3419)
    ///
    /// This is the sole writer of `files_touched`. A shared-tree run — where
    /// the shared-tree `FileChange` producer (`turn_files` /
    /// `WorkJournal::snapshot_worktree`, #3413) measures the turn — writes no
    /// row, and that is the decision rather than a gap.
    ///
    /// The two are not the same claim. Adoption knows *who* changed the file:
    /// a candidate's edits became the user's tree's, at one instant, by one
    /// `git apply`. The shared-tree producer measures the turn, not the actor,
    /// and says so — anything that ran during the turn, the user's own editor
    /// included, is inside its diff. Folding that into a per-execution ledger
    /// would attribute a teammate's concurrent save to the agent's execution
    /// id, permanently and with no way to tell the two apart afterwards. The
    /// event stream can carry a measurement that honest consumers read as one;
    /// a row in a table keyed by `execution_id` reads as attribution.
    ///
    /// So the two projections #2907 asked for are equal for the isolated case
    /// and deliberately unequal for the shared one. A shared-tree run's file
    /// activity is observable in its `FileChange` events, and is absent from
    /// `files_touched` on purpose.
    ///
    /// Default: no-op — a substrate whose edits are already the session's has
    /// nothing to attribute, and neither has one that keeps no such record.
    fn attribute_adopted(&self, _adopted: &[AdoptedChange]) {}
    /// Copy one accepted witness artifact from another workspace's root into
    /// this one at the same relative `path`, and fail if anything is already
    /// there.
    ///
    /// Witness authoring runs in a *pristine* sibling snapshot so the author
    /// never reads the implementation it is meant to pin (see
    /// [`crate::witness`]). The artifact it produces has to end up in the
    /// candidate that executed, because that is the tree the flip oracle
    /// observes the pass in. This is the one byte-for-byte crossing between
    /// the two, and it carries exactly one already-validated file.
    ///
    /// Create-only, and no link is followed on either side. A destination that
    /// already exists is [`WorkspaceError::Graft`], never an overwrite: the
    /// worker may legitimately have written a test at that path, and silently
    /// replacing it would destroy the candidate's own work to make room for
    /// scaffolding. The caller re-validates the identity of the *written* copy
    /// against the candidate's own repo status, so the bytes tamper exclusion
    /// pins are the bytes that will actually run.
    async fn graft_witness(&self, source_root: &str, path: &str) -> Result<(), WorkspaceError>;
    /// Remove the workspace. Best-effort and infallible — the cleanup
    /// discipline is the pipeline's (every workspace is removed on every
    /// path, except a winner whose adoption failed, which is preserved for
    /// recovery).
    async fn remove(&self);
}

/// Candidate isolation (L-E7): snapshots the current working-tree state into
/// one isolated [`CandidateWorkspace`] per candidate. Best-of-N uses it when
/// available; authored witnesses require it even for one candidate.
#[async_trait]
pub trait CandidateWorkspacePort: Send + Sync {
    /// Snapshot the current tree state into a fresh isolated workspace.
    async fn create(&self) -> Result<Box<dyn CandidateWorkspace>, WorkspaceError>;
}
