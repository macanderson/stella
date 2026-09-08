// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The clock a turn runs against, read once each step.
//!
//! [`crate::EngineConfig::turn_budget`] bounds a turn: it is a span measured
//! from the start of that turn.
//! [`crate::budget::BudgetGuard::task_deadline`] is a point in time set for
//! the whole task. In the shipped binary both come from `--turn-timeout`.
//! Nothing makes them agree. A turn must outlive neither. So [`TurnClock`]
//! keeps the earlier one, and every reader below the step boundary reads it.
//!
//! # What this fixes
//!
//! A `bash` call names its own time limit. The model picks it, and it may be
//! as long as ten minutes. Nothing weighed that against the clock the turn
//! had left. So a ten-minute call could start at t=350s of an 840-second
//! budget and run to t=950s. The harness kills the process first. Every edit
//! the turn made dies with it. On the 89-task Terminal-Bench 2.1 run of
//! 2026-09-07, 17 of 32 failures ended that way. The engine's own deadline
//! never fired once (`#6460`).
//!
//! The engine may not stop a call that is running (AGENTS.md #6). So the one
//! moment it can act is before the call starts. That is what this decides.
//!
//! # Pure, like its neighbours
//!
//! The driver reads the clock and hands `now` in (AGENTS.md #2). That is what
//! [`crate::driver::settlement`] and
//! [`ContinuationBudget`] do beside
//! it. Nothing here calls `Instant::now`.

use std::time::{Duration, Instant};

use super::truncation::ContinuationBudget;

/// The clock as the step boundary read it.
///
/// Copied down the call chain, not stored on [`crate::Engine`]. That engine
/// is a set of borrowed ports, shared across the tool futures. A clock kept
/// there would need a cell, written at one site and read far from it. ADR
/// 0041 records the choice.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct TurnClock {
    /// When this turn must be done: the earlier of the two bounds the module
    /// docs name. `None` when nothing is timing it.
    deadline: Option<Instant>,
    /// How long the last model call took, retries and all
    /// ([`super::step_pace::StepPace::model`]). It is the guess at what one
    /// more call costs. Both readers below hold that much back.
    last_model_call: Option<Duration>,
}

impl TurnClock {
    /// Read both bounds and keep the earlier one.
    ///
    /// A `turn_budget` so large that the clock cannot name its end is dropped
    /// rather than added. `Instant + Duration` panics on overflow, and
    /// `--turn-timeout` is a number a person types, so the sum is runtime
    /// data (AGENTS.md #5). A budget no clock can reach could never bind a
    /// call anyway, so `None` is also the right answer.
    pub(super) fn read(config: &crate::EngineConfig, state: &crate::step::TurnState) -> Self {
        let from_turn_budget = config
            .turn_budget
            .and_then(|budget| state.started_at.checked_add(budget));
        let deadline = match (from_turn_budget, state.budget.task_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (only, None) | (None, only) => only,
        };
        Self {
            deadline,
            last_model_call: state.pace.model(),
        }
    }

    /// Time left at `now`. `None` when nothing is timing the turn.
    fn remaining(&self, now: Instant) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    /// What a length continuation weighs itself against.
    ///
    /// `None` when either half is missing. With no deadline there is nothing
    /// to decline against. With no timed call there is no guess at what
    /// declining would save.
    pub(super) fn continuation_budget(&self, now: Instant) -> Option<ContinuationBudget> {
        Some(ContinuationBudget {
            remaining: self.remaining(now)?,
            last_step: self.last_model_call?,
        })
    }

