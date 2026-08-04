//! Reward extraction (#1043) — turning the verification ladder's verdict into
//! the scalar a training loop can learn from, or into a stated refusal to
//! label it at all.
//!
//! The verify stage already computes a tiered, evidence-based verdict for
//! every outcome: deterministic first, a model verifier only on genuinely
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
//! | [`LadderRung::ModelVerdict`] (pass) | **+0.5** | A model's opinion. Real signal, half the weight, because the verifier agreed with Terminal-Bench's own grader 46% of the time. |
//! | [`LadderRung::ModelVerdict`] (fail) | **−0.5** | Same discount, same reason. |
//! | everything else | **discarded** | See below. |
//!
//! # Why the magnitudes are configurable, and why only downward
//!
//! The 0.5 is an estimate of one verifier's accuracy, and the verifier in question is
//! whichever model a workspace pointed at it. A workspace whose verifier is worse
//! than the one that produced the 46% figure — a cheaper model, an unfamiliar
//! domain, a house style the verifier keeps mistaking for a defect — is entitled to
//! trust it less, so [`OutcomeWeights`] carries both magnitudes and a workspace
//! can lower the judged one.
//!
//! It cannot raise it past the deterministic weight. Above that line a model's
//! opinion outranks a test's observation, which inverts the premise the whole
//! ladder is built on: deterministic evidence is tried *first* precisely because
//! it is worth more. [`OutcomeWeights::validate`] refuses it, at config load and
//! again at [`label`], so a policy that got past one path cannot get past the
//! other.
//!
//! **Refused, not clamped** — deliberately the opposite posture to
//! `stella_context`'s retrieval tuning, which sanitizes an out-of-range knob
//! rather than failing. The postures differ because the failures do. A bad
//! retrieval knob degrades one turn, visibly, and the next turn recovers; the
//! worse answer there is failing a person's work over a typo. A bad reward
//! weight writes permanently mislabelled training data that is perfectly
//! well-formed — there is no turn to fail, and nothing downstream can tell that
//! a substituted weight was ever applied. So this one fails at launch, naming
//! the key, before any work starts.
//!
//! A judged weight of exactly `0.0` is legal and means something specific: *do
//! not train on this verifier at all*. It maps to [`DiscardReason::VerifierDistrusted`]
//! rather than to a `0.0` scalar, because a zero reward is a claim — "we watched
//! and it came out neutral" — and this setting is the opposite of a claim. That
//! is the same distinction the abstain rungs exist to preserve, applied to a
//! knob instead of a rung.
//!
//! # The policy travels with the label
//!
//! Every [`RewardLabel`] carries the [`RewardPolicy`] it was computed under.
//! Without it, two workspaces on different weights emit rows that are
//! arithmetically indistinguishable and silently incomparable — pooling them
//! would average a 0.5-scale judged pass against a 0.2-scale one as though they
//! were the same measurement. With it, a reader can renormalize, or select one
//! policy and drop the rest. `stella_core::comparison::ComparisonReport` already
//! stamps its own `RewardWeights` for the same reason; this is that rule applied
//! to the per-turn label.
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
//! not a verdict about the work at all, it is a verdict about the verifier being
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
//! Verifier reasoning and distress-guidance text are **steering**, never training
//! targets — the same discipline as the witness airlock
//! ([`crate::witness::airlock`]). Here that is enforced structurally rather
//! than by review: [`RewardLabel`] and everything it contains hold no
//! free-form `String` field, only enums and numbers, so there is no field a
//! model-authored sentence could be assigned to. [`Settlement::from_evidence`]
//! is deliberately the seam that proves it — it is handed the whole
//! [`VerdictEvidence`], summary included, and returns a value with no strings in
//! it. `tests::verifier_prose_never_reaches_a_label` is the property test.

use serde::{Deserialize, Serialize};
use stella_protocol::{VerdictEvidence, LadderRung};

/// Default magnitude of a label backed by a deterministic test observation.
const DETERMINISTIC_WEIGHT: f64 = 1.0;

/// Default magnitude of a label backed only by a model verifier's opinion. Half,
/// and the halving is measured rather than aesthetic: across an 89-task
/// Terminal-Bench run the verifier agreed with the benchmark's grader 46% of the
/// time.
const JUDGED_WEIGHT: f64 = 0.5;

/// What each tier of evidence is worth, before shaping.
///
/// Two numbers, one ordering rule: a verifier's opinion may be worth less than a
/// test's observation, never more. See the module docs for why the ceiling is
/// not negotiable and why `judged = 0.0` is a discard rather than a zero.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OutcomeWeights {
    /// Magnitude of a deterministic pass or fail. The unit the judged weight
    /// is measured against.
    pub deterministic: f64,
    /// Magnitude of a model verifier's pass or fail. `0.0` discards judged turns
    /// instead of scoring them.
    pub judged: f64,
}

