// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Anchoring context-size accounting to provider-reported usage (#2681).
//!
//! Every committed model call returns the ground truth for the exact
//! transcript prefix it was sent: the provider's own input-token count.
//! The byte estimator — even corrected by `crate::estimator::Calibration`'s
//! scalar factor — was measured running 1.8× low on whole transcripts
//! (#2671), because a single ratio cannot capture content-dependent
//! tokenizer variance (code, prose and JSON tokenize very differently). So
//! instead of estimating the whole transcript, the compaction decision
//! re-bases at each committed step: the anchored prefix is *measurement*,
//! and the estimator prices only the tail appended since that report.
//!
//! # How the anchor enters the decision
//!
//! `crate::compaction::compact_measured` compares its own whole-transcript
//! estimate against the budget it is handed, and the established seam for
//! correcting the *measure* is adjusting the *budget* (calibration already
//! divides the budget by its factor rather than threading a factor through
//! compaction's bookkeeping). The anchor uses the same seam. The decision
//! wanted is
//!
//! ```text
//! reported + factor · (est_full − est_prefix) > budget
//! ```
//!
//! which rearranges to `est_full > est_prefix + (budget − reported) / factor`
//! — so [`UsageAnchor::effective_budget`] returns that right-hand side and
//! compaction's unchanged `est_full > handed_budget` comparison computes
//! exactly the anchored measure. The calibration factor now only ever
//! corrects the tail estimate, and the fitted per-request overhead
//! (`Calibration::line`'s intercept — tool schemas, provider framing) needs
//! no model at all: it is inside `reported`, measured.
//!
//! # Validity
//!
//! An anchor describes one exact prefix, so it holds only while that prefix
//! is untouched. Appends (the assistant reply, tool results, steering
//! messages) extend the tail and leave it valid; any in-place rewrite —
//! a compaction pass, the overflow summarizer, a host mutating through
//! `TurnState::messages_mut` — invalidates it, and
//! `TurnState::mark_transcript_rewritten` (which every rewrite path already
//! calls, because the receipt memos have the same invariant) drops it. The
//! next committed step re-anchors. A provider that omits usage
//! ([`stella_protocol::CompletionUsage::reported`] is false, or the counters
//! are all zero) never anchors, so scripted tests and usage-less providers
//! keep the pure-estimator behavior byte-for-byte.

use stella_protocol::{CompletionMessage, CompletionUsage};

/// The provider's attested size of a transcript prefix, paired with the
/// estimator's view of the same prefix so the two accountings can be
/// exchanged inside one budget comparison (module docs).
///
/// Pure data — no I/O, no clock (invariant 2). Turn-scoped: it lives on
/// `TurnState` and dies with the turn; a cold turn's first compaction
/// decision runs on the estimator alone, exactly as before.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UsageAnchor {
    /// Every prompt token the provider read for the anchored prefix: fresh
    /// input (cache reads included — `input_tokens` counts them) plus cache
    /// writes, which adapters split out for pricing only. The same TRUE
    /// total the calibration feed uses, for the same reason.
    reported_tokens: u64,
    /// The estimator's answer for the same prefix, attachment weight
    /// included so it subtracts cleanly from compaction's own
    /// attachment-inclusive walk.
    estimated_tokens: u64,
}

impl UsageAnchor {
    /// Anchor a just-committed step: `messages` is the transcript exactly as
    /// sent (before dispatch appends to it) and `estimated_input_tokens` is
    /// the same attachment-free pre-call estimate `StepUsage` reports.
    ///
    /// `None` when the usage envelope carries no signal — unreported, or
    /// reported with zero prompt tokens — so an absent report can never
    /// anchor the budget to zero and compact every step forever.
    pub(crate) fn from_report(
        usage: &CompletionUsage,
        messages: &[CompletionMessage],
        estimated_input_tokens: u64,
    ) -> Option<Self> {
        let reported_tokens = usage.input_tokens.saturating_add(usage.cache_write_tokens);
        if !usage.reported || reported_tokens == 0 {
            return None;
        }
        Some(Self {
            reported_tokens,
            estimated_tokens: estimated_input_tokens.saturating_add(
                crate::estimator::estimate_conversation_attachment_tokens(messages),
            ),
        })
    }

