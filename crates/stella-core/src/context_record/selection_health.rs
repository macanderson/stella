//! Derived selection health — Phase 4 deliverable 4 (#715).
//!
//! How well a context record has been earning its place in frames, computed
//! from immutable records and nothing else.
//!
//! # Why this is a fold and not a table
//!
//! Spec §5.2 is explicit that `superseded`, `expired`, and `stale` are derived
//! projections and never stored states, and
//! [`RecordStatus`](super::RecordStatus) repeats it: "Staleness is NOT a status
//! (it is a separate derived selection-health value)." This module is that
//! value.
//!
//! The gate criterion is "aggregates rebuild exactly from immutable records",
//! which a stored counter cannot satisfy — a counter drifts the first time a
//! write is lost, and nothing can tell you it has. A fold over append-only
//! records is instead *definitionally* correct: recomputing is the only way to
//! read it, so there is no second copy to disagree.
//!
//! This is the same shape `fold_citation_stats` already uses for the citation
//! loop, deliberately. Two derived aggregates over immutable evidence, one per
//! evidence source.
//!
//! # The counting rule that matters
//!
//! **`not_helpful_ratio` is over ASSESSED uses, not over all uses.** Deliverable
//! 3 — "absence of citation is not evidence of uselessness" — is a statement
//! about a denominator. A record rendered a hundred times and judged twice has
//! an assessed population of two; dividing by a hundred would let silence
//! masquerade as approval, and dividing the *negative* count by a hundred would
//! let it masquerade as harmlessness. Both are wrong, and only the assessed
//! denominator is honest about how little is actually known.

use std::collections::{BTreeMap, BTreeSet};

use super::{ContextUse, ContextUseEvaluation, ContextUseFeedback};

/// The thresholds that turn raw counts into a verdict.
///
/// Mirrors `context.efficacy` in settings; kept as a plain struct here so the
/// fold stays pure and testable without a settings file. The defaults are the
/// shipped ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectionHealthPolicy {
    /// How many *assessed* uses a record needs before its ratio means anything.
    pub min_attributable_uses: u32,
    /// The not-helpful share (`0.0..=1.0`) at or above which a record is
    /// failing to earn its place.
    pub not_helpful_ratio_threshold: f64,
    /// The attribution confidence (`0..=100`) a verdict must carry to count.
    pub min_attribution_confidence: u8,
}

impl Default for SelectionHealthPolicy {
    fn default() -> Self {
        Self {
            min_attributable_uses: 5,
            not_helpful_ratio_threshold: 0.8,
            min_attribution_confidence: 80,
        }
    }
}

/// One context record's derived standing. Every field is recomputed; none is
/// stored.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionHealth {
    /// The record this is about.
    pub context_record_id: String,
    /// Times the record was put in front of the model.
    pub uses: u32,
    /// Distinct tasks those uses span. The anti-poisoning denominator: thirty
    /// uses inside one task are one task's worth of evidence (spec §7).
    pub distinct_tasks: u32,
    /// Uses carrying a verdict that clears the confidence floor and had a real
    /// opportunity to help. The honest denominator for [`Self::not_helpful_ratio`].
    pub assessed_uses: u32,
    /// Assessed uses judged helpful.
    pub helpful: u32,
    /// Assessed uses judged not helpful.
    pub not_helpful: u32,
    /// Assessed uses judged neutral.
    pub neutral: u32,
    /// `not_helpful / assessed_uses`, or `0.0` when nothing has been assessed.
    /// **Never** divided by [`Self::uses`] — see the module docs.
    pub not_helpful_ratio: f64,
    /// Assessed uses whose verdict came from a **pruning-eligible** method —
    /// the subset that may drive retirement. Always `<= assessed_uses`; the
    /// difference is agent self-reports and untrusted extensions, which inform
    /// but may not suppress.
    pub eligible_assessed: u32,
    /// Pruning-eligible assessed uses judged not helpful.
    pub eligible_not_helpful: u32,
    /// Whether enough *pruning-eligible* evidence exists for a verdict to
    /// carry weight.
    pub attributable: bool,
    /// Whether this record is failing to earn its place: attributable AND
    /// over the ratio threshold **on pruning-eligible evidence alone**.
    /// Necessary for retirement, never sufficient — the protected-category
    /// check is a separate gate that runs after this.
    pub failing: bool,
}

