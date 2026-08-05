//! Repair admission — whether a refuted success claim earns another attempt.
//!
//! Verification catching the model lying about success is the expensive half
//! of a run: the tests were run, the diff was read, the verdict was paid for.
//! Ending the run on that finding throws all of it away, and scores a *caught*
//! wrong answer identically to an uncaught one. What was missing is the cheap
//! half — a path from "the probe says FAIL" back into the loop while the run
//! still has room to act on it (#1479).
//!
//! Room is the whole of the decision, so this module is where it is decided,
//! and it is decided the same way [`crate::driver`]'s length-continuation
//! allowance is: pure synchronous functions over owned data, with the caller
//! supplying the measurements. Nothing here reads a clock, a budget guard, or
//! a config file.
//!
//! # Bounded three ways
//!
//! An unbounded verify/repair cycle trades one failure class for another —
//! a wrong answer becomes a timeout — so admission is bounded on three
//! independent axes, and the *tightest* one binds:
//!
//! 1. **The cap.** [`REPAIR_ATTEMPT_CAP`] is the hard ceiling on attempts for
//!    one verification arc, whatever any measurement says.
//! 2. **The allowance.** Attempts inside the caller's configured revision
//!    allowance are granted unconditionally — that is the behaviour every
//!    caller already had, and this module never takes it away.
//! 3. **The measurement.** Attempts *past* the allowance are granted only
//!    when a measured axis says another one fits.
//!
//! # Why an unmeasured run gets nothing extra
//!
//! [`RepairHeadroom`] is `Option` on every axis, and an axis nobody
//! configured abstains rather than approving. A caller that declared no
//! ceiling and no deadline therefore keeps its old bound exactly, which is
//! the conservative default the same reasoning gives
//! [`crate::driver::EngineConfig::turn_budget`]: only a caller that knows its
//! own limits can say there is room left, and inventing one here would spend
//! money and wall clock nobody agreed to.
//!
//! Refusal is never suppression. A refused repair still returns a named
//! reason for the caller to report; the verdict it was refused for is
//! reported either way (degradation warns, it never disables).

use std::time::Duration;

/// The hard ceiling on repair attempts within one verification arc.
///
/// Four, because the shape this exists for is a worker that was *close* —
/// it did the work and got one criterion wrong — and the measured
/// distribution of those is one or two more rounds, not ten. Past that the
/// evidence is not steering the model and more rounds buy a timeout rather
/// than a fix, which is the failure class this bound exists to avoid trading
/// into. It is a ceiling and not a target: the measured axes below routinely
/// stop well short of it.
pub const REPAIR_ATTEMPT_CAP: u32 = 4;

/// What one repair attempt is expected to cost, taken from the attempts
/// already run.
///
/// The estimate is deliberately backward-looking. A repair attempt re-runs
/// the same shape of work the refuted attempt just ran — one worker turn plus
/// one verification round — so what it already cost is the best available
/// statement of what the next one will cost. This mirrors
/// `driver::truncation::ContinuationBudget`, which decides the same question
/// for length continuations by the same rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RepairCost {
    /// Expected spend, in USD.
    pub usd: f64,
    /// Expected wall clock.
    pub wall: Duration,
}

impl RepairCost {
    /// The mean cost of `rounds` rounds that together spent `usd` and `wall`.
    ///
    /// `rounds` is saturated to at least one, so a caller that has not
    /// finished a round yet gets the whole measurement rather than a
    /// division by zero.
    #[must_use]
    pub fn mean_of(rounds: u32, usd: f64, wall: Duration) -> Self {
        let rounds = rounds.max(1);
        Self {
            usd: (usd / f64::from(rounds)).max(0.0),
            wall: wall / rounds,
        }
    }
}

/// What the run has left, on each axis the caller can actually measure.
///
/// `None` means "nobody is measuring this", never "there is none left" — an
/// unmeasured axis abstains from the decision entirely. See the module docs
/// for why that default is the conservative one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RepairHeadroom {
    /// USD left before the tightest configured budget limit trips
    /// (`BudgetGuard::headroom_usd`), or `None` when the guard is unmetered.
    pub budget_usd: Option<f64>,
    /// Wall clock left before the caller's own deadline
    /// ([`crate::driver::EngineConfig::turn_budget`]), or `None` when the
    /// caller declared no deadline.
    pub wall_clock: Option<Duration>,
}

impl RepairHeadroom {
    /// Whether no axis is measured at all.
    #[must_use]
    pub fn is_unmeasured(&self) -> bool {
        self.budget_usd.is_none() && self.wall_clock.is_none()
    }

    /// Whether every *measured* axis has strictly more left than `cost`.
    ///
    /// Strictly greater, and no safety margin invented here: the caller owns
    /// its limits and can set conservative ones. A margin baked in at this
    /// level would be a second, invisible policy — the same call
    /// `ContinuationBudget::affords_another` makes.
    #[must_use]
    pub fn affords(&self, cost: RepairCost) -> bool {
        self.budget_usd.is_none_or(|left| left > cost.usd)
            && self.wall_clock.is_none_or(|left| left > cost.wall)
    }
}

