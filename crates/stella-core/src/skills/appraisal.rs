//! Skill appraisal — the measured answer to "is this skill earning its place?",
//! in both directions.
//!
//! The adaptive loop mines reflections into `SKILL.md` files and ships ON. Its
//! promotion rule was **frequency**: something observed enough times became a
//! skill. That means an unhelpful skill was minted exactly as easily as a
//! helpful one, and once minted it was injected forever — a repo convention
//! changes, a workaround goes obsolete, a preference is superseded, and nothing
//! notices (#1067, #1068).
//!
//! This module is the gate on both ends, and it is deliberately one gate rather
//! than two. Promotion and retirement are the same question asked with opposite
//! signs — *does injecting this change outcomes?* — so they read the same
//! evidence through the same engine ([`crate::comparison`]) and differ only in
//! which direction cleared the bar.
//!
//! # The arms
//!
//! Two: [`WITH_SKILL_ARM`] holds the turns where `select_skills` injected the
//! skill, [`WITHOUT_SKILL_ARM`] the turns where it did not. The join from
//! *selection* to *outcome* is the caller's, and it must be explicit — read off
//! a recorded selection and the receipts for that turn, never inferred from
//! diff text or from a skill's subject matter appearing in the work.
//!
//! [`SkillTrial::task`] is the pairing key, and it carries the difference
//! between the two ways this is used:
//!
//! - **Offline, over a fixed task set** (#1067's promotion gate): the task's
//!   own name. Trials pair per task, so a candidate cannot win by being
//!   evaluated on an easier subset.
//! - **Live, over a sliding window** (#1068's decay detection): one shared key
//!   for the whole window. Every turn is unique, so per-turn pairing would
//!   leave nothing paired at all; with one key nothing is dropped and the
//!   comparison degrades to the unpaired two-sample test it actually is.
//!
//! # Four verdicts, and why `Inert` is separate from `Harms`
//!
//! A skill that makes turns *worse* and a skill that changes nothing are both
//! demotable, and they are not the same finding. `Harms` is a confident
//! measurement in the negative direction and can be reached as soon as the
//! evidence floor allows. [`SkillVerdict::Inert`] is the absence of a
//! measurement, so it needs the higher bar of a full
//! [`AppraisalConfig::window`] in **both** arms — otherwise "we did not measure
//! anything" would retire a skill for having been quiet, which is how a
//! retirement mechanism loses a user's trust in one afternoon.
//!
//! # Demotion is not deletion
//!
//! [`decide_demotion`] never touches a hand-authored skill, whatever the
//! numbers say — the no-clobber rule extends to retirement. And what it
//! recommends is removal from *selection*, with the file and a tombstone left
//! behind, so restore works. The caller owns both halves; nothing here does
//! I/O.

use serde::{Deserialize, Serialize};

use super::SkillOrigin;
use crate::comparison::{
    ArmTrials, ComparisonConfig, ComparisonReport, ComparisonVerdict, Guard, LiftEvidence, Metric,
    TrialRecord, compare,
};
use crate::self_tuning::{KeepReason, RewardWeights, SelectionConfig, TaskOutcome};

/// The arm holding turns where the skill was injected.
pub const WITH_SKILL_ARM: &str = "with_skill";

/// The arm holding turns where it was not — the baseline every promotion is
/// measured against.
pub const WITHOUT_SKILL_ARM: &str = "without_skill";

/// One finished turn, and whether the skill under appraisal was part of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillTrial {
    /// The pairing key — a task name offline, one shared key for a live
    /// window. See the module docs.
    pub task: String,
    /// Whether the skill was injected into this turn. The caller reads this
    /// off a recorded selection, never off the work the turn produced.
    pub selected: bool,
    /// The turn's outcome, as the reward function reads it.
    pub outcome: TaskOutcome,
    /// Model calls the turn spent.
    pub turns: u32,
}

