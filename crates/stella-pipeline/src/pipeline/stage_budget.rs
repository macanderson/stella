use std::time::{Duration, Instant};

use stella_core::{AbortKind, BudgetGuard, BudgetOutcome};
use stella_protocol::AgentEvent;

use super::{Pipeline, PipelineOutcome, PipelineStatus};
use crate::triage::TaskClass;

/// The turn's money, threaded through the candidate plane as one parameter
/// (#1809): the guard that decides whether the next paid call may happen, and
/// the running total the outcome reports. The two were passed side by side
/// through every function between `run` and the engine turn — always together,
/// because spending through the guard without recording it (or vice versa)
/// is a bug — so the pairing is now a type instead of a convention.
///
/// Borrows rather than owns: the guard and the total live with the caller of
/// [`super::Pipeline::run`] and in `run`'s locals respectively, and every
/// mutation must land there — an owned copy would need a write-back on each
/// of `run`'s early returns, and one missed return is a silently vanished
/// spend.
///
/// A signature still threading the loose pair adopts this the next time it
/// gains an input: `plan_stage` did so when #1778's `research` argument pushed
/// the loose form past clippy's arity limit. The bundle is the fix rather than
/// an `#[allow]` precisely because the pair was never two things.
pub(super) struct Spend<'a> {
    /// Gates each paid call; consulted between model calls only (invariant #6).
    pub(super) budget: &'a mut BudgetGuard,
    /// The run's settled cost in USD, reported on every outcome — including
    /// aborts, which is why it cannot ride inside the guard.
    pub(super) total: &'a mut f64,
}

/// A stage call hit a ceiling the run may not spend past — dollars or wall
/// clock. Kept distinct from provider/routing failures so raw stages must
/// propagate the stop rather than degrade through it.
///
/// One type for both axes because the *consequence* is identical at every
/// call site (stop at this safe boundary, report why), and the axes stay
/// distinguishable where it matters — in `reason`, which is what reaches the
/// stream. Named for the stop rather than for dollars since #2238 taught it
/// the clock: the old name, `PipelineBudgetAbort`, would have described what
/// the type used to be while it carried "the task deadline passed".
#[derive(Debug, Clone)]
pub(super) struct PipelineStageAbort {
    pub(super) reason: String,
}

pub(super) fn budget_abort(outcome: BudgetOutcome) -> Option<PipelineStageAbort> {
    let BudgetOutcome::AbortTurn {
        spent_usd,
        limit_usd,
        ..
    } = outcome
    else {
        return None;
    };
    Some(PipelineStageAbort {
        reason: format!(
            "budget exceeded after this call: spent ${spent_usd:.4} against a ${limit_usd:.2} limit"
        ),
    })
}

/// The stop for a stage reached after the task's wall-clock deadline passed
/// (#2238), phrased like `crate::driver::settlement`'s so the two rungs of the
/// same mechanism read the same way in a transcript.
///
/// "before spending anything further" is load-bearing: unlike the dollar
/// abort above, no call was made and no money changed hands — the run is
/// declining to start work it has no time to finish.
pub(super) fn deadline_abort(overrun: Duration) -> PipelineStageAbort {
    PipelineStageAbort {
        reason: format!(
            "task deadline exceeded by {:.1}s — stopping before spending anything further",
            overrun.as_secs_f64()
        ),
    }
}

/// The stop for a stage call refused because too little of the task's wall
/// clock remains to fit it (#2432) — the anticipatory half of
/// [`deadline_abort`], phrased like `crate::driver::settlement`'s `Closing`
/// sentence and for the same reason it has one: a reader who sees "exceeded"
/// on a run that never exceeded anything learns the wrong thing about it.
///
/// Names both numbers, so the refusal carries its own basis rather than asking
/// the reader to infer it — the clock that remains, and what this call was
/// declared to need.
pub(super) fn deadline_closing_abort(
    remaining: Duration,
    reserve: Duration,
) -> PipelineStageAbort {
    PipelineStageAbort {
        reason: format!(
            "task deadline in {:.1}s, less than the {:.1}s this call is bounded to take — \
             declining to start work the remaining clock cannot fit",
            remaining.as_secs_f64(),
            reserve.as_secs_f64()
        ),
    }
}

/// The tighter of the two clocks that can stop a pipeline run, or `None` when
/// neither is set.
///
/// Pure and free-standing so the arithmetic is testable apart from the
/// pipeline that supplies its inputs — the same posture
/// `witness_stage::witness_repair_bound` takes for its own derivation.
fn binding_clock(armed: Option<Duration>, declared: Option<Duration>) -> Option<Duration> {
    [armed, declared].into_iter().flatten().min()
}