/// Why a refuted verdict did not earn another attempt. Every variant is a
/// reportable reason — a refusal is stated, never silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairRefusal {
    /// [`REPAIR_ATTEMPT_CAP`] (or the caller's larger allowance) is spent.
    /// The evidence stopped steering the model; more rounds buy a timeout.
    CapReached,
    /// The configured allowance is spent and nothing measures the run, so
    /// nothing can say another attempt is affordable.
    Unmeasured,
    /// A measured axis cannot afford another attempt. Refused even *inside*
    /// the allowance: starting a repair that cannot finish converts a
    /// reported failure into a budget abort or an external kill, which is
    /// strictly worse than reporting the verdict.
    NoHeadroom,
}

impl RepairRefusal {
    /// One clause, for the operator-facing warning a caller emits alongside
    /// the verdict it is about to report.
    #[must_use]
    pub fn sentence(self) -> &'static str {
        match self {
            Self::CapReached => "the repair-attempt cap is spent",
            Self::Unmeasured => {
                "the revision allowance is spent and no budget or deadline is set to \
                 justify another attempt"
            }
            Self::NoHeadroom => "there is not enough budget or wall clock left to finish another",
        }
    }
}

/// What happens after verification refutes the model's success claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairPlan {
    /// Re-enter the loop. `attempt` is 1-based and `cap` is the ceiling it
    /// counts against, so a caller can say "repair 3/4" without re-deriving
    /// the bound.
    Attempt { attempt: u32, cap: u32 },
    /// Stop and report the verdict, for this reason.
    Stop(RepairRefusal),
}

impl RepairPlan {
    /// Whether this plan re-enters the loop.
    #[must_use]
    pub fn is_attempt(self) -> bool {
        matches!(self, Self::Attempt { .. })
    }
}

