//! One arm's trials folded into the aggregates a report shows and a guard
//! compares.

use serde::{Deserialize, Serialize};

use super::metric::{mean, median};
use super::trial::TrialRecord;
use crate::self_tuning::{RewardWeights, reward};

/// What one arm did, over the **paired** task set only.
///
/// Every figure here is computed from trials whose task ran under every arm
/// (see the module docs on pairing). `trials_excluded` is the count that were
/// left out, published beside the aggregates rather than in a footnote,
/// because "the candidate scored 90%" means something different when a third
/// of its trials were dropped and the reader cannot tell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmSummary {
    /// The arm's stable id.
    pub id: String,
    /// The configuration this arm represents.
    pub value: String,
    /// Paired trials folded into these figures.
    pub trials: usize,
    /// Trials this arm ran on tasks another arm never ran. Excluded from
    /// everything above; named so the exclusion is visible.
    pub trials_excluded: usize,
    /// Distinct paired tasks this arm ran.
    pub tasks: usize,
    /// Paired trials whose outcome succeeded.
    pub solved: usize,
    /// `solved / trials`, in `[0, 1]`. `0.0` for an arm with no paired trials.
    pub pass_rate: f64,
    /// Mean reward per paired trial under the comparison's weights.
    pub mean_reward: f64,
    /// USD across every paired trial.
    pub total_cost_usd: f64,
    /// USD per paired trial.
    pub mean_cost_usd: f64,
    /// Tokens across every paired trial.
    pub total_tokens: u64,
    /// Tokens per paired trial.
    pub mean_tokens: f64,
    /// Median model calls per paired trial — see [`super::Metric::Turns`] for
    /// why the median and not the mean.
    pub median_turns: f64,
    /// Retries per paired trial.
    pub mean_retries: f64,
}

impl ArmSummary {
    /// Fold `paired` (this arm's trials on tasks every arm ran) and
    /// `excluded_count` (the trials that were not) into the arm's aggregates.
    ///
    /// Total by construction: an empty `paired` yields zeros throughout rather
    /// than a `NaN` pass rate, so a report over an arm that never ran is
    /// readable instead of poisoned. Zero is not evidence — the sample floor in
    /// [`crate::self_tuning::select_winner`] is what refuses to promote on it.
    pub(super) fn fold(
        id: &str,
        value: &str,
        paired: &[&TrialRecord],
        excluded_count: usize,
        weights: &RewardWeights,
    ) -> Self {
        let trials = paired.len();
        let solved = paired.iter().filter(|t| t.outcome.succeeded).count();
        let total_cost_usd: f64 = paired.iter().map(|t| t.outcome.cost_usd).sum();
        // Saturating because a token total is a count, and a comparison must
        // not be the thing that overflows on an absurd input.
        let total_tokens: u64 = paired
            .iter()
            .fold(0u64, |acc, t| acc.saturating_add(t.outcome.tokens));
        let rewards: Vec<f64> = paired.iter().map(|t| reward(&t.outcome, weights)).collect();
        let turns: Vec<f64> = paired.iter().map(|t| f64::from(t.turns)).collect();
        let retries: Vec<f64> = paired
            .iter()
            .map(|t| f64::from(t.outcome.retries))
            .collect();
        let tokens: Vec<f64> = paired.iter().map(|t| t.outcome.tokens as f64).collect();

        let mut tasks: Vec<&str> = paired.iter().map(|t| t.task.as_str()).collect();
        tasks.sort_unstable();
        tasks.dedup();

        Self {
            id: id.to_string(),
            value: value.to_string(),
            trials,
            trials_excluded: excluded_count,
            tasks: tasks.len(),
            solved,
            pass_rate: if trials == 0 {
                0.0
            } else {
                solved as f64 / trials as f64
            },
            mean_reward: mean(&rewards),
            total_cost_usd,
            mean_cost_usd: mean(
                &paired
                    .iter()
                    .map(|t| t.outcome.cost_usd)
                    .collect::<Vec<_>>(),
            ),
            total_tokens,
            mean_tokens: mean(&tokens),
            median_turns: median(&turns),
            mean_retries: mean(&retries),
        }
    }
}
