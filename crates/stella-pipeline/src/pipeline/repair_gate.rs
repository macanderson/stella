//! The repair gate (#1479): the single place that answers "verification just
//! refuted the model's claim — is there room to try again?".
//!
//! Every failing rung of the verification ladder used to answer it with the
//! same bare count, `revisions >= max_revisions`. A count cannot see the
//! constraint that actually binds. The run that produced this issue caught
//! the worker claiming a byte-for-byte match that did not exist, spent its
//! two revisions, and reported `VerificationFailed` with roughly two thirds
//! of its allowance still unspent — so the expensive half of verification
//! (knowing the work is wrong) bought nothing, and a caught wrong answer
//! scored exactly like an uncaught one.
//!
//! The decision itself lives in [`stella_core::repair`], which is pure and
//! bounded and property-tested there. This module is only the measuring tape:
//! it reads what the pipeline actually knows about the run's remaining money
//! and wall clock and hands those over as owned data.

use std::time::Instant;

use stella_core::BudgetGuard;
use stella_core::repair::{RepairCost, RepairHeadroom, RepairPlan, plan_repair};
use stella_protocol::BudgetMode;

use super::{CandidateState, Pipeline};

/// Meters one candidate's verification arc from the moment it starts
/// verifying, so the gate can price the next repair attempt from the rounds
/// that already ran.
///
/// Started once per candidate and never restarted: the estimate the gate
/// wants is the *mean* round, and a mean over the whole arc is both steadier
/// than the last round alone and cheaper to thread — one value taken before
/// the loop instead of a reassignment on every path that revises.
///
/// It starts where verification starts, so the candidate's first worker turn
/// sits outside it and the first round's mean reads low. Every round after
/// that contains a whole repair (the revise turn plus the verification round
/// it earned), so the estimate only ever climbs toward the truth — and it
/// errs toward granting, which the cap is there to bound.
pub(super) struct RepairMeter {
    started_usd: f64,
    started_at: Instant,
}

impl RepairMeter {
    /// Begin metering at the run's current cumulative spend.
    pub(super) fn start(spent_usd: f64) -> Self {
        Self {
            started_usd: spent_usd,
            started_at: Instant::now(),
        }
    }

    /// The mean cost of the `rounds` verification rounds run so far, given
    /// the run's current cumulative spend.
    fn mean_round(&self, spent_usd: f64, rounds: u32) -> RepairCost {
        RepairCost::mean_of(
            rounds,
            (spent_usd - self.started_usd).max(0.0),
            self.started_at.elapsed(),
        )
    }
}

impl Pipeline<'_> {
    /// Whether a refuted verdict earns another repair round.
    ///
    /// `false` ends the candidate on the verdict it was just handed — and
    /// says why first, because a run that stops with room it could not use
    /// looks identical from the outside to one that simply gave up
    /// (degradation warns, it never disables).
    pub(super) fn affords_repair(
        &self,
        state: &CandidateState,
        meter: &RepairMeter,
        spent_usd: f64,
        budget: &BudgetGuard,
    ) -> bool {
        // Rounds run = revisions spent + the one that just produced this
        // refutation. All of them price the mean round, demands included —
        // a demand round runs the same worker-turn-plus-verification shape.
        let rounds = state.revisions.saturating_add(1);
        // Repair attempts spent = revision turns minus evidence demands
        // (#1509). A demand corroborates a PASSING verdict; a repair fixes a
        // refuted one — counting the demand here walked a candidate into its
        // next refutation one repair round short of the configured allowance.
        let repairs = state.revisions.saturating_sub(state.evidence_demands);
        match plan_repair(
            repairs,
            self.config.max_revisions,
            self.repair_headroom(budget),
            meter.mean_round(spent_usd, rounds),
        ) {
            RepairPlan::Attempt { .. } => true,
            RepairPlan::Stop(refusal) => {
                self.warn(format!(
                    "verification refuted this result and no repair round was started: {}.",
                    refusal.sentence()
                ));
                false
            }
        }
    }

    /// What this run can still measure about its own remaining room.
    ///
    /// Both axes are `None` unless the caller declared the limit itself. An
    /// `Off` budget guard is treated as unmetered even when limits are set on
    /// it: `Off` means nothing is gated on spend, and letting a decorative
    /// limit stop a repair round would make it a hard ceiling by the back
    /// door.
    fn repair_headroom(&self, budget: &BudgetGuard) -> RepairHeadroom {
        RepairHeadroom {
            budget_usd: (budget.mode() != BudgetMode::Off)
                .then(|| budget.headroom_usd())
                .flatten(),
            // The run-scoped deadline, measured from when this pipeline was
            // constructed. Deliberately NOT the engine's `turn_budget`
            // (#1507): that allowance is per engine turn and a multi-step
            // run holds many, so metering the whole run's elapsed time
            // against one turn's budget turned this axis into a false
            // refusal after the first turn's worth of elapsed time.
            wall_clock: self
                .config
                .run_budget
                .map(|budget| budget.saturating_sub(self.started.elapsed())),
        }
    }
}