impl Pipeline<'_> {
    /// The wall clock this run actually has left as of `now`, or `None` when
    /// nothing is measuring it.
    ///
    /// Two clocks can stop a run and they have different standing.
    /// [`super::PipelineConfig::run_budget`] is the caller's declared
    /// allowance — an estimate that enforces nothing.
    /// [`BudgetGuard::task_deadline`] is the armed one: since #2238 it is what
    /// actually refuses a stage call, and since #2240 it is what the journal
    /// reports. A consumer asking "how much room is left" wants whichever
    /// expires first, so this is their minimum rather than a preference for
    /// either.
    ///
    /// Preferring one outright would be wrong in a different direction each
    /// way. An armed deadline further out than the declared allowance must not
    /// buy back room the caller never declared; a generous allowance must not
    /// hide a deadline five seconds away — the defect this closes (#2433),
    /// where the repair gate bought a round the raw-call seam then refused
    /// mid-round. The two agree whenever `stella-cli` arms both from one
    /// `--turn-budget`, so the minimum changes nothing for the surface that
    /// has both today and fixes every surface that arms only one: an embedder
    /// on `stella-engine`/`stella-serve`, a fleet worker, or anything arming a
    /// deadline from another source.
    ///
    /// `None` from both is preserved deliberately: nobody is measuring, and
    /// the clock axis abstains rather than inventing a deadline the caller
    /// never declared (`run_budget`'s own contract).
    ///
    /// `now` is the caller's, for the reason
    /// [`BudgetGuard::deadline_remaining`] takes it (invariant 2): the clock is
    /// read at a boundary, never from inside the decision.
    pub(super) fn remaining_wall_clock(
        &self,
        budget: &BudgetGuard,
        now: Instant,
    ) -> Option<Duration> {
        binding_clock(
            budget.deadline_remaining(now),
            self.config.run_budget.map(|allowance| {
                allowance.saturating_sub(now.saturating_duration_since(self.started))
            }),
        )
    }
}

/// The terminal `Error` event and the `Aborted` outcome for a run that stopped
/// before execution ever started. Returned as a pair instead of taking a sink,
/// so this stays a pure function over owned data and the caller keeps its one
/// emission point (L-E1/L-T5).
pub(super) fn aborted_before_execute(
    task_class: TaskClass,
    total_cost: f64,
    reason: &str,
    kind: AbortKind,
) -> (AgentEvent, PipelineOutcome) {
    let event = AgentEvent::Error {
        message: reason.to_string(),
        retryable: false,
    };
    let outcome = PipelineOutcome {
        status: PipelineStatus::Aborted {
            reason: reason.to_string(),
            kind,
        },
        task_class,
        final_text: String::new(),
        total_cost_usd: total_cost,
        verdict: None,
        // Nothing executed, so nothing was graded.
        score: None,
        revisions: 0,
        candidates_run: 0,
    };
    (event, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2433: an armed deadline five seconds out binds a run whose declared
    /// allowance still shows ten minutes. Reading only the allowance is what
    /// sold the repair gate a round the raw-call seam then refused.
    #[test]
    fn the_armed_deadline_binds_a_generous_declared_allowance() {
        assert_eq!(
            binding_clock(
                Some(Duration::from_secs(5)),
                Some(Duration::from_secs(600))
            ),
            Some(Duration::from_secs(5))
        );
    }

    /// The converse, and the reason this is a minimum rather than a preference
    /// for the deadline: an allowance the caller declared is not bought back
    /// by a deadline armed further out.
    #[test]
    fn a_distant_deadline_does_not_buy_back_the_declared_allowance() {
        assert_eq!(
            binding_clock(
                Some(Duration::from_secs(600)),
                Some(Duration::from_secs(5))
            ),
            Some(Duration::from_secs(5))
        );
    }

    /// Either clock alone is the whole answer — the two surfaces that arm only
    /// one, which is the case #2433 exists for.
    #[test]
    fn one_clock_alone_is_the_answer() {
        assert_eq!(
            binding_clock(Some(Duration::from_secs(5)), None),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            binding_clock(None, Some(Duration::from_secs(5))),
            Some(Duration::from_secs(5))
        );
    }

    /// Abstention survives: neither clock set means nobody is measuring, which
    /// must stay distinguishable from "no time left".
    #[test]
    fn neither_clock_abstains_rather_than_reporting_zero() {
        assert_eq!(binding_clock(None, None), None);
    }
}
