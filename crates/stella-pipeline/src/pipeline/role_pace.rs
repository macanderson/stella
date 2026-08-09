//! What a management call costs in wall clock, measured per role (#2432).
//!
//! The pipeline's anticipatory deadline rung needs to answer "can the clock
//! that remains fit one more call of this role?", and an answer is only worth
//! having if it comes from a measurement. `stella-core`'s raw-call seam
//! deliberately holds no such measurement — a margin invented there would be a
//! second, invisible policy on top of the operator's `--turn-budget`, which is
//! exactly what `stella_core::driver::settlement::check_budget` refuses to do
//! to itself. This module is the pipeline's equivalent of the basis the engine
//! already has: where the step loop forecasts from `TurnState::last_step`,
//! which it measures, the pipeline forecasts from the last completed call of
//! the same role, which it measures here.
//!
//! Two properties are load-bearing and both are the conservative direction:
//!
//! - **No history, no forecast.** The first call of any role reports `None`
//!   and is never refused on anticipation. A pipeline that has measured
//!   nothing anticipates nothing.
//! - **Per role, not per run.** A triage classification and a verdict on a
//!   large diff are not interchangeable estimates, and the case #2432 names —
//!   a verdict dispatched with seconds left and taking a minute — is precisely
//!   a role whose history accumulates across revision rounds.

use std::sync::Mutex;
use std::time::Duration;

use stella_protocol::ModelCallRole;

/// The wall clock the last completed call of each role took.
///
/// Interior mutability because the dispatch seam that reads and writes it
/// (`super::Pipeline::dispatch_raw`) takes `&self`, the same reason
/// `Pipeline::raw_call_seq` is an atomic. A `Vec` of pairs rather than a map
/// because [`ModelCallRole`] is a closed enum of about a dozen variants and is
/// not `Hash` — a linear scan over twelve `Copy` values is cheaper than the
/// hashing it would replace, and needs no new derive on a wire type.
#[derive(Debug, Default)]
pub(super) struct RolePace {
    observed: Mutex<Vec<(ModelCallRole, Duration)>>,
}

impl RolePace {
    /// What one more call of `role` should be expected to cost, or `None` when
    /// no call of that role has completed yet.
    ///
    /// The last observation rather than a mean or a maximum, mirroring
    /// `TurnState::last_step`: the most recent call is the best available
    /// evidence about the model and prompt size in play right now, and a mean
    /// over a run whose prompts grow monotonically lags the truth in the one
    /// direction that matters.
    pub(super) fn forecast(&self, role: ModelCallRole) -> Option<Duration> {
        self.observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|(seen, _)| *seen == role)
            .map(|(_, elapsed)| *elapsed)
    }

    /// Record a completed call's wall clock, replacing any earlier observation
    /// for that role.
    ///
    /// Only completed calls belong here — a dispatch that was refused before
    /// it started, or that died in its first milliseconds, would report a
    /// duration that describes the failure rather than the work, and feeding
    /// that back would talk the forecast down exactly when a run is in
    /// trouble.
    pub(super) fn observe(&self, role: ModelCallRole, elapsed: Duration) {
        let mut observed = self
            .observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match observed.iter_mut().find(|(seen, _)| *seen == role) {
            Some(slot) => slot.1 = elapsed,
            None => observed.push((role, elapsed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anticipation needs a measurement: an unmeasured role abstains rather
    /// than reporting zero, so its first call is never refused on a forecast
    /// nobody made.
    #[test]
    fn an_unmeasured_role_has_no_forecast() {
        let pace = RolePace::default();
        assert_eq!(pace.forecast(ModelCallRole::Verdict), None);
        pace.observe(ModelCallRole::Triage, Duration::from_secs(1));
        assert_eq!(
            pace.forecast(ModelCallRole::Verdict),
            None,
            "one role's measurement is not evidence about another's"
        );
    }

    /// The latest observation wins, so a run whose verdict calls are getting
    /// slower forecasts the slowdown rather than averaging it away.
    #[test]
    fn the_last_observation_is_the_forecast() {
        let pace = RolePace::default();
        pace.observe(ModelCallRole::Verdict, Duration::from_secs(2));
        assert_eq!(
            pace.forecast(ModelCallRole::Verdict),
            Some(Duration::from_secs(2))
        );
        pace.observe(ModelCallRole::Verdict, Duration::from_secs(9));
        assert_eq!(
            pace.forecast(ModelCallRole::Verdict),
            Some(Duration::from_secs(9))
        );
    }
}
