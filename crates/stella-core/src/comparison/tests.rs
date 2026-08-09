//! Unit tests for the A/B comparison fold.

use super::*;
use crate::self_tuning::{KeepReason, RewardWeights, SelectionConfig, TaskOutcome};

/// A trial with the ingredients a test cares about and defaults for the rest.
fn trial(task: &str, succeeded: bool) -> TrialRecord {
    TrialRecord {
        task: task.to_string(),
        outcome: TaskOutcome {
            succeeded,
            cost_usd: 0.10,
            tokens: 1_000,
            retries: 0,
        },
        turns: 4,
        features: FeatureCounts::new(),
    }
}

fn priced(task: &str, succeeded: bool, cost_usd: f64, tokens: u64, turns: u32) -> TrialRecord {
    TrialRecord {
        task: task.to_string(),
        outcome: TaskOutcome {
            succeeded,
            cost_usd,
            tokens,
            retries: 0,
        },
        turns,
        features: FeatureCounts::new(),
    }
}

fn arm(id: &str, value: &str, trials: Vec<TrialRecord>) -> ArmTrials {
    ArmTrials {
        id: id.to_string(),
        value: value.to_string(),
        trials,
    }
}

/// Six trials each: enough to clear the default `min_samples_per_arm` of 5.
fn repeat(task: &str, succeeded: bool, n: usize) -> Vec<TrialRecord> {
    (0..n).map(|_| trial(task, succeeded)).collect()
}

/// The acceptance shape of #876: two configs over one task set produce
/// per-config pass rate, cost, tokens and turns, and a clear winner.
#[test]
fn two_configs_over_one_task_set_produce_aggregates_and_a_winner() {
    let arms = vec![
        arm(
            "base",
            "glm-4.7-flash",
            vec![
                priced("fix-git", false, 0.20, 5_000, 9),
                priced("fix-git", false, 0.20, 5_000, 9),
                priced("fix-git", false, 0.20, 5_000, 9),
                priced("prove-plus-comm", false, 0.20, 5_000, 11),
                priced("prove-plus-comm", false, 0.20, 5_000, 11),
                priced("prove-plus-comm", false, 0.20, 5_000, 11),
            ],
        ),
        arm(
            "candidate",
            "glm-5.2",
            vec![
                priced("fix-git", true, 0.10, 2_000, 3),
                priced("fix-git", true, 0.10, 2_000, 3),
                priced("fix-git", true, 0.10, 2_000, 3),
                priced("prove-plus-comm", true, 0.10, 2_000, 5),
                priced("prove-plus-comm", true, 0.10, 2_000, 5),
                priced("prove-plus-comm", true, 0.10, 2_000, 5),
            ],
        ),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));

    let base = report.arm("base").expect("base summary");
    assert_eq!(base.trials, 6);
    assert_eq!(base.tasks, 2);
    assert_eq!(base.solved, 0);
    assert_eq!(base.pass_rate, 0.0);
    assert!((base.total_cost_usd - 1.20).abs() < 1e-9);
    assert_eq!(base.total_tokens, 30_000);
    // Three 9s and three 11s: the median of the even set is the midpoint.
    assert!((base.median_turns - 10.0).abs() < 1e-9);

    let candidate = report.arm("candidate").expect("candidate summary");
    assert_eq!(candidate.pass_rate, 1.0);
    assert!((candidate.median_turns - 4.0).abs() < 1e-9);

    assert_eq!(report.verdict.winner(), Some("candidate"));
    assert!(report.unpaired_tasks.is_empty());
    assert_eq!(report.tasks.len(), 2);
    // Rows are sorted by task name, not by input order.
    assert_eq!(report.tasks[0].task, "fix-git");
    assert_eq!(report.tasks[1].task, "prove-plus-comm");
    assert_eq!(
        report.tasks[0].leader,
        Leader::Arm {
            id: "candidate".into()
        }
    );
}