/// The bar an appraisal is judged against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppraisalConfig {
    /// The metric a skill's contribution is measured on.
    pub primary: Metric,
    /// Guards a *promotion* must not regress. Applied to the with-skill arm
    /// against the without-skill baseline.
    ///
    /// Empty by default, and the omission is load-bearing rather than lazy: a
    /// skill is injected context, so [`Metric::Tokens`] rises in the with-skill
    /// arm **by construction**. A strict token guard would therefore refuse
    /// every skill that has ever helped anyone. An operator who wants to price
    /// context can set a token guard with a real tolerance; a default one would
    /// only ever be a silent off switch.
    pub guards: Vec<Guard>,
    /// Weights turning a turn's outcome into a reward.
    pub weights: RewardWeights,
    /// The evidence floor and confidence bar, shared with every other A/B in
    /// the workspace.
    pub selection: SelectionConfig,
    /// Trials required in **each** arm before [`SkillVerdict::Inert`] may be
    /// reached. Deliberately far above `selection.min_samples_per_arm`: a
    /// confident negative can be acted on early, but retiring a skill for
    /// having produced no measurement needs a window long enough that "no
    /// measurement" means something.
    pub window: usize,
}

impl Default for AppraisalConfig {
    fn default() -> Self {
        Self {
            primary: Metric::PassRate,
            guards: Vec::new(),
            weights: RewardWeights::default(),
            selection: SelectionConfig::default(),
            window: 20,
        }
    }
}

/// What an appraisal found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum SkillVerdict {
    /// Injecting the skill confidently improved the primary metric, and no
    /// guard regressed. The only verdict a promotion may act on.
    Helps { lift: f64 },
    /// Injecting it confidently improved the primary metric and moved a guard
    /// past its tolerance. A real lift at a price the operator has not agreed
    /// to — held, with both halves on the record.
    GuardBlocked { lift: f64 },
    /// **Removing** it confidently improved the primary metric: the skill is
    /// actively making turns worse.
    Harms { lift: f64 },
    /// A full window in both arms and no confident movement in either
    /// direction. The skill costs context and returns nothing measurable.
    Inert { trials: usize },
    /// Not enough evidence to say anything. Names the bar that was missed.
    Insufficient { reason: KeepReason },
}

impl SkillVerdict {
    /// `true` only for [`SkillVerdict::Helps`] — the verdict a promotion may
    /// act on. A guard-blocked lift answers `false`: the lift was real, but
    /// not at a price anything here may accept on the operator's behalf.
    #[must_use]
    pub fn promotes(&self) -> bool {
        matches!(self, SkillVerdict::Helps { .. })
    }

    /// `true` when the evidence supports removing the skill from selection.
    #[must_use]
    pub fn demotes(&self) -> bool {
        matches!(
            self,
            SkillVerdict::Harms { .. } | SkillVerdict::Inert { .. }
        )
    }
}

/// One skill's appraisal: the verdict, and the evidence behind it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillAppraisal {
    /// The skill's name, as it appears in the frontmatter and the filename.
    pub skill: String,
    /// What the evidence says.
    pub verdict: SkillVerdict,
    /// The forward comparison — with-skill measured against the no-skill
    /// baseline. Published whole so a promoted skill's *why* survives it: the
    /// task set, the arms, the thresholds, and the lift.
    pub report: ComparisonReport,
    /// The reverse comparison's evidence, present only when removing the skill
    /// confidently won. Carried rather than recomputed so a reader can see the
    /// harm finding's own numbers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harm: Option<LiftEvidence>,
}

