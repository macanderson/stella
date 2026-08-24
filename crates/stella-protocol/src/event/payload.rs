// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The leaf payload types an [`AgentEvent`] variant carries
//! — file-change kinds, verdict evidence, scope/hunk proposals, media refs, PR
//! and CI status, and the task-board item.
//!
//! Split out of `event.rs` for the same reason its tests were (#1857): the
//! wire contract itself has to stay under the file-size ratchet, and these are
//! the part of it that grows independently of the variant list. Nothing here
//! is a variant, so the totality machinery in [`super::tags`] and
//! [`super::consumers`] (invariant #10) is untouched by the move.
//!
//! Re-exported from `event` unchanged, so every consumer that spells
//! `stella_protocol::event::TaskItem` keeps resolving — the same courtesy the
//! `crate::receipt` re-export above it extends.

use serde::{Deserialize, Serialize};

use crate::ladder::LadderSnapshot;
// Referenced only by the intra-doc links below, which is a use rustc's
// `unused_imports` lint does not count. `cfg(doc)` is what keeps both halves
// honest: rustdoc sets it and resolves the links, every other build never sees
// the import and so has nothing to warn about. The alternative — spelling the
// links `[`AgentEvent::X`](super::AgentEvent::X)` — would leak a Rust module
// path into the published wire contract, because schemars exports these doc
// comments verbatim as the `description` of `docs/wire/agentevent.schema.json`
// and `agentevent.d.ts` (#3450).
#[cfg(doc)]
use super::AgentEvent;
// Same `cfg(doc)` treatment, and for the same reason: `TaskItem::contract`'s
// docs link it, and spelling the link as a path would put `crate::…` into the
// published wire contract.
#[cfg(doc)]
use crate::TaskContract;

/// What happened to a file in a [`AgentEvent::FileChange`] event.
///
/// Both live producers measure a tree against a tree, so every kind emitted
/// today is a mutation — see [`Self::Read`] for the one that is not, and why
/// it stays in the space anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    /// Content was successfully read — no mutation, never a diff.
    ///
    /// **Replay-only, permanently, and this is the decision rather than a gap
    /// awaiting one (#3413).** It had a producer when the `read` built-in
    /// declared its own touches; the 12-tool purge (#3244) removed that
    /// producer, and restoring `read_file` did not bring it back. Neither
    /// surviving producer can emit it: both diff a tree against a tree, and a
    /// read leaves no trace in a tree to diff. Re-acquiring one would
    /// mean going back to declaring file operations from tool inputs, which is
    /// the defect [`AgentEvent::FileChange`] documents rather than a capability
    /// worth restoring.
    ///
    /// It stays in the kind space because journals recorded before the purge
    /// carry it and replay must parse them — deleting the variant would make
    /// those streams unreadable. Consumers must keep handling it, and must not
    /// treat its absence from a live stream as evidence that nothing was read.
    Read,
    /// The file did not exist before this change.
    Created,
    /// An existing file's contents changed.
    Modified,
    /// The file was removed.
    Deleted,
}

impl FileChangeKind {
    /// Whether this kind describes a write — what the inline transcript diffs
    /// and the files-touched panel key on. [`Self::Read`] is the only `false`,
    /// and only ever arrives from a replayed journal.
    ///
    /// A `true` here is not a licence to claim the *agent* changed the file:
    /// the shared-tree producer measures the turn, not the actor (see
    /// [`AgentEvent::FileChange`]).
    #[must_use]
    pub fn is_mutation(self) -> bool {
        !matches!(self, FileChangeKind::Read)
    }
}