/// A fixture with two configs and known outcomes produces the expected winner
/// — the #876 acceptance test, stated as its own case.
#[test]
fn the_expected_config_wins_a_known_fixture() {
    let arms = vec![
        arm("a", "model-a", repeat("fix-git", false, 6)),
        arm("b", "model-b", repeat("fix-git", true, 6)),
    ];
    let report = compare(&arms, &ComparisonConfig::new("a", Metric::PassRate));
    assert!(report.verdict.is_winner());
    assert_eq!(report.verdict.winner(), Some("b"));
}

/// The same store and filter produce byte-identical output — reproducibility
/// is a property of the fold, not of the harness that feeds it.
#[test]
fn the_report_is_byte_identical_across_runs() {
    let arms = vec![
        arm("base", "medium", repeat("fix-git", false, 6)),
        arm("candidate", "high", repeat("fix-git", true, 6)),
    ];
    let config = ComparisonConfig::new("base", Metric::PassRate);
    let first = serde_json::to_string(&compare(&arms, &config)).unwrap();
    let second = serde_json::to_string(&compare(&arms, &config)).unwrap();
    assert_eq!(first, second);
}

/// Invariant #4: the whole report round-trips through `serde_json`.
#[test]
fn the_report_round_trips() {
    let arms = vec![
        arm("base", "medium", repeat("fix-git", false, 6)),
        arm("candidate", "high", repeat("fix-git", true, 6)),
    ];
    let report = compare(
        &arms,
        &ComparisonConfig::new("base", Metric::PassRate)
            .with_guards(vec![Guard::strict(Metric::CostUsd)]),
    );
    let json = serde_json::to_string(&report).unwrap();
    let back: ComparisonReport = serde_json::from_str(&json).unwrap();
    assert_eq!(report, back);
    // The thresholds travel with the verdict.
    assert!(json.contains("\"confidence_z\""));
    assert!(json.contains("\"verdict\":\"winner\""));
}

/// A task only one arm ran is excluded from every figure and named — the
/// shrunken-denominator lie the pairing rule exists to prevent.
#[test]
fn an_unpaired_task_is_excluded_and_named() {
    let mut base_trials = repeat("shared", true, 6);
    base_trials.extend(repeat("base-only", false, 3));
    let arms = vec![
        arm("base", "medium", base_trials),
        arm("candidate", "high", repeat("shared", true, 6)),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));

    assert_eq!(report.unpaired_tasks, vec!["base-only".to_string()]);
    let base = report.arm("base").expect("base summary");
    assert_eq!(base.trials, 6, "only the shared task's trials are folded");
    assert_eq!(base.trials_excluded, 3);
    assert_eq!(
        base.pass_rate, 1.0,
        "the three failures on the unpaired task must not count against it"
    );
    assert_eq!(report.tasks.len(), 1);
    assert_eq!(report.tasks[0].task, "shared");
}

/// A candidate that wins the primary metric while regressing a guard is
/// refused, and the refusal names the guard — #1065's acceptance shape.
#[test]
fn a_guard_regression_blocks_a_real_lift() {
    let base: Vec<TrialRecord> = (0..6).map(|_| priced("t", false, 0.10, 1_000, 4)).collect();
    // Solves everything, at four times the price.
    let candidate: Vec<TrialRecord> = (0..6).map(|_| priced("t", true, 0.40, 1_000, 4)).collect();
    let arms = vec![
        arm("base", "cheap", base),
        arm("candidate", "dear", candidate),
    ];

    let config = ComparisonConfig::new("base", Metric::PassRate)
        .with_guards(vec![Guard::strict(Metric::CostUsd)]);
    let report = compare(&arms, &config);

    let ComparisonVerdict::GuardBlocked(evidence) = &report.verdict else {
        panic!("expected a guard block, got {:?}", report.verdict);
    };
    assert_eq!(evidence.arm, "candidate");
    assert!(
        (evidence.promotion.lift - 1.0).abs() < 1e-9,
        "the lift is real"
    );
    let regressions = evidence.regressions();
    assert_eq!(regressions.len(), 1);
    assert_eq!(regressions[0].metric, Metric::CostUsd);
    assert!((regressions[0].baseline - 0.10).abs() < 1e-9);
    assert!((regressions[0].candidate - 0.40).abs() < 1e-9);
    assert!(
        (regressions[0].delta + 0.30).abs() < 1e-9,
        "cost rose $0.30"
    );
    assert_eq!(
        report.verdict.winner(),
        None,
        "a guard-blocked lift is not promotable"
    );
}