/// Fold immutable use and feedback records into per-record selection health.
///
/// Pure: same inputs, same output, in every ordering. The result is sorted by
/// `context_record_id` so two runs produce byte-identical output — the
/// determinism spec §5.3 asks of every derived value.
///
/// Feedback is matched to uses by `context_use_id`, which is why the extractor
/// carries the ledger's own id forward rather than re-deriving it: a feedback
/// naming an id no use has is evidence about nothing and is dropped, not
/// guessed at.
pub fn fold_selection_health(
    uses: &[(String, ContextUse)],
    feedback: &[ContextUseFeedback],
    policy: SelectionHealthPolicy,
) -> Vec<SelectionHealth> {
    // Which record each use id is about, so a verdict can be attributed
    // without trusting the feedback's own copy of the record id (it has none —
    // deliberately, so the two records cannot disagree).
    let use_index: BTreeMap<&str, &ContextUse> = uses
        .iter()
        .map(|(id, use_record)| (id.as_str(), use_record))
        .collect();

    let mut tasks: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut counts: BTreeMap<&str, Counters> = BTreeMap::new();

    for (_, use_record) in uses {
        let entry = counts
            .entry(use_record.context_record_id.as_str())
            .or_default();
        entry.uses += 1;
        tasks
            .entry(use_record.context_record_id.as_str())
            .or_default()
            .insert(use_record.task_id.as_str());
    }

    for verdict in feedback {
        // A verdict about a use the ledger does not hold cannot be attributed
        // to any record. Dropping it is the only honest option — the
        // alternative is inventing a subject for it.
        let Some(use_record) = use_index.get(verdict.context_use_id.as_str()) else {
            continue;
        };
        // Deliverable 3, enforced at the READ side too. The write path refuses
        // a `not_helpful` without opportunity and a method, but a record
        // written by an older binary, or a `neutral` verdict that never had an
        // opportunity, must not silently become evidence here either.
        if !verdict.had_opportunity {
            continue;
        }
        let Some(method) = verdict.evaluation_method.as_ref() else {
            continue;
        };
        if verdict.attribution_confidence.get() < policy.min_attribution_confidence {
            continue;
        }
        let entry = counts
            .entry(use_record.context_record_id.as_str())
            .or_default();
        entry.assessed += 1;
        // The two-tier policy: every recorded verdict counts toward what is
        // *known* about a record, but only a pruning-eligible method counts
        // toward what may *remove* it. An agent's own report is real evidence
        // and belongs in the display; it may not retire anything on its own.
        let eligible = method.is_pruning_eligible();
        if eligible {
            entry.eligible_assessed += 1;
        }
        match verdict.evaluation {
            ContextUseEvaluation::Helpful => entry.helpful += 1,
            ContextUseEvaluation::NotHelpful => {
                entry.not_helpful += 1;
                if eligible {
                    entry.eligible_not_helpful += 1;
                }
            }
            ContextUseEvaluation::Neutral => entry.neutral += 1,
        }
    }

    counts
        .into_iter()
        .map(|(record_id, c)| {
            let ratio = if c.assessed == 0 {
                0.0
            } else {
                f64::from(c.not_helpful) / f64::from(c.assessed)
            };
            // Both the floor and the ratio are computed on the eligible subset
            // — a record judged unhelpful fifty times by the agent alone is
            // not one the loop may retire, no matter how lopsided the count.
            let attributable = c.eligible_assessed >= policy.min_attributable_uses;
            let eligible_ratio = if c.eligible_assessed == 0 {
                0.0
            } else {
                f64::from(c.eligible_not_helpful) / f64::from(c.eligible_assessed)
            };
            SelectionHealth {
                context_record_id: record_id.to_string(),
                uses: c.uses,
                distinct_tasks: tasks.get(record_id).map_or(0, |t| t.len() as u32),
                assessed_uses: c.assessed,
                helpful: c.helpful,
                not_helpful: c.not_helpful,
                neutral: c.neutral,
                not_helpful_ratio: ratio,
                eligible_assessed: c.eligible_assessed,
                eligible_not_helpful: c.eligible_not_helpful,
                attributable,
                failing: attributable && eligible_ratio >= policy.not_helpful_ratio_threshold,
            }
        })
        .collect()
}

/// Mutable only for the duration of the fold.
#[derive(Default)]
struct Counters {
    uses: u32,
    assessed: u32,
    helpful: u32,
    not_helpful: u32,
    neutral: u32,
    eligible_assessed: u32,
    eligible_not_helpful: u32,
}

#[cfg(test)]
mod tests;
