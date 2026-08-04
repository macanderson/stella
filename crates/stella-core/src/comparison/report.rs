//! The assembled report: what each arm did, who led each task, and the
//! two-bar verdict.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::arm::ArmSummary;
use super::guard::{Guard, GuardFinding};
use super::metric::Metric;
use super::task::TaskRow;
use super::trial::{ArmTrials, TrialRecord};
use crate::self_tuning::{
    ArmSamples, Decision, KeepReason, Promotion, RewardWeights, SelectionConfig, select_winner,
};

/// Bump when [`ComparisonReport`]'s shape changes incompatibly, so a consumer
/// (a promotion gate, a trend job) can refuse a report it does not understand
/// instead of misreading it.
pub const COMPARISON_SCHEMA_VERSION: u32 = 1;

/// What the comparison is judged on: the incumbent, the primary metric, the
/// guards, and the evidence bar.
///
/// Carried into [`ComparisonReport::config`] verbatim, because a verdict
/// without its thresholds cannot be audited — a reader has to be able to see
/// that a promotion cleared a `1.96` z and not a `0.5` one someone lowered for
/// the occasion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonConfig {
    /// The arm every candidate is measured against.
    pub baseline_id: String,
    /// The metric the winner is chosen on.
    pub primary: Metric,
    /// Metrics that must not regress past their tolerance. Evaluated in this
    /// order, and every one is reported whether it held or not.
    pub guards: Vec<Guard>,
    /// Weights turning a trial outcome into a reward. The default is pure
    /// success, so `Metric::Reward` and `Metric::PassRate` coincide until an
    /// operator prices something in.
    pub weights: RewardWeights,
    /// The sample floor and confidence bar the primary lift must clear.
    pub selection: SelectionConfig,
}

impl ComparisonConfig {
    /// A comparison against `baseline_id` on `primary`, with no guards and the
    /// conservative default evidence bar.
    pub fn new(baseline_id: impl Into<String>, primary: Metric) -> Self {
        Self {
            baseline_id: baseline_id.into(),
            primary,
            guards: Vec::new(),
            weights: RewardWeights::default(),
            selection: SelectionConfig::default(),
        }
    }

    /// The same config with `guards` attached.
    pub fn with_guards(mut self, guards: Vec<Guard>) -> Self {
        self.guards = guards;
        self
    }
}

/// One arm confidently led the primary metric — with the evidence, and every
/// guard's finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiftEvidence {
    /// The leading arm's id.
    pub arm: String,
    /// The selector's evidence: both arms' means, the lift, the observed z,
    /// and the sample counts it was decided from.
    pub promotion: Promotion,
    /// One finding per configured guard, in config order — held or regressed.
    pub guards: Vec<GuardFinding>,
}

impl LiftEvidence {
    /// The guards that regressed, in config order.
    pub fn regressions(&self) -> Vec<&GuardFinding> {
        self.guards.iter().filter(|g| g.regressed).collect()
    }
}

/// The comparison's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum ComparisonVerdict {
    /// A candidate confidently beat the baseline on the primary metric and
    /// held every guard. This is the only verdict a promotion gate may act on.
    Winner(LiftEvidence),
    /// A candidate confidently beat the baseline **and** moved a guard metric
    /// past its tolerance. A named refusal carrying its evidence: the lift was
    /// real, the price was not acceptable, and both halves are on the record.
    GuardBlocked(LiftEvidence),
    /// No candidate confidently beat the baseline. The selector's own reason
    /// says which bar was missed — too few samples, a baseline that was
    /// already best, a lift too small or too noisy.
    NoWinner { reason: KeepReason },
}

impl ComparisonVerdict {
    /// The promotable arm's id — `Some` only for [`ComparisonVerdict::Winner`].
    /// A guard-blocked lift deliberately answers `None`: it named an arm, but
    /// not one anything may promote.
    pub fn winner(&self) -> Option<&str> {
        match self {
            ComparisonVerdict::Winner(evidence) => Some(evidence.arm.as_str()),
            ComparisonVerdict::GuardBlocked(_) | ComparisonVerdict::NoWinner { .. } => None,
        }
    }

    /// `true` only for [`ComparisonVerdict::Winner`].
    pub fn is_winner(&self) -> bool {
        matches!(self, ComparisonVerdict::Winner(_))
    }
}

/// The full comparison: the config it was judged under, every arm's
/// aggregates, every paired task's row, what was excluded, and the verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonReport {
    /// [`COMPARISON_SCHEMA_VERSION`].
    pub schema: u32,
    /// The thresholds and metric this report was decided under.
    pub config: ComparisonConfig,
    /// One summary per arm, in input order.
    pub arms: Vec<ArmSummary>,
    /// One row per paired task, sorted by task name.
    pub tasks: Vec<TaskRow>,
    /// Tasks at least one arm never ran, sorted. Excluded from every figure
    /// above, and named here so the exclusion is auditable.
    pub unpaired_tasks: Vec<String>,
    /// The two-bar answer.
    pub verdict: ComparisonVerdict,
}

