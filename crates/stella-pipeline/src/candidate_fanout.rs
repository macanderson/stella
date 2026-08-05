//! What a best-of-N fan-out costs once its candidates stop taking turns.
//!
//! Fully-isolated candidates never see each other's edits — the module-doc
//! guarantee `Pipeline::run_best_of_n` is built on, with `candidate_steering`
//! already giving every candidate its own replay cursor over the user's
//! steers. Nothing about the *work* required them to run one after another;
//! the only thing that did was the `&mut BudgetGuard` threaded through
//! `run_candidate` → `verify_candidate` → `verifier`. So `--candidates 3` cost
//! the **sum** of three candidate runtimes instead of the slowest, which on a
//! wall-clock-bounded harness is the difference between finishing and being
//! killed (#1215).
//!
//! This module owns the two decisions concurrency forces, kept synchronous
//! and testable away from the I/O sequencing in `pipeline::fanout_stage`: how
//! wide the fan-out runs, and how one turn's money is divided between
//! candidates that spend at the same time.
//!
//! # The budget window
//!
//! A `&mut BudgetGuard` cannot be shared, so each candidate spends against a
//! [`BudgetGuard::carve`] of the parent's headroom and folds its total back
//! with [`BudgetGuard::settle_child`] when it settles — the same arrangement
//! `stella_core::subagent` uses for a nested turn, and the same
//! snapshot-not-reservation posture `stella-fleet` documents on its own
//! dispatch gate.
//!
//! Two things bound the aggregate:
//!
//! 1. **A pre-dispatch gate.** A candidate claims its allowance from the
//!    shared parent as it takes its concurrency slot, and a parent already
//!    over an enforced cap hands out nothing — so a fan-out wider than the
//!    money stops launching rather than launching everything and discovering
//!    the cap N times.
//! 2. **A per-candidate share.** The allowance is `headroom / width`, not the
//!    whole headroom, so the candidates that are in flight *together* cannot
//!    collectively promise more than the parent has. This is the same divisor
//!    `fleet_cmd` applies before handing a child its guard, and for the same
//!    reason.
//!
//! What is left is one window: the budget is consulted between steps, never
//! mid-call (invariant 6), so each in-flight candidate can be one model call
//! past its own allowance when it stops. **The documented overshoot is
//! therefore at most `width` model calls beyond the cap** — at `width == 1`
//! that is the one call the sequential path always allowed, and the whole
//! guarantee degrades continuously from there.
//!
//! # What a carve changes, and what it does not
//!
//! [`BudgetGuard::carve`] lands its ceiling on the child's *session* axis by
//! contract ("a child's whole life is one turn"), so a candidate that trips a
//! cap now reports `BudgetAxis::Session` where a shared guard would have said
//! `Turn`. The *money* is identical — both axes are consulted when the
//! headroom is computed, so a turn limit still bounds the carve — and this is
//! only ever a label on the abort message. It is called out here because it is
//! the one observable difference between spending through a carve and spending
//! through the parent directly.

use std::sync::Mutex;

use stella_core::{BudgetGuard, BudgetOutcome};

/// How many isolated candidates may execute at once.
///
/// `None` — the default — runs the whole fan-out together, which is the point:
/// `--candidates N` should cost the slowest candidate, not the sum. A
/// configured width is clamped into `1..=n`, so `Some(1)` is the exact
/// sequential behaviour that predates concurrency and a width past `n` is not
/// an error, just unreachable.
pub(crate) fn fan_out_width(n: u32, configured: Option<u32>) -> u32 {
    let n = n.max(1);
    configured.map_or(n, |w| w.clamp(1, n))
}

/// The fan-out's shared parent budget: the pre-dispatch gate, the per-
/// candidate carve, and the settle that folds a finished candidate's spend
/// back — see the module docs for the bound these three produce together.
pub(crate) struct FanOutBudget {
    /// A `Mutex` rather than a `RefCell` on purpose: every candidate future is
    /// polled on one task today, but the guard travels inside futures the
    /// caller may hand to a spawner tomorrow, and a `RefCell` would make that
    /// a compile error at the wrong end of the codebase. Locks are held for a
    /// single `Copy` read/write and never across an `await`.
    parent: Mutex<BudgetGuard>,
    width: u32,
}

impl FanOutBudget {
    pub(crate) fn new(parent: BudgetGuard, width: u32) -> Self {
        Self {
            parent: Mutex::new(parent),
            width: width.max(1),
        }
    }

    /// Claim one candidate's allowance, or `None` when the fan-out has no
    /// money left to give it.
    ///
    /// `None` is a *skip*, not a failure: the candidate never reaches a model,
    /// so the caller scores it as a candidate that generated nothing rather
    /// than as work that was attempted and aborted.
    pub(crate) fn claim(&self) -> Option<BudgetGuard> {
        let parent = self.lock();
        if let BudgetOutcome::AbortTurn { .. } = parent.evaluate() {
            return None;
        }
        let share = parent.headroom_usd().map(|h| h / f64::from(self.width));
        let child = parent.carve(share);
        child.is_viable_carve().then_some(child)
    }

    /// Fold one finished candidate's spend into the parent, so the next
    /// candidate to claim a slot sees money that is already gone.
    ///
    /// Called in completion order — that is what makes the gate above live —
    /// while the run's *reported* cost is summed by candidate index, so the
    /// number the user sees does not depend on who finished first.
    pub(crate) fn settle(&self, child: &BudgetGuard) {
        self.lock().record_spend(child.session_spent_usd());
    }

