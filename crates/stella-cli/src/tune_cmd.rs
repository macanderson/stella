//! `stella tune` — eval-driven self-tuning of stella's own policy (#831, first
//! slice: the worker reasoning-effort knob).
//!
//! The flow is A/B on loop-bench, offline in this command: run loop-bench twice
//! over a fixed held-out task list — once at the baseline worker effort, once at
//! the candidate — capture each run's `--json` output, then:
//!
//! ```text
//! stella tune effort --baseline base.json --candidate cand.json \
//!     --baseline-effort medium --candidate-effort high [--promote]
//! ```
//!
//! It scores each arm from the per-trial rewards, and promotes the candidate
//! **only** when it confidently beats the baseline (see
//! [`stella_core::self_tuning`]). Without `--promote` it reports and records the
//! A/B receipt but changes nothing; with `--promote` it writes the winning
//! effort to settings and records a reversible rollback. `stella tune rollback`
//! reverts the last promotion; `stella tune status` shows the ledger.
//!
//! Offline: reads two result files and the local ledger, writes settings only
//! on `--promote`. No provider, no API key.

use std::path::{Path, PathBuf};

use clap::{Subcommand, ValueEnum};
use colored::Colorize;
use stella_core::self_tuning::{Decision, KeepReason, RewardWeights, SelectionConfig};

use crate::memory::self_tuning::{
    self as engine, BenchTrial, ExperimentReport, LedgerEntry, RollbackOutcome, SettingsScope,
};

/// Settings scope for `--scope`.
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum ScopeArg {
    Project,
    User,
}

impl From<ScopeArg> for SettingsScope {
    fn from(s: ScopeArg) -> Self {
        match s {
            ScopeArg::Project => SettingsScope::Project,
            ScopeArg::User => SettingsScope::User,
        }
    }
}

/// Subcommands under `stella tune`.
#[derive(Subcommand, Clone)]
pub enum TuneCmd {
    /// A/B the worker reasoning-effort knob over two loop-bench `--json` result
    /// files and select the winner. Reports only unless `--promote` is given.
    Effort {
        /// loop-bench `--json` output for the baseline arm.
        #[arg(long)]
        baseline: PathBuf,
        /// loop-bench `--json` output for the candidate arm.
        #[arg(long)]
        candidate: PathBuf,
        /// The worker effort the baseline arm ran at.
        #[arg(long, default_value = "medium")]
        baseline_effort: String,
        /// The worker effort the candidate arm ran at.
        #[arg(long, default_value = "high")]
        candidate_effort: String,
        /// Apply the winner to settings (default: report only).
        #[arg(long)]
        promote: bool,
        /// Which settings scope to write on `--promote`.
        #[arg(long, value_enum, default_value = "project")]
        scope: ScopeArg,
        /// Minimum trials required in every arm before any promotion.
        #[arg(long, default_value_t = 5)]
        min_samples: usize,
    },
    /// Revert the last effort promotion, restoring the prior worker effort.
    Rollback,
    /// Show the self-tuning ledger — experiments run and the promotion in effect.
    Status,
}

