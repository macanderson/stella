//! Per-feature counters — the comparison's **opaque attribution channel**.
//!
//! The aggregates beside these (pass rate, cost, tokens, turns) answer "which
//! arm won". They cannot answer "*which feature* moved", and that is the
//! question a paired ablation exists to settle: recall on vs off, the witness
//! stage on vs off, a foundry tool adopted vs not. Before this channel existed,
//! every such experiment needed a bespoke `jq` pass over the event stream, run
//! beside the report rather than inside it — and a number computed over a
//! *different* trial set than the report's is the exact measurement artifact
//! pairing exists to prevent.
//!
//! So a counter rides the trial. It is folded over the same **paired** trials
//! as every other figure, excluded by the same rule, and divided by the same
//! denominator. A feature delta and a pass-rate delta in one report are
//! therefore statements about one population.
//!
//! # The keys are the caller's, and this module does not know them
//!
//! [`FeatureCounts`] is a plain string-keyed tally. Nothing here knows what
//! `tool.graph_query` or `stage.witness` means, exactly as nothing here knows
//! whether an arm's `value` names a model or a prompt template — the arm stays
//! opaque (see [`super::ArmTrials`]), and so does the counter. A harness owns
//! its vocabulary and documents it; the engine folds numbers.
//!
//! Two rules the caller carries, because this module cannot enforce them:
//!
//! * **A key means the same thing in both arms.** Re-keying a counter between
//!   runs produces a delta against zero, which reads as a total regression.
//! * **Keys are bounded.** They become map entries in a report; a key minted
//!   per trial (a timestamp, a call id) turns a comparison into a log.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::trial::TrialRecord;

/// One trial's feature counters, keyed by a stable caller-owned name.
///
/// The value counts whatever the key names — recall events, frames admitted,
/// tokens injected, stage entries. Mixed units across keys are fine and
/// expected: each key is compared only against itself in the other arm.
pub type FeatureCounts = BTreeMap<String, u64>;

/// One feature counter folded over one arm's paired trials.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FeatureStat {
    /// Summed across every paired trial of the arm.
    pub total: u64,
    /// `total / trials` — the comparable figure.
    ///
    /// The total alone is not comparable: pairing excludes at *trial*
    /// granularity, so two arms over the same tasks can still fold different
    /// trial counts, and the arm that ran more trials wins every total by
    /// construction. `0.0` when the arm folded no paired trials.
    pub mean: f64,
}

impl FeatureStat {
    /// `total` spread over `trials`, with `trials == 0` yielding a zero mean
    /// rather than a `NaN` — a report over an arm that never ran stays
    /// readable, and the sample floor in
    /// [`crate::self_tuning::select_winner`] is what refuses to promote on it.
    fn new(total: u64, trials: usize) -> Self {
        Self {
            total,
            mean: if trials == 0 {
                0.0
            } else {
                total as f64 / trials as f64
            },
        }
    }
}

/// One arm's counter for one key, next to the baseline's.
///
/// Deliberately carries both arms' figures rather than the difference alone: a
/// `+2.0` delta is a different finding at `0 → 2` than at `40 → 42`, and a
/// reader who has to go back to the arm summaries to tell them apart will not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureDelta {
    /// The counter key, verbatim as the caller wrote it.
    pub key: String,
    /// The non-baseline arm this delta describes.
    pub arm: String,
    /// The baseline arm's figures for `key`, zeroed when it never counted it.
    pub baseline: FeatureStat,
    /// `arm`'s figures for `key`, zeroed when it never counted it.
    pub candidate: FeatureStat,
    /// `candidate.mean - baseline.mean`. The only derived number here, and it
    /// is derived rather than stored on the arms so it can never disagree with
    /// its operands.
    pub delta_mean: f64,
}

/// Fold one arm's paired trials into a per-key [`FeatureStat`].
///
/// Saturating, because a counter is a count and a comparison must not be the
/// thing that overflows on an absurd input — the same posture
/// [`super::ArmSummary`] takes on tokens.
pub(super) fn fold(paired: &[&TrialRecord]) -> BTreeMap<String, FeatureStat> {
    let mut totals: BTreeMap<&str, u64> = BTreeMap::new();
    for trial in paired {
        for (key, count) in &trial.features {
            let slot = totals.entry(key.as_str()).or_default();
            *slot = slot.saturating_add(*count);
        }
    }
    totals
        .into_iter()
        .map(|(key, total)| (key.to_string(), FeatureStat::new(total, paired.len())))
        .collect()
}
