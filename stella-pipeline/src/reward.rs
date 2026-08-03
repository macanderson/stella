//! Reward extraction (#1043) — turning the verification ladder's verdict into
//! the scalar a training loop can learn from, or into a stated refusal to
//! label it at all.
//!
//! The verify stage already computes a tiered, evidence-based verdict for
//! every outcome: deterministic first, a model judge only on genuinely
//! inconclusive evidence, an abstention when nothing could see. That verdict
//! *is* the reward signal this project has been generating all along. This
//! module is the (pure, I/O-free) mapping from it to a label, and the rules it
//! encodes are all about **what not to claim**.
//!
//! # Three tiers of confidence, and one refusal
//!
//! | Rung | Label | Why |
//! |---|---|---|
//! | [`LadderRung::SubmitFast`] | **+1.0** | A fail→pass flip of the tracked command. A hard label: a test observed it. |
//! | [`LadderRung::Revise`] | **−1.0** | Touched tests red. Equally hard, in the other direction. |
//! | [`LadderRung::ModelJudge`] (pass) | **+0.5** | A model's opinion. Real signal, half the weight, because the judge agreed with Terminal-Bench's own grader 46% of the time. |
//! | [`LadderRung::ModelJudge`] (fail) | **−0.5** | Same discount, same reason. |
//! | everything else | **discarded** | See below. |
//!
//! # Why the abstain rungs are discarded rather than punished
//!
//! This is the rule the whole module exists to enforce. [`LadderRung::Unverifiable`]
//! and [`LadderRung::NothingAttempted`] both end a turn *without* a claim about
//! the work, and the tempting shortcut — "no pass, therefore a fail" — is
//! exactly the inference the ladder was built to refuse. A Terminal-Bench trial
//! that wrote its answer through shell redirects recorded no touch, could not
//! be diffed, abstained, and scored 1.0 against its own verifier. Training on
//! that trajectory as a −1.0 would teach the model that a correct solution was
//! wrong.
//!
//! [`LadderRung::HeuristicFallback`] is discarded for a different reason: it is
//! not a verdict about the work at all, it is a verdict about the judge being
//! unavailable. [`LadderRung::Waived`] likewise — no reviewer was bought, so
//! nothing was determined either way.
//!
//! A discard is **not** a dropped record. [`RewardLabel`] always carries the
//! rung and always carries the [`DiscardReason`], so a consumer that
//! specifically wants the no-op trajectories (a curriculum, a failure-mode
//! study) can select them by outcome. What is withheld is the *scalar*, which
//! is the only part that would be a lie.
//!
//! # The airlock: labels carry no prose
//!
//! Judge reasoning and distress-guidance text are **steering**, never training
//! targets — the same discipline as the witness airlock
//! ([`crate::witness::airlock`]). Here that is enforced structurally rather
//! than by review: [`RewardLabel`] and everything it contains hold no
//! free-form `String` field, only enums and numbers, so there is no field a
//! model-authored sentence could be assigned to. [`Settlement::from_evidence`]
//! is deliberately the seam that proves it — it is handed the whole
//! [`JudgeEvidence`], summary included, and returns a value with no strings in
//! it. `tests::judge_prose_never_reaches_a_label` is the property test.

use serde::{Deserialize, Serialize};
use stella_protocol::{JudgeEvidence, LadderRung};

/// Magnitude of a label backed by a deterministic test observation.
const DETERMINISTIC_WEIGHT: f64 = 1.0;

/// Magnitude of a label backed only by a model judge's opinion. Half, and the
/// halving is measured rather than aesthetic: across an 89-task Terminal-Bench
/// run the judge agreed with the benchmark's grader 46% of the time.
const JUDGED_WEIGHT: f64 = 0.5;

