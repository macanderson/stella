// SPDX-License-Identifier: AGPL-3.0-only
//! Which measured change a transcript row may claim as its own (#4213).
//!
//! # The gap this closes
//!
//! `ToolRegistry::measures_alone` decides *which calls get measured* from each
//! tool's own schema (`read_only`, `parallel_safe`), so `bash`, every MCP tool
//! and every custom script tool is measured the moment it returns, exactly as
//! `edit_file` is (#4175, `stella_tools::call_measure`). Attribution in the
//! deck was gated on something else entirely — `summarize::is_file_mutation`, a
//! four-name list — so a `bash` running `sed -i` produced a real per-call
//! `FileChange` that the Files tab counted and no transcript row ever claimed.
//!
//! Widening the name list would not have been the fix. A `bash` call names no
//! path in its arguments, so `summarize::tool_input_path` has nothing to hand
//! the row; the path has to come from the **measurement**. And a name list
//! pointed at a path's latest change is exactly the misattribution #4227 took
//! out: a call that moved nothing would render whoever moved it next.
//!
//! # The rule
//!
//! A row claims the change that folded *under its own call* — between its
//! `ToolStart` and its `ToolResult` — whatever the tool is called. That is the
//! same discipline #4227 landed, stated positionally instead of by name:
//!
//! - The registry measures a solo call and publishes on the channel the engine
//!   then sends `ToolResult` on, so the `FileChange` lands strictly inside the
//!   window. Something in the window means this call moved the tree.
//! - The turn boundary sweeps what no single call can own and emits after every
//!   `ToolResult` of the turn has folded, so it never lands inside one. An
//!   empty window is therefore a measurement, not an absence of one — the call
//!   moved nothing, and the row says so by claiming nothing.
//!
//! A claim is **consumed** when it is taken, which is what keeps one change to
//! one row without any name-based reasoning. It also shuts the one interleaving
//! the fold cannot rule out from the outside: a measured call runs alone by
//! construction (`call_measure`'s scope note), but a concurrently dispatched
//! read whose `ToolStart` merely *precedes* a solo call's would otherwise see
//! that call's change inside its own window too. The solo call's result folds
//! first and takes it, and the read then finds nothing.
//!
//! The two predicates still fail safe in both directions, which the issue asks
//! to preserve: a measured call the deck cannot name claims a real change of
//! its own, and a call that was not measured has nothing to claim.

use std::collections::VecDeque;

use super::entry::InlineDiffRef;

/// How many unclaimed measurements the window retains.
///
/// One measured call publishes one `FileChange` per changed path, so this
/// bounds a codemod's fan-out, not a session's history: everything recorded
/// under a call is taken (or dropped) by that call's result, one event later.
/// A cap is still owed, because nothing on the wire promises a `ToolResult`
/// for every `ToolStart` — a turn abandoned mid-call would otherwise leave its
/// measurements in the window for the life of the session.
const MAX_UNCLAIMED: usize = 64;

/// One measured change, waiting for the row that made it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Claim {
    /// Position in the monotonic record order — what a call's baseline is
    /// compared against. Not `FileState::changes`, which is per path and so
    /// cannot order two paths against one call's window.
    index: u64,
    path: String,
    seq: u32,
}

/// The measurements folded so far that no transcript row has claimed.
///
/// Reconstructible by replay like every other field of
/// [`super::SessionModel`]: it is a pure function of the event order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ClaimWindow {
    entries: VecDeque<Claim>,
    next_index: u64,
}

impl ClaimWindow {
    /// The position a call dispatching *now* should record as its baseline.
    pub(super) fn open(&self) -> u64 {
        self.next_index
    }

    /// Record a measured change to `path`, at the [`super::FileState::changes`]
    /// value it was remembered at.
    pub(super) fn record(&mut self, path: &str, seq: u32) {
        self.entries.push_back(Claim {
            index: self.next_index,
            path: path.to_string(),
            seq,
        });
        self.next_index += 1;
        while self.entries.len() > MAX_UNCLAIMED {
            self.entries.pop_front();
        }
    }

