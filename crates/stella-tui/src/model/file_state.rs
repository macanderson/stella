//! One file's state in the files-touched panel, and the diff history that lets
//! an older tool result still render the change *it* made.
//!
//! The history is the point. Keeping only the newest diff per path made the
//! transcript erase itself: a result resolved its diff by checking the path's
//! mutation counter still matched the value recorded when the result folded,
//! so the second edit to a file silently stripped the first edit's diff off
//! its row. Attribution was always the goal — this reaches it by keeping the
//! right diff rather than by dropping every diff that might be the wrong one.
//!
//! # What bounds it, and why it is not a count
//!
//! The history used to be bounded twice by counts: eight diffs per path, and
//! 256 paths with LRU eviction. Both were guesses, and a census of the
//! recorded session journals under `~/.stella/sessions/` settled them (#4365).
//! Over the 8,448 mutating `FileChange` events in the 25 journals that carry
//! any:
//!
//! * the eight-deep per-path ring took the diff off **14** rows — 0.17%;
//! * the 256-path cap took it off **7,336** — 86.8%, because a session that
//!   sweeps a tree passes 256 paths long before it edits one file nine times;
//! * and neither bounded a single **byte**. One recorded session peaked at
//!   45.4 MiB of retained diff text under those caps, which is what a count
//!   of paths cannot see: its diffs averaged 47 KB each.
//!
//! So the depth cap is gone, the path cap is the point past which it stops
//! deciding anything (see [`MAX_TRACKED_FILES`]), and the real bound is
//! [`super::diff_budget::DIFF_TEXT_BUDGET`] — bytes, released oldest first.
//! Under it every recorded session but that one keeps every row's diff, and
//! the peak retained falls to 32 MiB.

use std::collections::VecDeque;

use stella_protocol::FileChangeKind;

/// The state of one file in the files-touched panel. [`Self::latest_diff`] is
/// literally the diff carried by the most recent *mutating* `FileChange` for
/// this path — the single event-borne data path (L-T5). Reads never touch
/// `kind`/`changes` (the latter doubles as the inline-diff freshness tag, so a
/// read bumping it would hide a still-current diff); they only grow `reads`.
#[derive(Debug, Clone, PartialEq)]
pub struct FileState {
    pub path: String,
    pub kind: FileChangeKind,
    /// Cumulative lines added / removed across this path's mutations, summed
    /// from the counts the emitter measured (`Pipeline::deliver_winner`).
    /// Never re-derived from `latest_diff`: the diff is a bounded rendering of
    /// the changed region, so counting it is a different — and smaller —
    /// number than the delta actually applied.
    pub added: u32,
    pub removed: u32,
    /// Every mutation of this path, each tagged with the `changes` value it
    /// was recorded at and the delta it carried, newest last and so ordered by
    /// `seq`.
    ///
    /// Keeping only the newest diff made the transcript *erase* its own
    /// history: the second edit to a file bumped `changes` past the seq the
    /// first edit's result recorded, so that result's diff stopped resolving
    /// and its row silently lost the change it had made. Scrolling back
    /// through a session that touched one file five times showed four edits
    /// with no diff and one with. The bytes still live on the single
    /// event-borne path (L-T5) — this is the same store, given a memory.
    ///
    /// An entry is never dropped; only its [`RememberedDiff::text`] is
    /// released, and only under [`super::diff_budget::DIFF_TEXT_BUDGET`]. The
    /// 12 bytes of `(seq, added, removed)` that survive are what let a row
    /// whose diff has been released still state `+N −M` rather than nothing —
    /// the #4155 shape, reached deliberately this time.
    pub recent_diffs: VecDeque<RememberedDiff>,
    /// How many mutating `FileChange` events have touched this path.
    pub changes: u32,
    /// How many times this path has been read.
    pub reads: u32,
    /// The model-wide touch counter's value at this path's most recent touch
    /// (read or mutation) — the recency key [`MAX_TRACKED_FILES`] eviction
    /// orders by. Purely event-derived, so replay determinism (L-T1) holds.
    pub touched_seq: u64,
}

