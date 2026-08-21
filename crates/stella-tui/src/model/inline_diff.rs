// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which change a mutating tool row renders, and how it learns that.
//!
//! # The problem, and the two answers that did not hold
//!
//! [`super::FileState`] remembers a path's last
//! [`DIFF_HISTORY`](super::file_state::DIFF_HISTORY) diffs, each tagged with
//! the `changes` seq it happened at, and `render::resolve_inline_diff` looks
//! one up by that seq. So a transcript row needs to name *its own* change's
//! seq, and the whole question is where that number comes from.
//!
//! It used to be arithmetic on the counter, and there is one such answer per
//! producer of `AgentEvent::FileChange` — which is why there were two, why
//! they contradicted each other, and why neither was right on its own (#4155,
//! #4203):
//!
//! - **`changes + 1`**, predicting a bump still to come. Correct for the
//!   turn-boundary sweep, which emits one aggregate per path *after* every
//!   result of the turn has folded. Every call that touched a path in one turn
//!   therefore predicted the *same* seq, so a second pass had to walk the
//!   transcript and blank all but the last row — and that surviving row then
//!   showed the whole turn's change to the file as its own work.
//! - **`changes` unadjusted**, predicting a bump already taken. Correct for
//!   per-call measurement (#4175), which measures the moment a solo mutating
//!   call returns and publishes before that call's `ToolResult`. Silently
//!   wrong when the reading produced nothing — reachable, because a
//!   `write_file` of bytes identical to what is on disk succeeds and moves
//!   nothing, and `snapshot_worktree` is best-effort and infallible by
//!   signature, so an unreadable tree simply measures nothing. The counter
//!   then names the *previous* call's change and the row renders an older edit
//!   as its own.
//!
//! Both survive only while one producer is the only producer, and the per-call
//! producer is deliberately **partial** — it measures a call that ran alone and
//! succeeded, leaving `delegate`'s writes, a concurrently-dispatched tool's,
//! and anything a human did in another window to the boundary sweep. So both
//! orderings are live in the same session, and an answer tuned to either one
//! is wrong for the other half of it.
//!
//! # The answer that does hold: observe, do not predict
//!
//! `ClaimWindow` records the mutations that fold *while a call is in flight*
//! and hands each one to at most one result row. A row shows a change it was
//! observed to produce, or nothing at all.
//!
//! That is not merely more accurate, it fails in the better direction. A
//! prediction that is wrong renders a plausible lie — a real diff, correctly
//! formatted, under the wrong call. A missing observation renders nothing, and
//! a reader can see nothing. It is also producer-agnostic in a way neither
//! prediction was: a third producer, or a change in when an existing one
//! emits, moves what rows can claim without making any row claim wrongly.

/// A mutating tool result's handle on the diff it may render inline: the path
/// into [`SessionModel::files`](super::SessionModel::files) plus the `changes`
/// seq of the mutation *this call produced*. The renderer resolves it against
/// [`FileState::recent_diffs`](super::FileState::recent_diffs), so a row shows
/// the change its own call made and never a neighbour's. Only the *reference*
/// lives here; the diff bytes stay on the single event-borne path (L-T5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineDiffRef {
    /// The key into [`SessionModel::files`](super::SessionModel::files).
    pub path: String,
    /// The [`FileState::changes`](super::FileState::changes) value this call's
    /// own mutation is recorded at.
    ///
    /// **Observed, never computed** — see this module's docs for the two
    /// arithmetic answers this replaced and what each of them cost. A call
    /// under which no change folded gets no `InlineDiffRef` at all, and its
    /// row degrades to naming its change rather than showing it: the same
    /// degradation [`DIFF_HISTORY`](super::file_state::DIFF_HISTORY) aging and
    /// [`MAX_TRACKED_FILES`](super::MAX_TRACKED_FILES) eviction already have.
    pub seq: u32,
}

/// The mutations a tool call has been observed to produce and not yet handed
/// to a row.
///
/// Plain synchronous logic over owned data, with no reference to the rest of
/// the model: everything it needs is the sequence of "a call started", "a
/// mutation folded", "a call settled" — which is what makes the attribution
/// rule testable on its own, without a transcript or a file ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ClaimWindow {
    /// Tool calls dispatched but not yet settled. A count, not a roster:
    /// nothing needs to know *which* calls are running, only whether any is,
    /// because that is what separates a change some call produced from one
    /// measured while the agent was not running a tool at all.
    in_flight: u32,
    /// Unclaimed mutations as `(path, seq)`, oldest first.
    unclaimed: Vec<(String, u32)>,
}

impl ClaimWindow {
    /// A call was dispatched.
    ///
    /// Opening a dispatch *group* is what closes the previous window:
    /// anything still unclaimed was measured while no call was in flight, or
    /// under a call whose row could not own it, and belongs to no row at all.
    ///
    /// On the 0 → 1 transition only, so a group larger than the engine's
    /// concurrency limit — where later `ToolStart`s are sent as slots free,
    /// after earlier siblings have already run — cannot clear a sibling's
    /// window mid-group.
    pub(super) fn call_started(&mut self) {
        if self.in_flight == 0 {
            self.unclaimed.clear();
        }
        self.in_flight += 1;
    }

    /// A call settled. Saturating, because the halted arm of
    /// `driver::dispatch::execute_tool_calls` answers a call it never started
    /// with a `ToolResult` and no `ToolStart` (#2661) — a result with no
    /// window to close. An underflow here would wrap to `u32::MAX` and make
    /// every later change in the session look attributable.
    pub(super) fn call_settled(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// Record a mutation, if a call was running to produce it.
    ///
    /// A mutation measured while nothing was in flight is the turn-boundary
    /// sweep reporting what the per-call readings did not claim — a
    /// `delegate`'s writes, a human editing in another window. It is dropped
    /// here rather than stored, which is both the attribution rule ("no call
    /// made this, so no row may show it") and what bounds this type: it never
    /// holds more than one call's worth of measured paths.
    pub(super) fn record(&mut self, path: &str, seq: u32) {
        if self.in_flight > 0 {
            self.unclaimed.push((path.to_string(), seq));
        }
    }

    /// Take the oldest unclaimed change to `path` and turn it into the
    /// reference a result row renders from — or `None` when no change to that
    /// path folded under this call.
    ///
    /// **Oldest first**, and that is the whole tiebreak: a per-call
    /// measurement is taken when a call *finishes*, and results fold in the
    /// order calls finish, so the earliest unclaimed change belongs to the
    /// earliest result still to fold. It matters only if two calls mutating
    /// one path are ever in flight together — which the engine does not do
    /// today, since a tool that is neither `read_only` nor `parallel_safe` is
    /// dispatched in a group of one — so this is the rule that keeps the
    /// answer right if that changes, rather than one that depends on it not
    /// changing.
    ///
    /// **Taking is the point.** A claimed change leaves the window, so a
    /// second row cannot claim it: "one change, one row" is a property of the
    /// data structure instead of a repair pass over the transcript. The
    /// `supersede_inline_diff` sweep this replaced had to walk every entry
    /// already rendered and blank the ones that had stamped the same
    /// `(path, seq)` — necessary only because the seq was predicted, and every
    /// call that touched a path in one turn predicted the same one.
    pub(super) fn claim(&mut self, path: &str) -> Option<InlineDiffRef> {
        let at = self.unclaimed.iter().position(|(p, _)| p == path)?;
        let (path, seq) = self.unclaimed.remove(at);
        Some(InlineDiffRef { path, seq })
    }
}

#[cfg(test)]
mod tests;
