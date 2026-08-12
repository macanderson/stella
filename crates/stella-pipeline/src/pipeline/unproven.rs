// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What happens when verification could not see the turn either way (#2908).
//!
//! # The invariant this module exists to hold
//!
//! The staged pipeline wraps the bare agent loop, so it may only ever **add a
//! claim** about the work. It must never **subtract the work itself**. A test
//! that cannot be written is a claim that cannot be made — not a task to
//! abandon. Operationally: the pipeline must never score worse than the bare
//! loop on the same task, and every bare-pass / pipeline-fail pair is a loop
//! bug.
//!
//! # What broke it
//!
//! [`LadderDecision::Unverified`](crate::verify::LadderDecision::Unverified)
//! and [`Unverifiable`](crate::verify::LadderDecision::Unverifiable) were both
//! terminal, and the reasoning written on them — "the work may well be
//! correct, and nothing available proved it either way" — is right about the
//! **claim** and wrong about the **run**. Neither rung can tell *correct but
//! unprovable* from *the worker is not finished yet*, and ending the run on
//! them converts the second into a loss.
//!
//! Measured on ArenaBench `w10p`, task `pypi-server`, SUT `main@554bb4f`, same
//! worker model in both arms: bare scored 1,1 (7.2m and 10.7m wall) and built
//! the deliverable; the pipeline scored 0,0 and never built it, ending at
//! `verdict` zero seconds after `witness_unavailable` — 202s into a task the
//! bare arm was still working on four minutes later. Not resource starvation:
//! the pipeline's worker had *more* steps and tool calls per trial, and on
//! that task spent 52 steps to bare's 24. The run simply stopped.
//!
//! # The repair, and its bound
//!
//! The claim is downgraded and nothing else is: an unproven run still reports
//! UNPROVEN, still scores [`CandidateScore::Unverified`](crate::candidate::CandidateScore::Unverified), and buys no verifier
//! opinion. What changes is that the worker is handed the result back and
//! given the turn the bare loop would have given it.
//!
//! The bound is the bare loop's own stopping condition, not a number invented
//! here. A bare turn ends exactly when a step requests no tools; so a
//! continuation that dispatches no tool call is a worker that has said it is
//! finished, and the run settles on the spot. Everything else is the existing
//! repair gate — money, wall clock, and `max_revisions` — which is why a
//! continuation goes through [`Pipeline::affords_continuation`] rather than
//! counting rounds of its own. It is an ordinary revision turn underneath, so
//! the budget guard still stops only at a safe boundary, never mid-tool
//! (invariant #6); the one thing it does differently is documented on
//! [`Pipeline::return_worker_to_work`] — an aborted continuation settles the
//! candidate rather than aborting it, because an addition that fails must not
//! take the result with it.
//!
//! # Why `Unverifiable` is included
//!
//! It has the same "we could not see it" character as `Unverified` — every
//! channel blind, or every channel clear-eyed over effects that escaped
//! collection — and therefore the same ambiguity between an unobservable
//! finished turn and an unfinished one. The distinction the two rungs draw is
//! about *which* channels went dark, and no reading of that tells you whether
//! the worker had more to do; only the worker can answer that, which is what
//! the handback asks. The rungs that are **not** included are the ones that
//! know something: `Revise` and `NothingAttempted` already return the worker
//! to work with a measurement in hand, `WitnessUnsatisfiable` is a finding
//! about the instrument that no further work can move, and `SubmitFast` is a
//! pass.

use super::revision::RevisionCause;
use super::{
    CandidateResult, CandidateState, CandidateSurface, Engine, LadderInputs, LadderSnapshot,
    Pipeline, Spend, score_from_verification, unverifiable_evidence, unverified_evidence,
};

/// The handback's text. It states an absence, names no defect, and offers the
/// worker the exit — because the worker taking that exit is precisely the
/// signal [`unproven_next`] reads to end the run.
pub(super) const UNPROVEN_CONTINUATION_NOTICE: &str = "If the task is complete, say so and stop \
     — an unproven result is not a failing one, and nothing here asks you to \
     redo correct work. If anything is still unfinished, finish it now: this \
     turn is the same turn you would have had without verification, and it is \
     yours to use or to decline.";

