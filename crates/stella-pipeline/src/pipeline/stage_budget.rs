use stella_core::BudgetOutcome;
use stella_protocol::AgentEvent;

use super::{PipelineOutcome, PipelineStatus};
use crate::triage::TaskClass;

/// A settled stage call crossed an enforced budget. Kept distinct from
/// provider/routing failures so raw stages must propagate the stop.
#[derive(Debug, Clone)]
pub(super) struct PipelineBudgetAbort {
    pub(super) reason: String,
}

pub(super) fn budget_abort(outcome: BudgetOutcome) -> Option<PipelineBudgetAbort> {
    let BudgetOutcome::AbortTurn {
        spent_usd,
        limit_usd,
        ..
    } = outcome
    else {
        return None;
    };
    Some(PipelineBudgetAbort {
        reason: format!(
            "budget exceeded after this call: spent ${spent_usd:.4} against a ${limit_usd:.2} limit"
        ),
    })
}

/// The terminal `Error` event and the `Aborted` outcome for a run that stopped
/// before execution ever started. Returned as a pair instead of taking a sink,
/// so this stays a pure function over owned data and the caller keeps its one
/// emission point (L-E1/L-T5).
pub(super) fn aborted_before_execute(
    task_class: TaskClass,
    total_cost: f64,
    reason: &str,
) -> (AgentEvent, PipelineOutcome) {
    let event = AgentEvent::Error {
        message: reason.to_string(),
        retryable: false,
    };
    let outcome = PipelineOutcome {
        status: PipelineStatus::Aborted {
            reason: reason.to_string(),
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