    /// The budget to hand `compact_measured` so its whole-transcript
    /// comparison computes `reported + factor · tail > configured_budget`
    /// (module docs): the anchored prefix's estimate plus the calibrated
    /// headroom the report leaves under the configured budget.
    ///
    /// Saturating on purpose: a report at or over the whole budget leaves
    /// zero headroom, so the handed budget floors at the prefix estimate and
    /// compaction fires on any non-empty tail — the transcript is measured
    /// over budget, and shrinking it is the only correct answer. A
    /// non-finite or non-positive `factor` (impossible from
    /// `Calibration::factor`'s clamp, but this is arithmetic on runtime
    /// data) degrades to the identity rather than poisoning the division.
    pub(crate) fn effective_budget(&self, configured_budget: u64, factor: f64) -> u64 {
        let factor = if factor.is_finite() && factor > 0.0 {
            factor
        } else {
            1.0
        };
        let headroom = configured_budget.saturating_sub(self.reported_tokens) as f64 / factor;
        self.estimated_tokens.saturating_add(headroom as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn reported(input_tokens: u64, cache_write_tokens: u64) -> CompletionUsage {
        CompletionUsage {
            reported: true,
            input_tokens,
            cache_write_tokens,
            ..CompletionUsage::default()
        }
    }

    fn anchor(reported_tokens: u64, estimated_tokens: u64) -> UsageAnchor {
        UsageAnchor {
            reported_tokens,
            estimated_tokens,
        }
    }

    #[test]
    fn an_absent_or_empty_report_never_anchors() {
        let unreported = CompletionUsage {
            input_tokens: 5_000,
            ..CompletionUsage::default()
        };
        assert!(
            UsageAnchor::from_report(&unreported, &[], 1_000).is_none(),
            "an unattested envelope is not a measurement"
        );
        assert!(
            UsageAnchor::from_report(&CompletionUsage::reported_zero(), &[], 1_000).is_none(),
            "zero prompt tokens carry no signal (scripted tests)"
        );
    }

    #[test]
    fn the_report_totals_fresh_input_plus_cache_writes() {
        let anchor = UsageAnchor::from_report(&reported(10, 100_000), &[], 1_000)
            .expect("a reported envelope anchors");
        // Cache writes are prompt tokens the provider read; omitting them
        // would under-anchor every cache-writing step (same rule as the
        // calibration feed).
        assert_eq!(anchor.reported_tokens, 100_010);
    }

    #[test]
    fn the_prefix_estimate_carries_the_attachment_weight() {
        let messages = vec![CompletionMessage::user_with_attachments(
            "look",
            vec![stella_protocol::Attachment::from_path(
                "shot.png",
                "image/png",
                350_000,
                "/tmp/shot.png",
            )],
        )];
        let attachment = crate::estimator::estimate_conversation_attachment_tokens(&messages);
        let anchor = UsageAnchor::from_report(&reported(4_000, 0), &messages, 1_000)
            .expect("a reported envelope anchors");
        // The text estimate and the attachment weight sum back to what
        // compaction's own walk sees for the prefix — otherwise the prefix's
        // attachment weight would leak into the tail term.
        assert_eq!(anchor.estimated_tokens, 1_000 + attachment);
    }

    /// An estimator that happens to be exact changes nothing: anchoring is a
    /// re-base, not a bias.
    #[test]
    fn an_exact_estimate_leaves_the_budget_unchanged() {
        assert_eq!(anchor(2_000, 2_000).effective_budget(150_000, 1.0), 150_000);
    }

    /// The 1.8× drift shape from #2671: the provider read far more than the
    /// estimator priced, so the handed budget shrinks by exactly the
    /// measured gap and compaction fires earlier.
    #[test]
    fn an_under_estimated_prefix_shrinks_the_budget_by_the_measured_gap() {
        // est 100k, reported 180k: the budget gives up the 80k the estimator
        // could not see.
        assert_eq!(
            anchor(180_000, 100_000).effective_budget(200_000, 1.0),
            120_000
        );
    }

    #[test]
    fn the_factor_calibrates_only_the_tail_headroom() {
        // factor 2 halves the headroom (the tail estimate runs 2× low), but
        // the prefix term is measurement and is not scaled.
        assert_eq!(
            anchor(50_000, 10_000).effective_budget(150_000, 2.0),
            10_000 + 50_000
        );
    }

    #[test]
    fn a_report_over_the_whole_budget_floors_at_the_prefix_estimate() {
        let anchored = anchor(400_000, 10_000).effective_budget(150_000, 1.0);
        assert_eq!(
            anchored, 10_000,
            "no headroom is left — any tail must trigger compaction"
        );
    }

    #[test]
    fn a_degenerate_factor_degrades_to_identity_instead_of_poisoning() {
        let anchored = anchor(50_000, 10_000).effective_budget(150_000, 1.0);
        for factor in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                anchor(50_000, 10_000).effective_budget(150_000, factor),
                anchored
            );
        }
    }

    proptest! {
        /// The identity behind the whole module: for any anchored budget,
        /// compaction's `est_full > handed_budget` comparison is exactly the
        /// anchored measure `reported + factor·tail > configured_budget`
        /// (integer division rounds the headroom down — the early-compaction
        /// direction — so the equivalence is one-sided only at the boundary
        /// token).
        #[test]
        fn the_handed_budget_computes_the_anchored_measure(
            budget in 1_000u64..1_000_000,
            reported_tokens in 1u64..1_000_000,
            est_prefix in 0u64..1_000_000,
            // Never 0 on the engine path: the committed step's assistant
            // reply is appended before the next decision runs.
            tail in 1u64..1_000_000,
            // The clamped range Calibration::factor can actually produce.
            factor in 0.5f64..=2.0,
        ) {
            let a = anchor(reported_tokens, est_prefix);
            let est_full = est_prefix + tail;
            let compaction_fires = est_full > a.effective_budget(budget, factor);
            let anchored_measure =
                reported_tokens as f64 + factor * tail as f64 > budget as f64;
            if compaction_fires {
                // Fired ⇒ the anchored measure is genuinely over (rounding
                // down can only make firing MORE eager by under a token).
                prop_assert!(
                    reported_tokens as f64 + factor * (tail as f64 - 1.0) <= budget as f64
                        || anchored_measure,
                );
            } else {
                prop_assert!(!anchored_measure);
            }
        }

        /// More reported tokens never yield a larger handed budget: the
        /// anchor can only tighten as the measured prefix grows.
        #[test]
        fn the_budget_is_monotone_in_the_report(
            budget in 0u64..1_000_000,
            est_prefix in 0u64..1_000_000,
            lo in 0u64..1_000_000,
            delta in 0u64..1_000_000,
            factor in 0.5f64..=2.0,
        ) {
            prop_assert!(
                anchor(lo + delta, est_prefix).effective_budget(budget, factor)
                    <= anchor(lo, est_prefix).effective_budget(budget, factor)
            );
        }
    }
}
