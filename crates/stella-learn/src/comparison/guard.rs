//! The guard set: metrics a winner may not regress, however good the primary
//! looks.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use super::arm::ArmSummary;
use super::metric::Metric;

/// A metric the candidate must not move against, and by how much it may.
///
/// The guard set exists because a single-metric A/B is trivially gameable by
/// the thing being measured: a candidate can win a pass-rate comparison by
/// spending four times the money, taking twice the turns, or retrying until
/// something sticks. Those are prices, not wins, and the operator is the one
/// who says which prices are acceptable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Guard {
    /// The metric being guarded.
    pub metric: Metric,
    /// The most the candidate may move **against** the metric's good
    /// direction, in the metric's own units. `0.0` forbids any regression at
    /// all; `0.05` on [`Metric::PassRate`] tolerates five points.
    ///
    /// A negative tolerance is not an error and is not silently clamped: it
    /// demands a strict *improvement* on the guarded metric, which is a
    /// coherent thing to ask for and the natural spelling of it.
    pub tolerance: f64,
}

impl Guard {
    /// A guard that forbids any regression at all on `metric`.
    pub fn strict(metric: Metric) -> Self {
        Self {
            metric,
            tolerance: 0.0,
        }
    }

    /// Measure the candidate against the baseline on this guard.
    pub fn evaluate(self, baseline: &ArmSummary, candidate: &ArmSummary) -> GuardFinding {
        let baseline_value = self.metric.report_value(baseline);
        let candidate_value = self.metric.report_value(candidate);
        // Signed movement in the metric's *good* direction, so one comparison
        // covers both orientations and no caller has to remember which way a
        // cost is supposed to go.
        let delta = if self.metric.higher_is_better() {
            candidate_value - baseline_value
        } else {
            baseline_value - candidate_value
        };
        GuardFinding {
            metric: self.metric,
            baseline: baseline_value,
            candidate: candidate_value,
            delta,
            tolerance: self.tolerance,
            // "Not provably within tolerance", rather than "provably outside
            // it". The difference is `NaN`: a metric that could not be
            // computed is incomparable, and this reads incomparable as a
            // regression. A guard that waves through the case it cannot
            // evaluate is not a guard.
            regressed: !matches!(
                delta.partial_cmp(&-self.tolerance),
                Some(Ordering::Greater | Ordering::Equal)
            ),
        }
    }
}

/// What one guard found: both sides' values, the signed movement, and the
/// verdict.
///
/// Emitted for every configured guard, held or regressed, because a promotion
/// record that lists only the failures cannot prove the others were checked.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GuardFinding {
    /// The metric this finding is about.
    pub metric: Metric,
    /// The baseline arm's value, in the metric's own units.
    pub baseline: f64,
    /// The candidate arm's value, in the metric's own units.
    pub candidate: f64,
    /// Movement in the metric's **good** direction: positive is an
    /// improvement, negative a regression, whichever way the raw units run.
    pub delta: f64,
    /// The tolerance this was judged against.
    pub tolerance: f64,
    /// `true` when `delta` fell past `-tolerance`, or could not be computed.
    pub regressed: bool,
}
