//! The comparison's input vocabulary: one trial, and the arm it ran under.

use serde::{Deserialize, Serialize};

use crate::self_tuning::TaskOutcome;

/// One task run once under one arm.
///
/// The reward ingredients live in [`TaskOutcome`] — the same type
/// [`crate::self_tuning::reward`] scores — so a comparison and a bandit trial
/// are describing the same event in the same words. `turns` rides beside it
/// rather than inside it because it is a *reported* metric, not a rewarded one:
/// a turn is neither good nor bad on its own, and folding it into the reward
/// would silently price it into every A/B that never asked for that.
///
/// Trials of the same task under the same arm are not distinguished from one
/// another. Repetition count is what matters (`--trials N`), and an index would
/// invite a caller to pair "repeat 3 of task X on arm A" with "repeat 3 on arm
/// B" as though the two shared a seed, which they do not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrialRecord {
    /// The task's stable name — the key trials are paired across arms by.
    pub task: String,
    /// Success, cost, tokens, and retries, as the reward function reads them.
    pub outcome: TaskOutcome,
    /// Model calls the trial spent. `0` is legitimate for a trial that never
    /// launched, so it is not a sentinel for "unknown".
    pub turns: u32,
}

/// One arm of the comparison: a named configuration and every trial run under
/// it.
///
/// Deliberately shaped like [`crate::self_tuning::ArmSamples`] — an id, a
/// human-readable value, and a bag of per-trial observations — because it is
/// the same idea one level up: the arm stays **opaque**. Nothing here knows
/// whether `value` names a model, a prompt template, a reasoning effort, or a
/// skill being A/B'd, which is exactly why one engine serves all of them.
///
/// Arm ids must be unique within a comparison. [`super::compare`] does not
/// police it (it stays total, and a duplicate is a caller bug rather than a
/// datum about the trials); the first arm carrying the baseline id is the one
/// the comparison measures against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmTrials {
    /// Stable id, e.g. `"base"` / `"candidate"`.
    pub id: String,
    /// The configuration this arm represents, for display and for the
    /// promotion record — a model name, an effort label, a skill slug.
    pub value: String,
    /// Every trial run under this arm, in any order.
    pub trials: Vec<TrialRecord>,
}

impl ArmTrials {
    /// The distinct task names this arm ran, sorted and deduplicated.
    pub fn task_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.trials.iter().map(|t| t.task.clone()).collect();
        names.sort();
        names.dedup();
        names
    }
}
