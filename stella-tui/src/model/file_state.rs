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
    /// The last [`DIFF_HISTORY`] diffs for this path, each tagged with the
    /// `changes` value it was recorded at, newest last.
    ///
    /// Keeping only `latest_diff` made the transcript *erase* its own history:
    /// the second edit to a file bumped `changes` past the seq the first
    /// edit's result recorded, so that result's diff stopped resolving and its
    /// row silently lost the change it had made. Scrolling back through a
    /// session that touched one file five times showed four edits with no
    /// diff and one with. The bytes still live on the single event-borne path
    /// (L-T5) — this is the same store, given a shallow memory instead of a
    /// depth of one.
    pub recent_diffs: VecDeque<(u32, String)>,
    /// How many mutating `FileChange` events have touched this path.
    pub changes: u32,
    /// How many times this path has been read.
    pub reads: u32,
}

/// How many past diffs a path keeps so older tool results can still render
/// the change they made. Deep enough to cover the common "edit the same file
/// a few times in one turn" shape; bounded so a file edited in a loop cannot
/// grow the model without limit. Beyond it, a result's row degrades to naming
/// the change rather than showing it — the pre-existing behaviour, now the
/// exception instead of the rule.
pub const DIFF_HISTORY: usize = 8;

impl FileState {
    /// Record a mutation's diff against the `changes` value it produced.
    /// Called only for mutations, and only after `changes` has been bumped, so
    /// the tag matches the seq a tool result folds at the same moment.
    pub(crate) fn remember_diff(&mut self, diff: &Option<String>) {
        let Some(text) = diff else { return };
        self.recent_diffs.push_back((self.changes, text.clone()));
        while self.recent_diffs.len() > DIFF_HISTORY {
            self.recent_diffs.pop_front();
        }
    }

    /// The diff this path produced at mutation `seq`, if still remembered.
    #[must_use]
    pub fn diff_at(&self, seq: u32) -> Option<&str> {
        self.recent_diffs
            .iter()
            .find(|(s, _)| *s == seq)
            .map(|(_, d)| d.as_str())
    }
}
