//! Unit tests for the `--compare` fold: what a distilled trial contributes to
//! an arm, and what the comparison makes of it.

use stella_core::comparison::{ComparisonConfig, ComparisonVerdict, Metric, compare};

use super::*;

/// A trial that reached the verifier with a known reward and spend.
fn report(task: &str, reward: f64, spend_usd: f64, tokens: u64, calls: u32) -> TrialReport {
    TrialReport {
        task: task.to_string(),
        reward: Some(reward),
        model_calls: calls,
        tool_calls: 3,
        spend_usd,
        tokens,
        parsed_lines: 10,
        terminal_event: true,
        ..Default::default()
    }
}

fn config(baseline: &str) -> ComparisonConfig {
    ComparisonConfig::new(baseline, Metric::PassRate).with_guards(default_guards())
}

/// The #876 acceptance shape, end to end from distilled trials: two configs
/// with known outcomes produce the expected winner.
#[test]
fn a_fixture_with_two_configs_produces_the_expected_winner() {
    let base: Vec<TrialReport> = (0..6)
        .map(|_| report("fix-git", 0.0, 0.20, 8_000, 9))
        .collect();
    let candidate: Vec<TrialReport> = (0..6)
        .map(|_| report("fix-git", 1.0, 0.10, 4_000, 4))
        .collect();
    let arms = vec![
        arm_trials("model-a", "model-a", &base),
        arm_trials("model-b", "model-b", &candidate),
    ];

    let report = compare(&arms, &config("model-a"));
    assert_eq!(report.verdict.winner(), Some("model-b"));
    let a = report.arm("model-a").expect("baseline arm");
    let b = report.arm("model-b").expect("candidate arm");
    assert_eq!(a.pass_rate, 0.0);
    assert_eq!(b.pass_rate, 1.0);
    assert_eq!(a.total_tokens, 48_000);
    assert_eq!(b.total_tokens, 24_000);
    assert!((b.median_turns - 4.0).abs() < 1e-9);
}

/// A candidate that wins on pass rate while spending more is blocked by the
/// harness's default guard set, and the report names the metric.
#[test]
fn the_default_guards_block_a_win_bought_with_spend() {
    let base: Vec<TrialReport> = (0..6).map(|_| report("t", 0.0, 0.05, 1_000, 4)).collect();
    let candidate: Vec<TrialReport> = (0..6).map(|_| report("t", 1.0, 0.50, 1_000, 4)).collect();
    let arms = vec![
        arm_trials("cheap", "cheap", &base),
        arm_trials("dear", "dear", &candidate),
    ];

    let report = compare(&arms, &config("cheap"));
    let ComparisonVerdict::GuardBlocked(evidence) = &report.verdict else {
        panic!("expected a guard block, got {:?}", report.verdict);
    };
    let regressed: Vec<Metric> = evidence.regressions().iter().map(|g| g.metric).collect();
    assert_eq!(regressed, vec![Metric::CostUsd]);
    // The other two guards ran and are on the record.
    assert_eq!(evidence.guards.len(), 3);
}

/// A failing trial is folded in as a failure. Dropping it is the exact
/// mechanism by which an A/B reports a regression as an improvement.
#[test]
fn a_broken_loop_counts_as_a_failure_not_as_missing_data() {
    let silent = TrialReport {
        task: "t".to_string(),
        reward: None,
        zero_work: true,
        parsed_lines: 4,
        ..Default::default()
    };
    assert!(silent.loop_broken(), "the fixture is a broken-loop trial");

    let arm = arm_trials("candidate", "candidate", &[silent]);
    assert_eq!(arm.trials.len(), 1);
    assert!(!arm.trials[0].outcome.succeeded);
}

/// `NOT-RUN` is the one row that is dropped: harbor never launched it, so it
/// is an absence of observation rather than an observation of failure.
#[test]
fn a_not_run_trial_contributes_nothing() {
    let never_launched = TrialReport {
        task: "t".to_string(),
        not_run: true,
        ..Default::default()
    };
    let ran = report("t", 1.0, 0.1, 100, 2);
    let arm = arm_trials("a", "a", &[never_launched, ran]);
    assert_eq!(arm.trials.len(), 1, "only the trial that ran is folded");
    assert!(arm.trials[0].outcome.succeeded);
}

/// A config whose every trial was NOT-RUN pairs to nothing, so the comparison
/// excludes the task from *both* arms and names it — never crediting the arm
/// that did run with a win over an arm that never started.
#[test]
fn a_task_only_one_arm_ran_is_reported_as_unpaired() {
    let base: Vec<TrialReport> = (0..6).map(|_| report("shared", 1.0, 0.1, 100, 2)).collect();
    let mut candidate = base.clone();
    candidate.push(TrialReport {
        task: "extra".to_string(),
        not_run: true,
        ..Default::default()
    });
    let arms = vec![
        arm_trials("a", "a", &base),
        arm_trials("b", "b", &candidate),
    ];
    let report = compare(&arms, &config("a"));
    assert!(
        report.unpaired_tasks.is_empty(),
        "a dropped NOT-RUN leaves no task behind"
    );
    assert_eq!(report.tasks.len(), 1);
}

/// Tokens and retries are distilled off `step_usage`, so the comparison's
/// token guard has something to read.
#[test]
fn token_and_retry_counts_come_off_the_event_stream() {
    let raw = r#"{"type":"step_usage","cost_usd":0.25,"input_tokens":120,"output_tokens":30,"retries":2}
{"type":"step_usage","cost_usd":0.25,"input_tokens":80,"output_tokens":20,"retries":0}
{"type":"tool_start","call":{"name":"write_file"}}
{"type":"complete"}"#;
    let distilled = crate::distill_events("t", raw);
    assert_eq!(distilled.tokens, 250);
    assert_eq!(distilled.retries, 2);
    assert_eq!(distilled.model_calls, 2);
    assert!((distilled.spend_usd - 0.50).abs() < 1e-9);
}

/// A stream with no usage fields reports zero rather than guessing — the
/// comparison's own sample floor is what refuses to promote on nothing.
#[test]
fn a_stream_without_usage_fields_reports_zero_tokens() {
    let distilled = crate::distill_events("t", r#"{"type":"step_usage"}"#);
    assert_eq!(distilled.tokens, 0);
    assert_eq!(distilled.retries, 0);
    assert_eq!(distilled.model_calls, 1);
}
