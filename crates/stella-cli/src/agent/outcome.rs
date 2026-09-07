//! Cost accounting and process-boundary projections shared by every surface
//! that drives a turn directly.
//!
//! [`settled_cost_since`] is the one place a spend delta is clamped. So a
//! provider that reports a total moving backwards can never bill a negative
//! amount into the ledger.
//!
//! [`turn_outcome_result`] is the process-exit projection every raw-loop
//! driver shares — the one-shot and the resume path. A resumed stuck-loop stop
//! and a fresh one exit with the same code (#1637).
//!
//! A five-way `stella_pipeline::PipelineStatus` projection lived here too: a
//! store label, a JSON `reason`, an episodic-memory outcome, the process exit
//! `Result`, and the terminal SESSIONS-registry status. That pipeline is gone
//! from this build (#3865), and with it every surface that made one.

use crate::failure::CliFailure;

/// Spend settled between two reads of the same cumulative counter, floored at
/// zero. The floor is not defensive noise: a cancelled turn reads the guard
/// after the dispatch it is unwinding, and a provider whose reported total
/// ever moves backwards must not credit the ledger.
pub(crate) fn settled_cost_since(start_usd: f64, current_usd: f64) -> f64 {
    (current_usd - start_usd).max(0.0)
}

/// The process-boundary answer a finished engine turn owes `main`.
///
/// The one projection from [`TurnOutcome`] to an exit code, so every surface
/// that drives a turn directly reads an abort the same way: the abort's typed
/// [`AbortKind`] decides.
///
/// It lives here because the resumed-turn driver
/// (`crate::agent::resume::run_resume`) once answered with a `String`, which
/// has no room for the `kind`. A resumed stuck-loop stop exited `1` while the
/// same un-resumed stop exited `3`, so a wrapper reading exit codes could not
/// tell a resumed policy stop from a resumed crash (#1637).
///
/// [`AbortKind`]: stella_core::AbortKind
/// [`TurnOutcome`]: stella_core::TurnOutcome
pub(crate) fn turn_outcome_result(outcome: &stella_core::TurnOutcome) -> Result<(), CliFailure> {
    match outcome {
        stella_core::TurnOutcome::Completed { .. } => Ok(()),
        stella_core::TurnOutcome::Aborted { reason, kind, .. } => {
            Err(CliFailure::from_abort(reason.clone(), *kind))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::AbortKind;
    use stella_core::TurnOutcome;

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