/// One remembered mutation: its diff text, the `changes` seq it happened at
/// (so an older tool result still resolves the change *it* made), and the
/// delta the emitter measured for it.
#[derive(Debug, Clone, PartialEq)]
pub struct RememberedDiff {
    pub seq: u32,
    /// The mutation's diff text, `None` when the emitter measured the change
    /// but could not attach a patch for it.
    ///
    /// Counts and diff text arrive independently — a producer builds each
    /// change from a name-status listing, then attaches numstat and diff text
    /// in two calls, either of which can fail, and [`FileState::best_diff`]
    /// names `diff: None` with real counts as a legitimate shape. Recording
    /// the entry only when text was present conflated "no patch" with "no
    /// measurement": [`FileState::delta_at`] answered `None` for a mutation whose
    /// `(added, removed)` the emitter had measured, so the row lost its
    /// `+N −M` as well as its diff (#4155).
    pub text: Option<String>,
    pub added: u32,
    pub removed: u32,
}

/// How many distinct paths the model tracks before evicting the
/// least-recently-touched one (#803).
///
/// **Measured, not chosen.** With the diff *text* bounded in bytes by
/// [`super::diff_budget::DIFF_TEXT_BUDGET`], a tracked path costs its own name
/// plus 12 bytes per mutation, so the question this number answers is only
/// "when does the row count start releasing diffs the byte budget would have
/// kept". Replayed over the recorded journals, a 32 MiB budget releases the
/// same 5,664 diffs at a cap of 2,048 as at 4,096 or 8,192, and 6,342 at
/// 1,024: past 2,048 the row count decides nothing and the budget decides
/// everything, which is the property this number is picked for.
///
/// At the old 256 it decided almost everything — 7,336 of the 7,350 rows that
/// lost their diff lost it here rather than to depth — while bounding no bytes
/// at all (module docs above).
///
/// Eviction stays user-visible: the files panel title carries the evicted
/// count, and an evicted path's transcript rows degrade to naming their
/// change.
pub const MAX_TRACKED_FILES: usize = 2048;

/// Remove the least-recently-touched entry — the [`MAX_TRACKED_FILES`]
/// eviction policy. The victim's diffs go with it, so transcript rows that
/// pointed at them degrade to naming their change. Returns the evicted path,
/// so the caller can release its bytes from the budget; `None` only on an
/// empty slice.
pub(crate) fn evict_lru(files: &mut Vec<FileState>) -> Option<String> {
    let oldest = files
        .iter()
        .enumerate()
        .min_by_key(|(_, f)| f.touched_seq)
        .map(|(i, _)| i)?;
    Some(files.remove(oldest).path)
}

impl FileState {
    /// Record a mutation against the `changes` value it produced, whether or
    /// not the emitter attached a patch for it. Called only for mutations, and
    /// only after `changes` has been bumped, so the tag matches the seq the
    /// mutation's own tool result stamped.
    ///
    /// A change carrying `diff: None` is recorded too: it still measured an
    /// `(added, removed)` the row wants to state, and skipping it made
    /// [`Self::delta_at`] deny a measurement that existed (#4155).
    /// [`Self::best_diff`] therefore scans back for the newest entry that
    /// *has* text rather than reading the last one.
    pub(crate) fn remember_diff(&mut self, diff: &Option<String>, added: u32, removed: u32) {
        self.recent_diffs.push_back(RememberedDiff {
            seq: self.changes,
            text: diff.clone(),
            added,
            removed,
        });
    }

    /// Release the text remembered for mutation `seq`, keeping the entry and
    /// its measured delta. Returns the bytes freed — 0 when the entry is gone
    /// or already textless.
    ///
    /// This is what the byte budget spends: a released row states `+N −M` and
    /// no diff, which is a weaker reading than the diff but a true one, and
    /// strictly more than the nothing a dropped entry leaves.
    pub(crate) fn release_text(&mut self, seq: u32) -> usize {
        let Some(entry) = self.remembered_mut(seq) else {
            return 0;
        };
        entry.text.take().map_or(0, |text| text.len())
    }