/// The same lift with a tolerance that admits the price promotes, and every
/// guard is still reported — a promotion record must prove the checks ran.
#[test]
fn a_tolerated_regression_still_promotes_and_reports_every_guard() {
    let base: Vec<TrialRecord> = (0..6).map(|_| priced("t", false, 0.10, 1_000, 4)).collect();
    let candidate: Vec<TrialRecord> = (0..6).map(|_| priced("t", true, 0.40, 1_000, 4)).collect();
    let arms = vec![
        arm("base", "cheap", base),
        arm("candidate", "dear", candidate),
    ];

    let config = ComparisonConfig::new("base", Metric::PassRate).with_guards(vec![
        Guard {
            metric: Metric::CostUsd,
            tolerance: 0.50,
        },
        Guard::strict(Metric::Turns),
    ]);
    let report = compare(&arms, &config);

    let ComparisonVerdict::Winner(evidence) = &report.verdict else {
        panic!("expected a winner, got {:?}", report.verdict);
    };
    assert_eq!(evidence.guards.len(), 2, "both guards are on the record");
    assert!(evidence.regressions().is_empty());
    assert_eq!(evidence.guards[1].metric, Metric::Turns);
    assert_eq!(evidence.guards[1].delta, 0.0, "turns held exactly");
}

/// A lower-is-better primary works through the same engine: a candidate that
/// halves the cost at an unchanged pass rate wins a `CostUsd` comparison.
#[test]
fn a_lower_is_better_primary_selects_the_cheaper_arm() {
    let base: Vec<TrialRecord> = (0..6).map(|_| priced("t", true, 0.40, 1_000, 4)).collect();
    let candidate: Vec<TrialRecord> = (0..6).map(|_| priced("t", true, 0.10, 1_000, 4)).collect();
    let arms = vec![
        arm("base", "dear", base),
        arm("candidate", "cheap", candidate),
    ];

    let config = ComparisonConfig::new("base", Metric::CostUsd)
        .with_guards(vec![Guard::strict(Metric::PassRate)]);
    let report = compare(&arms, &config);

    assert_eq!(report.verdict.winner(), Some("candidate"));
    let ComparisonVerdict::Winner(evidence) = &report.verdict else {
        unreachable!()
    };
    // The lift is stated in the oriented score: −0.10 beats −0.40 by 0.30.
    assert!((evidence.promotion.lift - 0.30).abs() < 1e-9);
    // The report shows the metric in its own units, not the negated score.
    assert!((Metric::CostUsd.report_value(report.arm("candidate").unwrap()) - 0.10).abs() < 1e-9);
}

/// Under-sampled arms are never promoted, and the report says which arm was
/// short — the evidence floor is `select_winner`'s, not a second copy.
#[test]
fn too_few_trials_keep_the_baseline() {
    let arms = vec![
        arm("base", "medium", repeat("t", false, 2)),
        arm("candidate", "high", repeat("t", true, 2)),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));
    match &report.verdict {
        ComparisonVerdict::NoWinner {
            reason: KeepReason::InsufficientSamples { n, required, .. },
        } => {
            assert_eq!(*n, 2);
            assert_eq!(*required, 5);
        }
        other => panic!("expected InsufficientSamples, got {other:?}"),
    }
}

