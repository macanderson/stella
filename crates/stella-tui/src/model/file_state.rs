//! One file's state in the files-touched panel, and the short diff history
//! that lets an older tool result still render the change *it* made.
//!
//! The history is the point. Keeping only the newest diff per path made the
//! transcript erase itself: a result resolved its diff by checking the path's
//! mutation counter still matched the value recorded when the result folded,
//! so the second edit to a file silently stripped the first edit's diff off
//! its row. Attribution was always the goal — this reaches it by keeping the
//! right diff rather than by dropping every diff that might be the wrong one.

use std::collections::VecDeque;

use stella_protocol::FileChangeKind;

/// The state of one file in the files-touched panel. `latest_diff` is
/// literally the diff carried by the most recent *mutating* `FileChange` for
/// this path — the single event-borne data path (L-T5). Reads never touch
/// `kind`/`latest_diff`/`changes` (the latter doubles as the inline-diff
/// freshness tag, so a read bumping it would hide a still-current diff);
/// they only grow `reads`.
#[derive(Debug, Clone, PartialEq)]
pub struct FileState {
    pub path: String,
    pub kind: FileChangeKind,
    pub latest_diff: Option<String>,
    /// Cumulative lines added / removed across this path's mutations, summed
    /// from the counts the emitter measured (`ToolRegistry::record_touch`).
    /// Never re-derived from `latest_diff`: the diff is a bounded rendering of
    /// the changed region, so counting it is a different — and smaller —
    /// number than the delta actually applied.
    pub added: u32,
    pub removed: u32,
    /// The last [`DIFF_HISTORY`] diffs for this path, each tagged with the
    /// `changes` value it was recorded at and the delta it carried, newest
    /// last.
    ///
    /// Keeping only `latest_diff` made the transcript *erase* its own history:
    /// the second edit to a file bumped `changes` past the seq the first
    /// edit's result recorded, so that result's diff stopped resolving and its
    /// row silently lost the change it had made. Scrolling back through a
    /// session that touched one file five times showed four edits with no
    /// diff and one with. The bytes still live on the single event-borne path
    /// (L-T5) — this is the same store, given a shallow memory instead of a
    /// depth of one.
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
    pub text: String,
    pub added: u32,
    pub removed: u32,
}

/// How many distinct paths the model tracks before evicting the
/// least-recently-touched one. A days-long session sweeping a large tree
/// would otherwise keep up to nine unbounded diff strings per path forever
/// (#803). Eviction is user-visible: the files panel title carries the
/// evicted count, and an evicted path's transcript rows degrade to naming
/// their change — the same shape `DIFF_HISTORY` eviction already has.
pub const MAX_TRACKED_FILES: usize = 256;

/// How many past diffs a path keeps so older tool results can still render
/// the change they made. Deep enough to cover the common "edit the same file
/// a few times in one turn" shape; bounded so a file edited in a loop cannot
/// grow the model without limit. Beyond it, a result's row degrades to naming
/// the change rather than showing it — the pre-existing behaviour, now the
/// exception instead of the rule.
pub const DIFF_HISTORY: usize = 8;

/// Remove the least-recently-touched entry — the [`MAX_TRACKED_FILES`]
/// eviction policy. The victim's diffs go with it, so transcript rows that
/// pointed at them degrade to naming their change, the same shape
/// [`DIFF_HISTORY`] aging already has. Returns whether a row was removed
/// (false only on an empty slice).
pub(crate) fn evict_lru(files: &mut Vec<FileState>) -> bool {
    let Some(oldest) = files
        .iter()
        .enumerate()
        .min_by_key(|(_, f)| f.touched_seq)
        .map(|(i, _)| i)
    else {
        return false;
    };
    files.remove(oldest);
    true
}

impl FileState {
    /// Record a mutation's diff against the `changes` value it produced.
    /// Called only for mutations, and only after `changes` has been bumped, so
    /// the tag matches the seq a tool result folds at the same moment.
    pub(crate) fn remember_diff(&mut self, diff: &Option<String>, added: u32, removed: u32) {
        let Some(text) = diff else { return };
        self.recent_diffs.push_back(RememberedDiff {
            seq: self.changes,
            text: text.clone(),
            added,
            removed,
        });
        while self.recent_diffs.len() > DIFF_HISTORY {
            self.recent_diffs.pop_front();
        }
    }

    /// The diff this path produced at mutation `seq`, if still remembered.
    #[must_use]
    pub fn diff_at(&self, seq: u32) -> Option<&str> {
        self.remembered_at(seq).map(|d| d.text.as_str())
    }

    /// That mutation's measured `(added, removed)` — the numbers a transcript
    /// row shows beside its inline diff, taken from the emitter rather than
    /// counted back out of the rendered text.
    #[must_use]
    pub fn delta_at(&self, seq: u32) -> Option<(u32, u32)> {
        self.remembered_at(seq).map(|d| (d.added, d.removed))
    }

    fn remembered_at(&self, seq: u32) -> Option<&RememberedDiff> {
        self.recent_diffs.iter().find(|d| d.seq == seq)
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
        if let Some(current) = self.latest_diff.as_deref() {
            return Some((current, true));
        }
        self.recent_diffs
            .back()
            .map(|d| (d.text.as_str(), false))
            .filter(|(text, _)| !text.is_empty())
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
            file.latest_diff, None,
            "the field stays honest — this mutation really brought no diff"
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
