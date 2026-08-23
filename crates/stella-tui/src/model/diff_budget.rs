// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How many bytes of remembered diff text one session keeps, and which row
//! gives its diff up first (#4365).
//!
//! # Why bytes
//!
//! The rule for the deck is that a `write_file` / `edit_file` row shows its
//! diff, and the two counts that used to bound the history —
//! `DIFF_HISTORY = 8` diffs per path, [`super::MAX_TRACKED_FILES`] paths —
//! were the last two exceptions to it. A census of the recorded journals under
//! `~/.stella/sessions/` (8,448 mutating `FileChange` events across the 25 that
//! carry any) says what each one bought:
//!
//! | policy | rows that lose their diff | peak text held |
//! | --- | --- | --- |
//! | 8 deep per path, 256 paths | 7,350 (87.0%) | 45.4 MiB |
//! | no depth cap, 2,048 paths, 32 MiB budget | 5,664 (67.1%) | 32.0 MiB |
//!
//! Strictly less memory *and* 1,686 more rows keeping their diff, because the
//! old bound counted the wrong thing: a session that sweeps a tree passes 256
//! paths long before it edits one file nine times, and a cap on paths cannot
//! see that one recorded session's diffs averaged 47 KB each.
//!
//! Every remaining loss in that second row is one pathological session — 6,160
//! mutations across 6,143 distinct paths. Excluding it, the corpus is 2,288
//! mutations, of which today's caps take the diff off 1,446 and this budget
//! takes it off **none**.
//!
//! # What is released, and in what order
//!
//! Oldest text first, across every path, because the oldest change is the one
//! furthest back in the transcript. Ordering by the *path's* recency instead
//! was measured too and released exactly the same diffs on this corpus, so the
//! simpler rule is the one that ships.
//!
//! A release takes the **text** and leaves the entry: the released row still
//! states the `+N −M` its emitter measured (`FileState::delta_at`), which is
//! 12 bytes rather than tens of kilobytes and is the difference between a row
//! that says less and a row that says nothing.

use std::collections::VecDeque;

/// Bytes of remembered diff text a session keeps before it starts releasing
/// the oldest.
///
/// 32 MiB is the knee of the corpus above: below it (16 MiB) a real, ordinary
/// session starts losing rows, and above it (64 MiB) only the pathological
/// sweep improves, and barely — 5,483 rows against 5,664 for twice the
/// memory. It is also under the 45.4 MiB the counts this replaces were
/// measured holding, so the change costs no memory to buy the rows back.
pub const DIFF_TEXT_BUDGET: usize = 32 * 1024 * 1024;

/// One remembered mutation's text, in the order it was remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    path: String,
    seq: u32,
    bytes: usize,
}

/// The session's remembered diff text, oldest first.
///
/// Reconstructible by replay like every other field of
/// [`super::SessionModel`]: a pure function of the event order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct DiffBudget {
    held: VecDeque<Held>,
    bytes: usize,
}

impl DiffBudget {
    /// Take `bytes` of text remembered for `(path, seq)` into the budget, and
    /// answer with the entries whose text the caller must now release to get
    /// back under [`DIFF_TEXT_BUDGET`], oldest first.
    ///
    /// A single change larger than the whole budget releases everything else
    /// and then itself, which leaves the budget empty rather than over — the
    /// alternative is a cap that one event can walk straight through.
    pub(super) fn record(&mut self, path: &str, seq: u32, bytes: usize) -> Vec<(String, u32)> {
        if bytes == 0 {
            return Vec::new();
        }
        self.held.push_back(Held {
            path: path.to_string(),
            seq,
            bytes,
        });
        self.bytes += bytes;
        let mut released = Vec::new();
        while self.bytes > DIFF_TEXT_BUDGET {
            let Some(oldest) = self.held.pop_front() else {
                break;
            };
            self.bytes -= oldest.bytes;
            released.push((oldest.path, oldest.seq));
        }
        released
    }

    /// Drop everything remembered for `path` — it has left the ledger, so its
    /// text is gone whether the budget wanted it or not.
    pub(super) fn forget(&mut self, path: &str) {
        self.held.retain(|h| {
            let mine = h.path == path;
            if mine {
                self.bytes -= h.bytes;
            }
            !mine
        });
    }

    /// Bytes of text the budget believes are held. Test-only: the assertion
    /// that the accounting matches what the ledger actually stores is what
    /// makes the budget a bound rather than a hope.
    #[cfg(test)]
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under budget, nothing is released.
    #[test]
    fn a_small_session_releases_nothing() {
        let mut budget = DiffBudget::default();
        for seq in 1..=100 {
            assert!(budget.record("src/a.rs", seq, 1024).is_empty());
        }
        assert_eq!(budget.bytes(), 100 * 1024);
    }

    /// Over budget, the oldest text goes first and the newest survives — the
    /// order a reader scrolling back from the end needs.
    #[test]
    fn the_oldest_text_is_released_first() {
        let mut budget = DiffBudget::default();
        let chunk = DIFF_TEXT_BUDGET / 4;
        for seq in 1..=4 {
            assert!(budget.record("src/a.rs", seq, chunk).is_empty());
        }
        let released = budget.record("src/a.rs", 5, chunk);
        assert_eq!(released, vec![("src/a.rs".to_string(), 1)]);
        assert!(budget.bytes() <= DIFF_TEXT_BUDGET);
    }

    /// A textless mutation costs nothing and is not queued: it has no text to
    /// release, and standing in the queue would let it displace one that has.
    #[test]
    fn a_measured_but_patchless_change_costs_nothing() {
        let mut budget = DiffBudget::default();
        assert!(budget.record("src/a.rs", 1, 0).is_empty());
        assert_eq!(budget.bytes(), 0);
    }

    /// Evicting a path returns its bytes, so a session that sweeps a tree does
    /// not spend its budget on paths the ledger no longer holds.
    #[test]
    fn forgetting_a_path_returns_its_bytes() {
        let mut budget = DiffBudget::default();
        budget.record("src/a.rs", 1, 1000);
        budget.record("src/b.rs", 1, 2000);
        budget.record("src/a.rs", 2, 3000);
        budget.forget("src/a.rs");
        assert_eq!(budget.bytes(), 2000);
        assert_eq!(budget.held.len(), 1);
    }

    /// One change bigger than the whole budget empties it rather than
    /// overrunning it: the bound holds even when a single event cannot fit.
    #[test]
    fn a_change_larger_than_the_budget_leaves_it_empty() {
        let mut budget = DiffBudget::default();
        budget.record("src/a.rs", 1, 4096);
        let released = budget.record("src/big.rs", 1, DIFF_TEXT_BUDGET + 1);
        assert_eq!(
            released,
            vec![("src/a.rs".to_string(), 1), ("src/big.rs".to_string(), 1)]
        );
        assert_eq!(budget.bytes(), 0);
    }
}
