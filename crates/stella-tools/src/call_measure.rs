// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The host's per-call work-tree measurement seam (#4175).
//!
//! # What this exists to lift
//!
//! The Command Deck's transcript could show a diff for at most one tool call
//! per `(path, turn)` — and in practice for none. `FileChange` had exactly one
//! producer left after `stella-pipeline` was deleted (#3865):
//! `stella_cli::turn_files::emit_shared_tree_changes`, one aggregate event per
//! path at the **turn boundary**, after every `ToolResult` of the turn has
//! already folded. So the model had one change to hand out among however many
//! calls edited that file, and the seq a `ToolResult` stamped as it folded was
//! the *pre-turn* count — one short of the seq `remember_diff` later recorded
//! at. A successful `edit_file` therefore rendered with no diff and no
//! `+N −M` at all, which is the symptom #4155 was filed on.
//!
//! # Why the measurement is here and not in the tools
//!
//! `turn_files`' module doc rules out the obvious repair — having the four
//! file tools emit from their own arguments — and both of its reasons still
//! hold at per-call granularity:
//!
//! - A tool's arguments are a *request*, not a measurement of what landed.
//!   Synthesizing counts from them is what #2290 was, and it rendered bulk
//!   edits as `+0 −0`.
//! - `bash` mutates the tree without naming a path in any schema, and so do
//!   MCP servers and custom script tools. A hook on the file tools would
//!   report a subset of the turn's changes while looking exhaustive.
//!
//! So the per-call producer is the *same* measurement as the turn-boundary
//! one — the work journal's shadow-git snapshot, measured with git's own
//! `--numstat` — simply taken more often. The registry is the one place that
//! sees every dispatched call whatever the tool, which is what makes it the
//! seam rather than any individual tool.
//!
//! # Why the counts cannot double
//!
//! This is the design question #4175 named as the reason it was not folded
//! into the #4155 fix: per-call and per-turn measurement feed the same
//! counters (`FileState::added` / `removed` sum every mutating `FileChange`
//! for a path), so introducing the former naively makes the Files tab report
//! double.
//!
//! It cannot here, because a snapshot **consumes** what it reports:
//! `WorkJournal::snapshot_worktree` commits the current tree onto the
//! session's snapshot ref and diffs against the *previous* commit, so
//! consecutive readings partition the turn rather than overlapping it. The
//! turn-boundary reading still happens and still writes both projections — it
//! now reports whatever the per-call readings did not already claim (a
//! concurrent `delegate`'s writes, a tool that mutated while advertising
//! `read_only`, a human editing in another window). The union is identical
//! and every byte is counted exactly once, by construction rather than by
//! bookkeeping.
//!
//! # What is deliberately not measured per call
//!
//! A snapshot attributes everything that changed since the last one to the
//! call that just finished, so that attribution is only honest when the call
//! ran **alone**. Two exclusions follow, and the engine's dispatch grouping
//! (`driver::dispatch::execute_tool_calls`) is what makes them exact:
//!
//! - **Read-only tools.** They are the ones dispatched concurrently, and by
//!   their own schema they changed nothing worth attributing.
//! - **`Tool::parallel_safe` tools.** `delegate` is the only mutating one, and
//!   it is dispatched in a concurrent group with its siblings, so no snapshot
//!   can say which sibling wrote what. Its writes reach the ledger through the
//!   turn-boundary sweep instead, exactly as they do today.
//!
//! Everything else — `write_file`, `edit_file`, `delete_file`, `bash`, every
//! MCP and custom tool — runs alone in its own dispatch group, which is
//! precisely when a before/after snapshot means what it says.
//!
//! # This is observability, never evidence
//!
//! Unchanged from `FileChange`'s standing contract, and it has teeth at this
//! granularity too: a tree reading answers *what changed while that call ran*,
//! not *what that call changed*. A user saving a file in another window during
//! a slow `bash` lands in that call's event indistinguishably. Nothing may
//! found a claim on it, and in particular a verification plugin must not read
//! it as proof that a tool did its job.

/// A host's ability to measure and publish what the work tree changed during
/// one tool call.
///
/// Declared here rather than in `stella-core::ports` — the engine never calls
/// it, never sees it, and must not grow an opinion about work-tree
/// measurement (invariant #2). It is a contract between the registry, which
/// knows *when* a call finished and whether it ran alone, and the host, which
/// knows *how* to measure and where to publish. `SubAgentDispatcher` is the
/// same shape a layer up; this one has no engine half at all.
pub trait CallMeasure: Send + Sync {
    /// Measure everything that changed since the previous measurement and
    /// publish it — as `AgentEvent::FileChange` on the turn's stream, and as
    /// durable `files_touched` rows.
    ///
    /// **Must consume what it reports.** The no-double-counting argument in
    /// this module's docs rests on consecutive readings partitioning the turn;
    /// an implementation that reported a cumulative delta would make every
    /// counter downstream sum a prefix of itself once per call.
    ///
    /// Best-effort and infallible by signature, like every other
    /// turn-boundary write: a call whose bytes are already on disk is not made
    /// less done by an unmeasurable tree, and a measurement failure must never
    /// become a tool failure.
    fn measure_and_publish(&self);
}

/// The registry's slot for a host-supplied [`CallMeasure`].
///
/// Late-attached and per-turn, for the same reason the event channel is: the
/// measurer holds that turn's `EventSender` and that turn's execution row, and
/// a stale one would publish this turn's changes onto a closed channel and a
/// finished execution.
///
/// `None` is the honest default and the common case for a registry that is not
/// driving an interactive session — a best-of-N candidate workspace, an
/// embedding host, a test harness. Those simply keep the turn-boundary
/// granularity they have always had.
pub type CallMeasureSlot =
    std::sync::Arc<std::sync::RwLock<Option<std::sync::Arc<dyn CallMeasure>>>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A slot with nothing in it is readable and answers `None` — the shape
    /// every non-session registry runs with, so it must not be a special case
    /// anywhere.
    #[test]
    fn an_unattached_slot_reads_as_none() {
        let slot: CallMeasureSlot = Arc::default();
        assert!(slot.read().unwrap().is_none());
    }

    /// Attaching replaces rather than accumulates: a second turn's measurer
    /// must displace the first, or the first turn's closed channel keeps
    /// receiving.
    #[test]
    fn attaching_twice_leaves_only_the_second_measurer() {
        struct Counting(Arc<AtomicUsize>);
        impl CallMeasure for Counting {
            fn measure_and_publish(&self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let slot: CallMeasureSlot = Arc::default();

        *slot.write().unwrap() = Some(Arc::new(Counting(first.clone())));
        *slot.write().unwrap() = Some(Arc::new(Counting(second.clone())));

        let measurer = slot.read().unwrap().clone().expect("attached");
        measurer.measure_and_publish();

        assert_eq!(
            first.load(Ordering::Relaxed),
            0,
            "the first turn's measurer is gone"
        );
        assert_eq!(second.load(Ordering::Relaxed), 1, "the live one measured");
    }
}