/// What a candidate that reached an unprovable rung does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnprovenNext {
    /// Hand the result back and give the worker another turn.
    ReturnToWork,
    /// Stop: report the run as unproven and settle the candidate.
    Settle,
}

/// Everything [`unproven_next`] reasons over — owned plain data, so the
/// decision is a pure function of one value (invariant #2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkerStanding {
    /// Tool calls dispatched by the most recent **continuation** turn, or
    /// `None` when no continuation has been spent on this candidate yet.
    ///
    /// The distinction matters and is the reason this is an `Option` rather
    /// than a count. The execute turn's own dispatch total says nothing about
    /// whether the worker is finished — every turn that ever ran ends with a
    /// step that requested no tools, so reading it would settle every run
    /// immediately and change nothing. The question is what the worker does
    /// *when handed the unproven result*, and only a continuation turn
    /// answers it.
    pub last_continuation_dispatched: Option<u32>,
    /// Whether the run can still afford a round at all
    /// ([`Pipeline::affords_continuation`]).
    pub affords_continuation: bool,
}

/// Whether an unprovable result returns the worker to work.
///
/// Ordered so the cheap refusal comes first: no headroom is no turn, whatever
/// the worker was doing. Then the bare loop's condition — a continuation that
/// asked for no tools is a worker that is done, and asking again would be the
/// mirror of the defect this fixes, spending the run's remaining money on a
/// worker with nothing left to say.
pub(super) fn unproven_next(standing: WorkerStanding) -> UnprovenNext {
    if !standing.affords_continuation {
        return UnprovenNext::Settle;
    }
    match standing.last_continuation_dispatched {
        Some(0) => UnprovenNext::Settle,
        _ => UnprovenNext::ReturnToWork,
    }
}

/// The continuation bookkeeping one candidate carries, folded into
/// [`CandidateState`] as a single field so the two counts cannot be
/// transposed.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Continuations {
    /// Tool calls the last continuation turn dispatched — `None` until one has
    /// run. See [`WorkerStanding::last_continuation_dispatched`].
    last_dispatched: Option<u32>,
}

