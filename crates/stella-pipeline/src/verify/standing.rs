// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a finished verdict means to a consumer that must decide whether the
//! run's work has been **cleared** (#2569).
//!
//! The ladder resolves a turn onto one of six wire rungs, and for a long time
//! the process boundary read only two things off the result: `passed`, and
//! whether the run aborted. That collapse is the defect this module exists to
//! end. `LadderDecision::Unverified` and `LadderDecision::Unverifiable` both
//! emit `Verdict { passed: true }` — deliberately, because *a run is not
//! failed by the absence of a way to check it*, and reporting an unprovable
//! run as broken is the exact failure the abstaining rung was built to avoid.
//! But `passed: true` was then the only thing `PipelineStatus` looked at, so a
//! turn the engine explicitly refused to prove settled as `Completed` and was
//! projected — label, episode outcome, session row, exit code — identically to
//! a flip-verified one.
//!
//! A `bool` cannot carry three answers. This is the third one, named.
//!
//! # Why the classification is a total match on the rung
//!
//! The rung is the ladder's own statement of how it resolved, recorded at
//! decision time ([`stella_protocol::LadderSnapshot::rung`]) rather than
//! re-derived — see that type's docs for why re-deriving cannot work. Reading
//! it here, with a `match` that names every variant, means a future rung
//! cannot be added without someone answering "does this clear delivery?" at
//! the point where the answer is decided. `matches!(rung, A | B)` would have
//! taken the new rung's default silently, which is how the collapse this
//! module fixes happened in the first place.

use stella_protocol::LadderRung;

/// The three answers a finished verdict can give a delivery gate.
///
/// Distinct from the verdict's `passed` flag, which is a two-valued question
/// ("is this a finding that the work is wrong?") and answers `true` for both
/// a proof and an abstention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationStanding {
    /// Something the pipeline trusts said the work is good: a deterministic
    /// fail→pass flip, or a review the warrant found nothing to demand.
    Cleared,
    /// A determinate negative finding about the work.
    Failed,
    /// Neither. The work may well be correct and nothing available proved it —
    /// which is *not* a pass, and must never be projected as one.
    Unproven,
}

/// Classify a settled verdict from the rung it came to rest on.
///
/// `passed` still wins where the two could disagree: a rung emitted with an
/// unexpected polarity is labelled by what actually happened rather than by
/// what the rung usually means, the same rule
/// [`crate::reward::outcome_term`] applies. So a `passed: false` verdict is
/// [`VerificationStanding::Failed`] whatever rung carried it.
///
/// `rung: None` is a verdict recorded before the rung joined the snapshot
/// (#1043). It resolves to [`VerificationStanding::Unproven`] rather than
/// `Cleared`, because the whole point of this function is that "nothing here
/// can tell you" must not read as a pass — and a missing rung is exactly that
/// state. Every verdict this pipeline emits carries one
/// (`Pipeline::ladder_snapshot` seeds it unconditionally), so the arm is
/// reachable only from replayed history.
#[must_use]
pub fn standing_of(rung: Option<LadderRung>, passed: bool) -> VerificationStanding {
    if !passed {
        return VerificationStanding::Failed;
    }
    let Some(rung) = rung else {
        return VerificationStanding::Unproven;
    };
    match rung {
        // A fail→pass flip of the tracked command, corroborated and within
        // budget. The one rung that is a proof.
        LadderRung::SubmitFast => VerificationStanding::Cleared,
        // The warrant read the executed diff and found nothing an independent
        // reviewer could catch — a reasoned decision that no proof was owed,
        // not a failure to obtain one. Deliberately `Cleared`: treating a
        // docs-only or formatting change as unproven would make the honest
        // state so common that a delivery gate consulting it would be turned
        // off, which is how a signal stops being read at all. The distinction
        // stays legible downstream — the rung is on the wire, and `replay`
        // and the reward ladder both discard it rather than scoring it.
        LadderRung::Waived => VerificationStanding::Cleared,
        // Determinate negatives. Both already carry `passed: false` from the
        // pipeline, so these arms are unreachable in practice; they are here
        // because the classification must be total and because a polarity the
        // caller inverted should still land on the honest side.
        LadderRung::Revise | LadderRung::NothingAttempted => VerificationStanding::Failed,
        // The two abstentions, and the reason this module exists: "no probe
        // could look" and "the probes looked and did not settle it" are
        // different facts, and neither is a pass.
        LadderRung::Unverifiable | LadderRung::Unverified => VerificationStanding::Unproven,
        // The instrument, not the work: an authored witness that failed
        // identically before and after execution cannot discriminate, so
        // nothing was proved and no revision could change that (#2540).
        LadderRung::WitnessUnsatisfiable => VerificationStanding::Unproven,
    }
}

#[cfg(test)]
mod tests {
    use super::{VerificationStanding, standing_of};
    use stella_protocol::LadderRung;

    /// #2569's core: the two abstaining rungs publish `passed: true`, and that
    /// must not read as a pass at the process boundary.
    #[test]
    fn the_abstaining_rungs_are_unproven_despite_publishing_passed() {
        for rung in [LadderRung::Unverified, LadderRung::Unverifiable] {
            assert_eq!(
                standing_of(Some(rung), true),
                VerificationStanding::Unproven,
                "{rung:?} publishes passed:true and proves nothing"
            );
        }
    }

    /// #2540: an unsatisfiable witness is a statement about the instrument.
    #[test]
    fn an_unsatisfiable_witness_is_unproven_not_cleared() {
        assert_eq!(
            standing_of(Some(LadderRung::WitnessUnsatisfiable), true),
            VerificationStanding::Unproven
        );
    }

    #[test]
    fn a_deterministic_flip_and_a_reasoned_waiver_clear() {
        assert_eq!(
            standing_of(Some(LadderRung::SubmitFast), true),
            VerificationStanding::Cleared
        );
        assert_eq!(
            standing_of(Some(LadderRung::Waived), true),
            VerificationStanding::Cleared
        );
    }

    #[test]
    fn a_failing_verdict_is_failed_whatever_rung_carried_it() {
        for rung in [
            LadderRung::SubmitFast,
            LadderRung::Revise,
            LadderRung::NothingAttempted,
            LadderRung::Unverifiable,
            LadderRung::Unverified,
            LadderRung::WitnessUnsatisfiable,
            LadderRung::Waived,
        ] {
            assert_eq!(standing_of(Some(rung), false), VerificationStanding::Failed);
        }
    }

    /// A pre-#1043 verdict states no rung. "Unknown" is not "cleared".
    #[test]
    fn a_verdict_with_no_rung_is_unproven_not_cleared() {
        assert_eq!(standing_of(None, true), VerificationStanding::Unproven);
    }
}
