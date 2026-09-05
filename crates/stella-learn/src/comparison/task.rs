//! The per-task rows: how each arm did on one task, and which led it.

use serde::{Deserialize, Serialize};

use super::metric::mean;
use super::trial::TrialRecord;
use crate::self_tuning::{RewardWeights, reward};

/// One task's result under one arm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskArmResult {
    /// The arm's id.
    pub arm: String,
    /// Trials this arm ran on this task. `0` means it never ran it, which is
    /// what makes the row [`Leader::Unpaired`].
    pub trials: usize,
    /// Of those, how many succeeded.
    pub solved: usize,
    /// Mean reward across those trials, `0.0` when there were none.
    pub mean_reward: f64,
}

/// Which arm led one task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "leader", rename_all = "snake_case")]
pub enum Leader {
    /// Exactly one arm had the strictly highest mean reward on this task.
    Arm { id: String },
    /// Two or more arms tied at the top — the usual outcome on a task every
    /// arm solves, and not a defect.
    Tie,
    /// At least one arm never ran this task, so no comparison is possible.
    /// The row is reported anyway; a task missing from an arm is a fact about
    /// the run that a reader needs, and hiding it is how a shrunken denominator
    /// becomes an accidental win.
    Unpaired,
}

/// One task across every arm, with its leader.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRow {
    /// The task's name.
    pub task: String,
    /// One entry per arm, in the report's arm order.
    pub arms: Vec<TaskArmResult>,
    /// Which arm led, or why nobody did.
    pub leader: Leader,
}

impl TaskRow {
    /// Build one task's row from each arm's `(id, trials-on-this-task)`.
    ///
    /// Leadership is decided on **mean reward**, not solved count, so a task
    /// that both arms pass is still won by the arm that passed it more cheaply
    /// once the weights price cost in — and is a `Tie` under the default
    /// weights, where reward is exactly the success indicator.
    pub(super) fn fold(
        task: &str,
        per_arm: &[(&str, Vec<&TrialRecord>)],
        weights: &RewardWeights,
    ) -> Self {
        let arms: Vec<TaskArmResult> = per_arm
            .iter()
            .map(|(id, trials)| TaskArmResult {
                arm: (*id).to_string(),
                trials: trials.len(),
                solved: trials.iter().filter(|t| t.outcome.succeeded).count(),
                mean_reward: mean(
                    &trials
                        .iter()
                        .map(|t| reward(&t.outcome, weights))
                        .collect::<Vec<_>>(),
                ),
            })
            .collect();
        Self {
            leader: leader_of(&arms),
            task: task.to_string(),
            arms,
        }
    }
}

/// The arm with the strictly highest mean reward, or why there isn't one.
///
/// `total_cmp` orders the means so a `NaN` cannot win by comparing unequal to
/// everything: it sorts above every real number under `total_cmp`, so a `NaN`
/// arm would lead — which is why the winner is required to be strictly greater
/// than the runner-up **and** finite.
fn leader_of(arms: &[TaskArmResult]) -> Leader {
    if arms.is_empty() || arms.iter().any(|a| a.trials == 0) {
        return Leader::Unpaired;
    }
    let mut ranked: Vec<&TaskArmResult> = arms.iter().collect();
    ranked.sort_by(|a, b| b.mean_reward.total_cmp(&a.mean_reward));
    let top = ranked[0];
    if !top.mean_reward.is_finite() {
        return Leader::Tie;
    }
    match ranked.get(1) {
        Some(second) if second.mean_reward >= top.mean_reward => Leader::Tie,
        _ => Leader::Arm {
            id: top.arm.clone(),
        },
    }
}