/// Arms that share no task at all pair to nothing. The report is empty rather
/// than wrong, and every task is named as unpaired.
#[test]
fn arms_sharing_no_task_pair_to_nothing() {
    let arms = vec![
        arm("base", "medium", repeat("alpha", true, 6)),
        arm("candidate", "high", repeat("beta", true, 6)),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));
    assert!(report.tasks.is_empty());
    assert_eq!(report.unpaired_tasks, vec!["alpha", "beta"]);
    assert_eq!(report.arm("base").unwrap().trials, 0);
    assert!(!report.verdict.is_winner());
}

/// No arms, and a baseline that is not among the arms, both resolve to a
/// stated refusal rather than a panic.
#[test]
fn a_degenerate_comparison_is_total() {
    let empty = compare(&[], &ComparisonConfig::new("base", Metric::PassRate));
    assert!(matches!(
        empty.verdict,
        ComparisonVerdict::NoWinner {
            reason: KeepReason::MissingBaseline { .. }
        }
    ));
    assert!(empty.arms.is_empty());

    let arms = vec![arm("only", "x", repeat("t", true, 6))];
    let missing = compare(&arms, &ComparisonConfig::new("absent", Metric::PassRate));
    assert!(matches!(
        missing.verdict,
        ComparisonVerdict::NoWinner {
            reason: KeepReason::MissingBaseline { .. }
        }
    ));
}

/// A guard metric that cannot be computed fails closed. Nothing in the fold
/// produces a `NaN` today, so the check is made directly against `evaluate`.
#[test]
fn an_uncomputable_guard_fails_closed() {
    let base = ArmSummary::fold("base", "b", &[], 0, &RewardWeights::default());
    let mut candidate = ArmSummary::fold("candidate", "c", &[], 0, &RewardWeights::default());
    candidate.mean_cost_usd = f64::NAN;
    let finding = Guard::strict(Metric::CostUsd).evaluate(&base, &candidate);
    assert!(
        finding.regressed,
        "a NaN delta must read as a regression, never as a pass"
    );
}

/// Reward weights reach both the primary score and every reported figure, so
/// a cost-aware comparison is one config change rather than a second code path.
#[test]
fn weights_price_cost_into_the_reward_metric() {
    let base: Vec<TrialRecord> = (0..6).map(|_| priced("t", true, 0.10, 1_000, 4)).collect();
    // Solves just as often, at ten times the price.
    let candidate: Vec<TrialRecord> = (0..6).map(|_| priced("t", true, 1.00, 1_000, 4)).collect();
    let arms = vec![
        arm("base", "cheap", base),
        arm("candidate", "dear", candidate),
    ];

    let mut config = ComparisonConfig::new("base", Metric::Reward);
    config.weights = RewardWeights {
        cost_usd: 1.0,
        ..RewardWeights::default()
    };
    let report = compare(&arms, &config);

    assert!(
        !report.verdict.is_winner(),
        "paying more for the same result is not a win"
    );
    let base_reward = report.arm("base").unwrap().mean_reward;
    let candidate_reward = report.arm("candidate").unwrap().mean_reward;
    assert!((base_reward - 0.90).abs() < 1e-9);
    assert!(candidate_reward < base_reward);
}

/// A tie on a task is reported as a tie, not as an arbitrary leader.
#[test]
fn an_evenly_solved_task_is_a_tie() {
    let arms = vec![
        arm("base", "medium", repeat("t", true, 6)),
        arm("candidate", "high", repeat("t", true, 6)),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));
    assert_eq!(report.tasks[0].leader, Leader::Tie);
    // An exact tie is refused for want of confidence rather than for the
    // baseline being best: `select_winner`'s `max_by` names the last maximal
    // arm, so the candidate "leads" by a lift of zero and fails the z test.
    // Both are refusals — what matters is that a tie never promotes.
    assert!(matches!(
        report.verdict,
        ComparisonVerdict::NoWinner {
            reason: KeepReason::LiftNotConfident { .. }
        }
    ));
}

