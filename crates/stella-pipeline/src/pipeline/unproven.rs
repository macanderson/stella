//! The two ladder rungs that could not observe the turn — `Unverifiable` and
//! `Unverified` — settle here rather than in `pipeline.rs`'s
//! `verify_candidate` loop, because `pipeline.rs` is closed to growth
//! (AGENTS.md § "God files"). Pulled out verbatim: no behavior changed.
//!
//! Both rungs share one shape: build the rung's evidence, log and emit the
//! `Verdict`, then — scoped to `effective_cmd.is_none()`, the exact shape
//! that let the ladder end a run ahead of the bare loop on `pypi-server`
//! (#2908) — offer the worker one more turn, bounded exactly like every
//! other back-edge on this ladder (`affords_repair`'s revision/budget/
//! wall-clock gate) plus the worker's own stopping condition (no further
//! mutating call, no diff movement), before settling `Unverified`.
//!
//! [`std::ops::ControlFlow::Break`] carries the finished [`CandidateResult`],
//! whether that is a settled verdict or a turn abort.
//! [`std::ops::ControlFlow::Continue`] hands the mutated [`CandidateState`]
//! back so the caller's loop can re-observe from the top — the same
//! ownership `verify_candidate`'s own loop already had over `state`, just
//! threaded through a call instead of inline.

use std::ops::ControlFlow;

use super::repair_gate::RepairMeter;
use super::*;

/// What this round's ladder decision knows about the turn, bundled so the
/// two settle methods below stay under clippy's argument-count gate rather
/// than growing an unexplained `#[allow]`.
pub(super) struct LadderContext<'c> {
    pub(super) snapshot: &'c LadderSnapshot,
    pub(super) inputs: &'c LadderInputs,
    pub(super) effective_cmd: Option<EffectiveTestCommand<'c>>,
}