    /// Whether a call naming `declared` may start at `now`.
    ///
    /// What is held back is the last model call. A refused call goes to the
    /// model as a result, and the model then has to answer. A call that fits
    /// the deadline to the second leaves nothing to answer with. The turn is
    /// killed on the way to saying what it did.
    ///
    /// Strictly less, like
    /// [`ContinuationBudget::affords_another`](super::truncation). The
    /// held-back time is a guess from one past sample. Spend the last of the
    /// clock on it and the turn lands on the deadline at best.
    ///
    /// No margin is made up here. The caller owns the deadline and can set a
    /// safe one. A margin here would be a second rule the operator never saw.
    /// `settlement::check_budget` says the same thing.
    pub(super) fn admit_tool(&self, declared: Duration, now: Instant) -> ToolAdmission {
        // No clock, no clamp. A caller that never said how long the turn has
        // must not have work turned down on a guess.
        let Some(remaining) = self.remaining(now) else {
            return ToolAdmission::Start;
        };
        let affordable = remaining.saturating_sub(self.last_model_call.unwrap_or_default());
        if declared < affordable {
            return ToolAdmission::Start;
        }
        ToolAdmission::Decline { affordable }
    }
}

/// What [`TurnClock::admit_tool`] said about one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolAdmission {
    /// Start the call.
    Start,
    /// Refuse it. `affordable` is the longest limit that still leaves room to
    /// report back. It is what the model is told.
    Decline { affordable: Duration },
}