    /// The bytes of remembered diff text this path actually holds.
    ///
    /// Test-only, and the reason it exists: the budget's running total is an
    /// accounting of what the ledger stores, and an accounting that can drift
    /// from what is stored is not a bound. Summed over `files` it is what the
    /// budget claims, or the claim is wrong.
    #[cfg(test)]
    pub(crate) fn text_bytes(&self) -> usize {
        self.recent_diffs
            .iter()
            .filter_map(|d| d.text.as_ref())
            .map(String::len)
            .sum()
    }

    /// The diff this path produced at mutation `seq`, if still remembered and
    /// if that mutation carried a patch at all.
    #[must_use]
    pub fn diff_at(&self, seq: u32) -> Option<&str> {
        self.remembered_at(seq).and_then(|d| d.text.as_deref())
    }

    /// That mutation's measured `(added, removed)` — the numbers a transcript
    /// row shows beside its inline diff, taken from the emitter rather than
    /// counted back out of the rendered text.
    #[must_use]
    pub fn delta_at(&self, seq: u32) -> Option<(u32, u32)> {
        self.remembered_at(seq).map(|d| (d.added, d.removed))
    }

    /// The diff carried by this path's most recent mutation, or `None` when
    /// that mutation brought none — or when the budget has released its text.
    ///
    /// A method rather than a field. It used to be an owned `String` beside
    /// `recent_diffs`, which meant every tracked path held a second copy of
    /// its newest diff; with the path cap raised to [`MAX_TRACKED_FILES`] that
    /// duplicate would have been the largest single cost of the change that
    /// raised it (#4365). Reading it off the back of the history cannot
    /// disagree with the history, either — the class of defect #1741 was.
    #[must_use]
    pub fn latest_diff(&self) -> Option<&str> {
        self.recent_diffs.back().and_then(|d| d.text.as_deref())
    }

    /// Entries are pushed with a strictly increasing `seq` (it is `changes`,
    /// bumped once per mutation), so the lookup is a binary search rather than
    /// a scan — a path edited a thousand times is now a thousand entries, and
    /// every visible row resolves one of them on every frame.
    fn remembered_at(&self, seq: u32) -> Option<&RememberedDiff> {
        let i = self
            .recent_diffs
            .binary_search_by_key(&seq, |d| d.seq)
            .ok()?;
        self.recent_diffs.get(i)
    }

    fn remembered_mut(&mut self, seq: u32) -> Option<&mut RememberedDiff> {
        let i = self
            .recent_diffs
            .binary_search_by_key(&seq, |d| d.seq)
            .ok()?;
        self.recent_diffs.get_mut(i)
    }

