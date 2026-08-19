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

/// Whether an **unverified** run must be reported as a failure at the process
/// boundary (#2569).
///
/// # Why the default is `Advisory`, and why that is not the expedient answer
///
/// This is the one projection where making the honest state honest would break
/// callers, and the split exists so the other four do not have to wait for
/// that decision.
///
/// A run is unverified whenever the ladder could not prove it, and with no
/// configured `--test-command` that is the *ordinary* outcome, not an exotic
/// one — this repository's own default reaches it. Flipping `stella run`'s
/// exit code from 0 to non-zero for it would turn every script and CI job that
/// shells out to Stella red overnight, on runs whose behaviour did not change
/// by one byte. That is a policy change for a maintainer to make deliberately,
/// with a release note; making it silently inside a bug fix is precisely the
/// "now vs. right" call CLAUDE.md reserves for a human.
///
/// So the *fact* is made unambiguous everywhere it is reported — status,
/// label, JSON `reason`, episode outcome — and the *exit code* becomes
/// available on request. A delivery gate that must not ship unproven work asks
/// for [`Self::Required`] (`stella run --require-verified`) or reads
/// `status == "unverified"` out of `--output-format json`; a script that only
/// wants to know whether the run fell over keeps today's behaviour.
///
/// Named for the property asked of the run, not for the mechanism that
/// enforces it: `--require-verified` states what must be true, where
/// `--fail-on-unverified` would state what the process does about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum VerificationRequirement {
    /// The default. An unverified run reports itself as unverified and still
    /// exits `0`.
    #[default]
    Advisory,
    /// `--require-verified`. An unverified run is a failure at the process
    /// boundary, exactly like a refuted one.
    Required,
}

pub(super) fn pipeline_status_label(status: &PipelineStatus) -> &'static str {
    match status {
        PipelineStatus::Completed => "completed",
        // A distinct label, not a qualifier on "completed": `status` is what a
        // machine consumer branches on, and the whole defect (#2569) was that
        // a run nothing proved was indistinguishable here from a proved one.
        PipelineStatus::Unverified { .. } => "unverified",
        PipelineStatus::VerificationFailed { .. } => "verification_failed",
        PipelineStatus::Aborted { .. } => "aborted",
    }
}

pub(super) fn pipeline_failure_reason(status: &PipelineStatus) -> Option<String> {
    match status {
        PipelineStatus::Completed => None,
        // Populated even though this is not a failure: the field is the
        // envelope's one place for "why does the status say that", and the
        // verdict summary is the only text naming which channel fell short.
        PipelineStatus::Unverified { verdict } => Some(verdict.summary.clone()),
        PipelineStatus::VerificationFailed { verdict } => {
            Some(format!("verification failed: {}", verdict.summary))
        }
        PipelineStatus::Aborted { reason, .. } => Some(reason.clone()),
    }
}

pub(super) fn pipeline_episode_outcome(status: &PipelineStatus) -> EpisodeOutcome {
    match status {
        PipelineStatus::Completed => EpisodeOutcome::Success,
        // Not `Success` — recall would otherwise hand a future session an
        // unproven episode as a worked example of a solved task. Not
        // `Failure` either: nothing found the work wrong.
        PipelineStatus::Unverified { .. } => EpisodeOutcome::Unverified,
        PipelineStatus::VerificationFailed { .. } => EpisodeOutcome::Failure,
        PipelineStatus::Aborted { .. } => EpisodeOutcome::Aborted,
    }
}

pub(crate) fn pipeline_status_result(
    status: &PipelineStatus,
    requirement: VerificationRequirement,
) -> Result<(), CliFailure> {
    match status {
        PipelineStatus::Completed => Ok(()),
        PipelineStatus::Unverified { verdict } => match requirement {
            VerificationRequirement::Advisory => Ok(()),
            VerificationRequirement::Required => Err(CliFailure::error(format!(
                "--require-verified: the run completed but nothing proved it — {}",
                verdict.summary
            ))),
        },
        PipelineStatus::VerificationFailed { verdict } => Err(CliFailure::error(format!(
            "verification failed: {}",
            verdict.summary
        ))),
        PipelineStatus::Aborted { reason, kind } => {
            Err(CliFailure::from_abort(reason.clone(), *kind))
        }
    }
}

