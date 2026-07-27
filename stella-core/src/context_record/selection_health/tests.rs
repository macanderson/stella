//! Phase 4 deliverable 4: selection health is derived, honest about its
//! denominator, and rebuilds exactly.

use super::*;
use crate::context_record::{
    Confidence, ContextEvaluationMethod, ContextInfluenceStage, ContextOutcomeRelation,
    ContextUseKind,
};

fn a_use(id: &str, record_id: &str, task: &str) -> (String, ContextUse) {
    (
        id.to_string(),
        ContextUse {
            use_kind: ContextUseKind::Rendered,
            context_record_id: record_id.to_string(),
            use_trace_id: format!("ut_{task}"),
            task_id: task.to_string(),
            influence_stage: ContextInfluenceStage::None,
            observed_at: "2026-07-26T00:00:00Z".into(),
        },
    )
}

/// A verdict that clears every attribution bar.
fn a_verdict(use_id: &str, evaluation: ContextUseEvaluation) -> ContextUseFeedback {
    ContextUseFeedback {
        context_use_id: use_id.to_string(),
        use_trace_id: "ut_t".into(),
        task_id: "t".into(),
        evaluation,
        had_opportunity: true,
        influence_stage: ContextInfluenceStage::FinalResponse,
        outcome_relation: ContextOutcomeRelation::Unknown,
        observable_effect_refs: Vec::new(),
        evaluation_method: ContextEvaluationMethod::new(
            ContextEvaluationMethod::EXPLICIT_USER_FEEDBACK,
        )
        .ok(),
        attribution_confidence: Confidence::new(90).expect("in range"),
        outcome_assessment_id: None,
        influence_statement: None,
        observed_at: "2026-07-26T00:00:00Z".into(),
    }
}

/// A permissive policy, so tests exercise the fold rather than the thresholds.
fn policy(min_uses: u32, ratio: f64) -> SelectionHealthPolicy {
    SelectionHealthPolicy {
        min_attributable_uses: min_uses,
        not_helpful_ratio_threshold: ratio,
        min_attribution_confidence: 80,
    }
}

#[test]
fn uses_and_verdicts_fold_into_one_record_standing() {
    let uses = vec![
        a_use("cu_1", "nod_a", "task1"),
        a_use("cu_2", "nod_a", "task2"),
    ];
    let feedback = vec![
        a_verdict("cu_1", ContextUseEvaluation::Helpful),
        a_verdict("cu_2", ContextUseEvaluation::NotHelpful),
    ];

    let health = fold_selection_health(&uses, &feedback, policy(1, 0.8));
    assert_eq!(health.len(), 1);
    assert_eq!(health[0].context_record_id, "nod_a");
    assert_eq!(health[0].uses, 2);
    assert_eq!(health[0].distinct_tasks, 2);
    assert_eq!(health[0].assessed_uses, 2);
    assert_eq!(health[0].helpful, 1);
    assert_eq!(health[0].not_helpful, 1);
    assert!((health[0].not_helpful_ratio - 0.5).abs() < f64::EPSILON);
}

#[test]
fn the_ratio_denominator_is_assessed_uses_not_all_uses() {
    // Rendered ten times, judged twice, both negative. The honest reading is
    // "everything known about this is negative" (1.0), not "it was fine 80% of
    // the time" (0.2). Absence of citation is not evidence either way.
    let mut uses = Vec::new();
    for i in 0..10 {
        uses.push(a_use(&format!("cu_{i}"), "nod_a", &format!("task{i}")));
    }
    let feedback = vec![
        a_verdict("cu_0", ContextUseEvaluation::NotHelpful),
        a_verdict("cu_1", ContextUseEvaluation::NotHelpful),
    ];

    let health = fold_selection_health(&uses, &feedback, policy(2, 0.8));
    assert_eq!(health[0].uses, 10);
    assert_eq!(health[0].assessed_uses, 2);
    assert!(
        (health[0].not_helpful_ratio - 1.0).abs() < f64::EPSILON,
        "ratio must divide by assessed uses, got {}",
        health[0].not_helpful_ratio
    );
}

#[test]
fn silence_alone_never_makes_a_record_fail() {
    // Ninety-nine uses, not one verdict. This is the exact scenario
    // deliverable 3 protects: disuse is not a negative.
    let uses: Vec<_> = (0..99)
        .map(|i| a_use(&format!("cu_{i}"), "nod_a", &format!("task{i}")))
        .collect();

    let health = fold_selection_health(&uses, &[], policy(1, 0.0));
    assert_eq!(health[0].assessed_uses, 0);
    assert!((health[0].not_helpful_ratio - 0.0).abs() < f64::EPSILON);
    assert!(
        !health[0].attributable,
        "nothing assessed means nothing attributable"
    );
    assert!(
        !health[0].failing,
        "a record nobody judged must never read as failing"
    );
}

