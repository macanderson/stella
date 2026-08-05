//! One reading of a finished pipeline run, shared by every surface.
//!
//! A [`PipelineStatus`] has to be projected four different ways — a store
//! label, a JSON `reason`, an episodic-memory outcome, and the process exit
//! `Result` — and each surface (one-shot, deck, fleet, arena) needs the same
//! projections. Keeping them here, as total `match`es over the enum, is what
//! stops `stella run --output-format json` and the audit row from disagreeing
//! about whether the same run passed.
//!
//! Cost accounting lives here for the same reason: [`settled_cost_since`] is
//! the one place a spend delta is clamped, so a provider that reports a
//! non-monotonic total can never bill a negative amount into the ledger.

use stella_context::EpisodeOutcome;
use stella_pipeline::{PipelineOutcome, PipelineRunError, PipelineStatus};

use crate::failure::CliFailure;

/// Spend settled between two reads of the same cumulative counter, floored at
/// zero. The floor is not defensive noise: a cancelled turn reads the guard
/// after the dispatch it is unwinding, and a provider whose reported total
/// ever moves backwards must not credit the ledger.
pub(crate) fn settled_cost_since(start_usd: f64, current_usd: f64) -> f64 {
    (current_usd - start_usd).max(0.0)
}

/// The `(outcome label, cost)` pair an execution row closes out with. A hard
/// pipeline error still reports the spend of the stages that ran before it —
/// the run cost real money whether or not it produced an answer.
pub(crate) fn pipeline_execution_closeout(
    result: &Result<PipelineOutcome, PipelineRunError>,
) -> (&'static str, f64) {
    match result {
        Ok(outcome) => (
            pipeline_status_label(&outcome.status),
            outcome.total_cost_usd,
        ),
        Err(error) => ("error", error.total_cost_usd),
    }
}

pub(super) fn pipeline_status_label(status: &PipelineStatus) -> &'static str {
    match status {
        PipelineStatus::Completed => "completed",
        PipelineStatus::VerificationFailed { .. } => "verification_failed",
        PipelineStatus::Aborted { .. } => "aborted",
    }
}

pub(super) fn pipeline_failure_reason(status: &PipelineStatus) -> Option<String> {
    match status {
        PipelineStatus::Completed => None,
        PipelineStatus::VerificationFailed { verdict } => {
            Some(format!("verification failed: {}", verdict.summary))
        }
        PipelineStatus::Aborted { reason, .. } => Some(reason.clone()),
    }
}

pub(super) fn pipeline_episode_outcome(status: &PipelineStatus) -> EpisodeOutcome {
    match status {
        PipelineStatus::Completed => EpisodeOutcome::Success,
        PipelineStatus::VerificationFailed { .. } => EpisodeOutcome::Failure,
        PipelineStatus::Aborted { .. } => EpisodeOutcome::Aborted,
    }
}

pub(super) fn pipeline_status_result(status: &PipelineStatus) -> Result<(), CliFailure> {
    match status {
        PipelineStatus::Completed => Ok(()),
        PipelineStatus::VerificationFailed { verdict } => Err(CliFailure::error(format!(
            "verification failed: {}",
            verdict.summary
        ))),
        PipelineStatus::Aborted { reason, kind } => {
            Err(CliFailure::from_abort(reason.clone(), *kind))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_pipeline::{PipelineError, PipelineRunError};

    #[test]
    fn hard_pipeline_error_closeout_retains_prior_stage_cost() {
        let result = Err(PipelineRunError {
            cause: PipelineError::ScopeReviewRequiredHeadless,
            total_cost_usd: 0.42,
        });

        assert_eq!(pipeline_execution_closeout(&result), ("error", 0.42));
    }

    #[test]
    fn cancellation_after_settled_spend_persists_the_dispatch_delta() {
        assert!((settled_cost_since(1.25, 1.75) - 0.50).abs() < 1e-9);
    }

    #[test]
    fn cancellation_before_spend_persists_zero() {
        assert_eq!(settled_cost_since(1.25, 1.25), 0.0);
    }
}
