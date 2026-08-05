//! `--compare` — the A/B half of the harness: the same task set under two or
//! more model configurations, folded into one
//! [`stella_core::comparison::ComparisonReport`].
//!
//! # Why the report shape is not defined here
//!
//! #876 exists because a promotion gate — an adapter (#836), a tuned knob
//! (#831), an eval-gated skill (#1067) — needs a measurement instrument, and
//! the offline instrument (this) and the live one (shadow tuning, #1065) must
//! answer in the same words. So the aggregates, the guard set, the significance
//! test and the verdict vocabulary all live in `stella-core::comparison`, and
//! this module does exactly two things a bench harness should: turn a
//! [`TrialReport`] into the comparison's input vocabulary, and render the
//! result for a human.
//!
//! # What a trial contributes
//!
//! Every distilled trial becomes one [`TrialRecord`], including the ones the
//! loop gate calls broken. A candidate that silently dies on the two hardest
//! tasks has *failed* them, and dropping those rows is precisely how an A/B
//! reports a regression as an improvement. The one exception is `NOT-RUN`,
//! which is not a result at all — see [`arm_trials`].

use stella_core::comparison::{
    ArmTrials, ComparisonReport, ComparisonVerdict, Guard, Leader, LiftEvidence, Metric,
    TrialRecord,
};
use stella_core::self_tuning::{KeepReason, TaskOutcome};

use crate::{TrialReport, truncate};

/// The guard set every `--compare` run applies: a model that wins on pass rate
/// while spending more, reading more, or looping longer has bought the win,
/// and the operator should have to say so out loud.
///
/// Tolerances are deliberately zero. This is a harness, not a policy: it
/// reports the refusal, and the human deciding whether to promote is the one
/// who gets to accept a price.
pub fn default_guards() -> Vec<Guard> {
    vec![
        Guard::strict(Metric::CostUsd),
        Guard::strict(Metric::Tokens),
        Guard::strict(Metric::Turns),
    ]
}

/// Turn one config's distilled trials into an arm of the comparison.
///
/// `NOT-RUN` rows are dropped, and they are the only ones that are. A trial
/// harbor never launched produced no observation — folding it in as a failure
/// would attribute an infrastructure outcome to the model, and folding it in
/// as a pass is obviously worse. Dropping it makes the task unpaired for this
/// arm, so [`stella_core::comparison::compare`] excludes it from *both* arms
/// and names it in `unpaired_tasks`, which is the honest result: nothing was
/// measured about that task.
pub fn arm_trials(id: &str, value: &str, reports: &[TrialReport]) -> ArmTrials {
    ArmTrials {
        id: id.to_string(),
        value: value.to_string(),
        trials: reports
            .iter()
            .filter(|r| !r.not_run)
            .map(|r| TrialRecord {
                task: r.task.clone(),
                outcome: TaskOutcome {
                    succeeded: r.passed(),
                    cost_usd: r.spend_usd,
                    tokens: r.tokens,
                    retries: r.retries,
                },
                turns: r.model_calls,
            })
            .collect(),
    }
}

/// Width of the rendered comparison table: 24 (task) + one 13-wide cell per
/// arm + 12 for the leader column, plus separators. Sized for the two-arm case
/// the flag is named for; a wider comparison simply wraps, which is preferable
/// to a table that silently truncates an arm out of existence.
const TABLE_WIDTH: usize = 24 + 13 * 2 + 12 + 4;

