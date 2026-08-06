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
use stella_core::TurnOutcome;
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

/// The process-boundary answer a finished engine turn owes `main`.
///
/// The one projection from [`TurnOutcome`] to an exit code, so every surface
/// that drives a turn directly reads an abort the same way: the abort's typed
/// [`AbortKind`] decides, exactly as it does for a pipeline run
/// ([`pipeline_status_result`]).
///
/// It lives here because the resumed-turn driver
/// (`crate::agent::resume::run_resume`) used to answer with a `String`, which
/// has no room for the `kind` — so a resumed stuck-loop stop exited `1` while
/// the identical un-resumed stop exited `3`, and a wrapper reading exit codes
/// could not tell a resumed policy stop from a resumed crash (#1637).
///
/// [`AbortKind`]: stella_core::AbortKind
pub(super) fn turn_outcome_result(outcome: &TurnOutcome) -> Result<(), CliFailure> {
    match outcome {
        TurnOutcome::Completed { .. } => Ok(()),
        TurnOutcome::Aborted { reason, kind, .. } => {
            Err(CliFailure::from_abort(reason.clone(), *kind))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::AbortKind;
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

    #[test]
    fn a_deliberately_stopped_turn_carries_its_own_exit_code() {
        let failure = turn_outcome_result(&TurnOutcome::Aborted {
            reason: stella_core::driver::step_cap_reason(40),
            kind: AbortKind::DeliberateStop,
            cost_usd: 0.0,
        })
        .expect_err("an aborted turn is not a success");

        assert_eq!(
            failure.exit_code(),
            std::process::ExitCode::from(crate::failure::DELIBERATE_STOP_EXIT_CODE)
        );
    }

    #[test]
    fn a_turn_that_fell_over_keeps_the_generic_failure_exit() {
        let failure = turn_outcome_result(&TurnOutcome::Aborted {
            reason: "the model call would not commit after retries".into(),
            kind: AbortKind::Failure,
            cost_usd: 0.0,
        })
        .expect_err("an aborted turn is not a success");

        assert_eq!(failure.exit_code(), std::process::ExitCode::FAILURE);
    }

    #[test]
    fn a_completed_turn_is_a_success() {
        assert!(
            turn_outcome_result(&TurnOutcome::Completed {
                text: "done".into(),
                cost_usd: 0.0,
            })
            .is_ok()
        );
    }
}