/// More than two arms is the same fold — the engine was never binary.
#[test]
fn three_arms_compare_against_one_baseline() {
    let arms = vec![
        arm("base", "low", repeat("t", false, 6)),
        arm("mid", "medium", repeat("t", false, 6)),
        arm("top", "high", repeat("t", true, 6)),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));
    assert_eq!(report.arms.len(), 3);
    assert_eq!(report.tasks[0].arms.len(), 3);
    assert_eq!(report.verdict.winner(), Some("top"));
}

/// A repeated arm id is a caller bug, not a crash: the first arm carrying the
/// baseline id is the one measured against, and the fold stays total.
#[test]
fn a_duplicated_arm_id_does_not_panic() {
    let arms = vec![
        arm("base", "medium", repeat("t", false, 6)),
        arm("base", "medium", repeat("t", true, 6)),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));
    assert_eq!(report.arms.len(), 2);
    assert!(!report.verdict.is_winner());
}

/// The metric vocabulary's orientation is the one place direction is written
/// down; a guard and a table both read it from here.
#[test]
fn metric_orientation_is_declared_once() {
    assert!(Metric::PassRate.higher_is_better());
    assert!(Metric::Reward.higher_is_better());
    for metric in [
        Metric::CostUsd,
        Metric::Tokens,
        Metric::Turns,
        Metric::Retries,
    ] {
        assert!(!metric.higher_is_better(), "{} is a cost", metric.as_str());
    }
    // The wire token and the display token are the same string.
    let json = serde_json::to_string(&Metric::PassRate).unwrap();
    assert_eq!(json, format!("\"{}\"", Metric::PassRate.as_str()));
}

/// An explicitly configured evidence bar reaches the decision, so a caller
/// that wants a stricter gate gets one.
#[test]
fn a_stricter_selection_config_is_honored() {
    let arms = vec![
        arm("base", "medium", repeat("t", false, 6)),
        arm("candidate", "high", repeat("t", true, 6)),
    ];
    let mut config = ComparisonConfig::new("base", Metric::PassRate);
    config.selection = SelectionConfig {
        min_samples_per_arm: 20,
        ..SelectionConfig::default()
    };
    let report = compare(&arms, &config);
    assert!(!report.verdict.is_winner());
    assert_eq!(report.config.selection.min_samples_per_arm, 20);
}

// ── The per-feature attribution channel (#2382) ─────────────────────────────

/// A trial carrying feature counters.
fn counted(task: &str, succeeded: bool, counts: &[(&str, u64)]) -> TrialRecord {
    TrialRecord {
        features: counts
            .iter()
            .map(|(key, count)| ((*key).to_string(), *count))
            .collect(),
        ..trial(task, succeeded)
    }
}

/// Counters fold over the arm's paired trials, and the mean is per trial —
/// the only comparable figure, since pairing excludes at trial granularity and
/// two arms over the same tasks can still fold different trial counts.
#[test]
fn feature_counters_fold_into_a_total_and_a_per_trial_mean() {
    let arms = vec![
        arm(
            "base",
            "off",
            (0..4).map(|_| counted("t", false, &[])).collect(),
        ),
        arm(
            "candidate",
            "on",
            (0..4)
                .map(|_| counted("t", true, &[("recall.frames", 3)]))
                .collect(),
        ),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));
    let stat = report.arm("candidate").expect("arm").features["recall.frames"];
    assert_eq!(stat.total, 12);
    assert!((stat.mean - 3.0).abs() < 1e-9);
    assert!(
        report.arm("base").expect("arm").features.is_empty(),
        "an arm that counted nothing carries no keys"
    );
}