    /// The best diff this path can show, and whether it describes the path's
    /// CURRENT state.
    ///
    /// `Some((text, true))` is [`Self::latest_diff`] — the most recent
    /// mutation's own diff. `Some((text, false))` is the newest diff still
    /// remembered, from an EARLIER mutation, and a caller that renders it owes
    /// the reader that distinction.
    ///
    /// The fallback exists because a mutating `FileChange` may legitimately
    /// carry `diff: None` while carrying counts: the pipeline's adoption
    /// re-emit builds each change from `git diff --name-status`, then attaches
    /// numstat and diff text in two independent calls, either of which can
    /// fail. A path only the numstat named keeps `diff: None` on purpose
    /// (`attach_diffs` would rather report nothing than misattribute a patch).
    ///
    /// Without this, such an event left the pane claiming `(no diff captured)`
    /// for a file whose diff was sitting one field away in `recent_diffs`
    /// (#1741). `latest_diff` keeps its documented meaning — the diff OF the
    /// most recent mutation, `None` when that mutation brought none — rather
    /// than being quietly widened to "the last diff we saw", which would have
    /// put an older change's text under a newer change's counts with nothing
    /// on screen to say so.
    #[must_use]
    pub fn best_diff(&self) -> Option<(&str, bool)> {
        if let Some(current) = self.latest_diff() {
            return Some((current, true));
        }
        // The newest entry that carries text, not simply the newest: since
        // #4155 a measured-but-patchless change is remembered too, and reading
        // `back()` alone would answer `None` for a path whose diff is sitting
        // one entry further in — the exact regression #1741 fixed.
        self.recent_diffs
            .iter()
            .rev()
            .find_map(|d| d.text.as_deref().filter(|text| !text.is_empty()))
            .map(|text| (text, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SessionModel;
    use stella_protocol::AgentEvent;

    /// The model half of #1741: a mutation carrying no diff must not destroy
    /// the one before it, and `best_diff` must say which mutation it is
    /// handing back.
    ///
    /// `latest_diff` keeps its documented meaning — the diff OF the most
    /// recent mutation, `None` when that mutation brought none. Widening it to
    /// "the last diff we saw" would have fixed the blank pane by making the
    /// field lie, which is how the pane ends up showing an older change's text
    /// under a newer change's counts with nothing to mark it.
    #[test]
    fn a_diffless_mutation_leaves_the_previous_diff_reachable_and_marked_stale() {
        let mut model = SessionModel::new();
        let mutate = |diff: Option<String>, added: u32| AgentEvent::FileChange {
            path: "src/a.rs".into(),
            kind: FileChangeKind::Modified,
            added,
            removed: 0,
            diff,
        };

        model.apply(&mutate(Some("@@\n+first".into()), 1));
        let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
        assert_eq!(
            file.best_diff(),
            Some(("@@\n+first", true)),
            "the only mutation's own diff is current"
        );

        // The adoption shape: mutating, real counts, no text.
        model.apply(&mutate(None, 5));
        let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
        assert_eq!(
            file.latest_diff(),
            None,
            "it stays honest — this mutation really brought no diff"
        );
        assert_eq!(
            file.best_diff(),
            Some(("@@\n+first", false)),
            "but the earlier diff is still reachable, and flagged as earlier"
        );
        assert_eq!(
            (file.added, file.removed),
            (6, 0),
            "counts stay cumulative and transported, never recounted from text"
        );
    }

    /// **The witness (#4365).** The ninth mutation of one path leaves the
    /// first mutation's diff resolving.
    ///
    /// Fails on the eight-deep ring this replaced: `diff_at(1)` answered
    /// `None` the moment the ninth entry pushed the first off the front, so
    /// scrolling back to the first edit's row rendered it with no diff.
    #[test]
    fn a_ninth_mutation_leaves_the_first_ones_diff_resolvable() {
        let mut model = SessionModel::new();
        for i in 1..=9 {
            model.apply(&AgentEvent::FileChange {
                path: "src/a.rs".into(),
                kind: FileChangeKind::Modified,
                added: 1,
                removed: 0,
                diff: Some(format!("@@\n+edit_{i}")),
            });
        }
        let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
        assert_eq!(file.changes, 9);
        for seq in 1..=9 {
            assert_eq!(
                file.diff_at(seq),
                Some(format!("@@\n+edit_{seq}").as_str()),
                "mutation {seq} lost the diff its own row renders"
            );
            assert_eq!(file.delta_at(seq), Some((1, 0)));
        }
    }

    /// **The witness (#4365), second half.** A path admitted past the old
    /// 256-path cap keeps every row's diff.
    ///
    /// Fails at `MAX_TRACKED_FILES = 256`: the 257th path evicted the first,
    /// taking its whole history with it. This is the loss that dominated the
    /// census — 7,336 of 7,350 rows — and it is why the cap moved rather than
    /// the depth.
    #[test]
    fn a_path_past_the_old_cap_keeps_its_diff() {
        let mut model = SessionModel::new();
        let change = |path: String| AgentEvent::FileChange {
            path,
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 0,
            diff: Some("@@\n+x".into()),
        };
        model.apply(&change("src/first.rs".into()));
        for i in 0..300 {
            model.apply(&change(format!("src/f{i}.rs")));
        }
        let file = model
            .files
            .iter()
            .find(|f| f.path == "src/first.rs")
            .expect("the first path is still tracked past 256 others");
        assert_eq!(file.diff_at(1), Some("@@\n+x"));
        assert_eq!(model.files_evicted, 0);
    }

    /// The byte budget is what bounds the history now, and it spends the
    /// **text** rather than the entry: a released row still states the
    /// `+N −M` its emitter measured.
    ///
    /// Also checks the accounting itself — a running total that can drift
    /// from what the ledger stores is not a bound, so the two are compared.
    #[test]
    fn the_byte_budget_releases_the_oldest_text_and_keeps_its_counts() {
        let mut model = SessionModel::new();
        // Five quarter-budget diffs: the fifth cannot fit beside the other
        // four, so exactly the first is released.
        let chunk = crate::model::DIFF_TEXT_BUDGET / 4;
        for _ in 0..5 {
            model.apply(&AgentEvent::FileChange {
                path: "src/a.rs".into(),
                kind: FileChangeKind::Modified,
                added: 2,
                removed: 1,
                diff: Some("x".repeat(chunk)),
            });
        }
        let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
        assert_eq!(file.changes, 5);
        assert!(
            file.diff_at(1).is_none(),
            "the oldest text is what the budget spent"
        );
        assert_eq!(
            file.delta_at(1),
            Some((2, 1)),
            "and the row it belongs to still states its size"
        );
        assert!(
            file.diff_at(5).is_some(),
            "the newest mutation — the one on screen — keeps its diff"
        );
        let held: usize = model.files.iter().map(FileState::text_bytes).sum();
        assert!(
            held <= crate::model::DIFF_TEXT_BUDGET,
            "{held} bytes held against a {} byte budget",
            crate::model::DIFF_TEXT_BUDGET
        );
    }

    /// #803: the ledger holds at [`MAX_TRACKED_FILES`], evicting by recency
    /// (not insertion order) and counting what it dropped so the files panel
    /// can say so. Driven through the real fold, so replay determinism
    /// (L-T1) covers the eviction path too.
    #[test]
    fn the_file_ledger_evicts_the_least_recently_touched_path_at_the_cap() {
        let mut model = SessionModel::new();
        let change = |path: String| AgentEvent::FileChange {
            path,
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 0,
            diff: Some("@@\n+x".into()),
        };
        for i in 0..MAX_TRACKED_FILES {
            model.apply(&change(format!("src/f{i}.rs")));
        }
        assert_eq!(model.files.len(), MAX_TRACKED_FILES);
        assert_eq!(model.files_evicted, 0);

        // Re-touch the oldest path, then admit a new one: the *second*-oldest
        // is the LRU victim.
        model.apply(&change("src/f0.rs".into()));
        model.apply(&change("src/new.rs".into()));

        assert_eq!(model.files.len(), MAX_TRACKED_FILES, "cap holds");
        assert_eq!(model.files_evicted, 1);
        assert!(model.files.iter().any(|f| f.path == "src/f0.rs"));
        assert!(model.files.iter().any(|f| f.path == "src/new.rs"));
        assert!(
            !model.files.iter().any(|f| f.path == "src/f1.rs"),
            "the least-recently-touched path was evicted"
        );
    }

    /// `/clear` keeps the file ledger, so it must also keep the counter that
    /// stamps [`FileState::touched_seq`] — the recency key this eviction orders
    /// by. Restarting it at 0 under retained files ranks every surviving path
    /// above every new one, and the cap then evicts newest-first: the path the
    /// agent is working on right now is the one that disappears.
    #[test]
    fn a_conversation_reset_keeps_the_recency_key_so_eviction_stays_oldest_first() {
        let mut model = SessionModel::new();
        let change = |path: String| AgentEvent::FileChange {
            path,
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 0,
            diff: Some("@@\n+x".into()),
        };
        for i in 0..MAX_TRACKED_FILES {
            model.apply(&change(format!("src/f{i}.rs")));
        }

        model.reset_conversation();

        // The first post-clear touch admits a new path at the cap: the victim
        // must be the oldest pre-clear file, never this arrival.
        model.apply(&change("src/after_clear.rs".into()));

        assert_eq!(model.files.len(), MAX_TRACKED_FILES, "cap holds");
        assert!(
            model.files.iter().any(|f| f.path == "src/after_clear.rs"),
            "the newly touched path must not be its own eviction victim"
        );
        assert!(
            !model.files.iter().any(|f| f.path == "src/f0.rs"),
            "the oldest pre-clear path is the victim"
        );
    }
}