/// What the verify stage settled on for one trajectory, as a reader recovers
/// it from the journal.
///
/// Three variants because a reader genuinely faces three situations, and
/// collapsing them would silently label two of them wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "settlement", rename_all = "snake_case")]
pub enum Settlement {
    /// A verdict that named its rung.
    Settled {
        /// The rung the ladder came to rest on.
        rung: LadderRung,
        /// The verdict's own `passed`.
        passed: bool,
    },
    /// A verdict recorded before the rung joined the wire (#1043). The rung
    /// cannot be re-derived from the surrounding flags — see [`LadderRung`] —
    /// so this is stated rather than guessed.
    RungUnknown,
    /// No verdict reached the journal at all: the turn was interrupted, or it
    /// never ran through the verify stage.
    Absent,
}

impl Settlement {
    /// Read the settlement off a `JudgeVerdict` event's parts.
    ///
    /// The whole [`JudgeEvidence`] is taken, summary and all, and none of it
    /// survives into the return value. That is the airlock stated as a
    /// signature: the only bytes that cross are `passed` and the rung.
    pub fn from_evidence(passed: bool, evidence: &JudgeEvidence) -> Self {
        match evidence.ladder.as_deref().and_then(|ladder| ladder.rung) {
            Some(rung) => Settlement::Settled { rung, passed },
            None => Settlement::RungUnknown,
        }
    }
}

/// Why a trajectory carries no reward scalar.
///
/// Every variant is a distinct, actionable fact about the run — never a
/// catch-all — because "discarded" without a reason is indistinguishable from
/// "lost".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscardReason {
    /// The ladder abstained: no evidence channel could observe the turn.
    /// Absence of evidence, and this module will not report it as evidence of
    /// absence.
    Abstained,
    /// The turn dispatched no call capable of changing the workspace. A real
    /// finding, and a valuable one — but not one the reward scale is
    /// calibrated for, so it is selected by rung rather than scored.
    NothingAttempted,
    /// The judge call failed or returned something unparseable, and the
    /// conservative heuristic stood in. A fact about the pipeline's
    /// availability, not about the work.
    JudgeUnavailable,
    /// No independent review was bought at all, so nothing was determined
    /// either way.
    ReviewWaived,
    /// The verdict predates the rung field, so which tier it belongs to is
    /// unknown.
    RungUnknown,
    /// The trajectory never reached a verdict.
    NoVerdict,
    /// The shaping terms could not be computed — a non-finite cost. A reward
    /// that is `NaN` is not a smaller reward, it is a corrupt one.
    CostNotFinite,
}

/// How the composite reward prices the effort a trajectory spent.
///
/// The defaults are #1043's stated starting point:
/// `outcome − 0.02·steps − 0.5·cost_usd − 0.1·revisions`. They are a starting
/// point in the literal sense — the weights were chosen to make a
/// twenty-step, one-dollar, three-revision success worth visibly less than a
/// four-step, ten-cent one, and they should be re-derived once there is enough
/// labelled data to fit them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RewardShaping {
    /// Subtracted per model call.
    pub per_step: f64,
    /// Subtracted per USD.
    pub per_usd: f64,
    /// Subtracted per revision round.
    pub per_revision: f64,
}

impl Default for RewardShaping {
    fn default() -> Self {
        Self {
            per_step: 0.02,
            per_usd: 0.5,
            per_revision: 0.1,
        }
    }
}

/// What one trajectory spent, as the shaping terms read it.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct TrajectoryCost {
    /// Model calls in the trajectory — every role, not just the worker's,
    /// because a turn that bought three judge calls did cost three judge
    /// calls.
    pub steps: u32,
    /// Settled USD for the whole trajectory.
    pub cost_usd: f64,
    /// Verification rounds beyond the first: how many times verify sent the
    /// worker back.
    ///
    /// Counted from the verdicts the journal holds. Under best-of-N fan-out
    /// that counts every candidate's verdicts rather than one candidate's
    /// revisions, so a fanned-out turn is over-penalized. The direction is
    /// deliberate: shaping only ever subtracts, so the error under-rewards an
    /// expensive turn rather than over-rewarding it.
    pub revisions: u32,
}