/// Decide whether a refuted success claim earns another repair attempt.
///
/// `spent` is how many repair attempts this verification arc has already
/// made, `allowance` the caller's configured revision allowance, and the
/// remaining two arguments are the measurements described on
/// [`RepairHeadroom`] and [`RepairCost`]. The effective cap is
/// `max(allowance, REPAIR_ATTEMPT_CAP)` — a caller that configured a *larger*
/// allowance than the ceiling keeps it, because the ceiling exists to bound
/// what this module grants on its own, never to withdraw what the caller
/// already asked for.
///
/// The order the bounds are tested in is load-bearing. The cap is absolute,
/// so it comes first. Affordability comes next and applies at every level,
/// including inside the allowance: a run with a measured axis that cannot pay
/// for another attempt must not start one, whatever its count says. Only then
/// does the allowance grant, and only past the allowance does an unmeasured
/// run stop.
#[must_use]
pub fn plan_repair(
    spent: u32,
    allowance: u32,
    headroom: RepairHeadroom,
    cost: RepairCost,
) -> RepairPlan {
    let cap = allowance.max(REPAIR_ATTEMPT_CAP);
    if spent >= cap {
        return RepairPlan::Stop(RepairRefusal::CapReached);
    }
    if !headroom.affords(cost) {
        return RepairPlan::Stop(RepairRefusal::NoHeadroom);
    }
    if spent >= allowance && headroom.is_unmeasured() {
        return RepairPlan::Stop(RepairRefusal::Unmeasured);
    }
    RepairPlan::Attempt {
        attempt: spent.saturating_add(1),
        cap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Room on the budget axis and nothing else measured.
    fn funded() -> RepairHeadroom {
        RepairHeadroom {
            budget_usd: Some(5.0),
            wall_clock: None,
        }
    }

    /// A cheap round: any realistic headroom affords another one.
    fn cheap() -> RepairCost {
        RepairCost {
            usd: 0.01,
            wall: Duration::from_secs(1),
        }
    }

    /// The witness this module exists for: the allowance is spent, a measured
    /// axis has room, and the refutation buys another round instead of ending
    /// the run.
    #[test]
    fn a_spent_allowance_with_measured_headroom_still_earns_an_attempt() {
        assert_eq!(
            plan_repair(1, 1, funded(), cheap()),
            RepairPlan::Attempt { attempt: 2, cap: 4 }
        );
    }

    /// The conservative default: an unmeasured run keeps its configured bound
    /// exactly, so no caller starts spending more than it used to by doing
    /// nothing.
    #[test]
    fn an_unmeasured_run_stops_at_its_allowance() {
        assert_eq!(
            plan_repair(1, 1, RepairHeadroom::default(), cheap()),
            RepairPlan::Stop(RepairRefusal::Unmeasured)
        );
        // ...and inside the allowance it is unaffected.
        assert!(plan_repair(0, 1, RepairHeadroom::default(), cheap()).is_attempt());
    }

    /// The cap binds however much room the measurements report.
    #[test]
    fn the_cap_binds_a_run_with_unlimited_measured_room() {
        let rich = RepairHeadroom {
            budget_usd: Some(1_000_000.0),
            wall_clock: Some(Duration::from_secs(86_400)),
        };
        assert_eq!(
            plan_repair(REPAIR_ATTEMPT_CAP, 1, rich, cheap()),
            RepairPlan::Stop(RepairRefusal::CapReached)
        );
    }

    /// A caller that configured a larger allowance than the ceiling keeps it:
    /// the ceiling bounds what this module grants unasked, not what the
    /// caller asked for.
    #[test]
    fn a_larger_configured_allowance_is_never_withdrawn() {
        let cap = REPAIR_ATTEMPT_CAP + 4;
        assert_eq!(
            plan_repair(REPAIR_ATTEMPT_CAP, cap, RepairHeadroom::default(), cheap()),
            RepairPlan::Attempt {
                attempt: REPAIR_ATTEMPT_CAP + 1,
                cap,
            }
        );
    }

    /// Refusing *inside* the allowance is the point of testing affordability
    /// before the count: a repair that cannot finish turns a reported failure
    /// into a budget abort or an external kill.
    #[test]
    fn an_unaffordable_attempt_is_refused_even_inside_the_allowance() {
        let broke = RepairHeadroom {
            budget_usd: Some(0.001),
            wall_clock: None,
        };
        assert_eq!(
            plan_repair(0, 3, broke, cheap()),
            RepairPlan::Stop(RepairRefusal::NoHeadroom)
        );
    }

    /// Each axis refuses on its own — a run with money and no time is as
    /// stopped as one with time and no money.
    #[test]
    fn either_measured_axis_can_refuse_alone() {
        let out_of_time = RepairHeadroom {
            budget_usd: Some(500.0),
            wall_clock: Some(Duration::from_millis(10)),
        };
        assert_eq!(
            plan_repair(0, 3, out_of_time, cheap()),
            RepairPlan::Stop(RepairRefusal::NoHeadroom)
        );
    }

    /// An exactly-equal axis refuses: `affords` is strictly greater, so a run
    /// with precisely one attempt's worth left does not start one it would
    /// finish on the line.
    #[test]
    fn headroom_equal_to_the_cost_does_not_afford() {
        let exact = RepairHeadroom {
            budget_usd: Some(cheap().usd),
            wall_clock: None,
        };
        assert!(!plan_repair(0, 3, exact, cheap()).is_attempt());
    }

    #[test]
    fn mean_cost_divides_by_the_rounds_that_produced_it() {
        let mean = RepairCost::mean_of(4, 1.0, Duration::from_secs(8));
        assert!((mean.usd - 0.25).abs() < 1e-9);
        assert_eq!(mean.wall, Duration::from_secs(2));
        // Zero rounds is the caller's first measurement, not a division by
        // zero.
        let first = RepairCost::mean_of(0, 1.0, Duration::from_secs(8));
        assert!((first.usd - 1.0).abs() < 1e-9);
    }

    proptest! {
        /// The bound is absolute: whatever the measurements say, an attempt
        /// is never granted at or past the effective cap, and a granted
        /// attempt number never exceeds it.
        #[test]
        fn no_inputs_grant_an_attempt_past_the_cap(
            spent in 0u32..64,
            allowance in 0u32..16,
            budget in proptest::option::of(0.0f64..1_000.0),
            secs in proptest::option::of(0u64..10_000),
            cost_usd in 0.0f64..10.0,
            cost_secs in 0u64..600,
        ) {
            let plan = plan_repair(
                spent,
                allowance,
                RepairHeadroom {
                    budget_usd: budget,
                    wall_clock: secs.map(Duration::from_secs),
                },
                RepairCost { usd: cost_usd, wall: Duration::from_secs(cost_secs) },
            );
            let cap = allowance.max(REPAIR_ATTEMPT_CAP);
            if let RepairPlan::Attempt { attempt, cap: reported } = plan {
                prop_assert_eq!(reported, cap);
                prop_assert!(attempt <= cap);
                prop_assert!(spent < cap);
            }
        }

        /// Termination: repeatedly granting attempts from a run whose costs
        /// are always affordable still stops, in at most `cap` steps. This is
        /// the property that makes the re-entry safe to wire into a loop.
        #[test]
        fn granting_every_affordable_attempt_still_terminates(allowance in 0u32..16) {
            let unlimited = RepairHeadroom {
                budget_usd: Some(f64::MAX),
                wall_clock: Some(Duration::MAX),
            };
            let cap = allowance.max(REPAIR_ATTEMPT_CAP);
            let mut spent = 0u32;
            while plan_repair(spent, allowance, unlimited, cheap()).is_attempt() {
                spent += 1;
                prop_assert!(spent <= cap, "granted more attempts than the cap allows");
            }
            prop_assert_eq!(spent, cap);
        }
    }
}
