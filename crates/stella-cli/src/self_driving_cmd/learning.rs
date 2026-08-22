//! What a turn taught, counted from the state it leaves behind.
//!
//! The loop cannot observe a turn learning something. A turn runs in a child
//! process ([`super::work::run_turn`] spawns `stella run --output-format json`),
//! so no event reaches this process as it happens — and none reaches it on the
//! wire either, because a machine-format turn deliberately emits no reflection
//! frame so `Complete` stays the unique final one
//! (`crate::agent::goal::surface_reflection`).
//!
//! What *is* observable is durable. `run_turn` points the child's workspace
//! state root at the repository rather than the throwaway worktree precisely so
//! the reflection log, the context store and the telemetry store outlive the
//! unit of work — so the honest producer for the learning counters is a delta:
//! read the artifacts before the turn, read them after, and report the
//! difference.
//!
//! The three artifacts are deliberately three different questions. Every
//! lesson reaches the reflection log, restatements included; only a *novel*
//! lesson becomes a memory; only a recurring one becomes a proposal. Counting
//! one number three times would be worse than counting none.

use std::path::Path;

use stella_core::context_record::ContextRecordKind;

/// The learning artifacts under one state root, counted at one instant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct LearningTally {
    /// Lessons in the reflection mining log, novel and restated alike.
    pub reflections: u32,
    /// Distinct memory lineages — memories, as a user counts them.
    pub memories: u32,
    /// Record-proposal revisions the context ledger holds.
    pub proposals: u32,
}

impl LearningTally {
    /// What `self` holds beyond `earlier`.
    ///
    /// Saturating in every field, because retention runs: a prune or a
    /// `stella memory forget` between the two readings can lower a count, and
    /// "this turn taught minus three lessons" is not a thing to report.
    #[must_use]
    pub(super) fn since(self, earlier: Self) -> Self {
        Self {
            reflections: self.reflections.saturating_sub(earlier.reflections),
            memories: self.memories.saturating_sub(earlier.memories),
            proposals: self.proposals.saturating_sub(earlier.proposals),
        }
    }

    /// Fold this delta into the session's counters.
    pub(super) fn add_to(self, stats: &mut stella_autonomy::SessionStats) {
        stats.reflections_logged += self.reflections;
        stats.memories_created += self.memories;
        stats.proposals_made += self.proposals;
    }
}

/// Count what the workspace at `state_root` has learned so far.
///
/// Reads only what already exists. A workspace that has never reflected must
/// read as zero rather than gain an empty database as a side effect of being
/// counted — the rule `stella memory index` states and this shares, with the
/// added reason that this runs twice per turn on somebody's live repository.
///
/// Unreadable state is zero rather than an error for the same reason every
/// other write on this path is best-effort: a counter must never be able to
/// stop a turn.
pub(super) fn tally(state_root: &Path) -> LearningTally {
    let (memories, proposals) = context_tally(state_root);
    LearningTally {
        reflections: reflection_lines(state_root),
        memories,
        proposals,
    }
}

/// Lessons in `.stella/private/reflections.jsonl` — one line each.
///
/// The log rather than the `reflections` table in `store.db`: they mirror each
/// other, and the log is the surface the field is named after and the one the
/// skill and rule miners actually read.
fn reflection_lines(state_root: &Path) -> u32 {
    let Ok(Some(path)) =
        stella_store::existing_workspace_private_state_path(state_root, "reflections.jsonl")
    else {
        return 0;
    };
    let count = std::fs::read_to_string(&path)
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0);
    clamp(count)
}

/// Memory lineages and record proposals, from `.stella/private/context.db`.
///
/// One open serves both, because both are the same store answering two
/// questions and opening it twice per reading would double the only cost here.
fn context_tally(state_root: &Path) -> (u32, u32) {
    let Ok(Some(path)) =
        stella_store::existing_workspace_private_sqlite_path(state_root, "context.db")
    else {
        return (0, 0);
    };
    let Ok(store) = stella_context::ContextStore::open(&path) else {
        return (0, 0);
    };

    // Lineages, not live rows: a lineage is one memory however many times it
    // has been revised, and a revision is not a memory created.
    let memories = store
        .memory_lineage_stats()
        .map_or(0, |stats| clamp(stats.lineages));

    // `record_counts` rather than a bounded `records_of_kind` read: the ledger
    // is append-only, so a capped read stops moving once the log passes the
    // cap, and a delta over a frozen number is silently zero forever.
    let proposals = store.record_counts().map_or(0, |counts| {
        counts
            .into_iter()
            .find(|(kind, _)| kind == ContextRecordKind::RecordProposal.as_str())
            .map_or(0, |(_, n)| u32::try_from(n).unwrap_or(u32::MAX))
    });

    (memories, proposals)
}

fn clamp(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