    /// Take the change a call that dispatched at `since` may claim, consuming
    /// every measurement that folded under it.
    ///
    /// `prefer` is the path the call's own arguments named, when it named one:
    /// a measurement of that path is the call's subject, and anything else that
    /// moved while it ran is a bystander (a human saving in another window —
    /// `call_measure`'s "observability, never evidence"). With no named path,
    /// or none of the measurements matching it, the newest is the row's best
    /// answer: it is the one whose post-state the tree reading describes.
    ///
    /// A multi-path call still surfaces exactly one change, because a row has
    /// one inline diff to give — see #4214.
    pub(super) fn claim(&mut self, since: u64, prefer: Option<&str>) -> Option<InlineDiffRef> {
        let first = self.entries.partition_point(|c| c.index < since);
        let mine: Vec<Claim> = self.entries.drain(first..).collect();
        let claim = prefer
            .and_then(|path| mine.iter().rev().find(|c| c.path == path))
            .or_else(|| mine.last())?;
        Some(InlineDiffRef {
            path: claim.path.clone(),
            seq: claim.seq,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape every measured call has: a baseline taken at dispatch, one
    /// measurement under it, claimed by its own result.
    #[test]
    fn a_call_claims_the_change_recorded_under_it() {
        let mut window = ClaimWindow::default();
        let base = window.open();
        window.record("src/a.rs", 1);

        assert_eq!(
            window.claim(base, None),
            Some(InlineDiffRef {
                path: "src/a.rs".into(),
                seq: 1,
            })
        );
    }

    /// A claim is consumed: the row that folds first takes it, and no later row
    /// can render the same change as its own.
    #[test]
    fn a_claimed_change_is_gone() {
        let mut window = ClaimWindow::default();
        let earlier = window.open();
        let later = window.open();
        window.record("src/a.rs", 1);

        assert!(
            window.claim(later, None).is_some(),
            "the solo call takes it"
        );
        assert!(
            window.claim(earlier, None).is_none(),
            "a call whose window merely overlapped finds nothing left"
        );
    }

    /// Nothing recorded under a call is a measurement, not a gap — the row
    /// claims nothing rather than reaching backwards for the previous change.
    #[test]
    fn a_call_with_an_empty_window_claims_nothing() {
        let mut window = ClaimWindow::default();
        window.record("src/a.rs", 1);
        let base = window.open();

        assert_eq!(window.claim(base, None), None);
    }

    /// A call that named a path claims that path, even when something else
    /// moved while it ran and was measured later in the same window.
    #[test]
    fn a_named_path_wins_over_a_bystander() {
        let mut window = ClaimWindow::default();
        let base = window.open();
        window.record("src/subject.rs", 3);
        window.record("src/bystander.rs", 1);

        let claimed = window.claim(base, Some("src/subject.rs")).expect("claimed");
        assert_eq!(claimed.path, "src/subject.rs");
        assert_eq!(claimed.seq, 3);
    }

    /// With no named path — every `bash`, MCP and custom-tool call — the newest
    /// measurement answers, because it is the one the tree reading describes.
    #[test]
    fn an_unnamed_call_claims_the_newest_measurement() {
        let mut window = ClaimWindow::default();
        let base = window.open();
        window.record("src/a.rs", 1);
        window.record("src/b.rs", 1);

        assert_eq!(
            window.claim(base, None).map(|r| r.path),
            Some("src/b.rs".into())
        );
    }

    /// The window is bounded, and bounded from the front: an abandoned call's
    /// measurements age out while the newest — the ones a live call is about to
    /// claim — stay.
    #[test]
    fn the_window_retains_the_newest_measurements() {
        let mut window = ClaimWindow::default();
        let base = window.open();
        for n in 0..(MAX_UNCLAIMED as u32 + 10) {
            window.record("src/a.rs", n);
        }

        assert_eq!(window.entries.len(), MAX_UNCLAIMED);
        assert_eq!(
            window.claim(base, None).map(|r| r.seq),
            Some(MAX_UNCLAIMED as u32 + 9)
        );
    }
}