/// Appraise one skill from its trials.
///
/// Runs the comparison twice with the baseline swapped. That is not
/// redundancy: [`crate::self_tuning::select_winner`] answers "did the leading
/// arm confidently beat the named baseline", which is a one-directional
/// question, and both directions are findings here. The forward run asks
/// whether the skill helps; the reverse asks whether removing it does.
///
/// Total: no trials, one-sided trials, and non-finite outcomes all resolve to
/// a stated [`SkillVerdict::Insufficient`] rather than a panic.
pub fn appraise(skill: &str, trials: &[SkillTrial], config: &AppraisalConfig) -> SkillAppraisal {
    let arms = arms_from(trials);
    let report = compare(&arms, &forward_config(config));

    let verdict = match &report.verdict {
        ComparisonVerdict::Winner(evidence) => SkillVerdict::Helps {
            lift: evidence.promotion.lift,
        },
        ComparisonVerdict::GuardBlocked(evidence) => SkillVerdict::GuardBlocked {
            lift: evidence.promotion.lift,
        },
        ComparisonVerdict::NoWinner { reason } => {
            // The skill did not confidently help. Ask the other question
            // before concluding anything: not-helping and actively-harming are
            // different findings with different consequences.
            let reverse = compare(&arms, &reverse_config(config));
            return match reverse.verdict {
                // Guards are not applied in reverse. A guard exists to refuse a
                // *promotion* whose price is too high; a skill that is making
                // turns worse does not get to keep its place because removing
                // it would also cost tokens.
                ComparisonVerdict::Winner(evidence) | ComparisonVerdict::GuardBlocked(evidence) => {
                    SkillAppraisal {
                        skill: skill.to_string(),
                        verdict: SkillVerdict::Harms {
                            lift: evidence.promotion.lift,
                        },
                        report,
                        harm: Some(evidence),
                    }
                }
                ComparisonVerdict::NoWinner { .. } => SkillAppraisal {
                    skill: skill.to_string(),
                    verdict: inert_or_insufficient(&report, reason.clone(), config),
                    report,
                    harm: None,
                },
            };
        }
    };

    SkillAppraisal {
        skill: skill.to_string(),
        verdict,
        report,
        harm: None,
    }
}

/// Neither direction won. That is `Inert` — demotable — only once **both**
/// arms have run a full window; below it the honest answer is that nothing has
/// been measured yet.
fn inert_or_insufficient(
    report: &ComparisonReport,
    reason: KeepReason,
    config: &AppraisalConfig,
) -> SkillVerdict {
    let full_window = |id: &str| {
        report
            .arm(id)
            .is_some_and(|arm| arm.trials >= config.window)
    };
    if full_window(WITH_SKILL_ARM) && full_window(WITHOUT_SKILL_ARM) {
        SkillVerdict::Inert {
            trials: report.arms.iter().map(|arm| arm.trials).sum(),
        }
    } else {
        SkillVerdict::Insufficient { reason }
    }
}

/// Does the skill help? Baseline is the turns that went without it.
fn forward_config(config: &AppraisalConfig) -> ComparisonConfig {
    ComparisonConfig {
        baseline_id: WITHOUT_SKILL_ARM.to_string(),
        primary: config.primary,
        guards: config.guards.clone(),
        weights: config.weights,
        selection: config.selection,
    }
}

/// Does removing it help? Baseline is the turns that had it. Guardless — see
/// [`appraise`].
fn reverse_config(config: &AppraisalConfig) -> ComparisonConfig {
    ComparisonConfig {
        baseline_id: WITH_SKILL_ARM.to_string(),
        primary: config.primary,
        guards: Vec::new(),
        weights: config.weights,
        selection: config.selection,
    }
}

/// Split trials into the two arms. Both arms always exist, empty if need be,
/// so a one-sided window reports `InsufficientSamples` naming the empty arm
/// rather than `MissingBaseline`, which would read as a caller error.
fn arms_from(trials: &[SkillTrial]) -> Vec<ArmTrials> {
    let record = |t: &SkillTrial| TrialRecord {
        task: t.task.clone(),
        outcome: t.outcome,
        turns: t.turns,
    };
    vec![
        ArmTrials {
            id: WITHOUT_SKILL_ARM.to_string(),
            value: "baseline".to_string(),
            trials: trials.iter().filter(|t| !t.selected).map(record).collect(),
        },
        ArmTrials {
            id: WITH_SKILL_ARM.to_string(),
            value: "injected".to_string(),
            trials: trials.iter().filter(|t| t.selected).map(record).collect(),
        },
    ]
}

