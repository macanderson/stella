// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How long a step took, as the guess for what the next one needs.
//!
//! The engine stops a turn early when the clock left is less than one more
//! step needs. A step is not just its model call. It is the call plus every
//! tool the call asked for. A reserve read off the call alone keeps 8
//! seconds for a step that thought for 8 seconds and then ran `bash` for 4
//! minutes, and the turn then starts a step it cannot finish.
//!
//! [`StepPace`] keeps both times. `model` is the model call by itself.
//! `step` is the whole step: the same call, its tools, and any wait a tool
//! asked for. The reserve is the larger of the two.
//!
//! # Why the larger, and not the sum
//!
//! `step` already holds `model` inside it, so on a step that ran to the
//! boundary it is the bigger number and it wins. `model` is the floor for a
//! step that ends sooner. A budget abort or a cut-off reply leaves the model
//! time timed and no tool time to time. The larger of the two covers both,
//! and the caller never has to say which kind of step it just saw.
//!
//! # Why the last step, and not a mean
//!
//! [`crate::driver::settlement`] adds no safety margin of its own, because
//! the caller owns the deadline. A weight for a rolling mean would be such a
//! margin, picked by taste, with nothing to measure it against. It would
//! also read low right after the slow step this exists to catch, which is
//! the one moment the reserve has to be right.
//!
//! Pure data and pure sums. The driver reads the clock and hands the
//! numbers in (AGENTS.md #2).

use std::time::Duration;

/// The pace of this turn's steps: the last model call, and the last whole
/// step. Lives on [`crate::step::TurnState`] and dies with the turn.
///
/// Not part of a checkpoint, for the reason the other turn latches are not:
/// a turn that resumes has a new caller with a new deadline, so it times its
/// own first step and forecasts from that.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct StepPace {
    model: Option<Duration>,
    step: Option<Duration>,
}

impl StepPace {
    /// Record how long one model call took, retries and all. That is what
    /// the same call would cost again, not the one try that worked.
    pub(crate) fn observe_model(&mut self, took: Duration) {
        self.model = Some(took);
    }

    /// Record how long one whole step took: the model call, the tools it
    /// asked for, and any wait a tool asked the turn to take.
    pub(crate) fn observe_step(&mut self, took: Duration) {
        self.step = Some(took);
    }

    /// The last model call by itself, or `None` before the first one.
    ///
    /// Read by one caller only:
    /// [`ContinuationBudget`](crate::driver::truncation::ContinuationBudget).
    /// A length continuation re-runs a step that made no tool call, so tool
    /// time is not part of what it will cost.
    pub(crate) fn model(&self) -> Option<Duration> {
        self.model
    }

    /// How much clock to keep for one more step. Zero until a step has been
    /// timed, which leaves the deadline check reactive — there is nothing to
    /// forecast from yet, and a made-up number would be a guess at the model.
    pub(crate) fn reserve(&self) -> Duration {
        self.model
            .unwrap_or_default()
            .max(self.step.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The witness for the whole module. A step whose tools ran far longer
    /// than its model call must keep the whole step, not the call.
    #[test]
    fn tool_time_is_part_of_the_reserve() {
        let mut pace = StepPace::default();
        pace.observe_model(Duration::from_secs(8));
        pace.observe_step(Duration::from_secs(248));

        assert_eq!(
            pace.reserve(),
            Duration::from_secs(248),
            "a step that thought for 8s and then ran a tool for 4 minutes \
             must not reserve 8s"
        );
    }

    /// A step that ends before its tools run leaves the model time as the
    /// floor, so the reserve never drops to zero after a real call.
    #[test]
    fn a_step_that_never_dispatched_keeps_the_model_time() {
        let mut pace = StepPace::default();
        pace.observe_model(Duration::from_secs(30));

        assert_eq!(pace.reserve(), Duration::from_secs(30));
        assert_eq!(
            pace.model(),
            Some(Duration::from_secs(30)),
            "and the continuation forecast is that same call"
        );
    }

    /// Nothing timed yet means no forecast. The deadline check then behaves
    /// as it did before the reserve existed.
    #[test]
    fn an_untimed_turn_reserves_nothing() {
        let pace = StepPace::default();

        assert_eq!(pace.reserve(), Duration::ZERO);
        assert_eq!(pace.model(), None);
    }

    /// The continuation forecast stays the model call alone even after a
    /// slow step. A continuation re-runs a step that called no tool.
    #[test]
    fn the_continuation_forecast_leaves_tool_time_out() {
        let mut pace = StepPace::default();
        pace.observe_model(Duration::from_secs(5));
        pace.observe_step(Duration::from_secs(200));

        assert_eq!(pace.model(), Some(Duration::from_secs(5)));
    }

    /// Each step replaces the last. A fast step after a slow one lowers the
    /// reserve, so one long tool call cannot hold the turn back all turn.
    #[test]
    fn the_newest_step_is_the_one_that_counts() {
        let mut pace = StepPace::default();
        pace.observe_model(Duration::from_secs(1));
        pace.observe_step(Duration::from_secs(240));
        pace.observe_model(Duration::from_secs(1));
        pace.observe_step(Duration::from_secs(2));

        assert_eq!(pace.reserve(), Duration::from_secs(2));
    }
}