impl Pipeline<'_> {
    /// Settle the `LadderDecision::Unverifiable` rung: every channel blind,
    /// or every channel clear-eyed and empty over dispatched mutating calls
    /// (#1701). The verifier is not asked, because the only thing it could
    /// do is guess from an empty record — which in the wild it did,
    /// returning `FAIL … the file likely does not exist` about a file that
    /// was in the container (#973).
    ///
    /// `passed: true` because a run is not failed by the absence of a way to
    /// check it, and this shape already exists for the review-waived path.
    /// What keeps it from reading as a pass is the pair beside it: the
    /// summary says UNVERIFIABLE in its first word, and the score is
    /// `Unverified`, so this candidate can never tie a genuinely verified
    /// sibling in best-of-N and then win the smaller-diff tiebreak.
    ///
    /// `LadderDecision::NothingAttempted` is what makes this defensible.
    /// Reaching here means the turn *did* dispatch mutating calls and no
    /// channel saw their effect — a real state: a Terminal-Bench trial that
    /// wrote its answer through shell redirects recorded no touch, could not
    /// be diffed, landed here, and scored 1.0 against its verifier. Failing
    /// that closed would report a correct run as broken, and no revision can
    /// clear it — that workspace never becomes observable.
    pub(super) async fn settle_unverifiable(
        &self,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        spend: &mut Spend<'_>,
        meter: &RepairMeter,
        mut state: CandidateState,
        ladder: LadderContext<'_>,
    ) -> ControlFlow<CandidateResult, CandidateState> {
        let mut evidence = unverifiable_evidence(ladder.inputs);
        evidence.ladder = Some(Box::new(ladder.snapshot.clone()));
        self.unverifiable(&evidence.summary);
        self.emit(AgentEvent::Verdict {
            passed: true,
            evidence: evidence.clone(),
        });
        // #2908: `Unverifiable` is a claim, not a verdict on the run — "we
        // could not see" must not read as "stop here." Scoped to
        // `effective_cmd.is_none()` — no tracked command AND no witness —
        // because that is the evidenced failure: a run with a real command
        // that merely went blind this round (infra noise on the
        // confirmation, say) is a narrower, already-tested shape, and
        // widening to it too is a separate change, not this one. Bounded
        // exactly like every other back-edge on this ladder
        // (`affords_repair`'s revision/budget/wall-clock gate), and
        // additionally by the worker's OWN stopping condition: a round that
        // dispatched no further mutating call and moved no diff line
        // answered the nudge by declining to act, which is bare's own
        // termination — adopting it here is what keeps this from looping
        // the full revision allowance against a worker that is already
        // done.
        if ladder.effective_cmd.is_none()
            && self.affords_repair(&state, meter, *spend.total, spend.budget)
        {
            let mutating_before = state.signals.mutating_actions;
            let diff_lines_before = state.diff_lines;
            if let Err(abort) = self
                .revise_candidate(
                    engine,
                    surface,
                    RevisionCause::Deterministic(UNPROVEN_CONTINUE_NUDGE),
                    spend,
                    &mut state,
                )
                .await
            {
                return ControlFlow::Break(CandidateResult::turn_aborted(state.messages, abort));
            }
            if state.signals.mutating_actions != mutating_before
                || state.diff_lines != diff_lines_before
            {
                return ControlFlow::Continue(state);
            }
        }
        ControlFlow::Break(state.into_verified(
            true,
            &evidence,
            score_from_verification(false, None),
        ))
    }

    /// Settle the `LadderDecision::Unverified` rung: escalation on evidence
    /// exhausted, or never warranted. Terminal by default — nothing
    /// deterministic settled this turn, and nothing else is going to: no
    /// model is asked, because the evidence that reaches this arm is by
    /// construction the evidence no oracle could settle, and a model handed
    /// it would be guessing at it too — only with a verdict's authority
    /// attached. Measured, that guess agreed with Terminal-Bench's grader
    /// 46% of the time, and 17 of its false passes cost 5 tasks outright.
    ///
    /// `passed: true` for exactly the reason [`Pipeline::settle_unverifiable`]
    /// uses it — a run is not failed by the absence of a way to check it —
    /// and, exactly as there, what keeps it from reading as a pass is the
    /// pair beside it: the summary says UNVERIFIED in its first word and the
    /// score is `Unverified`, so this candidate can never tie a genuinely
    /// verified sibling in best-of-N and then win the smaller-diff tiebreak.
    pub(super) async fn settle_unverified(
        &self,
        engine: &Engine<'_>,
        surface: CandidateSurface<'_>,
        spend: &mut Spend<'_>,
        meter: &RepairMeter,
        mut state: CandidateState,
        ladder: LadderContext<'_>,
    ) -> ControlFlow<CandidateResult, CandidateState> {
        let mut evidence = unverified_evidence(ladder.inputs, state.oracle.tracked_command());
        evidence.ladder = Some(Box::new(ladder.snapshot.clone()));
        self.unproven_verdict(&evidence.summary);
        self.emit(AgentEvent::Verdict {
            passed: true,
            evidence: evidence.clone(),
        });
        // #2908: scoped to `effective_cmd.is_none()` — no tracked command AND
        // no witness, the exact shape that let the ladder end a run four
        // minutes before the bare loop on the same worker, on `pypi-server`.
        // Where a command DOES exist, either the evidence-demand back-edge
        // already spent its one ask, or it declined to (off, or already
        // spent, or the pass does not stand alone) — those are this arm's
        // own established, separately-tested contract and are unaffected
        // here. The pipeline may downgrade the CLAIM to unproven; it may not
        // downgrade the RUN by ending it a revision before the worker itself
        // would have stopped. Same bound as `settle_unverifiable`:
        // `affords_repair`'s existing revision/budget/wall-clock gate, plus
        // the worker's own stopping condition (no further mutating call, no
        // diff movement) so a worker that is genuinely done is not walked
        // through the rest of its allowance for nothing.
        if ladder.effective_cmd.is_none()
            && self.affords_repair(&state, meter, *spend.total, spend.budget)
        {
            let mutating_before = state.signals.mutating_actions;
            let diff_lines_before = state.diff_lines;
            if let Err(abort) = self
                .revise_candidate(
                    engine,
                    surface,
                    RevisionCause::Deterministic(UNPROVEN_CONTINUE_NUDGE),
                    spend,
                    &mut state,
                )
                .await
            {
                return ControlFlow::Break(CandidateResult::turn_aborted(state.messages, abort));
            }
            if state.signals.mutating_actions != mutating_before
                || state.diff_lines != diff_lines_before
            {
                return ControlFlow::Continue(state);
            }
        }
        ControlFlow::Break(state.into_verified(
            true,
            &evidence,
            score_from_verification(false, None),
        ))
    }
}
