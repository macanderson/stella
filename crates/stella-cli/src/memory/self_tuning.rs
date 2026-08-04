//! Self-tuning — the I/O half of the eval-driven effort bandit (#831, first
//! slice). It sits beside [`crate::memory::rules_mining`]: the pure selection
//! math is [`stella_core::self_tuning`]; this module reads loop-bench results,
//! folds each trial into a reward sample, asks the core which arm won, and — on
//! a confident win — writes the winning worker reasoning-effort to settings and
//! appends a reversible [`RollbackRecord`] to an append-only ledger under
//! `.stella/private/`.
//!
//! **Why loop-bench.** loop-bench already emits a per-trial verifier reward in
//! its `--json` output; each arm is one loop-bench run (over a fixed held-out
//! task list) at a fixed worker effort, and its samples are those per-trial
//! rewards. The shipping CLI does **not** depend on the dev-only `loop-bench`
//! crate — it parses the JSON with the minimal [`BenchTrial`] view below.
//!
//! **The `effort_auto` gotcha.** A pinned `agents.worker.effort` is ignored
//! while `effort_auto: on` (the auto table picks the effort instead). So a
//! promotion also turns `effort_auto` off in the target scope, and the rollback
//! record captures the prior `effort_auto` so a rollback restores it exactly.
//! Worker effort is not safety-relevant policy (authority, scope-review, and
//! budget are), so this is within the allowed blast radius — and it is fully
//! reversible.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use stella_core::self_tuning::{
    ArmSamples, ArmStats, Decision, RewardWeights, RollbackRecord, SelectionConfig, TaskOutcome,
    reward, select_winner,
};
use stella_protocol::completion::ReasoningEffort;

use crate::settings::{
    AgentEngineAgent, AgentEngineConfig, EngineAgentKind, Toggle, project_config_path,
    user_config_path,
};

/// The settings path the effort knob lives at — the `knob` field of every
/// ledger record, and what a rollback restores.
pub(crate) const WORKER_EFFORT_KNOB: &str = "agent_engine_config.agents.worker.effort";

/// The append-only self-tuning ledger, under `.stella/private/` (git-ignored).
const LEDGER: &str = "self_tuning.jsonl";

/// Which settings scope a promotion writes to and a rollback restores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SettingsScope {
    /// `<workspace>/.stella/settings.json` — the default (highest precedence,
    /// so a pinned effort always binds).
    Project,
    /// `~/.stella/settings.json`.
    User,
}

impl SettingsScope {
    fn path(self, workspace_root: &Path) -> Result<PathBuf, String> {
        match self {
            SettingsScope::Project => Ok(project_config_path(workspace_root)),
            SettingsScope::User => user_config_path()
                .ok_or_else(|| "HOME is not set; cannot resolve user settings".to_string()),
        }
    }
}

/// The minimal view of a loop-bench `TrialReport` this module needs. loop-bench
/// serializes with the Rust field names verbatim; unknown fields are ignored,
/// and every field we read defaults so a shape change there degrades gracefully
/// rather than failing the parse. `reward` is `Option<f64>`: `None` means the
/// trial never reached the verifier and is **excluded** (it is not a zero).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct BenchTrial {
    #[serde(default)]
    pub reward: Option<f64>,
    #[serde(default)]
    pub spend_usd: f64,
    #[serde(default)]
    pub loop_detected: u32,
}

/// The object form of a loop-bench `--json` report (#1299), where a single run
/// used to emit the bare `trials` array. Both shapes are accepted: tuning
/// against an artifact from an older run is an ordinary thing to do, and the
/// trials inside are the same objects either way.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct BenchReport {
    /// Deliberately **not** `#[serde(default)]`, unlike every other field
    /// here: it is what makes this shape identifiable. Defaulted, any JSON
    /// object at all would parse as a report of zero trials, and a wrong file
    /// would reach the A/B as "insufficient samples" instead of "this is not a
    /// loop-bench report".
    pub trials: Vec<BenchTrial>,
    #[serde(default)]
    pub tally: BenchTally,
}