#[test]
fn a_verdict_without_opportunity_is_not_evidence() {
    let uses = vec![a_use("cu_1", "nod_a", "task1")];
    let mut verdict = a_verdict("cu_1", ContextUseEvaluation::NotHelpful);
    verdict.had_opportunity = false;

    let health = fold_selection_health(&uses, &[verdict], policy(1, 0.5));
    assert_eq!(
        health[0].assessed_uses, 0,
        "a use with no opportunity to help cannot be counted against the record"
    );
    assert!(!health[0].failing);
}

#[test]
fn a_verdict_without_a_recorded_method_is_not_evidence() {
    let uses = vec![a_use("cu_1", "nod_a", "task1")];
    let mut verdict = a_verdict("cu_1", ContextUseEvaluation::NotHelpful);
    verdict.evaluation_method = None;

    let health = fold_selection_health(&uses, &[verdict], policy(1, 0.5));
    assert_eq!(
        health[0].assessed_uses, 0,
        "deliverable 3: the assessment method must be recorded to count"
    );
}

#[test]
fn a_low_confidence_verdict_is_not_evidence() {
    let uses = vec![a_use("cu_1", "nod_a", "task1")];
    let mut verdict = a_verdict("cu_1", ContextUseEvaluation::NotHelpful);
    verdict.attribution_confidence = Confidence::new(50).expect("in range");

    let health = fold_selection_health(&uses, &[verdict], policy(1, 0.5));
    assert_eq!(
        health[0].assessed_uses, 0,
        "a verdict below the confidence floor must not drive anything"
    );
}

#[test]
fn a_verdict_naming_an_unknown_use_is_dropped() {
    let uses = vec![a_use("cu_1", "nod_a", "task1")];
    let orphan = a_verdict("cu_nonexistent", ContextUseEvaluation::NotHelpful);

    let health = fold_selection_health(&uses, &[orphan], policy(1, 0.5));
    assert_eq!(health.len(), 1);
    assert_eq!(health[0].assessed_uses, 0);
}

#[test]
fn failing_requires_clearing_the_attributable_floor() {
    // One negative verdict out of one assessed use is a ratio of 1.0 — but a
    // single data point must not retire anything.
    let uses = vec![a_use("cu_1", "nod_a", "task1")];
    let feedback = vec![a_verdict("cu_1", ContextUseEvaluation::NotHelpful)];

    let strict = fold_selection_health(&uses, &feedback, policy(5, 0.8));
    assert!((strict[0].not_helpful_ratio - 1.0).abs() < f64::EPSILON);
    assert!(!strict[0].attributable);
    assert!(!strict[0].failing, "one verdict is a ratio, not a pattern");

    let lenient = fold_selection_health(&uses, &feedback, policy(1, 0.8));
    assert!(lenient[0].failing);
}

#[test]
fn thirty_uses_in_one_task_are_one_task_of_evidence() {
    // Spec §7's anti-poisoning rule: count distinct tasks, not events.
    let uses: Vec<_> = (0..30)
        .map(|i| a_use(&format!("cu_{i}"), "nod_a", "the-same-task"))
        .collect();

    let health = fold_selection_health(&uses, &[], policy(1, 0.8));
    assert_eq!(health[0].uses, 30);
    assert_eq!(health[0].distinct_tasks, 1);
}

#[test]
fn the_fold_rebuilds_exactly_and_is_order_independent() {
    // THE GATE: "aggregates rebuild exactly from immutable records."
    let uses = vec![
        a_use("cu_1", "nod_a", "task1"),
        a_use("cu_2", "nod_b", "task1"),
        a_use("cu_3", "nod_a", "task2"),
    ];
    let feedback = vec![
        a_verdict("cu_1", ContextUseEvaluation::Helpful),
        a_verdict("cu_3", ContextUseEvaluation::NotHelpful),
        a_verdict("cu_2", ContextUseEvaluation::Neutral),
    ];

    let once = fold_selection_health(&uses, &feedback, policy(1, 0.8));
    let twice = fold_selection_health(&uses, &feedback, policy(1, 0.8));
    assert_eq!(once, twice, "the same records must rebuild the same value");

    // Append order is a storage detail, not a fact about the evidence, so a
    // ledger read that returns rows in another order must agree exactly.
    let mut shuffled_uses = uses.clone();
    shuffled_uses.reverse();
    let mut shuffled_feedback = feedback.clone();
    shuffled_feedback.reverse();
    let reordered = fold_selection_health(&shuffled_uses, &shuffled_feedback, policy(1, 0.8));
    assert_eq!(once, reordered, "the fold must not depend on row order");
}

#[test]
fn output_is_sorted_so_two_runs_are_byte_identical() {
    let uses = vec![
        a_use("cu_1", "nod_zzz", "task1"),
        a_use("cu_2", "nod_aaa", "task1"),
        a_use("cu_3", "nod_mmm", "task1"),
    ];

    let health = fold_selection_health(&uses, &[], SelectionHealthPolicy::default());
    let ids: Vec<&str> = health
        .iter()
        .map(|h| h.context_record_id.as_str())
        .collect();
    assert_eq!(ids, vec!["nod_aaa", "nod_mmm", "nod_zzz"]);
}