/// Evidence backing a `Verdict`. `deterministic` distinguishes the
/// flip-oracle/tests ladder from a model verifier's opinion — the two are
/// never conflated (L-E11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VerdictEvidence {
    /// One line naming what was checked and what it showed.
    pub summary: String,
    /// `true` when the verdict came from the deterministic ladder (a
    /// fail→pass flip of the same normalized test command, touched-tests
    /// green, diff budget) rather than a model verifier.
    pub deterministic: bool,
    /// Pointers to the underlying artifacts (`trace:t1#verify`, a test
    /// command, a diff), so a reader can go check the summary rather than
    /// take it on faith.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    /// The full ladder input snapshot this verdict was decided from (#865).
    /// `replay` answers "why did this run fast-submit / revise / verifier?"
    /// from here without re-deriving, and a verifier escalation renders it into
    /// the prompt (#864) so the verifier sees *why* the ladder was inconclusive
    /// rather than a diff cold. Absent on events recorded before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ladder: Option<Box<LadderSnapshot>>,
}

/// What a `ScopeReview` gate presents for approval before a large plan
/// executes (L-E5).
///
/// Everything after `estimated_cost_usd` is additive (`serde(default)`), so
/// streams recorded before those fields existed parse with every one absent,
/// and a proposal that names none serializes exactly as it always has:
/// `repo`/`branch`, the read and write globs and the shell policy are the
/// scope-card grid's facts, and `revision` is the plan breadcrumb's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScopeProposal {
    /// One line describing the work, for the approval prompt's headline.
    pub summary: String,
    /// The plan's steps, in the order the worker will attempt them.
    pub steps: Vec<String>,
    /// How many files the plan expects to touch — the magnitude the gate's
    /// thresholds are compared against.
    ///
    /// `0` is *not stated*, the same as an empty glob list below, and a
    /// surface must render it as nothing rather than as "~0 files": a
    /// producer that knows the steps but not the blast radius is the common
    /// case, and a plan claiming it touches no files is a different and
    /// false statement.
    pub estimated_files: u32,
    /// Projected spend, when the planner could estimate one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    /// The repository the scope binds to (`owner/name`, or a workspace
    /// path), when the planner named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The branch the work lands on, when named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Path globs the plan intends to WRITE within. Empty = not stated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_globs: Vec<String>,
    /// Path globs the plan reads beyond its write set. Empty = not stated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_globs: Vec<String>,
    /// The shell policy in force for the run (e.g. `allowlisted`,
    /// `read-only`, `none`), when stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_policy: Option<String>,
    /// Which revision of the plan this is: `1` for a plan's first proposal,
    /// incremented each time a changed plan is re-proposed. What a producer
    /// counts a plan's lifetime as is its own to decide — the deck's gate
    /// resets per turn, because that is where the deck also drops the plan
    /// it was holding.
    ///
    /// `None` means the producer does not track revisions — every recording
    /// written before this field existed decodes that way, and a surface
    /// rendering a breadcrumb must say nothing rather than claim `r1` for a
    /// plan whose history it cannot see (#4333).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
}

/// What a `HunkReview` gate presents for per-hunk approval before a mutating
/// tool call writes anything (#1265).
///
/// The hunks are a **flat, ordered list across every file the call touches**,
/// not a per-file tree: the reviewer's answer is a set of indices into this
/// list, and one flat coordinate space is what keeps that answer unambiguous
/// when two files change in the same call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HunkProposal {
    /// Correlates the decision — and the synthetic `ToolResult` that clears the
    /// card — back to this review. Distinct from the model's tool-call id: one
    /// call raises one review, but the review is the host's object, not the
    /// model's.
    pub id: String,
    /// The tool whose write is being reviewed (a custom or MCP write tool) —
    /// the card names it so a reviewer knows what declining costs.
    pub tool: String,
    /// Every proposed hunk, in file-then-position order.
    pub hunks: Vec<ProposedHunk>,
}

/// One reviewable hunk: which file, what it does, and how it renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProposedHunk {
    /// Workspace-relative path of the file this hunk changes.
    pub path: String,
    /// The hunk as unified-diff text, `@@` header included — ready for
    /// `stella_tui::diff::body_lines` with no further parsing.
    pub diff: String,
    /// Lines this hunk adds. Authoritative: taken from the decomposition, never
    /// re-counted from `diff` (which is capped and carries context lines).
    pub lines_added: u32,
    /// Lines this hunk removes, on the same terms.
    pub lines_removed: u32,
}