/// The run-level counts loop-bench folds its trials into. Only the ones this
/// module has something to say about are read.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub(crate) struct BenchTally {
    #[serde(default)]
    pub total: usize,
    /// Trials harbor recorded as having raised. They count as failures in the
    /// A/B below — dropping them would let an arm improve by crashing — but a
    /// tuning decision made largely out of crashed trials is a decision about
    /// the machine, so it is worth saying out loud.
    #[serde(default)]
    pub crashed: usize,
}

/// Parse a loop-bench `--json` result file's contents.
///
/// Two shapes are accepted. Since #1299 a single run emits
/// `{"trials": [...], "tally": {...}, "reconciliation": {...}}`; before that it
/// was the bare `trials` array. Tuning against an artifact from an older run is
/// an ordinary thing to do — the file exists to be trended — so the older shape
/// stays readable rather than failing with a parse error that sounds like the
/// file is corrupt.
///
/// The object is tried first. An array cannot deserialize into it, so the
/// fallback is a genuine "not that shape" rather than a sniffing guess. An old
/// file simply reports a zero tally: it carries no crash count, and no count is
/// reported as no count rather than as an affirmative "nothing crashed".
pub(crate) fn parse_bench_report(contents: &str) -> Result<BenchReport, String> {
    if let Ok(report) = serde_json::from_str::<BenchReport>(contents) {
        return Ok(report);
    }
    serde_json::from_str::<Vec<BenchTrial>>(contents)
        .map(|trials| BenchReport {
            trials,
            tally: BenchTally::default(),
        })
        .map_err(|e| {
            format!(
                "expected a loop-bench --json report — an object with a `trials` array, \
                 or the bare array older runs emitted: {e}"
            )
        })
}

/// The result of one A/B experiment: the decision plus each arm's stats, ready
/// to print and to append to the ledger as a receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExperimentReport {
    pub knob: String,
    pub decision: Decision,
    pub baseline: ArmStats,
    pub candidate: ArmStats,
}

/// One line of the self-tuning ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub(crate) enum LedgerEntry {
    /// An A/B was evaluated (whatever the decision) — the published receipt.
    /// Boxed: the report carries two [`ArmStats`] and a [`Decision`], far larger
    /// than the other variants, so boxing keeps every `LedgerEntry` small.
    Experiment {
        report: Box<ExperimentReport>,
        at_ms: u64,
    },
    /// A winner was promoted to settings.
    Promotion {
        rollback: RollbackRecord,
        /// The `effort_auto` value before the promotion (`None` = absent), so a
        /// rollback restores it exactly.
        previous_effort_auto: Option<bool>,
        scope: SettingsScope,
        at_ms: u64,
    },
    /// A promotion was reverted.
    Rolledback {
        knob: String,
        restored_value: Option<String>,
        restored_effort_auto: Option<bool>,
        scope: SettingsScope,
        at_ms: u64,
    },
}

/// Parse a reasoning-effort label. Accepts exactly the five settings spellings.
pub(crate) fn parse_effort(label: &str) -> Result<ReasoningEffort, String> {
    match label {
        "low" => Ok(ReasoningEffort::Low),
        "medium" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        "xhigh" => Ok(ReasoningEffort::Xhigh),
        "max" => Ok(ReasoningEffort::Max),
        other => Err(format!(
            "unknown reasoning effort `{other}` (expected low|medium|high|xhigh|max)"
        )),
    }
}

/// The settings label for an effort (the inverse of [`parse_effort`]).
pub(crate) fn effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}