#[test]
fn the_shipped_defaults_are_the_settings_defaults() {
    // If `context.efficacy`'s defaults move, this fold's must move with them —
    // two different answers to "what counts as failing" is the drift this
    // catches.
    let default = SelectionHealthPolicy::default();
    assert_eq!(default.min_attributable_uses, 5);
    assert!((default.not_helpful_ratio_threshold - 0.8).abs() < f64::EPSILON);
    assert_eq!(default.min_attribution_confidence, 80);
}

/// A verdict from `method`, otherwise identical to [`a_verdict`].
fn a_verdict_via(
    use_id: &str,
    evaluation: ContextUseEvaluation,
    method: &str,
) -> ContextUseFeedback {
    let mut verdict = a_verdict(use_id, evaluation);
    verdict.evaluation_method = ContextEvaluationMethod::new(method).ok();
    verdict
}

#[test]
fn an_agent_self_report_informs_but_can_never_retire() {
    // THE two-tier invariant. The agent judged its own context unhelpful five
    // times out of five. That is real evidence and must be visible — but a
    // self-report that could retire its own subject is self-reinforcing, so it
    // may not drive the decision.
    let uses: Vec<_> = (0..5)
        .map(|i| a_use(&format!("cu_{i}"), "nod_a", &format!("task{i}")))
        .collect();
    let feedback: Vec<_> = (0..5)
        .map(|i| {
            a_verdict_via(
                &format!("cu_{i}"),
                ContextUseEvaluation::NotHelpful,
                ContextEvaluationMethod::AGENT_SELF_REPORT,
            )
        })
        .collect();

    let health = fold_selection_health(&uses, &feedback, policy(5, 0.8));
    // Visible…
    assert_eq!(health[0].assessed_uses, 5);
    assert_eq!(health[0].not_helpful, 5);
    assert!((health[0].not_helpful_ratio - 1.0).abs() < f64::EPSILON);
    // …but not actionable.
    assert_eq!(health[0].eligible_assessed, 0);
    assert_eq!(health[0].eligible_not_helpful, 0);
    assert!(!health[0].attributable);
    assert!(
        !health[0].failing,
        "an agent self-report must never be able to retire its own subject"
    );
}

#[test]
fn a_pruning_eligible_method_does_drive_the_decision() {
    let uses: Vec<_> = (0..5)
        .map(|i| a_use(&format!("cu_{i}"), "nod_a", &format!("task{i}")))
        .collect();
    let feedback: Vec<_> = (0..5)
        .map(|i| {
            a_verdict_via(
                &format!("cu_{i}"),
                ContextUseEvaluation::NotHelpful,
                ContextEvaluationMethod::DETERMINISTIC_VALIDATION,
            )
        })
        .collect();

    let health = fold_selection_health(&uses, &feedback, policy(5, 0.8));
    assert_eq!(health[0].eligible_assessed, 5);
    assert!(
        health[0].failing,
        "deterministic validation is strong enough"
    );
}

#[test]
fn an_unregistered_extension_method_is_not_pruning_eligible() {
    // Host policy must trust a method before it can suppress anything.
    let uses: Vec<_> = (0..5)
        .map(|i| a_use(&format!("cu_{i}"), "nod_a", &format!("task{i}")))
        .collect();
    let feedback: Vec<_> = (0..5)
        .map(|i| {
            a_verdict_via(
                &format!("cu_{i}"),
                ContextUseEvaluation::NotHelpful,
                "vendor.custom_heuristic.v1",
            )
        })
        .collect();

    let health = fold_selection_health(&uses, &feedback, policy(5, 0.8));
    assert_eq!(health[0].assessed_uses, 5, "still counted as assessment");
    assert_eq!(health[0].eligible_assessed, 0);
    assert!(!health[0].failing);
}

#[test]
fn self_reports_do_not_dilute_a_genuine_eligible_verdict() {
    // Mixed evidence: one deterministic negative, four agent self-report
    // positives. The eligible population is the single strong verdict, so the
    // eligible ratio is 1.0 — the self-reports must not water it down.
    let uses: Vec<_> = (0..5)
        .map(|i| a_use(&format!("cu_{i}"), "nod_a", &format!("task{i}")))
        .collect();
    let mut feedback = vec![a_verdict_via(
        "cu_0",
        ContextUseEvaluation::NotHelpful,
        ContextEvaluationMethod::DETERMINISTIC_VALIDATION,
    )];
    for i in 1..5 {
        feedback.push(a_verdict_via(
            &format!("cu_{i}"),
            ContextUseEvaluation::Helpful,
            ContextEvaluationMethod::AGENT_SELF_REPORT,
        ));
    }

    let health = fold_selection_health(&uses, &feedback, policy(1, 0.8));
    assert_eq!(health[0].eligible_assessed, 1);
    assert_eq!(health[0].eligible_not_helpful, 1);
    assert!(health[0].failing);
}