/// The fleet ledger's reading of one finished attempt: `(summary, ok, label)`.
///
/// Here rather than in `fleet_cmd.rs` for the reason this module exists — the
/// fleet is one of the four surfaces that must read a finished run the same
/// way — and because `fleet_cmd.rs` is a god file closed to growth.
///
/// #2569: an unverified attempt is `ok` (the work stands and may be adopted;
/// nothing found it wrong) but is **not** labelled `completed`. The label is
/// what a human reads when choosing between a run's attempts, and two attempts
/// that differ in whether anything proved them must not print the same word.
pub(crate) fn fleet_attempt(outcome: &PipelineOutcome) -> (String, bool, &'static str) {
    match &outcome.status {
        PipelineStatus::Completed => (outcome.final_text.clone(), true, "completed"),
        PipelineStatus::Unverified { verdict } => (verdict.summary.clone(), true, "unverified"),
        PipelineStatus::VerificationFailed { verdict } => (
            format!("verification failed: {}", verdict.summary),
            false,
            "verification_failed",
        ),
        PipelineStatus::Aborted { reason, .. } => (reason.clone(), false, "aborted"),
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
/// `requirement` is threaded so the registry row agrees with the exit code:
/// under `--require-verified` an unverified run is a failure, and the row that
/// says otherwise would be the same lie in a second place. Under the default
/// it records `Complete` — the session's *lifecycle* did complete, and
/// [`stella_store::SessionStatus`] is a lifecycle axis (in progress, needs
/// input, stopped, complete) rather than a verification one. Adding an
/// unverified state to it was considered and rejected: it would have to bypass
/// [`crate::daemon::outcome_status`], which is deliberately the single decider
/// every registry writer shares (#1653), to carry a fact that is already
/// stated in the execution row's label and the JSON envelope's `status`.
pub(super) fn pipeline_session_status(
    result: &Result<PipelineOutcome, PipelineRunError>,
    requirement: VerificationRequirement,
) -> stella_store::SessionStatus {
    let terminal = match result {
        Ok(outcome) => pipeline_status_result(&outcome.status, requirement),
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
            pipeline_session_status(&stopped, VerificationRequirement::Advisory),
            stella_store::SessionStatus::Stopped,
            "a run that ended itself by policy must not read as a crash"
        );
        assert_eq!(
            pipeline_session_status(&crashed, VerificationRequirement::Advisory),
            stella_store::SessionStatus::Error
        );
    }

    /// The arms the widening must not disturb: a completed run stays
    /// `Complete`, and a hard pipeline error — which carries no abort kind —
    /// stays `Error`.
    #[test]
    fn the_remaining_terminal_arms_project_unchanged() {
        assert_eq!(
            pipeline_session_status(
                &Ok(pipeline_outcome(PipelineStatus::Completed)),
                VerificationRequirement::Advisory
            ),
            stella_store::SessionStatus::Complete
        );
        let hard = Err(PipelineRunError {
            cause: PipelineError::ScopeReviewRequiredHeadless,
            total_cost_usd: 0.0,
        });
        assert_eq!(
            pipeline_session_status(&hard, VerificationRequirement::Advisory),
            stella_store::SessionStatus::Error
        );
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
    /// An unverified run, as the pipeline settles one: the ladder abstained,
    /// the verdict publishes `passed: true`, and nothing proved the work.
    fn unverified_status() -> PipelineStatus {
        PipelineStatus::Unverified {
            verdict: stella_pipeline::Verdict {
                passed: true,
                deterministic: false,
                summary: "UNVERIFIED — no test command was tracked".into(),
                ladder: None,
            },
        }
    }

    /// #2569's witness at the surfaces a human and a machine actually read.
    ///
    /// The defect was that `LadderDecision::Unverified` and `Unverifiable`
    /// both emit `Verdict { passed: true }`, `PipelineStatus` had no state for
    /// that, and so every projection here read a success: `"completed"`,
    /// `EpisodeOutcome::Success`, and `Ok(())`. A turn the engine explicitly
    /// refused to prove was indistinguishable from a flip-verified one.
    ///
    /// On `main` this does not compile — the variant it is written against
    /// does not exist — which is the failing direction for a missing state.
    #[test]
    fn an_unverified_run_never_projects_as_a_success() {
        let status = unverified_status();
        assert_eq!(pipeline_status_label(&status), "unverified");
        assert_eq!(
            pipeline_episode_outcome(&status),
            EpisodeOutcome::Unverified
        );
        assert!(
            pipeline_failure_reason(&status).is_some_and(|r| r.contains("UNVERIFIED")),
            "the envelope must say which channel fell short"
        );
    }

    /// …and the exit code is the one projection that stays where it was, on
    /// purpose. `stella run` exits 0 for an unverified run by default — with
    /// no `--test-command` that is the ordinary outcome, and flipping it would
    /// break every script that shells out to Stella — so the change is offered
    /// as an explicit request instead.
    #[test]
    fn the_exit_code_holds_by_default_and_moves_only_when_asked() {
        let status = unverified_status();
        assert!(
            pipeline_status_result(&status, VerificationRequirement::Advisory).is_ok(),
            "the default must not break callers that exist today"
        );
        let failure = pipeline_status_result(&status, VerificationRequirement::Required)
            .expect_err("--require-verified refuses an unproven run");
        assert_eq!(failure.exit_code(), std::process::ExitCode::FAILURE);
    }

    /// The registry row agrees with the exit code rather than contradicting
    /// it: an unverified run under `--require-verified` is not `Complete`.
    #[test]
    fn the_session_row_follows_the_requirement_it_was_run_under() {
        let result = Ok(pipeline_outcome(unverified_status()));
        assert_eq!(
            pipeline_session_status(&result, VerificationRequirement::Advisory),
            stella_store::SessionStatus::Complete
        );
        assert_eq!(
            pipeline_session_status(&result, VerificationRequirement::Required),
            stella_store::SessionStatus::Error
        );
    }
}