/// Fold one arm's loop-bench trials into reward samples. A trial that never
/// reached the verifier (`reward == None`) is excluded — it is missing data,
/// not a zero. `succeeded` mirrors loop-bench's own "solved" bar (verifier
/// reward `>= 1.0`); cost and loop events feed the (weighted) penalties.
fn arm_from_trials(
    id: &str,
    value: &str,
    trials: &[BenchTrial],
    weights: &RewardWeights,
) -> ArmSamples {
    let rewards = trials
        .iter()
        .filter(|t| t.reward.is_some())
        .map(|t| {
            let outcome = TaskOutcome {
                succeeded: t.reward.is_some_and(|r| r >= 1.0),
                cost_usd: t.spend_usd,
                // loop-bench does not expose per-trial token counts; the token
                // weight is inert here and lives for the general receipts path.
                tokens: 0,
                retries: t.loop_detected,
            };
            reward(&outcome, weights)
        })
        .collect();
    ArmSamples {
        id: id.to_string(),
        value: value.to_string(),
        rewards,
    }
}

/// Run the A/B: score both arms' loop-bench trials and ask the core selector
/// whether the candidate confidently beats the baseline. Pure — no I/O — so it
/// is testable with synthetic trials.
pub(crate) fn run_experiment(
    baseline_trials: &[BenchTrial],
    baseline_effort: &str,
    candidate_trials: &[BenchTrial],
    candidate_effort: &str,
    weights: &RewardWeights,
    config: &SelectionConfig,
) -> ExperimentReport {
    let baseline_arm = arm_from_trials("baseline", baseline_effort, baseline_trials, weights);
    let candidate_arm = arm_from_trials("candidate", candidate_effort, candidate_trials, weights);
    let baseline = ArmStats::from_samples(&baseline_arm);
    let candidate = ArmStats::from_samples(&candidate_arm);
    let decision = select_winner(&[baseline_arm, candidate_arm], "baseline", config);
    ExperimentReport {
        knob: WORKER_EFFORT_KNOB.to_string(),
        decision,
        baseline,
        candidate,
    }
}

/// Read the current `agent_engine_config` block from one scope file, or the
/// default when the file/key is absent. Only that block is loaded; `save_to`
/// preserves every other key when writing back.
fn load_scope_engine_config(path: &Path) -> AgentEngineConfig {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
        .and_then(|v| v.get("agent_engine_config").cloned())
        .and_then(|v| serde_json::from_value::<AgentEngineConfig>(v).ok())
        .unwrap_or_default()
}