/// Render the comparison for a human: the per-task rows, the per-arm
/// aggregates, then the verdict with its evidence.
pub fn print_comparison(report: &ComparisonReport) {
    let arms: Vec<&str> = report.arms.iter().map(|a| a.id.as_str()).collect();

    print!("\n{:<24}", "task");
    for arm in &arms {
        print!(" {:>12}", truncate(arm, 12));
    }
    println!("  leader");
    println!("{}", "─".repeat(TABLE_WIDTH));
    for row in &report.tasks {
        print!("{:<24}", truncate(&row.task, 24));
        for result in &row.arms {
            print!(" {:>12}", format!("{}/{}", result.solved, result.trials));
        }
        println!("  {}", leader_label(&row.leader));
    }
    println!("{}", "─".repeat(TABLE_WIDTH));

    for arm in &report.arms {
        println!(
            "{:<24} pass {:>5.1}%  ${:>7.2}  {:>9} tok  {:>5.1} turns  n={}",
            truncate(&format!("{} ({})", arm.id, arm.value), 24),
            arm.pass_rate * 100.0,
            arm.total_cost_usd,
            arm.total_tokens,
            arm.median_turns,
            arm.trials,
        );
        if arm.trials_excluded > 0 {
            println!(
                "{:>26}└ {} trial(s) excluded: their task did not run under every arm",
                "", arm.trials_excluded
            );
        }
    }
    if !report.unpaired_tasks.is_empty() {
        println!(
            "\n  unpaired (excluded from every figure): {}",
            report.unpaired_tasks.join(", ")
        );
    }
    print_verdict(report);
}

fn leader_label(leader: &Leader) -> String {
    match leader {
        Leader::Arm { id } => truncate(id, 12),
        Leader::Tie => "tie".to_string(),
        Leader::Unpaired => "—".to_string(),
    }
}

/// The verdict block: who won on what, and every guard's finding. A refusal
/// prints its reason — a comparison that ends in silence is a comparison the
/// operator has to re-derive by hand.
fn print_verdict(report: &ComparisonReport) {
    let primary = report.config.primary.as_str();
    println!();
    match &report.verdict {
        ComparisonVerdict::Winner(evidence) => {
            println!(
                "WINNER: {} — {primary} lift {:+.3} over `{}` (z {}, n {}/{})",
                evidence.arm,
                evidence.promotion.lift,
                evidence.promotion.baseline.id,
                evidence
                    .promotion
                    .observed_z
                    .map(|z| format!("{z:.2}"))
                    .unwrap_or_else(|| "∞".into()),
                evidence.promotion.winner.n,
                evidence.promotion.baseline.n,
            );
            print_guards(evidence);
        }
        ComparisonVerdict::GuardBlocked(evidence) => {
            println!(
                "BLOCKED: {} led {primary} by {:+.3}, but a guard metric regressed.",
                evidence.arm, evidence.promotion.lift
            );
            print_guards(evidence);
            println!("  ⚠ the lift is real; the price is not one this harness will call a win.");
        }
        ComparisonVerdict::NoWinner { reason } => {
            println!("NO WINNER on {primary}: {}", keep_reason_label(reason));
        }
    }
}

fn print_guards(evidence: &LiftEvidence) {
    for guard in &evidence.guards {
        println!(
            "  {} {:<9} {:>10.4} → {:>10.4}  ({:+.4} vs tolerance {:.4})",
            if guard.regressed { "✗" } else { "✓" },
            guard.metric.as_str(),
            guard.baseline,
            guard.candidate,
            guard.delta,
            guard.tolerance,
        );
    }
}

/// A one-line rendering of the selector's refusal. Kept exhaustive rather than
/// falling back to `{:?}`, so a new reason has to be given words here before it
/// can reach an operator as a debug dump.
fn keep_reason_label(reason: &KeepReason) -> String {
    match reason {
        KeepReason::InsufficientSamples { arm, n, required } => {
            format!("arm `{arm}` has {n} trial(s), {required} required — raise --trials")
        }
        KeepReason::MissingBaseline { baseline_id } => {
            format!("no arm is named `{baseline_id}`")
        }
        KeepReason::BaselineIsBest { baseline_mean } => {
            format!("the baseline already leads (mean {baseline_mean:.3})")
        }
        KeepReason::BelowMinEffect { lift, min_effect } => {
            format!("lift {lift:+.3} is under the {min_effect:.3} minimum effect")
        }
        KeepReason::LiftNotConfident {
            lift,
            observed_z,
            threshold_z,
        } => format!("lift {lift:+.3} is not confident (z {observed_z:.2} < {threshold_z:.2})"),
    }
}

#[cfg(test)]
mod tests;