impl Pipeline<'_> {
    /// Hand an unprovable result back to the worker, if the run can still
    /// afford it and the worker has not already declined.
    ///
    /// `true` means a continuation turn ran and the caller should re-observe
    /// from the top of the verification loop — the tree may now carry work a
    /// deterministic rung can settle. `false` means the run is finished and
    /// the caller should settle it as unproven.
    ///
    /// # An aborted continuation settles; it never aborts the candidate
    ///
    /// This is the one place a turn's abort is deliberately *not* propagated.
    /// The handback is an addition to a run that had already earned its
    /// unproven verdict, so letting the extra turn's death take that verdict
    /// with it would be the same defect this module fixes, wearing the other
    /// mask: the pipeline would have subtracted a result the bare loop would
    /// have kept. The reason is warned rather than swallowed, and the abort
    /// still ends the candidate — by settling it, which is where it was going
    /// anyway.
    pub(super) async fn return_worker_to_work(
        &self,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        spend: &mut Spend<'_>,
        state: &mut CandidateState,
        affords_continuation: bool,
    ) -> bool {
        let standing = WorkerStanding {
            last_continuation_dispatched: state.continuations.last_dispatched,
            affords_continuation,
        };
        if unproven_next(standing) == UnprovenNext::Settle {
            return false;
        }
        // Bracketed across the turn, because the running total is cumulative
        // over every turn this candidate has taken and the question is about
        // this one.
        let before = state.signals.dispatched_actions;
        if let Err(abort) = self
            .revise_candidate(
                engine,
                surface,
                RevisionCause::Unproven(UNPROVEN_CONTINUATION_NOTICE),
                spend,
                state,
            )
            .await
        {
            self.warn(format!(
                "the unproven result was handed back and that turn did not finish: {}. \
                 The work already done stands, and the run is reported as unproven.",
                abort.reason
            ));
            return false;
        }
        state.continuations.last_dispatched =
            Some(state.signals.dispatched_actions.saturating_sub(before));
        true
    }

    /// Settle a candidate on the `Unverified` rung: nothing deterministic
    /// settled the turn and nothing else is going to.
    ///
    /// No model is asked, because the evidence that reaches this rung is by
    /// construction the evidence no oracle could settle, and a model handed it
    /// would be guessing at it too — only with a verdict's authority attached.
    /// Measured, that guess agreed with Terminal-Bench's grader 46% of the
    /// time, and 17 of its false passes cost 5 tasks outright.
    ///
    /// `passed: true` because a run is not failed by the absence of a way to
    /// check it. What keeps that from reading as a pass is the pair beside it:
    /// the summary says UNVERIFIED in its first word and the score is
    /// [`CandidateScore::Unverified`](crate::candidate::CandidateScore::Unverified),
    /// so this candidate can never tie a
    /// genuinely verified sibling in best-of-N and then win the smaller-diff
    /// tiebreak.
    pub(super) fn settle_unverified(
        &self,
        inputs: &LadderInputs,
        snapshot: &LadderSnapshot,
        state: CandidateState,
    ) -> CandidateResult {
        let mut evidence = unverified_evidence(inputs, state.oracle.tracked_command());
        evidence.ladder = Some(Box::new(snapshot.clone()));
        self.unproven_verdict(&evidence.summary);
        self.emit(stella_protocol::AgentEvent::Verdict {
            passed: true,
            evidence: evidence.clone(),
        });
        state.into_verified(true, &evidence, score_from_verification(false, None))
    }

    /// Settle a candidate on the `Unverifiable` rung: the turn went unobserved,
    /// every channel blind or every channel clear-eyed and empty over
    /// dispatched mutating calls (#1701).
    ///
    /// The verifier is not asked, because the only thing it could do is guess
    /// from an empty record — which in the wild it did, returning `FAIL … the
    /// file likely does not exist` about a file that was in the container
    /// (#973). `passed: true` and the summary/score pair carry the same
    /// meaning documented on [`Pipeline::settle_unverified`]; the
    /// `NothingAttempted` rung is what keeps this defensible, because reaching
    /// here means the turn *did* dispatch mutating calls and no channel saw
    /// their effect — a real state, and one no revision can clear, since that
    /// workspace never becomes observable.
    pub(super) fn settle_unverifiable(
        &self,
        inputs: &LadderInputs,
        snapshot: &LadderSnapshot,
        state: CandidateState,
    ) -> CandidateResult {
        let mut evidence = unverifiable_evidence(inputs);
        evidence.ladder = Some(Box::new(snapshot.clone()));
        self.unverifiable(&evidence.summary);
        self.emit(stella_protocol::AgentEvent::Verdict {
            passed: true,
            evidence: evidence.clone(),
        });
        state.into_verified(true, &evidence, score_from_verification(false, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect itself, at the decision layer: a worker that has just been
    /// told nothing could prove its turn gets the turn, exactly as it would
    /// have without the pipeline wrapped around it.
    #[test]
    fn an_unproven_result_returns_the_worker_to_work() {
        assert_eq!(
            unproven_next(WorkerStanding {
                last_continuation_dispatched: None,
                affords_continuation: true,
            }),
            UnprovenNext::ReturnToWork
        );
    }

    /// The bare loop's own stopping condition, and the guard against the
    /// obvious failure mode: a continuation that requested no tools is a
    /// worker that is finished, and the run ends there rather than looping.
    #[test]
    fn a_worker_that_asks_for_no_tools_is_done() {
        assert_eq!(
            unproven_next(WorkerStanding {
                last_continuation_dispatched: Some(0),
                affords_continuation: true,
            }),
            UnprovenNext::Settle
        );
    }

    /// A worker still calling tools was not finished — which is the whole
    /// finding — so it keeps the turn it would have had.
    #[test]
    fn a_worker_still_calling_tools_keeps_working() {
        assert_eq!(
            unproven_next(WorkerStanding {
                last_continuation_dispatched: Some(3),
                affords_continuation: true,
            }),
            UnprovenNext::ReturnToWork
        );
    }

    /// The money and wall-clock bound outranks everything: an out-of-headroom
    /// run settles even mid-flow, which is what keeps this from being an
    /// unbounded loop when the worker keeps fidgeting.
    #[test]
    fn no_headroom_settles_whatever_the_worker_is_doing() {
        for last in [None, Some(0), Some(9)] {
            assert_eq!(
                unproven_next(WorkerStanding {
                    last_continuation_dispatched: last,
                    affords_continuation: false,
                }),
                UnprovenNext::Settle,
                "the repair gate is the outer bound: {last:?}"
            );
        }
    }
}