impl Default for OutcomeWeights {
    fn default() -> Self {
        Self {
            deterministic: DETERMINISTIC_WEIGHT,
            judged: JUDGED_WEIGHT,
        }
    }
}

/// Why a set of [`OutcomeWeights`] cannot be used.
///
/// Carries no `String`, so a caller can render it wherever it needs to and a
/// label can name it without opening a hole in the airlock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeightError {
    /// A weight was `NaN` or infinite. Arithmetic on it produces a corrupt
    /// scalar rather than a small one.
    NotFinite,
    /// The deterministic weight is zero or negative. A scale whose unit is
    /// zero has no directions in it, and a negative one silently inverts every
    /// label's sign.
    DeterministicNotPositive,
    /// A negative judged weight. It would label a verifier's *pass* as a penalty,
    /// which no configuration can have meant.
    JudgedNegative,
    /// The judged weight exceeds the deterministic one — an opinion outranking
    /// an observation. See the module docs.
    JudgedAboveDeterministic,
    /// A negative shaping price, which would pay a trajectory for spending
    /// more. See [`RewardShaping::validate`].
    ShapingNegative,
}

impl WeightError {
    /// A one-line explanation, for a config error a person has to act on.
    #[must_use]
    pub fn explain(self) -> &'static str {
        match self {
            WeightError::NotFinite => "must be a finite number",
            WeightError::DeterministicNotPositive => {
                "deterministic_weight must be greater than zero — it is the unit \
                 every other weight is measured against"
            }
            WeightError::JudgedNegative => {
                "verifier_weight must not be negative — a negative weight scores a \
                 verifier's pass as a penalty"
            }
            WeightError::JudgedAboveDeterministic => {
                "verifier_weight must not exceed deterministic_weight — above it a \
                 model's opinion outranks a test's observation, which inverts \
                 the verification ladder. Lower it (0.0 discards judged turns \
                 entirely) rather than raising the ceiling"
            }
            WeightError::ShapingNegative => {
                "shaping prices must not be negative — a negative price pays a \
                 trajectory for spending more"
            }
        }
    }
}

impl OutcomeWeights {
    /// Check the ordering rule. Called at config load so a bad value is a loud
    /// launch failure, and again inside [`label`] so a policy that arrived by
    /// some other path (a deserialized record, a direct struct literal) is
    /// still refused rather than applied.
    pub fn validate(&self) -> Result<(), WeightError> {
        if !self.deterministic.is_finite() || !self.judged.is_finite() {
            return Err(WeightError::NotFinite);
        }
        if self.deterministic <= 0.0 {
            return Err(WeightError::DeterministicNotPositive);
        }
        if self.judged < 0.0 {
            return Err(WeightError::JudgedNegative);
        }
        if self.judged > self.deterministic {
            return Err(WeightError::JudgedAboveDeterministic);
        }
        Ok(())
    }
}

/// The whole reward policy: what evidence is worth, and what effort costs.
///
/// One value so a workspace configures, validates, and stamps a single thing —
/// a label computed under half the outcome weights but the default shaping is
/// as incomparable to a default label as one computed under different shaping,
/// and splitting them would let a reader stamp one and forget the other.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct RewardPolicy {
    /// What each rung's verdict is worth.
    pub outcome: OutcomeWeights,
    /// What the trajectory's effort subtracts.
    pub shaping: RewardShaping,
}

impl RewardPolicy {
    /// Check every rule this policy has to satisfy.
    pub fn validate(&self) -> Result<(), WeightError> {
        self.outcome.validate()?;
        self.shaping.validate()
    }
}

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
    /// Read the settlement off a `Verdict` event's parts.
    ///
    /// The whole [`VerdictEvidence`] is taken, summary and all, and none of it
    /// survives into the return value. That is the airlock stated as a
    /// signature: the only bytes that cross are `passed` and the rung.
    pub fn from_evidence(passed: bool, evidence: &VerdictEvidence) -> Self {
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
    /// The verifier call failed or returned something unparseable, and the
    /// conservative heuristic stood in. A fact about the pipeline's
    /// availability, not about the work.
    VerifierUnavailable,
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
    /// The workspace set its judged weight to `0.0`: this verifier is not trusted
    /// to label anything. Distinct from a `0.0` reward, which would assert that
    /// a neutral outcome was observed — see the module docs.
    VerifierDistrusted,
    /// The [`RewardPolicy`] itself is invalid, so no scalar computed under it
    /// would mean anything. The rung is still published, because the rung is
    /// still true; only the arithmetic is refused.
    PolicyInvalid,
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

impl RewardShaping {
    /// Every price must be finite and non-negative.
    ///
    /// Non-negative is not fussiness: "shaping only ever subtracts" is a stated
    /// invariant of this module and a property test
    /// (`tests::shaping_never_raises_a_reward`) that a consumer is entitled to
    /// rely on. A negative price would *pay* a trajectory for spending more,
    /// quietly turning a cost model into an incentive to burn tokens.
    pub fn validate(&self) -> Result<(), WeightError> {
        for price in [self.per_step, self.per_usd, self.per_revision] {
            if !price.is_finite() {
                return Err(WeightError::NotFinite);
            }
            if price < 0.0 {
                return Err(WeightError::ShapingNegative);
            }
        }
        Ok(())
    }
}

/// What one trajectory spent, as the shaping terms read it.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct TrajectoryCost {
    /// Model calls in the trajectory — every role, not just the worker's,
    /// because a turn that bought three verifier calls did cost three verifier
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
/// nowhere for verifier prose to land. `tests::a_label_has_no_free_text_leaves`
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
    /// The policy this label was computed under, stamped so the number above is
    /// interpretable outside the workspace that produced it. Always present,
    /// including on a discard — a reader pooling records has to be able to tell
    /// a `VerifierDistrusted` discard from a `judged = 0.5` workspace that simply
    /// never reached the verifier.
    pub policy: RewardPolicy,
}

