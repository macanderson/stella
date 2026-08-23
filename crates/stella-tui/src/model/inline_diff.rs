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
//! A row claims **every** change that folded *under its own call* — between its
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
//! One call can move several paths — an `apply_edits` batch, a `bash` codemod —
//! and the per-call producer emits one `FileChange` for each, so a window can
//! hold several. All of them are that call's, and the row takes all of them
//! (#4214): taking one and leaving the others to expire is how a twenty-file
//! codemod rendered as a one-file edit. Which one *leads* — and so which one the
//! row draws inline — is the only thing `prefer` decides.
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

    /// Take **every** change a call that dispatched at `since` may claim,
    /// consuming each measurement that folded under it.
    ///
    /// The returned order is the row's reading order, and its head is what the
    /// row renders inline. `prefer` is the path the call's own arguments named,
    /// when it named one: a measurement of that path is the call's subject and
    /// leads, because it is what the reader came to the row to see. With no
    /// named path — every `bash`, MCP and custom-tool call — the newest leads,
    /// since it is the one whose post-state the tree reading describes. The
    /// rest follow in the order they were measured.
    ///
    /// Everything under the window is returned, not just the head. A call that
    /// rewrites twenty files measured twenty changes and the row states all
    /// twenty (#4214); before this it took one and the other nineteen expired
    /// unclaimed, so a multi-file batch rendered one of its files and silently
    /// dropped the rest. The consequence is that a *bystander* — a human
    /// saving in another window while a measured call ran — is now counted by
    /// the row rather than discarded. That is the honest trade: the claim
    /// window cannot tell the two apart, and under-reporting a real codemod on
    /// every call is a larger error than over-reporting an edit nobody else was
    /// making. `prefer` still decides which change *leads*, so the subject is
    /// what shows.
    ///
    /// Empty when nothing folded under the call — a measurement that the call
    /// moved nothing, not an absence of one.
    pub(super) fn claim(&mut self, since: u64, prefer: Option<&str>) -> Vec<InlineDiffRef> {
        let first = self.entries.partition_point(|c| c.index < since);
        let mine: Vec<Claim> = self.entries.drain(first..).collect();
        let Some(lead) = prefer
            .and_then(|path| mine.iter().rposition(|c| c.path == path))
            .or_else(|| mine.len().checked_sub(1))
        else {
            return Vec::new();
        };
        std::iter::once(&mine[lead])
            .chain(mine.iter().enumerate().filter_map(|(i, c)| {
                // The lead is already first; re-emitting it would give one
                // change two rows in the same call's own list, which is the
                // property `claim` exists to hold.
                (i != lead).then_some(c)
            }))
            .map(|c| InlineDiffRef {
                path: c.path.clone(),
                seq: c.seq,
            })
            .collect()
    }
}

/// How many distinct paths this set of claims spans.
///
/// The count the row states (`3 files`) is of **paths**, not of claims: two
/// measurements of one file are one file, and a row saying otherwise would be
/// a number nothing measured. Order-preserving rather than sorted, so it is a
/// pure count of what is there.
#[must_use]
pub(crate) fn distinct_paths(refs: &[InlineDiffRef]) -> usize {
    let mut seen: Vec<&str> = Vec::with_capacity(refs.len());
    for r in refs {
        if !seen.contains(&r.path.as_str()) {
            seen.push(&r.path);
        }
    }
    seen.len()
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
            vec![InlineDiffRef {
                path: "src/a.rs".into(),
                seq: 1,
            }]
        );
    }

    /// **Witness (#4214).** A call that moved three paths claims all three, in
    /// reading order: the subject it named first, the bystanders behind it.
    ///
    /// Fails on the singular `Option`, which returned the subject and dropped
    /// the other two on the floor.
    #[test]
    fn a_multi_path_call_claims_every_change() {
        let mut window = ClaimWindow::default();
        let base = window.open();
        window.record("crates/a/src/lib.rs", 1);
        window.record("crates/b/src/lib.rs", 1);
        window.record("crates/c/src/lib.rs", 1);

        let claimed = window.claim(base, Some("crates/b/src/lib.rs"));
        assert_eq!(
            claimed.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            vec![
                "crates/b/src/lib.rs",
                "crates/a/src/lib.rs",
                "crates/c/src/lib.rs",
            ],
        );
        assert_eq!(distinct_paths(&claimed), 3);
    }

    /// Two edits to one file are one file: the count the row states is of
    /// paths, so a repeated path does not inflate it.
    #[test]
    fn two_changes_to_one_path_are_one_file() {
        let refs = [
            InlineDiffRef {
                path: "src/a.rs".into(),
                seq: 1,
            },
            InlineDiffRef {
                path: "src/a.rs".into(),
                seq: 2,
            },
        ];
        assert_eq!(distinct_paths(&refs), 1);
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
            !window.claim(later, None).is_empty(),
            "the solo call takes it"
        );
        assert!(
            window.claim(earlier, None).is_empty(),
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

        assert_eq!(window.claim(base, None), Vec::new());
    }

    /// A call that named a path *leads* with that path, even when something
    /// else moved while it ran and was measured later in the same window.
    ///
    /// The bystander is still claimed (#4214) — it is behind the subject, not
    /// discarded, because the window cannot tell a bystander from the second
    /// file of a batch.
    #[test]
    fn a_named_path_leads_over_a_bystander() {
        let mut window = ClaimWindow::default();
        let base = window.open();
        window.record("src/subject.rs", 3);
        window.record("src/bystander.rs", 1);

        let claimed = window.claim(base, Some("src/subject.rs"));
        assert_eq!(claimed[0].path, "src/subject.rs");
        assert_eq!(claimed[0].seq, 3);
        assert_eq!(claimed[1].path, "src/bystander.rs");
    }

    /// With no named path — every `bash`, MCP and custom-tool call — the newest
    /// measurement leads, because it is the one the tree reading describes.
    #[test]
    fn an_unnamed_call_leads_with_the_newest_measurement() {
        let mut window = ClaimWindow::default();
        let base = window.open();
        window.record("src/a.rs", 1);
        window.record("src/b.rs", 1);

        let claimed = window.claim(base, None);
        assert_eq!(claimed[0].path, "src/b.rs");
        assert_eq!(claimed[1].path, "src/a.rs");
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
        assert_eq!(window.claim(base, None)[0].seq, MAX_UNCLAIMED as u32 + 9);
    }
}
