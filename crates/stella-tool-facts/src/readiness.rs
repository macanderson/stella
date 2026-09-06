// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How far behind the search index is, and whether to hold a prompt for it.
//!
//! The index fills in the background. A search over a half-filled index ranks
//! what is there and says so. That is fast. It is also worse than waiting
//! when the index is nearly empty. An answer drawn from 3% of the files is
//! not a fast answer. It is a wrong one, sent fast.
//!
//! So the first prompt waits while the one-time pass runs, and the user is
//! told why. Three rules keep this a gate and not a trap.
//!
//! - **The pass ends the hold, not the count.** [`IndexReadiness::settled`]
//!   is set as soon as the pass stops. It stops when it is done, when it
//!   fails, and when it never ran. A settled index holds nothing, however far
//!   behind it is. A bad API key must not lock a user out of their own tool.
//! - **Counts alone decide it.** So this is a pure function, and two screens
//!   cannot disagree about when it fires.
//! - **A count that cannot be read holds nothing.** The read in
//!   `stella_tools::search::readiness` fails toward letting the prompt
//!   through. A note no one needed costs a line. A hold no one needed costs a
//!   turn.

/// The most files that may be missing from the index before a prompt waits.
///
/// Twelve. The number is a judgement, not a measurement. It is small enough
/// that a workspace at this count is one pass from done: an edit session
/// dirties a few files, and those embed in one batch. It is large enough that
/// a stray file or two never holds a prompt on its own. A file the embedder
/// turned down is one. So is a rename that lands between the walk and the
/// embed pass. Above this line, the search draws on so little of the tree
/// that a miss says nothing.
pub const MAX_UNINDEXED_FILES: usize = 12;

/// What the index looks like right now: two counts, plus whether anything is
/// still filling it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexReadiness {
    /// Files in the code-graph index.
    pub total_files: usize,
    /// Files with no vector yet under the live fingerprint. The read in
    /// `stella_tools::search::readiness` says why this is a max and not a
    /// sum.
    pub unindexed_files: usize,
    /// True once the pass has stopped, whatever it got done.
    ///
    /// The field the gate rests on. A hold that outlives the pass is a trap,
    /// not a gate. An embedder no one set up, a workspace no one indexed, a
    /// backend that is down — each one leaves the index behind for good, and
    /// each must leave a usable agent.
    pub settled: bool,
}

impl IndexReadiness {
    /// A workspace nothing has read yet: no files, none pending, and
    /// **settled**. A screen that never hears from the indexer holds nothing.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            total_files: 0,
            unindexed_files: 0,
            settled: true,
        }
    }

    /// Files already embedded — what the ranking can see.
    #[must_use]
    pub const fn indexed_files(&self) -> usize {
        self.total_files.saturating_sub(self.unindexed_files)
    }

    /// Whether a prompt sent right now should wait.
    #[must_use]
    pub const fn holds_prompts(&self) -> bool {
        !self.settled && self.unindexed_files > MAX_UNINDEXED_FILES
    }

    /// What to tell the user whose prompt just waited. `None` when nothing is
    /// held, so a caller cannot draw a hold that did not happen.
    ///
    /// Each clause answers a question the user would ask, in the order they
    /// ask them. What happened to my prompt. Why. How long. Must I do
    /// anything.
    #[must_use]
    pub fn hold_message(&self) -> Option<String> {
        self.holds_prompts().then(|| {
            format!(
                "Your prompt was NOT sent — Stella is still indexing this workspace. {} of {} \
                 files are embedded for search, {} still to go. This is a one-time pass for a \
                 new workspace (after it, only files you change are re-embedded), it is running \
                 in the background right now, and it needs nothing from you. Press ⏎ again in a \
                 moment; your text is still in the composer.",
                self.indexed_files(),
                self.total_files,
                self.unindexed_files,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn behind(unindexed: usize, settled: bool) -> IndexReadiness {
        IndexReadiness {
            total_files: 1_000,
            unindexed_files: unindexed,
            settled,
        }
    }

    /// The line is a strict `>`. Exactly [`MAX_UNINDEXED_FILES`] behind is the
    /// end of an ordinary edit session, not a reason to wait.
    #[test]
    fn the_hold_starts_one_file_past_the_limit() {
        assert!(!behind(MAX_UNINDEXED_FILES, false).holds_prompts());
        assert!(behind(MAX_UNINDEXED_FILES + 1, false).holds_prompts());
    }

    /// **The rule that keeps this a gate and not a trap.** A pass that
    /// stopped — done, failed, or never begun — frees every prompt, however
    /// far behind the index is left.
    #[test]
    fn a_settled_index_never_holds_a_prompt() {
        assert!(!behind(100_000, true).holds_prompts());
        assert!(behind(100_000, true).hold_message().is_none());
        assert!(!IndexReadiness::unknown().holds_prompts());
    }

    /// A held prompt is told what happened, that this is one-time, and that
    /// its text is still there. Those are the three things a user acts on.
    #[test]
    fn the_hold_message_says_what_happened_and_that_it_is_one_time() {
        let message = behind(447, false).hold_message().expect("a hold");
        assert!(message.contains("NOT sent"), "{message}");
        assert!(message.contains("one-time"), "{message}");
        assert!(message.contains("447"), "{message}");
        assert!(message.contains("553"), "the embedded count: {message}");
        assert!(message.contains("still in the composer"), "{message}");
    }
}