/// Why a skill was left in selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepSkillReason {
    /// Someone wrote or edited it. Never auto-demoted, whatever the numbers
    /// say — the no-clobber rule extends to retirement.
    HandAuthored,
    /// The evidence says it helps.
    Helping,
    /// The evidence does not yet say anything.
    NotEnoughEvidence,
}

/// Why a skill was removed from selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum DemotionReason {
    /// Removing it confidently improved the primary metric.
    Harmful { lift: f64 },
    /// A full window in both arms with no measurable contribution.
    Inert { trials: usize },
}

/// Whether to stop selecting a skill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum DemotionDecision {
    /// Stop selecting it. The caller writes a tombstone and leaves the file:
    /// demotion is not deletion, and restore must survive it.
    Demote { reason: DemotionReason },
    /// Leave it in selection, with the reason stated.
    Keep { reason: KeepSkillReason },
}

impl DemotionDecision {
    /// `true` only for [`DemotionDecision::Demote`].
    #[must_use]
    pub fn is_demotion(&self) -> bool {
        matches!(self, DemotionDecision::Demote { .. })
    }
}

/// Decide whether an appraised skill should leave selection.
///
/// The origin check comes **first** and is absolute. A hand-authored skill
/// with an identically regressing window is kept, because the person who wrote
/// it made a decision this loop is not entitled to reverse — and because the
/// measurement cannot tell "this skill is bad" from "this skill is applied to
/// the hard turns", which is exactly what a good hand-authored skill looks
/// like in the numbers.
pub fn decide_demotion(origin: SkillOrigin, appraisal: &SkillAppraisal) -> DemotionDecision {
    if origin != SkillOrigin::AutoCreated {
        return DemotionDecision::Keep {
            reason: KeepSkillReason::HandAuthored,
        };
    }
    match &appraisal.verdict {
        SkillVerdict::Harms { lift } => DemotionDecision::Demote {
            reason: DemotionReason::Harmful { lift: *lift },
        },
        SkillVerdict::Inert { trials } => DemotionDecision::Demote {
            reason: DemotionReason::Inert { trials: *trials },
        },
        SkillVerdict::Helps { .. } | SkillVerdict::GuardBlocked { .. } => DemotionDecision::Keep {
            reason: KeepSkillReason::Helping,
        },
        SkillVerdict::Insufficient { .. } => DemotionDecision::Keep {
            reason: KeepSkillReason::NotEnoughEvidence,
        },
    }
}

/// What an evaluation has to say about a candidate skill that does not exist
/// yet — the input [`super::decide_auto_creation`] gates on.
///
/// Carries no numbers on purpose. The lift, the task set and the thresholds
/// belong to the [`SkillAppraisal`] record the caller keeps; what the creation
/// gate needs is only the verdict, and a gate that took a scalar would invite
/// a second threshold to drift from the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalEvidence {
    /// An appraisal measured a confident lift with no guard regression.
    MeasuredLift,
    /// An appraisal ran and did not find one.
    NoLift,
    /// Nothing has evaluated this candidate. The default, and not a synonym
    /// for [`EvalEvidence::NoLift`]: one says the skill was measured and found
    /// wanting, the other says nobody looked.
    Unevaluated,
}

impl EvalEvidence {
    /// Read the gate's answer off an appraisal.
    #[must_use]
    pub fn from_verdict(verdict: &SkillVerdict) -> Self {
        if verdict.promotes() {
            EvalEvidence::MeasuredLift
        } else {
            EvalEvidence::NoLift
        }
    }
}

#[cfg(test)]
mod tests;
