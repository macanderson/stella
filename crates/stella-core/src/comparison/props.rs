//! Property tests for the comparison fold: it never panics, never invents a
//! winner, and never lets an unpaired task reach a figure.

use proptest::prelude::*;

use super::*;
use crate::self_tuning::TaskOutcome;

/// A small task alphabet so generated arms genuinely overlap — with unique
/// names, every run would pair to nothing and the interesting properties would
/// never be exercised.
fn arb_trial() -> impl Strategy<Value = TrialRecord> {
    (
        prop::sample::select(vec!["alpha", "beta", "gamma"]),
        any::<bool>(),
        0.0f64..5.0,
        0u64..100_000,
        0u32..8,
        0u32..40,
    )
        .prop_map(
            |(task, succeeded, cost_usd, tokens, retries, turns)| TrialRecord {
                task: task.to_string(),
                outcome: TaskOutcome {
                    succeeded,
                    cost_usd,
                    tokens,
                    retries,
                },
                turns,
            },
        )
}

fn arb_arm(id: &'static str) -> impl Strategy<Value = ArmTrials> {
    proptest::collection::vec(arb_trial(), 0..14).prop_map(move |trials| ArmTrials {
        id: id.to_string(),
        value: id.to_string(),
        trials,
    })
}

fn arb_metric() -> impl Strategy<Value = Metric> {
    prop::sample::select(vec![
        Metric::PassRate,
        Metric::Reward,
        Metric::CostUsd,
        Metric::Tokens,
        Metric::Turns,
        Metric::Retries,
    ])
}

proptest! {
    /// The fold is total for any arms, any primary metric, and any guard set.
    #[test]
    fn compare_never_panics(
        base in arb_arm("base"),
        candidate in arb_arm("candidate"),
        primary in arb_metric(),
        guard in arb_metric(),
        tolerance in -1.0f64..1.0,
    ) {
        let config = ComparisonConfig::new("base", primary)
            .with_guards(vec![Guard { metric: guard, tolerance }]);
        let _ = compare(&[base, candidate], &config);
    }

    /// Same input, same report — the reproducibility #876 asks for, asserted
    /// over the serialized bytes rather than over a hand-picked field.
    #[test]
    fn compare_is_deterministic(
        base in arb_arm("base"),
        candidate in arb_arm("candidate"),
        primary in arb_metric(),
    ) {
        let config = ComparisonConfig::new("base", primary);
        let first = compare(&[base.clone(), candidate.clone()], &config);
        let second = compare(&[base, candidate], &config);
        prop_assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
    }

    /// A winner is never the baseline, always cleared the sample floor, and
    /// never carries a regressed guard. The three claims a promotion gate acts
    /// on, checked together.
    #[test]
    fn a_winner_is_well_formed(
        base in arb_arm("base"),
        candidate in arb_arm("candidate"),
        primary in arb_metric(),
    ) {
        let config = ComparisonConfig::new("base", primary)
            .with_guards(vec![Guard::strict(Metric::PassRate), Guard::strict(Metric::CostUsd)]);
        let report = compare(&[base, candidate], &config);
        if let ComparisonVerdict::Winner(evidence) = &report.verdict {
            prop_assert_ne!(evidence.arm.as_str(), "base");
            prop_assert!(evidence.promotion.lift > 0.0);
            prop_assert!(evidence.promotion.winner.n >= config.selection.min_samples_per_arm);
            prop_assert!(evidence.promotion.baseline.n >= config.selection.min_samples_per_arm);
            prop_assert!(evidence.regressions().is_empty());
            prop_assert_eq!(evidence.guards.len(), 2);
        }
    }

    /// Every figure is computed over paired trials only: an arm's folded trial
    /// count plus its excluded count is exactly what it was handed, and no
    /// task appears both paired and unpaired.
    #[test]
    fn pairing_accounts_for_every_trial(
        base in arb_arm("base"),
        candidate in arb_arm("candidate"),
    ) {
        let handed: Vec<usize> = [&base, &candidate].iter().map(|a| a.trials.len()).collect();
        let report = compare(&[base, candidate], &ComparisonConfig::new("base", Metric::PassRate));
        for (summary, handed) in report.arms.iter().zip(handed) {
            prop_assert_eq!(summary.trials + summary.trials_excluded, handed);
        }
        for row in &report.tasks {
            prop_assert!(!report.unpaired_tasks.contains(&row.task));
            // A paired row has every arm present, so it is never `Unpaired`.
            prop_assert_ne!(&row.leader, &Leader::Unpaired);
        }
    }

    /// Aggregates stay inside their own definitions whatever the input: a pass
    /// rate is a fraction, counts never exceed the trials they came from, and
    /// no figure is `NaN`.
    #[test]
    fn aggregates_are_well_formed(
        base in arb_arm("base"),
        candidate in arb_arm("candidate"),
    ) {
        let report = compare(&[base, candidate], &ComparisonConfig::new("base", Metric::PassRate));
        for summary in &report.arms {
            prop_assert!((0.0..=1.0).contains(&summary.pass_rate));
            prop_assert!(summary.solved <= summary.trials);
            prop_assert!(summary.tasks <= summary.trials);
            prop_assert!(summary.median_turns.is_finite());
            prop_assert!(summary.mean_cost_usd.is_finite());
            prop_assert!(summary.mean_reward.is_finite());
        }
    }

    /// A guard's delta always reads positive for an improvement, whichever way
    /// the metric's raw units run — the orientation is the guard's job, never
    /// the caller's.
    #[test]
    fn guard_delta_is_oriented_to_the_metric(
        base in arb_arm("base"),
        candidate in arb_arm("candidate"),
        metric in arb_metric(),
    ) {
        let report = compare(&[base, candidate], &ComparisonConfig::new("base", Metric::PassRate));
        let (Some(b), Some(c)) = (report.arm("base"), report.arm("candidate")) else {
            return Ok(());
        };
        let finding = Guard::strict(metric).evaluate(b, c);
        let raw = finding.candidate - finding.baseline;
        let expected = if metric.higher_is_better() { raw } else { -raw };
        prop_assert!((finding.delta - expected).abs() < 1e-9);
        // A strict guard regresses exactly when the delta is below zero or is
        // incomparable to it.
        let expected_regressed = matches!(
            finding.delta.partial_cmp(&0.0),
            None | Some(std::cmp::Ordering::Less)
        );
        prop_assert_eq!(finding.regressed, expected_regressed);
    }
}