impl RewardLabel {
    /// `true` when this trajectory carries a usable scalar.
    #[must_use]
    pub fn is_scored(&self) -> bool {
        self.reward.is_some()
    }
}

/// Label one trajectory under `policy`.
///
/// Total: every settlement, every rung, any cost — including a non-finite one —
/// and any policy, including an invalid one, resolves to a `RewardLabel`, never
/// a panic (invariant #5). A trajectory that cannot be scored says so in
/// [`RewardLabel::discard`], and the policy is stamped on the result either way.
pub fn label(settlement: Settlement, cost: TrajectoryCost, policy: &RewardPolicy) -> RewardLabel {
    let rung = match settlement {
        Settlement::Settled { rung, .. } => Some(rung),
        Settlement::RungUnknown | Settlement::Absent => None,
    };
    // Every early return stamps the same policy, so a discard is as
    // interpretable as a score. `unscored` exists to make forgetting that
    // impossible rather than merely discouraged.
    let unscored = |discard: DiscardReason, outcome: Option<f64>| RewardLabel {
        rung,
        outcome,
        reward: None,
        discard: Some(discard),
        cost,
        policy: *policy,
    };

    // The policy is checked before the rung is priced: a weight set this module
    // would refuse at config load must not be quietly honored just because the
    // value reached `label` by another path.
    if policy.validate().is_err() {
        return unscored(DiscardReason::PolicyInvalid, None);
    }

    let outcome = match settlement {
        Settlement::Settled { rung, passed } => outcome_term(rung, passed, &policy.outcome),
        Settlement::RungUnknown => Err(DiscardReason::RungUnknown),
        Settlement::Absent => Err(DiscardReason::NoVerdict),
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(discard) => return unscored(discard, None),
    };

    let shaping = &policy.shaping;
    let reward = outcome
        - shaping.per_step * f64::from(cost.steps)
        - shaping.per_usd * cost.cost_usd
        - shaping.per_revision * f64::from(cost.revisions);
    if !reward.is_finite() {
        // A corrupt cost poisons the scalar but not the record: the rung and
        // the outcome term are still true, so they are still published.
        return unscored(DiscardReason::CostNotFinite, Some(outcome));
    }
    RewardLabel {
        rung,
        outcome: Some(outcome),
        reward: Some(reward),
        discard: None,
        cost,
        policy: *policy,
    }
}

/// The unshaped outcome term a rung earns under `weights`, or the reason it
/// earns none.
///
/// The sign comes from the verdict's own `passed` rather than from the rung,
/// so a rung emitted with an unexpected polarity is labelled by what actually
/// happened instead of by what the rung usually means.
///
/// A judged rung under a zero judged weight is a **discard**, not a `0.0`. The
/// difference is the whole reason [`DiscardReason::VerifierDistrusted`] exists —
/// see the module docs.
pub fn outcome_term(
    rung: LadderRung,
    passed: bool,
    weights: &OutcomeWeights,
) -> Result<f64, DiscardReason> {
    let magnitude = match rung {
        LadderRung::SubmitFast | LadderRung::Revise => weights.deterministic,
        LadderRung::ModelVerdict if weights.judged == 0.0 => {
            return Err(DiscardReason::VerifierDistrusted);
        }
        LadderRung::ModelVerdict => weights.judged,
        LadderRung::Unverifiable => return Err(DiscardReason::Abstained),
        LadderRung::NothingAttempted => return Err(DiscardReason::NothingAttempted),
        LadderRung::HeuristicFallback => return Err(DiscardReason::VerifierUnavailable),
        LadderRung::Waived => return Err(DiscardReason::ReviewWaived),
    };
    Ok(if passed { magnitude } else { -magnitude })
}

#[cfg(test)]
mod tests;