/// What a call refused for time hands back to the model.
///
/// It names a number. [`super::dispatch::HALTED_TOOL_RESULT`] may not, and
/// its own doc says why: loop detection keys on output that matches byte for
/// byte, so a figure that shifts per call hides a run of like calls from the
/// rungs that read them.
///
/// The number is the fix, and that is what makes it safe here. A model told
/// it may ask for 40 seconds asks for 40 seconds. The refusal ends the repeat
/// rather than feeding it. A turn in this state is also one model call from
/// its deadline. It cannot outlast a rung that wants three in a row.
///
/// The words name a limit, never `timeout_secs`. That key belongs to
/// `stella-tools`. The engine does not know it (AGENTS.md #1).
pub(super) fn refused_for_time(declared: Duration, affordable: Duration) -> String {
    format!(
        "not executed — this call declared a {:.0}s time limit, and only {:.0}s of this turn's \
         wall clock remain once room is kept to report back. Starting it would end the turn from \
         outside and throw away the work already done. Re-run it with a limit under {:.0}s, split \
         it into shorter steps, or finish now with what you already have.",
        declared.as_secs_f64(),
        affordable.as_secs_f64(),
        affordable.as_secs_f64(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock with `remaining` left and a `last_model_call` reserve, built
    /// against `now` so the tests read as durations rather than instants.
    fn clock(now: Instant, remaining: u64, reserve: u64) -> TurnClock {
        TurnClock {
            deadline: Some(now + Duration::from_secs(remaining)),
            last_model_call: Some(Duration::from_secs(reserve)),
        }
    }

    /// **The witness for `#6460`.** A ten-minute `bash` call against a turn
    /// with under eight minutes left is refused, and the refusal names what
    /// the model may ask for instead.
    #[test]
    fn a_call_longer_than_the_turn_has_left_is_refused() {
        let now = Instant::now();
        let clock = clock(now, 490, 40);

        let ToolAdmission::Decline { affordable } = clock.admit_tool(Duration::from_secs(600), now)
        else {
            panic!("a 600s call cannot start with 490s left");
        };
        assert_eq!(affordable, Duration::from_secs(450));

        let message = refused_for_time(Duration::from_secs(600), affordable);
        assert!(message.contains("600s"), "{message}");
        assert!(message.contains("450s"), "{message}");
    }

    /// The control: the same clock admits a call that fits.
    #[test]
    fn a_call_that_fits_starts() {
        let now = Instant::now();
        let clock = clock(now, 490, 40);

        assert_eq!(
            clock.admit_tool(Duration::from_secs(120), now),
            ToolAdmission::Start
        );
    }

    /// Exactly enough is not enough, the rule `affords_another` already
    /// follows: the reserve is one sample, so spending the last of the clock
    /// on it lands on the deadline in the best case.
    #[test]
    fn exactly_enough_time_is_not_enough() {
        let now = Instant::now();
        let clock = clock(now, 160, 40);

        assert!(matches!(
            clock.admit_tool(Duration::from_secs(120), now),
            ToolAdmission::Decline { .. }
        ));
    }

    /// A turn nobody is timing behaves exactly as it did before the clamp
    /// existed. Declining work on a clock that was never set would be a guess.
    #[test]
    fn an_untimed_turn_clamps_nothing() {
        let now = Instant::now();
        let clock = TurnClock::default();

        assert_eq!(
            clock.admit_tool(Duration::from_secs(600), now),
            ToolAdmission::Start
        );
        assert!(clock.continuation_budget(now).is_none());
    }

    /// Before any model call has been timed the reserve is zero, which leaves
    /// the clamp reactive rather than guessing at a number — the discipline
    /// `StepPace::reserve` already applies to the deadline check.
    #[test]
    fn an_untimed_call_reserves_nothing_and_still_clamps() {
        let now = Instant::now();
        let clock = TurnClock {
            deadline: Some(now + Duration::from_secs(100)),
            last_model_call: None,
        };

        assert_eq!(
            clock.admit_tool(Duration::from_secs(600), now),
            ToolAdmission::Decline {
                affordable: Duration::from_secs(100)
            }
        );
        assert_eq!(
            clock.admit_tool(Duration::from_secs(60), now),
            ToolAdmission::Start
        );
    }

    /// A deadline already past refuses everything, including a call declaring
    /// nothing at all — there is no clock left to run it against.
    #[test]
    fn a_passed_deadline_refuses_every_call() {
        let now = Instant::now();
        let clock = TurnClock {
            deadline: Some(now - Duration::from_secs(5)),
            last_model_call: Some(Duration::from_secs(40)),
        };

        assert_eq!(
            clock.admit_tool(Duration::ZERO, now),
            ToolAdmission::Decline {
                affordable: Duration::ZERO
            }
        );
    }

    /// The clock closes as the step's earlier calls run. A pair of calls that
    /// each fit at the top of the step must not both start when only one of
    /// them fits by the time the second is reached.
    #[test]
    fn the_clock_is_re_read_as_a_step_proceeds() {
        let start = Instant::now();
        let clock = clock(start, 700, 40);
        let declared = Duration::from_secs(400);

        assert_eq!(clock.admit_tool(declared, start), ToolAdmission::Start);
        assert!(matches!(
            clock.admit_tool(declared, start + declared),
            ToolAdmission::Decline { .. }
        ));
    }

    /// A budget the clock cannot reach the end of is dropped, not added. The
    /// sum would panic, and a deadline that far out binds nothing in any case.
    #[test]
    fn an_unreachable_turn_budget_is_no_deadline_at_all() {
        let now = Instant::now();
        let config = crate::EngineConfig {
            turn_budget: Some(Duration::MAX),
            ..crate::EngineConfig::default()
        };
        let state = crate::step::TurnState::new(
            Vec::new(),
            crate::budget::BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None),
            &config,
        );

        let clock = TurnClock::read(&config, &state);

        assert_eq!(
            clock.admit_tool(Duration::from_secs(600), now),
            ToolAdmission::Start
        );
    }

    /// The earlier of the two ceilings wins, whichever it is. Nothing makes
    /// an advisory turn budget and an enforced task deadline agree, and the
    /// turn must outlive neither.
    #[test]
    fn the_tighter_of_the_two_ceilings_binds() {
        let now = Instant::now();
        let tight_task = TurnClock {
            deadline: Some(now + Duration::from_secs(600)),
            last_model_call: None,
        };
        let tight_turn = TurnClock {
            deadline: Some(now + Duration::from_secs(60)),
            last_model_call: None,
        };

        assert_eq!(
            tight_task.admit_tool(Duration::from_secs(120), now),
            ToolAdmission::Start
        );
        assert!(matches!(
            tight_turn.admit_tool(Duration::from_secs(120), now),
            ToolAdmission::Decline { .. }
        ));
    }

    /// The continuation forecast survives the move onto this clock: it still
    /// needs a deadline and a timed call, and still reports what each was.
    #[test]
    fn the_continuation_budget_carries_both_halves() {
        let now = Instant::now();
        let budget = clock(now, 490, 40)
            .continuation_budget(now)
            .expect("a clock with both halves answers");

        assert_eq!(budget.remaining, Duration::from_secs(490));
        assert_eq!(budget.last_step, Duration::from_secs(40));
    }
}
