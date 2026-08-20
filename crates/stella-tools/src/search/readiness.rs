// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How far behind the semantic index is, and whether that is far enough to
//! hold a prompt.
//!
//! This is the half of #4043 that pays for the other half. Once the backfill
//! left the query path ([`super::backfill`]), a search over a workspace whose
//! index is still filling ranks over whatever exists and says so — fast, and
//! honest, and *worse than waiting* if the index is nearly empty because the
//! session started thirty seconds ago. A first prompt answered off 3% of a
//! repository is not a fast answer; it is a wrong one delivered promptly.
//!
//! So the interactive door holds that first prompt while the one-time pass
//! runs, and says why. Three properties make it a gate rather than a wedge:
//!
//! - **It is bounded by the pass, not by the count.** [`IndexReadiness::settled`]
//!   is set the moment the background pass stops — finished, failed, or
//!   skipped — and a settled index never holds anything, however far behind it
//!   is. A workspace with a broken embedder converges to "never indexed" and
//!   must still be usable: the alternative is a tool that a bad API key locks
//!   the user out of.
//! - **It is decided from counts alone**, so it is a pure function and the
//!   surfaces that render it cannot disagree about when it fires.
//! - **An unreadable counter does not hold.** [`measure`] fails toward
//!   *letting the prompt through*, which is the opposite of the direction
//!   [`super::engine`]'s coverage note fails in, and deliberately: a disclosure
//!   nobody needed costs a line of prose, while a hold nobody needed costs the
//!   user their turn.

use stella_graph::CodeGraph;

/// The most files that may be missing from the semantic index before an
/// interactive prompt is held back.
///
/// Twelve, and the number is a judgement rather than a measurement: it is
/// small enough that a workspace at this count is one incremental pass from
/// done (an ordinary edit session dirties a handful of files, and those embed
/// in one batch of [`super::semantic::EMBED_BATCH`]), and large enough that a
/// couple of stragglers — a file the embedder rejected, a rename landing
/// between the index walk and the embed pass — never hold a prompt on their
/// own. Above it, a search's answer is drawn from a corpus so incomplete that
/// its miss says nothing at all.
pub const MAX_UNINDEXED_FILES: usize = 12;

/// What the workspace's semantic index looks like right now, as the two
/// numbers a decision needs plus whether anything is still filling it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexReadiness {
    /// Files in the code-graph index.
    pub total_files: usize,
    /// Files that are not fully embedded under the active fingerprint — see
    /// [`measure`] for why this is a max rather than a sum.
    pub unindexed_files: usize,
    /// True once the background pass has stopped, whatever it achieved.
    ///
    /// The load-bearing field. A hold that outlives the pass filling it is not
    /// a gate, it is a wedge: an unconfigured embedder, a workspace nobody
    /// indexed, an embedding backend that is down — each leaves a permanently
    /// behind index, and each must leave a perfectly usable agent.
    pub settled: bool,
}

impl IndexReadiness {
    /// A workspace nothing has measured yet: no files, nothing pending, and
    /// **settled**, so a surface that never hears from the indexer holds
    /// nothing. Same direction as the unreadable-counter case in [`measure`].
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            total_files: 0,
            unindexed_files: 0,
            settled: true,
        }
    }

    /// Files already embedded — what the ranking can actually see.
    #[must_use]
    pub const fn indexed_files(&self) -> usize {
        self.total_files.saturating_sub(self.unindexed_files)
    }

    /// Whether a prompt submitted right now should be held.
    #[must_use]
    pub const fn holds_prompts(&self) -> bool {
        !self.settled && self.unindexed_files > MAX_UNINDEXED_FILES
    }

    /// What to tell the user whose prompt was just held — `None` when nothing
    /// is held, so a caller cannot render a hold that did not happen.
    ///
    /// Every clause answers a question the user would otherwise ask, and the
    /// order is the order they ask them: what happened to my prompt, why, how
    /// long, and do I have to do anything.
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

/// Read the two counters off an open graph.
///
/// `unindexed_files` is the **larger** of the two pending sets, not their sum.
/// They are overlapping sets of the same files — a file that has just been
/// indexed has neither a whole-file vector nor chunk vectors, and would be
/// counted twice — so the max is the tightest bound that is certainly true,
/// and the union is never smaller than it. Both halves are counts of *files*
/// for the reason [`super::engine`]'s coverage note gives: chunk rows dedup on
/// the rendered text's hash, so a count of chunks could never be compared
/// against a count of files.
///
/// A counter that cannot be read reports **zero pending**, which lets prompts
/// through. That is the opposite of the coverage note's direction and is the
/// right one here: the note's failure mode is an unstated caveat, this one's
/// is a locked door.
#[must_use]
pub fn measure(graph: &CodeGraph, fingerprint: &str, settled: bool) -> IndexReadiness {
    let total_files = graph.file_count().unwrap_or(0);
    let embedded = graph.embedded_file_count(fingerprint).unwrap_or(total_files);
    let chunks_pending = graph.pending_chunk_file_count(fingerprint).unwrap_or(0);
    IndexReadiness {
        total_files,
        unindexed_files: total_files.saturating_sub(embedded).max(chunks_pending),
        settled,
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

    /// The threshold is a strict `>`: exactly [`MAX_UNINDEXED_FILES`] files
    /// behind is the ordinary end of an edit session, not a reason to hold.
    #[test]
    fn the_hold_starts_one_file_past_the_limit() {
        assert!(!behind(MAX_UNINDEXED_FILES, false).holds_prompts());
        assert!(behind(MAX_UNINDEXED_FILES + 1, false).holds_prompts());
    }

    /// **The property that keeps this a gate rather than a wedge.** A pass
    /// that stopped — finished, failed, or never started — releases every
    /// prompt, however far behind the index is left.
    #[test]
    fn a_settled_index_never_holds_a_prompt() {
        assert!(!behind(100_000, true).holds_prompts());
        assert!(behind(100_000, true).hold_message().is_none());
        assert!(!IndexReadiness::unknown().holds_prompts());
    }

    /// A held prompt is told what happened to it, that this is one-time, and
    /// that its text survived — the three things a user acts on.
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