/// One trajectory's training label: the rung it came to rest on, the outcome
/// term that rung earned, and the composite reward — or the reason there
/// isn't one.
///
/// **No field here is a free-form string, and none may become one.** Every
/// text-shaped value is an enum with a closed wire vocabulary, so there is
/// nowhere for judge prose to land. `tests::a_label_has_no_free_text_leaves`
/// fails if that ever stops being true.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RewardLabel {
    /// The rung, when the verdict named one. Always recorded, including for a
    /// discard: selecting the no-op trajectories by rung is the whole reason a
    /// discard is a marked record rather than a deleted one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rung: Option<LadderRung>,
    /// The unshaped outcome term (`±1.0` / `±0.5`), `None` when discarded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<f64>,
    /// The composite reward after shaping, `None` when discarded. This is the
    /// number a trainer reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reward: Option<f64>,
    /// Why there is no reward, when there isn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discard: Option<DiscardReason>,
    /// The shaping inputs, republished so a reader can recompute the reward
    /// under different weights without re-reading the journal.
    pub cost: TrajectoryCost,
}

impl RewardLabel {
    /// `true` when this trajectory carries a usable scalar.
    #[must_use]
    pub fn is_scored(&self) -> bool {
        self.reward.is_some()
    }
}

/// Label one trajectory.
///
/// Total: every settlement, every rung, and any cost — including a non-finite
/// one — resolves to a `RewardLabel`, never a panic (invariant #5). A
/// trajectory that cannot be scored says so in [`RewardLabel::discard`].
pub fn label(settlement: Settlement, cost: TrajectoryCost, shaping: &RewardShaping) -> RewardLabel {
    let (rung, outcome) = match settlement {
        Settlement::Settled { rung, passed } => (Some(rung), outcome_term(rung, passed)),
        Settlement::RungUnknown => (None, Err(DiscardReason::RungUnknown)),
        Settlement::Absent => (None, Err(DiscardReason::NoVerdict)),
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(discard) => {
            return RewardLabel {
                rung,
                outcome: None,
                reward: None,
                discard: Some(discard),
                cost,
            };
        }
    };
    let reward = outcome
        - shaping.per_step * f64::from(cost.steps)
        - shaping.per_usd * cost.cost_usd
        - shaping.per_revision * f64::from(cost.revisions);
    if !reward.is_finite() {
        // A corrupt cost poisons the scalar but not the record: the rung and
        // the outcome term are still true, so they are still published.
        return RewardLabel {
            rung,
            outcome: Some(outcome),
            reward: None,
            discard: Some(DiscardReason::CostNotFinite),
            cost,
        };
    }
    RewardLabel {
        rung,
        outcome: Some(outcome),
        reward: Some(reward),
        discard: None,
        cost,
    }
}

/// The unshaped outcome term a rung earns, or the reason it earns none.
///
/// The sign comes from the verdict's own `passed` rather than from the rung,
/// so a rung emitted with an unexpected polarity is labelled by what actually
/// happened instead of by what the rung usually means.
pub fn outcome_term(rung: LadderRung, passed: bool) -> Result<f64, DiscardReason> {
    let magnitude = match rung {
        LadderRung::SubmitFast | LadderRung::Revise => DETERMINISTIC_WEIGHT,
        LadderRung::ModelJudge => JUDGED_WEIGHT,
        LadderRung::Unverifiable => return Err(DiscardReason::Abstained),
        LadderRung::NothingAttempted => return Err(DiscardReason::NothingAttempted),
        LadderRung::HeuristicFallback => return Err(DiscardReason::JudgeUnavailable),
        LadderRung::Waived => return Err(DiscardReason::ReviewWaived),
    };
    Ok(if passed { magnitude } else { -magnitude })
}

#[cfg(test)]
mod tests;
