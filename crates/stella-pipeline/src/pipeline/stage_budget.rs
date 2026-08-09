use std::time::Duration;

use stella_core::{AbortKind, BudgetGuard, BudgetOutcome};
use stella_protocol::AgentEvent;

use super::{PipelineOutcome, PipelineStatus};
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