/// The key set is the **union**. A counter only the candidate emitted is the
/// most interesting row a feature ablation produces, and an intersection would
/// delete exactly that finding.
#[test]
fn the_feature_key_set_is_the_union_of_both_arms() {
    let arms = vec![
        arm(
            "base",
            "off",
            (0..3)
                .map(|_| counted("t", false, &[("tool.grep", 2)]))
                .collect(),
        ),
        arm(
            "candidate",
            "on",
            (0..3)
                .map(|_| counted("t", true, &[("recall.frames", 1)]))
                .collect(),
        ),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));
    assert_eq!(report.feature_keys(), vec!["recall.frames", "tool.grep"]);

    let deltas = report.feature_deltas();
    assert_eq!(deltas.len(), 2, "one per (non-baseline arm, key)");
    let frames = deltas
        .iter()
        .find(|d| d.key == "recall.frames")
        .expect("key");
    assert_eq!(frames.baseline.total, 0, "absent is zero occurrences");
    assert!((frames.delta_mean - 1.0).abs() < 1e-9);
    let grep = deltas.iter().find(|d| d.key == "tool.grep").expect("key");
    assert!((grep.delta_mean + 2.0).abs() < 1e-9);
}

/// Counters obey pairing like every other figure: a trial excluded from the
/// aggregates is excluded from the counters too. A feature delta computed over
/// a wider trial set than the pass rate beside it is not comparable with it.
#[test]
fn an_excluded_trials_counters_are_excluded_too() {
    let mut candidate: Vec<TrialRecord> = (0..3)
        .map(|_| counted("shared", true, &[("recall.frames", 1)]))
        .collect();
    candidate.push(counted("solo", true, &[("recall.frames", 999)]));
    let arms = vec![
        arm(
            "base",
            "off",
            (0..3)
                .map(|_| counted("shared", false, &[("recall.frames", 1)]))
                .collect(),
        ),
        arm("candidate", "on", candidate),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));
    assert_eq!(report.unpaired_tasks, vec!["solo".to_string()]);
    assert_eq!(
        report.arm("candidate").expect("arm").features["recall.frames"].total,
        3
    );
}

/// With no arm named by the config there is nothing to measure against, so no
/// delta is stated. Rebasing onto the first arm would silently answer a
/// different question than the one that was asked.
#[test]
fn a_missing_baseline_yields_no_feature_deltas() {
    let arms = vec![arm(
        "only",
        "only",
        (0..3)
            .map(|_| counted("t", true, &[("tool.grep", 1)]))
            .collect(),
    )];
    let report = compare(&arms, &ComparisonConfig::new("absent", Metric::PassRate));
    assert_eq!(
        report.feature_keys(),
        vec!["tool.grep"],
        "the keys are still visible"
    );
    assert!(report.feature_deltas().is_empty());
}

/// Additive, and checked rather than asserted: a report from a producer that
/// counts nothing serializes exactly as it did before the channel existed, and
/// a report recorded before it existed still deserializes.
#[test]
fn the_feature_channel_is_additive_on_the_wire() {
    let arms = vec![
        arm("base", "off", repeat("t", false, 6)),
        arm("candidate", "on", repeat("t", true, 6)),
    ];
    let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));
    let json = serde_json::to_value(&report).expect("serialize");
    assert!(
        json["arms"][0].get("features").is_none(),
        "an arm that counted nothing adds no key: {}",
        json["arms"][0]
    );
    assert_eq!(
        COMPARISON_SCHEMA_VERSION, 1,
        "the channel is additive, so the schema version does not move"
    );

    // A trial recorded before the field existed round-trips to an empty map.
    let older = serde_json::json!({
        "task": "t",
        "outcome": {"succeeded": true, "cost_usd": 0.1, "tokens": 10, "retries": 0},
        "turns": 3,
    });
    let record: TrialRecord = serde_json::from_value(older).expect("older shape still parses");
    assert!(record.features.is_empty());

    // And the whole report round-trips byte-for-byte (invariant #4).
    let text = serde_json::to_string(&report).expect("serialize");
    let back: ComparisonReport = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(serde_json::to_string(&back).expect("re-serialize"), text);
}