/// Milliseconds since the Unix epoch (best-effort; `0` if the clock is before
/// the epoch). Only used to timestamp ledger lines.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Append one entry to the self-tuning ledger.
fn append_entry(workspace_root: &Path, entry: &LedgerEntry) -> Result<(), String> {
    let line = serde_json::to_string(entry).map_err(|e| e.to_string())?;
    stella_store::append_workspace_private_line(workspace_root, LEDGER, &line)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Read every parseable ledger entry (bad lines skipped), oldest first.
pub(crate) fn read_ledger(workspace_root: &Path) -> Vec<LedgerEntry> {
    let Ok(path) = stella_store::workspace_private_state_path(workspace_root, LEDGER) else {
        return Vec::new();
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<LedgerEntry>(l).ok())
        .collect()
}

/// The promotion currently in effect for the effort knob: the last
/// [`LedgerEntry::Promotion`] not since reverted by a [`LedgerEntry::Rolledback`].
fn active_promotion(
    workspace_root: &Path,
) -> Option<(RollbackRecord, Option<bool>, SettingsScope)> {
    let mut active = None;
    for entry in read_ledger(workspace_root) {
        match entry {
            LedgerEntry::Promotion {
                rollback,
                previous_effort_auto,
                scope,
                ..
            } if rollback.knob == WORKER_EFFORT_KNOB => {
                active = Some((rollback, previous_effort_auto, scope));
            }
            LedgerEntry::Rolledback { knob, .. } if knob == WORKER_EFFORT_KNOB => {
                active = None;
            }
            _ => {}
        }
    }
    active
}

/// Publish an experiment receipt to the ledger (called whatever the decision).
pub(crate) fn publish_experiment(
    workspace_root: &Path,
    report: &ExperimentReport,
) -> Result<(), String> {
    append_entry(
        workspace_root,
        &LedgerEntry::Experiment {
            report: Box::new(report.clone()),
            at_ms: now_ms(),
        },
    )
}

/// The prior state captured before a promotion, so the caller can report and
/// so a rollback can restore it.
pub(crate) struct PriorState {
    pub effort: Option<String>,
    pub effort_auto: Option<bool>,
}

/// Write `chosen` worker effort to `scope`, disabling `effort_auto` so the
/// pinned value binds, and append a promotion record. Returns the prior state
/// captured for rollback. `save_to` preserves every other settings key.
pub(crate) fn promote(
    workspace_root: &Path,
    scope: SettingsScope,
    chosen: ReasoningEffort,
    promotion: &stella_core::self_tuning::Promotion,
) -> Result<PriorState, String> {
    let path = scope.path(workspace_root)?;
    let mut cfg = load_scope_engine_config(&path);

    let prior_effort = cfg
        .agent(EngineAgentKind::Worker)
        .and_then(|a| a.effort)
        .map(|e| effort_label(e).to_string());
    let prior_auto = cfg.effort_auto.map(Toggle::is_on);

    let agents = cfg.agents.get_or_insert_with(Default::default);
    let worker = agents
        .get_mut(EngineAgentKind::Worker)
        .get_or_insert_with(AgentEngineAgent::default);
    worker.effort = Some(chosen);
    // Disable auto-effort so the pinned worker effort actually takes effect.
    cfg.effort_auto = Some(Toggle::Off);

    cfg.save_to(&path)?;

    let record =
        RollbackRecord::from_promotion(WORKER_EFFORT_KNOB, prior_effort.clone(), promotion);
    append_entry(
        workspace_root,
        &LedgerEntry::Promotion {
            rollback: record,
            previous_effort_auto: prior_auto,
            scope,
            at_ms: now_ms(),
        },
    )?;

    Ok(PriorState {
        effort: prior_effort,
        effort_auto: prior_auto,
    })
}

/// The outcome of a rollback attempt.
pub(crate) enum RollbackOutcome {
    /// Nothing to roll back — no active promotion.
    Nothing,
    /// Restored the worker effort to the given prior value (`None` = cleared
    /// back to auto/default), in the named scope.
    Restored {
        knob: String,
        restored: Option<String>,
        scope: SettingsScope,
    },
}

/// Revert the last effort promotion: restore the worker effort and the
/// `effort_auto` value that were in place before it, in the scope it wrote.
pub(crate) fn rollback(workspace_root: &Path) -> Result<RollbackOutcome, String> {
    let Some((record, prior_auto, scope)) = active_promotion(workspace_root) else {
        return Ok(RollbackOutcome::Nothing);
    };
    let path = scope.path(workspace_root)?;
    let mut cfg = load_scope_engine_config(&path);

    // Restore worker effort to its prior value (or clear it entirely).
    let restored_effort = match &record.previous_value {
        Some(label) => Some(parse_effort(label)?),
        None => None,
    };
    let agents = cfg.agents.get_or_insert_with(Default::default);
    let worker = agents
        .get_mut(EngineAgentKind::Worker)
        .get_or_insert_with(AgentEngineAgent::default);
    worker.effort = restored_effort;

    // Restore effort_auto exactly (absent stays absent).
    cfg.effort_auto = prior_auto.map(|on| if on { Toggle::On } else { Toggle::Off });

    cfg.save_to(&path)?;

    append_entry(
        workspace_root,
        &LedgerEntry::Rolledback {
            knob: record.knob.clone(),
            restored_value: record.previous_value.clone(),
            restored_effort_auto: prior_auto,
            scope,
            at_ms: now_ms(),
        },
    )?;

    Ok(RollbackOutcome::Restored {
        knob: record.knob,
        restored: record.previous_value,
        scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // These fixtures write JSON directly, so they name the JSON path rather
    // than going through `project_config_path` (which prefers a stella.toml
    // when one exists).
    use crate::settings::project_settings_path;

    fn trial(reward: Option<f64>) -> BenchTrial {
        BenchTrial {
            reward,
            spend_usd: 0.0,
            loop_detected: 0,
        }
    }

    fn trials(rewards: &[Option<f64>]) -> Vec<BenchTrial> {
        rewards.iter().copied().map(trial).collect()
    }

    fn worker_effort_in(path: &Path) -> Option<String> {
        let cfg = load_scope_engine_config(path);
        cfg.agent(EngineAgentKind::Worker)
            .and_then(|a| a.effort)
            .map(|e| effort_label(e).to_string())
    }

    /// #1299: loop-bench's single-run `--json` became an object where it was a
    /// bare array. Both shapes must read, and to the same trials — an operator
    /// A/Bing this month's arm against last month's artifact is the ordinary
    /// use of this command, not an edge case.
    #[test]
    fn a_bench_report_reads_in_either_shape() {
        let array = r#"[
            {"task":"a","reward":1.0,"spend_usd":0.50,"loop_detected":0},
            {"task":"b","reward":null,"spend_usd":0.10,"loop_detected":2}
        ]"#;
        let object = format!(
            r#"{{"trials":{array},
                 "tally":{{"total":2,"solved":1,"loop_broken":1,"crashed":1}},
                 "reconciliation":null}}"#
        );

        let old = parse_bench_report(array).expect("the pre-#1299 array still reads");
        let new = parse_bench_report(&object).expect("the object reads");
        assert_eq!(old.trials.len(), 2);
        assert_eq!(new.trials.len(), 2);
        assert_eq!(new.trials[0].reward, Some(1.0));
        assert_eq!(new.trials[1].reward, None, "never verified is not a zero");
        assert_eq!(new.trials[1].loop_detected, 2);
        assert_eq!(new.tally.crashed, 1);
        assert_eq!(
            old.tally.crashed, 0,
            "an older file carries no crash count; that is silence, and the note \
             it drives simply does not fire"
        );

        // The arithmetic must not depend on which shape the file was in.
        let weights = RewardWeights::default();
        let from_array = arm_from_trials("a", "high", &old.trials, &weights);
        let from_object = arm_from_trials("a", "high", &new.trials, &weights);
        assert_eq!(from_array.rewards, from_object.rewards);
    }

    /// A file that is neither shape stays an error. In particular a JSON object
    /// that is not a report must not read as a report of zero trials, which
    /// would reach the A/B as "insufficient samples" — a statement about the
    /// experiment, when the truth is a statement about the file.
    #[test]
    fn a_file_that_is_not_a_bench_report_is_an_error() {
        for contents in ["not json at all", r#"{"schema_version":3,"rows":[]}"#, "{}"] {
            let err = parse_bench_report(contents)
                .err()
                .unwrap_or_else(|| panic!("{contents} must not parse as a report"));
            assert!(err.contains("`trials`"), "{err}");
        }
    }

    #[test]
    fn parse_and_label_round_trip() {
        for label in ["low", "medium", "high", "xhigh", "max"] {
            assert_eq!(effort_label(parse_effort(label).unwrap()), label);
        }
        assert!(parse_effort("turbo").is_err());
    }

    #[test]
    fn none_reward_trials_are_excluded_not_zeroed() {
        let arm = arm_from_trials(
            "candidate",
            "high",
            &trials(&[Some(1.0), None, Some(1.0)]),
            &RewardWeights::default(),
        );
        // Two finished trials (both solved), the unfinished one dropped.
        assert_eq!(arm.rewards, vec![1.0, 1.0]);
    }

    #[test]
    fn a_clean_win_promotes_and_rollback_restores() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let baseline = trials(&[Some(0.0); 6]);
        let candidate = trials(&[Some(1.0); 6]);
        let report = run_experiment(
            &baseline,
            "medium",
            &candidate,
            "high",
            &RewardWeights::default(),
            &SelectionConfig::default(),
        );
        assert!(report.decision.is_promotion(), "clean win should promote");
        publish_experiment(root, &report).unwrap();

        let Decision::Promote(p) = &report.decision else {
            unreachable!()
        };
        let chosen = parse_effort(&p.winner.value).unwrap();
        let prior = promote(root, SettingsScope::Project, chosen, p).unwrap();
        assert_eq!(prior.effort, None, "worker effort was unset before");

        let settings = project_settings_path(root);
        assert_eq!(worker_effort_in(&settings).as_deref(), Some("high"));
        // effort_auto was forced off so the pinned effort binds.
        let cfg = load_scope_engine_config(&settings);
        assert_eq!(cfg.effort_auto, Some(Toggle::Off));

        // The ledger carries the experiment receipt and the promotion.
        let entries = read_ledger(root);
        assert!(
            entries
                .iter()
                .any(|e| matches!(e, LedgerEntry::Promotion { .. }))
        );

        // Rollback restores the prior (unset) worker effort.
        match rollback(root).unwrap() {
            RollbackOutcome::Restored { restored, .. } => assert_eq!(restored, None),
            RollbackOutcome::Nothing => panic!("expected a rollback"),
        }
        assert_eq!(
            worker_effort_in(&settings),
            None,
            "effort cleared on rollback"
        );

        // A second rollback is a no-op (the promotion is already reverted).
        assert!(matches!(rollback(root).unwrap(), RollbackOutcome::Nothing));
    }

    #[test]
    fn rollback_restores_a_prior_effort_value() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let settings = project_settings_path(root);

        // Seed a prior worker effort of "low".
        let mut seed = AgentEngineConfig::default();
        let agents = seed.agents.get_or_insert_with(Default::default);
        agents
            .get_mut(EngineAgentKind::Worker)
            .get_or_insert_with(AgentEngineAgent::default)
            .effort = Some(ReasoningEffort::Low);
        seed.save_to(&settings).unwrap();

        let report = run_experiment(
            &trials(&[Some(0.0); 6]),
            "low",
            &trials(&[Some(1.0); 6]),
            "high",
            &RewardWeights::default(),
            &SelectionConfig::default(),
        );
        let Decision::Promote(p) = &report.decision else {
            panic!("expected promotion")
        };
        let prior = promote(
            root,
            SettingsScope::Project,
            parse_effort(&p.winner.value).unwrap(),
            p,
        )
        .unwrap();
        assert_eq!(prior.effort.as_deref(), Some("low"));
        assert_eq!(worker_effort_in(&settings).as_deref(), Some("high"));

        rollback(root).unwrap();
        assert_eq!(
            worker_effort_in(&settings).as_deref(),
            Some("low"),
            "rollback restores the pre-promotion effort"
        );
    }

    #[test]
    fn an_inconclusive_experiment_does_not_promote() {
        let noisy_a = trials(&[
            Some(1.0),
            Some(0.0),
            Some(1.0),
            Some(0.0),
            Some(1.0),
            Some(0.0),
        ]);
        let noisy_b = trials(&[
            Some(1.0),
            Some(0.0),
            Some(1.0),
            Some(1.0),
            Some(0.0),
            Some(1.0),
        ]);
        let report = run_experiment(
            &noisy_a,
            "medium",
            &noisy_b,
            "high",
            &RewardWeights::default(),
            &SelectionConfig::default(),
        );
        assert!(
            !report.decision.is_promotion(),
            "overlapping noisy arms must not promote"
        );
    }
}
