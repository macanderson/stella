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
//! and it is decided the same way the engine's length-continuation allowance
//! is: pure synchronous functions over owned data, with the caller supplying
//! the measurements. Nothing here reads a clock, a budget guard, or a config
//! file.
//!
//! It lives here because the caller it serves is a verification plugin. The
//! old caller was the staged pipeline, since deleted; `candidate_grant`, the
//! other pure piece that survived, came to this crate too. Nothing in the
//! workspace calls it today. Either a host wires it to the verification path
//! or it goes; Refs #6264 is where that is decided.
//!
//! # Bounded three ways
//!
//! An unbounded verify/repair cycle trades one failure class for another —
//! a wrong answer becomes a timeout — so admission is bounded on three
//! independent axes, and the *tightest* one binds:
//!
//! 1. **The cap.** [`RepairBounds::cap`] bounds one arc's attempts whatever
//!    any measurement says. It defaults to [`REPAIR_ATTEMPT_CAP`].
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
//! `stella_core::driver::EngineConfig::turn_budget`: only a caller that knows its
//! own limits can say there is room left, and inventing one here would spend
//! money and wall clock nobody agreed to.
//!
//! Refusal is never suppression. A refused repair still returns a named
//! reason for the caller to report; the verdict it was refused for is
//! reported either way (degradation warns, it never disables).

use std::time::Duration;

/// The ceiling on repair attempts a caller gets without choosing one.
///
/// A caller that wants a different ceiling sets [`RepairBounds::cap`]; this is
/// what [`RepairBounds::new`] fills in for one that does not.
///
/// Four, because the shape this exists for is a worker that was *close* —
/// it did the work and got one criterion wrong — and the measured
/// distribution of those is one or two more rounds, not ten. Past that the
/// evidence is not steering the model and more rounds buy a timeout rather
/// than a fix, which is the failure class this bound exists to avoid trading
/// into. It is a ceiling and not a target: the measured axes below routinely
/// stop well short of it.
///
/// Carries no `MEASURED:` marker (#4572), for the reason the
/// paragraph above already gives: the distribution bounds this from below and
/// the value sits above it with room to spare.
pub const REPAIR_ATTEMPT_CAP: u32 = 4;

/// The two count-based bounds on one verification arc's repair attempts.
///
/// They answer different questions, and a caller needs to move them apart.
/// The allowance is how many attempts are granted whatever the measurements
/// say; the cap is where granting stops even when a measured axis still has
/// room. Welded together — the shape this replaces, where the ceiling was the
/// constant alone and the allowance was the only knob — "grant more rounds,
/// but only while the budget says so" was inexpressible: raising the ceiling
/// meant raising the unconditional allowance by the same amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairBounds {
    /// Attempts granted whatever the measurements say.
    pub allowance: u32,
    /// Where this module stops granting on its own.
    pub cap: u32,
}

impl RepairBounds {
    /// `allowance` unconditional attempts under the default ceiling.
    #[must_use]
    pub fn new(allowance: u32) -> Self {
        Self {
            allowance,
            cap: REPAIR_ATTEMPT_CAP,
        }
    }

    /// The same allowance under a ceiling the caller chose.
    #[must_use]
    pub fn with_cap(self, cap: u32) -> Self {
        Self { cap, ..self }
    }

    /// The ceiling that binds: `max(allowance, cap)`.
    ///
    /// A cap set below the allowance never withdraws the allowance. The cap
    /// bounds what this module grants unasked; the allowance is what the
    /// caller already asked for, and lowering one knob must not silently
    /// shrink the other.
    #[must_use]
    pub fn effective_cap(self) -> u32 {
        self.allowance.max(self.cap)
    }
}

/// What one repair attempt is expected to cost, taken from the attempts
/// already run.
///
/// The estimate is backward-looking. A repair attempt re-runs
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
    /// Wall clock left before the caller's own deadline — one whose
    /// denominator matches what elapsed time is measured against (a run
    /// budget covers a whole run; a per-turn allowance like
    /// `stella_core::driver::EngineConfig::turn_budget` does not, #1507) — or
    /// `None` when the caller declared no deadline.
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

