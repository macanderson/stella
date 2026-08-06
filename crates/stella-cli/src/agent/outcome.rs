//! One reading of a finished pipeline run, shared by every surface.
//!
//! A [`PipelineStatus`] has to be projected five different ways — a store
//! label, a JSON `reason`, an episodic-memory outcome, the process exit
//! `Result`, and the terminal SESSIONS-registry status — and each surface
//! (one-shot, deck, fleet, arena) needs the same projections. Keeping them here, as total `match`es over the enum, is what
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

pub(crate) fn pipeline_status_result(status: &PipelineStatus) -> Result<(), CliFailure> {
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

/// The terminal SESSIONS-registry status a finished pipeline run records —
/// the fifth projection, feeding `SessionPresence::finish` on the
/// unsupervised paths (`stella run` in a pipe, CI, `--foreground`) that
/// `crate::daemon::record_outcome_if_supervised` never reaches (#1826).
///
/// Routed through [`crate::daemon::outcome_status`] rather than re-matched
/// here, so every registry writer — supervised or not — reads a deliberate
/// stop (`Stopped`), a user interrupt (`Cancelled`), and a crash (`Error`)
/// off the same decider (#1653).
pub(super) fn pipeline_session_status(
    result: &Result<PipelineOutcome, PipelineRunError>,
) -> stella_store::SessionStatus {
    let terminal = match result {
        Ok(outcome) => pipeline_status_result(&outcome.status),
        // A hard pipeline error carries no abort kind — a genuine failure,
        // the same reading the process exit gives it.
        Err(error) => Err(CliFailure::error(error.to_string())),
    };
    crate::daemon::outcome_status(terminal.as_ref().map(|_| ()))
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
pub(crate) fn turn_outcome_result(outcome: &TurnOutcome) -> Result<(), CliFailure> {
    match outcome {
        TurnOutcome::Completed { .. } => Ok(()),
        TurnOutcome::Aborted { reason, kind, .. } => {
            Err(CliFailure::from_abort(reason.clone(), *kind))
        }
    }
}

/// The terminal break a goal round's pipeline status folds to — `None` keeps
/// the loop running to its verifier assessment. The goal-loop analogue of
/// [`pipeline_status_result`], with the loop's own message prefixes: the fold
/// used to stringify the status into `Err(String)`, which dropped the abort's
/// typed [`AbortKind`] right there, so the goal loop's terminal registry
/// write could only record a policy-stopped round as an error (#1862).
///
/// [`AbortKind`]: stella_core::AbortKind
pub(crate) fn goal_round_break(status: &PipelineStatus) -> Option<CliFailure> {
    match status {
        PipelineStatus::Completed => None,
        PipelineStatus::VerificationFailed { verdict } => Some(CliFailure::error(format!(
            "goal not met: verification failed: {}",
            verdict.summary
        ))),
        PipelineStatus::Aborted { reason, kind } => Some(CliFailure::from_abort(
            format!("goal not met: working round aborted: {reason}"),
            *kind,
        )),
    }
}

/// The failure an unmet goal loop reports at the process boundary — the raw
/// (`--no-pipeline`) goal loop's half of #1862. The abort's typed kind
/// survives when a working turn aborted; the backstops that are not turn
/// aborts (round cap, unreachable verifier) carry no kind and stay plain
/// failures, exactly as the untyped chain always read them.
pub(crate) fn goal_unmet_failure(
    rounds: usize,
    reason: &str,
    kind: Option<stella_core::AbortKind>,
) -> CliFailure {
    let message = format!("goal not met after {rounds} round(s): {reason}");
    match kind {
        Some(kind) => CliFailure::from_abort(message, kind),
        None => CliFailure::error(message),
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

    /// A pipeline run terminal enough to project, with every field the
    /// projection ignores held constant.
    fn pipeline_outcome(status: PipelineStatus) -> PipelineOutcome {
        PipelineOutcome {
            status,
            task_class: stella_pipeline::TaskClass::SingleTask,
            final_text: String::new(),
            total_cost_usd: 0.0,
            verdict: None,
            score: None,
            revisions: 0,
            candidates_run: 1,
        }
    }

    /// #1826 witness — the presence half of #1653: the projection the
    /// unsupervised registry writers feed `SessionPresence::finish`
    /// distinguishes a deliberate stop from a crash. Before, `finish(ok:
    /// bool, …)` collapsed both to [`stella_store::SessionStatus::Error`] —
    /// this projection did not exist, and the widened `finish` call sites do
    /// not compile against the old signature.
    ///
    /// Like `daemon::tests::a_deliberate_stop_records_a_status_distinct_from_a_crash`,
    /// this relies on no test in this binary ever calling
    /// `signals::note_interrupt`, so `interrupted_exit_code()` is reliably
    /// `None` here.
    #[test]
    fn an_unsupervised_deliberate_stop_projects_stopped_not_error() {
        let stopped = Ok(pipeline_outcome(PipelineStatus::Aborted {
            reason: "stuck-loop detected (persisted after a steering warning)".into(),
            kind: AbortKind::DeliberateStop,
        }));
        let crashed = Ok(pipeline_outcome(PipelineStatus::Aborted {
            reason: "the model call would not commit after retries".into(),
            kind: AbortKind::Failure,
        }));

        assert_eq!(
            pipeline_session_status(&stopped),
            stella_store::SessionStatus::Stopped,
            "a run that ended itself by policy must not read as a crash"
        );
        assert_eq!(
            pipeline_session_status(&crashed),
            stella_store::SessionStatus::Error
        );
    }

    /// The arms the widening must not disturb: a completed run stays
    /// `Complete`, and a hard pipeline error — which carries no abort kind —
    /// stays `Error`.
    #[test]
    fn the_remaining_terminal_arms_project_unchanged() {
        assert_eq!(
            pipeline_session_status(&Ok(pipeline_outcome(PipelineStatus::Completed))),
            stella_store::SessionStatus::Complete
        );
        let hard = Err(PipelineRunError {
            cause: PipelineError::ScopeReviewRequiredHeadless,
            total_cost_usd: 0.0,
        });
        assert_eq!(
            pipeline_session_status(&hard),
            stella_store::SessionStatus::Error
        );
    }

    /// #1862 witness, the goal loop's half of #1826's: the fold an aborted
    /// working round takes on its way to the terminal registry write keeps
    /// the abort's typed kind, so `run_goal_cmd`'s `presence.finish`
    /// projects a policy-stopped goal round as `Stopped`. Before, the fold
    /// stringified the status into `Err(String)` and the terminal write
    /// could only reconstruct `CliFailure::error` — every stopped goal run
    /// aged into the registry as a crash.
    ///
    /// Like `an_unsupervised_deliberate_stop_projects_stopped_not_error`,
    /// this relies on no test in this binary ever calling
    /// `signals::note_interrupt`, so `interrupted_exit_code()` is reliably
    /// `None` here.
    #[test]
    fn a_policy_stopped_goal_round_projects_stopped_not_error() {
        let stopped = goal_round_break(&PipelineStatus::Aborted {
            reason: "stuck-loop detected (persisted after a steering warning)".into(),
            kind: AbortKind::DeliberateStop,
        })
        .expect("an aborted round ends the goal loop");
        let crashed = goal_round_break(&PipelineStatus::Aborted {
            reason: "the model call would not commit after retries".into(),
            kind: AbortKind::Failure,
        })
        .expect("an aborted round ends the goal loop");

        assert_eq!(
            crate::daemon::outcome_status(Err(&stopped)),
            stella_store::SessionStatus::Stopped,
            "a goal round that ended itself by policy must not read as a crash"
        );
        assert_eq!(
            crate::daemon::outcome_status(Err(&crashed)),
            stella_store::SessionStatus::Error
        );
        // A completed round is not a break at all — the loop goes on to its
        // verifier assessment.
        assert!(goal_round_break(&PipelineStatus::Completed).is_none());
    }

    /// The raw (`--no-pipeline`) goal loop's half of the same witness: an
    /// `Unmet` outcome whose working turn deliberately stopped projects
    /// `Stopped`, while the kind-less backstops keep reading as failures.
    #[test]
    fn a_policy_stopped_raw_goal_loop_projects_stopped_not_error() {
        let stopped = goal_unmet_failure(
            2,
            "working turn aborted: session budget limit reached",
            Some(AbortKind::DeliberateStop),
        );
        let capped = goal_unmet_failure(8, "round cap (8) reached without a passing verdict", None);

        assert_eq!(
            crate::daemon::outcome_status(Err(&stopped)),
            stella_store::SessionStatus::Stopped
        );
        assert_eq!(
            crate::daemon::outcome_status(Err(&capped)),
            stella_store::SessionStatus::Error
        );
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