/// Which kind of media artifact a job produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A raster image.
    Image,
    /// Vector artwork emitted as SVG source.
    Svg,
    /// A video clip — the only kind whose job is asynchronous and long-lived.
    Video,
}

/// Lifecycle of an async media job. `Failed` carries the reason inline —
/// a failed job must never be distinguishable only by the absence of a
/// success event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MediaJobState {
    /// Accepted by the provider, not yet started.
    Queued,
    /// Generation is under way.
    Running,
    /// The artifact landed; a [`AgentEvent::MediaComplete`] follows.
    Succeeded,
    /// Generation failed terminally.
    Failed {
        /// Why it failed, inline — a failed job is never signalled only by
        /// the absence of a success event.
        reason: String,
    },
}

/// A completed media artifact: id + kind + where it landed on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MediaArtifactRef {
    /// The artifact id, matching the `artifact_id` its
    /// [`AgentEvent::MediaProgress`] events carried.
    pub id: String,
    /// What was produced.
    pub kind: MediaKind,
    /// Path under `.stella/artifacts/` (the generation tools may never
    /// write outside it).
    pub path: String,
    /// Human label for citation/display.
    pub label: String,
}

/// A pull request's status as observed by the fleet monitor. Reconciled
/// against the live source before rendering, never served from cache
/// alone (L-V3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PrStatus {
    /// Opened as a draft — not yet asking for review.
    Draft,
    /// Open and reviewable.
    Open,
    /// Merged into its base branch.
    Merged,
    /// Closed without merging.
    Closed,
}

/// Aggregate CI verdict for a PR's head commit, as observed by the
/// fleet monitor (`gh pr checks`). Reconciled against the live source
/// before rendering, never served from cache alone (L-V3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CiStatus {
    /// Checks exist but none have started reporting.
    Pending,
    /// At least one check is still running and none have failed.
    Running,
    /// Every check reported and all of them succeeded.
    Passing,
    /// At least one check failed — terminal for this head commit.
    Failing,
}

/// One entry on the turn's task board (the `task_*` tools). The board is
/// session-scoped working state — what the agent has planned, is doing,
/// and has finished — mirrored to the store for cross-session findability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TaskItem {
    /// Stable per-session ordinal id ("1", "2", …) — what `task_complete`
    /// / `task_cancel` / `task_assign` reference.
    pub id: String,
    /// Imperative title ("Fix the auth redirect loop").
    pub subject: String,
    /// What needs to be done, if the creator elaborated beyond the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Where the task is in its lifecycle ([`TaskStatus`]).
    pub status: TaskStatus,
    /// Which agent lane owns the task: `None` until claimed, `Some("lead")`
    /// for the lead, or the sub-agent lane id once `task_assign` spawned a
    /// dedicated worker for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// What this task means by done (SPEC 7.1).
    ///
    /// `None` is *nobody has said yet*, and is deliberately not the same fact
    /// as [`TaskContract::ReadOnly`], which is *somebody looked and there is
    /// nothing to prove*. A board that collapsed the two would let an
    /// undeclared task close on the same terms as one declared harmless —
    /// which is the self-report [`TaskContract`] exists to end.
    ///
    /// Optional because the board predates contracts and a session may still
    /// create a task without one; `stella_core::tasks` refuses the *close*, not
    /// the creation, so an undeclared task is visible on the board rather than
    /// rejected at the door.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<crate::task_contract::TaskContract>,
}

/// Lifecycle of a `TaskItem`. Terminal states are `Completed` and
/// `Cancelled`; a cancelled task keeps its row (the board is an audit
/// surface, not just a scheduler).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Created and not yet started.
    Pending,
    /// Claimed by a lane and being worked.
    InProgress,
    /// Finished successfully. Terminal.
    Completed,
    /// Abandoned. Terminal, and the row is kept — the board is an audit
    /// surface, not just a scheduler.
    Cancelled,
}

impl TaskStatus {
    /// Whether the task can still change state. Terminal tasks reject
    /// further transitions (enforced by the board logic in `stella-core`).
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, TaskStatus::Pending | TaskStatus::InProgress)
    }
}