    /// The parent guard, with every settled candidate's spend folded in.
    pub(crate) fn into_parent(self) -> BudgetGuard {
        self.parent
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BudgetGuard> {
        self.parent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::BudgetMode;

    /// The default is the whole fan-out at once — the wall-clock lever the
    /// concurrency exists for.
    #[test]
    fn an_unconfigured_width_runs_every_candidate_together() {
        assert_eq!(fan_out_width(3, None), 3);
        assert_eq!(fan_out_width(1, None), 1);
    }

    /// `Some(1)` is how an operator asks for the pre-concurrency behaviour
    /// back, and it has to be exactly expressible.
    #[test]
    fn a_width_of_one_is_the_sequential_fan_out() {
        assert_eq!(fan_out_width(4, Some(1)), 1);
        assert_eq!(fan_out_width(4, Some(0)), 1, "0 is not a stalled fan-out");
    }

    /// A width past the candidate count is a wish, not an error.
    #[test]
    fn a_width_wider_than_the_fan_out_is_clamped_to_it() {
        assert_eq!(fan_out_width(2, Some(9)), 2);
    }

    /// The whole point of dividing by the width: candidates that are in
    /// flight together cannot each be promised the full headroom.
    #[test]
    fn concurrent_candidates_split_the_headroom_rather_than_each_taking_it() {
        let parent = BudgetGuard::new(BudgetMode::Enforced, Some(6.0), None);
        let budgets = FanOutBudget::new(parent, 3);

        let shares: Vec<f64> = (0..3)
            .map(|_| {
                budgets
                    .claim()
                    .expect("a fresh cap funds every candidate")
                    .session_limit_usd()
                    .expect("an enforced cap carves a ceiling")
            })
            .collect();

        assert_eq!(shares, vec![2.0, 2.0, 2.0]);
        assert!(
            shares.iter().sum::<f64>() <= 6.0,
            "the in-flight window must never promise more than the cap: {shares:?}"
        );
    }

    /// The aggregate bound the acceptance criterion names: with every
    /// candidate spending its whole allowance, the parent lands at the cap
    /// rather than at `width x cap`.
    #[test]
    fn settled_candidates_leave_the_parent_at_the_cap_not_a_multiple_of_it() {
        let parent = BudgetGuard::new(BudgetMode::Enforced, Some(6.0), None);
        let budgets = FanOutBudget::new(parent, 3);

        for _ in 0..3 {
            let mut child = budgets.claim().expect("a fresh cap funds every candidate");
            child.record_spend(2.0);
            budgets.settle(&child);
        }

        let settled = budgets.into_parent();
        assert!(
            (settled.spent_usd() - 6.0).abs() < 1e-9,
            "aggregate spend must equal the cap, not 3x it: {}",
            settled.spent_usd()
        );
    }

    /// A fan-out wider than the money stops launching. Without the
    /// pre-dispatch gate the later candidates would each buy one doomed model
    /// call before discovering the cap.
    #[test]
    fn a_spent_parent_stops_funding_further_candidates() {
        let parent = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), None);
        let budgets = FanOutBudget::new(parent, 1);

        let mut first = budgets.claim().expect("the first candidate is funded");
        first.record_spend(1.5);
        budgets.settle(&first);

        assert!(
            budgets.claim().is_none(),
            "a breached cap must not fund another candidate"
        );
    }

    /// Metering is not gating: an observed budget warns and keeps going, so
    /// its candidates are always funded even past the limit. Escalating a
    /// carve to a wall here would turn a stated warn-only posture into a hard
    /// stop, one level down.
    #[test]
    fn an_observed_budget_keeps_funding_candidates_past_its_limit() {
        let parent = BudgetGuard::new(BudgetMode::Observed, Some(1.0), None);
        let budgets = FanOutBudget::new(parent, 2);

        let mut first = budgets.claim().expect("the first candidate is funded");
        first.record_spend(5.0);
        budgets.settle(&first);

        assert!(
            budgets.claim().is_some(),
            "observed mode warns; it never denies"
        );
    }

    /// An unmetered run must not acquire a ceiling just by fanning out.
    #[test]
    fn an_unlimited_parent_carves_unlimited_candidates() {
        let budgets = FanOutBudget::new(BudgetGuard::new(BudgetMode::Off, None, None), 4);
        let child = budgets.claim().expect("no cap funds everything");
        assert_eq!(child.session_limit_usd(), None);
        assert_eq!(child.turn_limit_usd(), None);
    }

    /// Settling is order-independent in what it *accounts* — every
    /// candidate's spend reaches the parent regardless of who finished first
    /// — which is what lets the caller settle in completion order and still
    /// report a total summed by index.
    #[test]
    fn every_candidate_settles_whatever_the_completion_order() {
        let costs = [0.25f64, 1.5, 0.75];
        let mut totals = Vec::new();
        for order in [[0usize, 1, 2], [2, 1, 0], [1, 0, 2]] {
            let budgets = FanOutBudget::new(BudgetGuard::new(BudgetMode::Off, None, None), 3);
            let mut children: Vec<BudgetGuard> = (0..3)
                .map(|_| budgets.claim().expect("unmetered"))
                .collect();
            for i in order {
                children[i].record_spend(costs[i]);
                budgets.settle(&children[i]);
            }
            totals.push(budgets.into_parent().spent_usd());
        }
        assert!(
            totals.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-9),
            "completion order must not change what the parent accounts: {totals:?}"
        );
    }
}