impl ComparisonReport {
    /// The named arm's summary, if the comparison had one.
    pub fn arm(&self, id: &str) -> Option<&ArmSummary> {
        self.arms.iter().find(|a| a.id == id)
    }
}

/// Fold trials from two or more arms into one comparison.
///
/// Total: an empty arm list, a missing baseline, arms that share no task, and
/// non-finite metrics all resolve to a report with a stated
/// [`ComparisonVerdict::NoWinner`] rather than a panic (invariant #5).
///
/// Deterministic: tasks are keyed and sorted, arms keep input order, and
/// nothing reads a clock — the same trials yield a byte-identical report.
pub fn compare(arms: &[ArmTrials], config: &ComparisonConfig) -> ComparisonReport {
    let paired = paired_tasks(arms);

    let mut summaries = Vec::with_capacity(arms.len());
    let mut samples = Vec::with_capacity(arms.len());
    // task -> arm index -> that arm's trials on the task. `BTreeMap` so the
    // row order is the task name's, not a hash seed's.
    let mut by_task: BTreeMap<&str, Vec<Vec<&TrialRecord>>> = BTreeMap::new();
    for task in &paired {
        by_task.insert(task.as_str(), vec![Vec::new(); arms.len()]);
    }

    for (index, arm) in arms.iter().enumerate() {
        let mut kept: Vec<&TrialRecord> = Vec::new();
        let mut excluded = 0usize;
        for trial in &arm.trials {
            if paired.contains(&trial.task) {
                kept.push(trial);
                if let Some(slots) = by_task.get_mut(trial.task.as_str())
                    && let Some(slot) = slots.get_mut(index)
                {
                    slot.push(trial);
                }
            } else {
                excluded += 1;
            }
        }
        summaries.push(ArmSummary::fold(
            &arm.id,
            &arm.value,
            &kept,
            excluded,
            &config.weights,
        ));
        samples.push(ArmSamples {
            id: arm.id.clone(),
            value: arm.value.clone(),
            rewards: kept
                .iter()
                .map(|t| config.primary.trial_score(t, &config.weights))
                .collect(),
        });
    }

    let tasks: Vec<TaskRow> = by_task
        .into_iter()
        .map(|(task, per_arm)| {
            let joined: Vec<(&str, Vec<&TrialRecord>)> =
                arms.iter().map(|a| a.id.as_str()).zip(per_arm).collect();
            TaskRow::fold(task, &joined, &config.weights)
        })
        .collect();

    let verdict = decide(&samples, &summaries, config);

    ComparisonReport {
        schema: COMPARISON_SCHEMA_VERSION,
        config: config.clone(),
        arms: summaries,
        tasks,
        unpaired_tasks: unpaired_tasks(arms, &paired),
        verdict,
    }
}

/// Apply the two bars: the selector's confident-lift test, then the guard set.
fn decide(
    samples: &[ArmSamples],
    summaries: &[ArmSummary],
    config: &ComparisonConfig,
) -> ComparisonVerdict {
    let promotion = match select_winner(samples, &config.baseline_id, &config.selection) {
        Decision::Promote(promotion) => promotion,
        Decision::Keep(reason) => return ComparisonVerdict::NoWinner { reason },
    };
    // `select_winner` found both arms, so both summaries exist — but stay
    // total rather than indexing on that reasoning.
    let (Some(baseline), Some(candidate)) = (
        summaries.iter().find(|s| s.id == promotion.baseline.id),
        summaries.iter().find(|s| s.id == promotion.winner.id),
    ) else {
        return ComparisonVerdict::NoWinner {
            reason: KeepReason::MissingBaseline {
                baseline_id: config.baseline_id.clone(),
            },
        };
    };
    let guards: Vec<GuardFinding> = config
        .guards
        .iter()
        .map(|guard| guard.evaluate(baseline, candidate))
        .collect();
    let evidence = LiftEvidence {
        arm: promotion.winner.id.clone(),
        promotion,
        guards,
    };
    if evidence.guards.iter().any(|g| g.regressed) {
        ComparisonVerdict::GuardBlocked(evidence)
    } else {
        ComparisonVerdict::Winner(evidence)
    }
}

/// The tasks **every** arm ran — the only ones any figure in the report is
/// computed over. Empty when there are no arms: nothing is paired with nothing.
fn paired_tasks(arms: &[ArmTrials]) -> BTreeSet<String> {
    let mut iter = arms.iter();
    let Some(first) = iter.next() else {
        return BTreeSet::new();
    };
    let mut paired: BTreeSet<String> = first.task_names().into_iter().collect();
    for arm in iter {
        let names: BTreeSet<String> = arm.task_names().into_iter().collect();
        paired.retain(|task| names.contains(task));
    }
    paired
}

/// Every task some arm ran that is not in the paired set, sorted.
fn unpaired_tasks(arms: &[ArmTrials], paired: &BTreeSet<String>) -> Vec<String> {
    let mut all: BTreeSet<String> = BTreeSet::new();
    for arm in arms {
        all.extend(arm.task_names());
    }
    all.difference(paired).cloned().collect()
}