/// Why a refuted verdict did not earn another attempt. Every case is a
/// reportable reason — a refusal is stated, never silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairRefusal {
    /// [`RepairBounds::effective_cap`] is spent. The evidence stopped
    /// steering the model; more rounds buy a timeout.
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
/// made, `bounds` the two counts described on [`RepairBounds`], and the
/// remaining two arguments are the measurements described on
/// [`RepairHeadroom`] and [`RepairCost`].
///
/// The order the bounds are tested in is decisive. The cap is absolute,
/// so it comes first. Affordability comes next and applies at every level,
/// including inside the allowance: a run with a measured axis that cannot pay
/// for another attempt must not start one, whatever its count says. Only then
/// does the allowance grant, and only past the allowance does an unmeasured
/// run stop.
#[must_use]
pub fn plan_repair(
    spent: u32,
    bounds: RepairBounds,
    headroom: RepairHeadroom,
    cost: RepairCost,
) -> RepairPlan {
    let cap = bounds.effective_cap();
    if spent >= cap {
        return RepairPlan::Stop(RepairRefusal::CapReached);
    }
    if !headroom.affords(cost) {
        return RepairPlan::Stop(RepairRefusal::NoHeadroom);
    }
    if spent >= bounds.allowance && headroom.is_unmeasured() {
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
            plan_repair(1, RepairBounds::new(1), funded(), cheap()),
            RepairPlan::Attempt { attempt: 2, cap: 4 }
        );
    }

    /// The conservative default: an unmeasured run keeps its configured bound
    /// exactly, so no caller starts spending more than it used to by doing
    /// nothing.
    #[test]
    fn an_unmeasured_run_stops_at_its_allowance() {
        assert_eq!(
            plan_repair(1, RepairBounds::new(1), RepairHeadroom::default(), cheap()),
            RepairPlan::Stop(RepairRefusal::Unmeasured)
        );
        // ...and inside the allowance it is unaffected.
        assert!(
            plan_repair(0, RepairBounds::new(1), RepairHeadroom::default(), cheap()).is_attempt()
        );
    }

    /// The cap binds however much room the measurements report.
    #[test]
    fn the_cap_binds_a_run_with_unlimited_measured_room() {
        let rich = RepairHeadroom {
            budget_usd: Some(1_000_000.0),
            wall_clock: Some(Duration::from_secs(86_400)),
        };
        assert_eq!(
            plan_repair(REPAIR_ATTEMPT_CAP, RepairBounds::new(1), rich, cheap()),
            RepairPlan::Stop(RepairRefusal::CapReached)
        );
    }

    /// A caller that configured a larger allowance than the ceiling keeps it:
    /// the ceiling bounds what this module grants unasked, not what the
    /// caller asked for.
    #[test]
    fn a_larger_configured_allowance_is_never_withdrawn() {
        let allowance = REPAIR_ATTEMPT_CAP + 4;
        assert_eq!(
            plan_repair(
                REPAIR_ATTEMPT_CAP,
                RepairBounds::new(allowance),
                RepairHeadroom::default(),
                cheap()
            ),
            RepairPlan::Attempt {
                attempt: REPAIR_ATTEMPT_CAP + 1,
                cap: allowance,
            }
        );
    }

    /// The witness for the knob this module gained: a caller that raises only
    /// the cap gets exactly that many attempts, and every one past its
    /// allowance is still bought by a measured axis rather than granted.
    #[test]
    fn a_configured_cap_grants_exactly_that_many_measured_attempts() {
        let bounds = RepairBounds::new(1).with_cap(7);
        for spent in 0..7 {
            assert_eq!(
                plan_repair(spent, bounds, funded(), cheap()),
                RepairPlan::Attempt {
                    attempt: spent + 1,
                    cap: 7,
                },
                "attempt {spent} should have been granted"
            );
        }
        assert_eq!(
            plan_repair(7, bounds, funded(), cheap()),
            RepairPlan::Stop(RepairRefusal::CapReached)
        );
        // The allowance stayed where it was: past it, an unmeasured run still
        // stops, so raising the cap bought rounds only for a run that can pay.
        assert_eq!(
            plan_repair(1, bounds, RepairHeadroom::default(), cheap()),
            RepairPlan::Stop(RepairRefusal::Unmeasured)
        );
    }

    /// The other half of that knob: a cap set below the allowance does not
    /// take the allowance away, so the two can be moved independently without
    /// one silently shrinking the other.
    #[test]
    fn a_cap_below_the_allowance_still_honours_the_allowance() {
        let bounds = RepairBounds::new(6).with_cap(1);
        assert_eq!(bounds.effective_cap(), 6);
        assert_eq!(
            plan_repair(5, bounds, RepairHeadroom::default(), cheap()),
            RepairPlan::Attempt { attempt: 6, cap: 6 }
        );
        assert_eq!(
            plan_repair(6, bounds, RepairHeadroom::default(), cheap()),
            RepairPlan::Stop(RepairRefusal::CapReached)
        );
    }

    /// A caller that names no cap keeps the behaviour it had before the cap
    /// was a field at all.
    #[test]
    fn the_default_cap_is_the_constant() {
        assert_eq!(RepairBounds::new(0).cap, REPAIR_ATTEMPT_CAP);
        assert_eq!(RepairBounds::new(0).effective_cap(), REPAIR_ATTEMPT_CAP);
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
            plan_repair(0, RepairBounds::new(3), broke, cheap()),
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
            plan_repair(0, RepairBounds::new(3), out_of_time, cheap()),
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
        assert!(!plan_repair(0, RepairBounds::new(3), exact, cheap()).is_attempt());
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
            configured_cap in 0u32..24,
            budget in proptest::option::of(0.0f64..1_000.0),
            secs in proptest::option::of(0u64..10_000),
            cost_usd in 0.0f64..10.0,
            cost_secs in 0u64..600,
        ) {
            let bounds = RepairBounds::new(allowance).with_cap(configured_cap);
            let plan = plan_repair(
                spent,
                bounds,
                RepairHeadroom {
                    budget_usd: budget,
                    wall_clock: secs.map(Duration::from_secs),
                },
                RepairCost { usd: cost_usd, wall: Duration::from_secs(cost_secs) },
            );
            let cap = allowance.max(configured_cap);
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
        fn granting_every_affordable_attempt_still_terminates(
            allowance in 0u32..16,
            configured_cap in 0u32..24,
        ) {
            let unlimited = RepairHeadroom {
                budget_usd: Some(f64::MAX),
                wall_clock: Some(Duration::MAX),
            };
            let bounds = RepairBounds::new(allowance).with_cap(configured_cap);
            let cap = bounds.effective_cap();
            let mut spent = 0u32;
            while plan_repair(spent, bounds, unlimited, cheap()).is_attempt() {
                spent += 1;
                prop_assert!(spent <= cap, "granted more attempts than the cap allows");
            }
            prop_assert_eq!(spent, cap);
        }
    }
}