/// Dispatch a `stella tune` subcommand. Offline; workspace root is the CWD,
/// matching the other local-only commands (`proposals`, `stats`).
pub fn run_tune(cmd: &TuneCmd) -> Result<(), String> {
    let workspace_root = std::env::current_dir().map_err(|e| e.to_string())?;
    match cmd {
        TuneCmd::Effort {
            baseline,
            candidate,
            baseline_effort,
            candidate_effort,
            promote,
            scope,
            min_samples,
        } => run_effort(
            &workspace_root,
            baseline,
            candidate,
            baseline_effort,
            candidate_effort,
            *promote,
            (*scope).into(),
            *min_samples,
        ),
        TuneCmd::Rollback => run_rollback(&workspace_root),
        TuneCmd::Status => run_status(&workspace_root),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_effort(
    workspace_root: &Path,
    baseline_path: &Path,
    candidate_path: &Path,
    baseline_effort: &str,
    candidate_effort: &str,
    promote: bool,
    scope: SettingsScope,
    min_samples: usize,
) -> Result<(), String> {
    // Validate the effort labels up front so a typo fails before any I/O.
    engine::parse_effort(baseline_effort)?;
    engine::parse_effort(candidate_effort)?;

    let baseline_trials = read_trials(baseline_path)?;
    let candidate_trials = read_trials(candidate_path)?;

    let config = SelectionConfig {
        min_samples_per_arm: min_samples,
        ..SelectionConfig::default()
    };
    let report = engine::run_experiment(
        &baseline_trials,
        baseline_effort,
        &candidate_trials,
        candidate_effort,
        &RewardWeights::default(),
        &config,
    );

    print_report(&report);
    // Always publish the receipt — a kept baseline is evidence too.
    engine::publish_experiment(workspace_root, &report)?;

    match &report.decision {
        Decision::Promote(p) => {
            if promote {
                let chosen = engine::parse_effort(&p.winner.value)?;
                let prior = engine::promote(workspace_root, scope, chosen, p)?;
                println!(
                    "\n{} worker effort → {} (was {}) in {} settings.",
                    "promoted".green().bold(),
                    p.winner.value.bold(),
                    prior.effort.as_deref().unwrap_or("unset/auto"),
                    scope_label(scope),
                );
                if prior.effort_auto == Some(true) {
                    println!(
                        "  note: disabled effort_auto so the pinned worker effort binds \
                         (restored on rollback)."
                    );
                }
                println!("  revert with: {}", "stella tune rollback".cyan());
            } else {
                println!(
                    "\n{}: candidate `{}` beats baseline `{}`. Re-run with {} to apply.",
                    "winner".green().bold(),
                    p.winner.value.bold(),
                    p.baseline.value,
                    "--promote".cyan(),
                );
            }
        }
        Decision::Keep(reason) => {
            println!(
                "\n{}: {}",
                "kept baseline".yellow().bold(),
                describe_keep(reason)
            );
        }
    }
    Ok(())
}

fn run_rollback(workspace_root: &Path) -> Result<(), String> {
    match engine::rollback(workspace_root)? {
        RollbackOutcome::Nothing => {
            println!("nothing to roll back — no effort promotion is in effect.");
        }
        RollbackOutcome::Restored {
            knob,
            restored,
            scope,
        } => {
            println!(
                "{} {} → {} in {} settings.",
                "rolled back".green().bold(),
                knob,
                restored.as_deref().unwrap_or("unset/auto"),
                scope_label(scope),
            );
        }
    }
    Ok(())
}

fn run_status(workspace_root: &Path) -> Result<(), String> {
    let entries = engine::read_ledger(workspace_root);
    if entries.is_empty() {
        println!("no self-tuning history yet. Run `stella tune effort --help`.");
        return Ok(());
    }
    let experiments = entries
        .iter()
        .filter(|e| matches!(e, LedgerEntry::Experiment { .. }))
        .count();
    let promotions = entries
        .iter()
        .filter(|e| matches!(e, LedgerEntry::Promotion { .. }))
        .count();
    let rollbacks = entries
        .iter()
        .filter(|e| matches!(e, LedgerEntry::Rolledback { .. }))
        .count();
    println!(
        "self-tuning ledger: {experiments} experiment(s), {promotions} promotion(s), \
         {rollbacks} rollback(s)."
    );

    // The promotion currently in effect, if any.
    let active = entries.iter().rev().find_map(|e| match e {
        LedgerEntry::Promotion { rollback, .. } => Some(rollback),
        LedgerEntry::Rolledback { .. } => None,
        _ => None,
    });
    match active {
        Some(r) => println!(
            "in effect: {} = {} (was {}), lift {:+.3}.",
            r.knob,
            r.promoted_value.bold(),
            r.previous_value.as_deref().unwrap_or("unset/auto"),
            r.lift,
        ),
        None => println!("in effect: none (baseline)."),
    }
    Ok(())
}

/// Read a loop-bench `--json` result file into trials.
///
/// Two shapes are accepted. Since #1299 a single run emits
/// `{"trials": [...], "tally": {...}, "reconciliation": {...}}`; before that it
/// was the bare array. Reading an artifact from an older run is an ordinary
/// thing to do here — the whole point of the file is to be trended — so the
/// older shape stays readable rather than failing with a parse error that
/// sounds like the file is corrupt.
fn read_trials(path: &Path) -> Result<Vec<BenchTrial>, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let report = engine::parse_bench_report(&contents)
        .map_err(|problem| format!("cannot parse {}: {problem}", path.display()))?;
    if report.tally.crashed > 0 {
        eprintln!(
            "note: {} of {} trials in {} did not finish — harbor recorded a crash. \
             They count as failures below, which reads an infrastructure outcome as \
             this knob's fault; re-run those trials before promoting on them.",
            report.tally.crashed,
            report.tally.total,
            path.display()
        );
    }
    Ok(report.trials)
}

fn print_report(report: &ExperimentReport) {
    println!("A/B: {}", report.knob);
    println!(
        "  baseline {:<6} n={:<3} mean={:.3} ± {:.3}",
        report.baseline.value, report.baseline.n, report.baseline.mean, report.baseline.std_error,
    );
    println!(
        "  candidate {:<5} n={:<3} mean={:.3} ± {:.3}",
        report.candidate.value,
        report.candidate.n,
        report.candidate.mean,
        report.candidate.std_error,
    );
}

fn describe_keep(reason: &KeepReason) -> String {
    match reason {
        KeepReason::InsufficientSamples { arm, n, required } => {
            format!("arm `{arm}` has only {n} trials; {required} required per arm")
        }
        KeepReason::MissingBaseline { baseline_id } => {
            format!("no baseline arm `{baseline_id}` in the results")
        }
        KeepReason::BaselineIsBest { baseline_mean } => {
            format!("baseline already leads (mean {baseline_mean:.3})")
        }
        KeepReason::BelowMinEffect { lift, min_effect } => {
            format!("lift {lift:+.3} is below the min effect {min_effect:.3}")
        }
        KeepReason::LiftNotConfident {
            lift,
            observed_z,
            threshold_z,
        } => format!("lift {lift:+.3} is not confident (z={observed_z:.2}, need {threshold_z:.2})"),
    }
}

fn scope_label(scope: SettingsScope) -> &'static str {
    match scope {
        SettingsScope::Project => "project",
        SettingsScope::User => "user",
    }
}
