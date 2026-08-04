//! The metric vocabulary a comparison can be judged on, with the orientation
//! that tells a guard which way is worse.

use serde::{Deserialize, Serialize};

use super::arm::ArmSummary;
use super::trial::TrialRecord;
use crate::self_tuning::{RewardWeights, reward};

/// A measurable property of an arm — the shared vocabulary of the primary
/// criterion ([`super::ComparisonConfig::primary`]) and every
/// [`super::Guard`].
///
/// Each variant carries an **orientation** ([`Metric::higher_is_better`]),
/// which is what lets one guard type cover both "the pass rate must not drop"
/// and "the cost must not rise" without the caller restating the direction and
/// getting it backwards. A metric with no agreed direction does not belong
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// Fraction of trials that succeeded, in `[0, 1]`. Higher is better.
    PassRate,
    /// Mean reward per trial under the comparison's
    /// [`RewardWeights`] — pass rate net of whatever penalties the operator
    /// priced in. Higher is better.
    Reward,
    /// Mean USD per trial. Lower is better.
    CostUsd,
    /// Mean tokens per trial. Lower is better.
    Tokens,
    /// Median model calls per trial. Lower is better.
    ///
    /// Median rather than mean on purpose: one task that spun to the step cap
    /// drags a mean far enough to hide a real improvement across every other
    /// task, and turn counts are exactly the shape (long right tail, hard
    /// ceiling) where that happens.
    Turns,
    /// Mean retries per trial. Lower is better.
    Retries,
}

impl Metric {
    /// Whether a *higher* value of this metric is the better outcome.
    pub fn higher_is_better(self) -> bool {
        match self {
            Metric::PassRate | Metric::Reward => true,
            Metric::CostUsd | Metric::Tokens | Metric::Turns | Metric::Retries => false,
        }
    }

    /// The wire token — the same string serde emits, so a rendered table and a
    /// JSON report name the metric identically.
    pub fn as_str(self) -> &'static str {
        match self {
            Metric::PassRate => "pass_rate",
            Metric::Reward => "reward",
            Metric::CostUsd => "cost_usd",
            Metric::Tokens => "tokens",
            Metric::Turns => "turns",
            Metric::Retries => "retries",
        }
    }

    /// This metric's value on a folded arm, in the metric's **own units and
    /// own direction** — a cost reads as dollars, not as negative dollars.
    /// This is the number a human reads in a table and a guard compares.
    pub fn report_value(self, arm: &ArmSummary) -> f64 {
        match self {
            Metric::PassRate => arm.pass_rate,
            Metric::Reward => arm.mean_reward,
            Metric::CostUsd => arm.mean_cost_usd,
            Metric::Tokens => arm.mean_tokens,
            Metric::Turns => arm.median_turns,
            Metric::Retries => arm.mean_retries,
        }
    }

    /// One trial's contribution to this metric, **oriented so higher is
    /// better** — the per-trial sample the significance test reads.
    ///
    /// The negation for lower-is-better metrics is the whole trick: it lets a
    /// single [`crate::self_tuning::select_winner`] decide "cost went down
    /// confidently" with the identical arithmetic it uses for "pass rate went
    /// up confidently", instead of a second comparison path that would drift
    /// from the first. Read a value from here only through
    /// [`Metric::report_value`] when showing it to a human.
    ///
    /// [`Metric::Turns`] is the one metric whose per-trial score and reported
    /// value disagree in more than sign: the report shows the median, the
    /// sample is this trial's own count. That is correct — the significance
    /// test wants the raw distribution, and handing it a median would collapse
    /// the variance it exists to measure.
    pub fn trial_score(self, trial: &TrialRecord, weights: &RewardWeights) -> f64 {
        match self {
            Metric::PassRate => f64::from(u8::from(trial.outcome.succeeded)),
            Metric::Reward => reward(&trial.outcome, weights),
            Metric::CostUsd => -trial.outcome.cost_usd,
            Metric::Tokens => -(trial.outcome.tokens as f64),
            Metric::Turns => -f64::from(trial.turns),
            Metric::Retries => -f64::from(trial.outcome.retries),
        }
    }
}

/// The mean of `values`, or `0.0` when there are none. Never `NaN` from an
/// empty input — an arm that ran nothing reports zero, and the sample floor in
/// [`crate::self_tuning::select_winner`] is what stops zero being mistaken for
/// evidence.
pub(super) fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// The median of `values`, or `0.0` when there are none. An even count
/// averages the two middle elements.
///
/// Sorts a copy with `total_cmp`, so a `NaN` sorts to one end deterministically
/// rather than corrupting the partition the way `partial_cmp` + `unwrap` would
/// — and rather than panicking, which library code may not do (invariant #5).
pub(super) fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}
