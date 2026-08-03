//! Unit and property tests for reward extraction — the tiers, the discards,
//! and the airlock that keeps model prose out of a training label.

use proptest::prelude::*;
use stella_protocol::LadderSnapshot;

use super::*;

fn cost(steps: u32, cost_usd: f64, revisions: u32) -> TrajectoryCost {
    TrajectoryCost {
        steps,
        cost_usd,
        revisions,
    }
}

fn settled(rung: LadderRung, passed: bool) -> Settlement {
    Settlement::Settled { rung, passed }
}

/// A snapshot carrying `rung`, so `from_evidence` has something to read.
fn snapshot(rung: Option<LadderRung>) -> LadderSnapshot {
    LadderSnapshot {
        rung,
        tracked_command: None,
        oracle_trace: Vec::new(),
        flip_achieved: false,
        unstable_flip: false,
        flip_refused_different_failure: false,
        touched_tests_passed: None,
        test_infra: None,
        diff_lines: 0,
        diff_budget: 0,
        diff_available: false,
        file_change_events: 0,
        mutating_actions: 0,
        new_diag_errors: 0,
        new_diag_warnings: 0,
        witness_intact: None,
        witness_mutation: None,
    }
}

fn evidence(summary: &str, rung: Option<LadderRung>) -> JudgeEvidence {
    JudgeEvidence {
        summary: summary.to_string(),
        deterministic: false,
        evidence_refs: vec![summary.to_string()],
        ladder: Some(Box::new(snapshot(rung))),
    }
}

/// The four scored tiers, at their stated magnitudes. Zero cost so the
/// outcome term is the whole reward.
#[test]
fn the_four_tiers_score_at_their_stated_weights() {
    let free = cost(0, 0.0, 0);
    let shaping = RewardShaping::default();
    for (rung, passed, expected) in [
        (LadderRung::SubmitFast, true, 1.0),
        (LadderRung::Revise, false, -1.0),
        (LadderRung::ModelJudge, true, 0.5),
        (LadderRung::ModelJudge, false, -0.5),
    ] {
        let label = label(settled(rung, passed), free, &shaping);
        assert_eq!(label.rung, Some(rung));
        assert_eq!(label.discard, None);
        assert_eq!(
            label.reward,
            Some(expected),
            "{rung:?} passed={passed} should score {expected}"
        );
        assert!(label.is_scored());
    }
}

/// The composite subtracts steps, cost, and revisions at the documented
/// weights: `1.0 − 0.02·10 − 0.5·0.40 − 0.1·2 = 0.4`.
#[test]
fn shaping_prices_steps_cost_and_revisions() {
    let label = label(
        settled(LadderRung::SubmitFast, true),
        cost(10, 0.40, 2),
        &RewardShaping::default(),
    );
    assert_eq!(label.outcome, Some(1.0));
    let reward = label.reward.expect("scored");
    assert!((reward - 0.4).abs() < 1e-9, "got {reward}");
    // The inputs travel with the label, so a consumer can re-shape without
    // re-reading the journal.
    assert_eq!(label.cost.steps, 10);
    assert_eq!(label.cost.revisions, 2);
}

/// More of any priced quantity strictly lowers the reward — the property that
/// makes the shaping mean anything.
#[test]
fn every_priced_quantity_is_monotone() {
    let shaping = RewardShaping::default();
    let base = label(
        settled(LadderRung::SubmitFast, true),
        cost(4, 0.1, 0),
        &shaping,
    )
    .reward
    .unwrap();
    for dearer in [cost(5, 0.1, 0), cost(4, 0.2, 0), cost(4, 0.1, 1)] {
        let reward = label(settled(LadderRung::SubmitFast, true), dearer, &shaping)
            .reward
            .unwrap();
        assert!(reward < base, "{dearer:?} should cost more than the base");
    }
}

/// The rule the module exists for: an abstain rung is discarded, never flipped
/// into a negative label. A legitimately-passing trial takes that path.
#[test]
fn the_abstain_rungs_are_discarded_not_punished() {
    let shaping = RewardShaping::default();
    for (rung, expected) in [
        (LadderRung::Unverifiable, DiscardReason::Abstained),
        (
            LadderRung::NothingAttempted,
            DiscardReason::NothingAttempted,
        ),
    ] {
        // Both polarities, because the pipeline emits `Unverifiable` as a pass
        // and `NothingAttempted` as a fail — neither may become a label.
        for passed in [true, false] {
            let label = label(settled(rung, passed), cost(9, 1.0, 3), &shaping);
            assert_eq!(label.reward, None, "{rung:?} must carry no scalar");
            assert_eq!(label.outcome, None);
            assert_eq!(label.discard, Some(expected));
            assert_eq!(
                label.rung,
                Some(rung),
                "a discard keeps its rung so it stays selectable"
            );
            assert!(!label.is_scored());
        }
    }
}

/// A heuristic verdict describes the judge's availability, not the work, and a
/// waived review determined nothing at all. Both are discarded — and the two
/// reasons stay distinct, because they are different facts.
#[test]
fn a_verdict_about_the_pipeline_is_not_a_verdict_about_the_work() {
    let shaping = RewardShaping::default();
    let heuristic = label(
        settled(LadderRung::HeuristicFallback, true),
        cost(3, 0.1, 0),
        &shaping,
    );
    assert_eq!(heuristic.discard, Some(DiscardReason::JudgeUnavailable));
    let waived = label(settled(LadderRung::Waived, true), cost(3, 0.1, 0), &shaping);
    assert_eq!(waived.discard, Some(DiscardReason::ReviewWaived));
    assert_ne!(heuristic.discard, waived.discard);
}

/// A `deterministic: true, passed: true` waived verdict — the shape that would
/// read as the ladder's strongest result if the rung were inferred from the
/// evidence flags — is discarded.
#[test]
fn a_waived_pass_is_never_mistaken_for_a_deterministic_pass() {
    let mut waived = evidence(
        "no witness test warranted: pure docs",
        Some(LadderRung::Waived),
    );
    waived.deterministic = true;
    let settlement = Settlement::from_evidence(true, &waived);
    let label = label(settlement, cost(2, 0.05, 0), &RewardShaping::default());
    assert_eq!(label.reward, None);
    assert_eq!(label.discard, Some(DiscardReason::ReviewWaived));
}

/// A verdict recorded before the rung existed says so, rather than being
/// guessed at from the flags that cannot separate the rungs.
#[test]
fn a_rungless_verdict_is_labelled_unknown_not_guessed() {
    let settlement = Settlement::from_evidence(true, &evidence("PASS looks right", None));
    assert_eq!(settlement, Settlement::RungUnknown);
    let label = label(settlement, cost(4, 0.2, 1), &RewardShaping::default());
    assert_eq!(label.rung, None);
    assert_eq!(label.discard, Some(DiscardReason::RungUnknown));
}

/// A trajectory that never reached the verify stage is its own discard reason.
#[test]
fn a_trajectory_with_no_verdict_says_so() {
    let label = label(
        Settlement::Absent,
        cost(1, 0.0, 0),
        &RewardShaping::default(),
    );
    assert_eq!(label.discard, Some(DiscardReason::NoVerdict));
    assert_eq!(label.rung, None);
}

/// A corrupt cost withholds the scalar without discarding the rung: a `NaN`
/// reward is not a smaller reward, it is an unusable one.
#[test]
fn a_non_finite_cost_withholds_the_scalar_only() {
    for broken in [f64::NAN, f64::INFINITY] {
        let label = label(
            settled(LadderRung::SubmitFast, true),
            cost(2, broken, 0),
            &RewardShaping::default(),
        );
        assert_eq!(label.reward, None);
        assert_eq!(label.discard, Some(DiscardReason::CostNotFinite));
        assert_eq!(
            label.outcome,
            Some(1.0),
            "the rung's own term is still true"
        );
    }
}

/// The airlock, stated structurally: a serialized label contains no string
/// outside the closed enum vocabularies. Adding a `judge_reasoning: String`
/// field to `RewardLabel` fails here, which is the point.
#[test]
fn a_label_has_no_free_text_leaves() {
    const TOKENS: &[&str] = &[
        // LadderRung
        "submit_fast",
        "revise",
        "nothing_attempted",
        "unverifiable",
        "model_judge",
        "heuristic_fallback",
        "waived",
        // DiscardReason
        "abstained",
        "judge_unavailable",
        "review_waived",
        "rung_unknown",
        "no_verdict",
        "cost_not_finite",
    ];
    let mut leaves = Vec::new();
    for rung in [
        LadderRung::SubmitFast,
        LadderRung::Revise,
        LadderRung::ModelJudge,
        LadderRung::Unverifiable,
        LadderRung::NothingAttempted,
        LadderRung::HeuristicFallback,
        LadderRung::Waived,
    ] {
        for passed in [true, false] {
            let value = serde_json::to_value(label(
                settled(rung, passed),
                cost(3, 0.2, 1),
                &RewardShaping::default(),
            ))
            .unwrap();
            collect_strings(&value, &mut leaves);
        }
    }
    assert!(
        !leaves.is_empty(),
        "the probe must actually see some strings"
    );
    for leaf in leaves {
        assert!(
            TOKENS.contains(&leaf.as_str()),
            "`{leaf}` is not a closed-vocabulary token — a label must carry no free text"
        );
    }
}

fn collect_strings(value: &serde_json::Value, into: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => into.push(text.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|i| collect_strings(i, into)),
        serde_json::Value::Object(map) => map.values().for_each(|v| collect_strings(v, into)),
        _ => {}
    }
}

/// Invariant #4: the label round-trips.
#[test]
fn a_label_round_trips() {
    let label = label(
        settled(LadderRung::ModelJudge, false),
        cost(7, 0.33, 2),
        &RewardShaping::default(),
    );
    let json = serde_json::to_string(&label).unwrap();
    let back: RewardLabel = serde_json::from_str(&json).unwrap();
    assert_eq!(label, back);
}

proptest! {
    /// The airlock as a property: however a judge phrases itself, and whatever
    /// rung the verdict carries, none of that prose reaches the label.
    #[test]
    fn judge_prose_never_reaches_a_label(
        reasoning in "[A-Za-z ]{12,60}",
        passed in any::<bool>(),
        steps in 0u32..50,
        cost_usd in 0.0f64..5.0,
        revisions in 0u32..8,
    ) {
        let evidence = evidence(&reasoning, Some(LadderRung::ModelJudge));
        let settlement = Settlement::from_evidence(passed, &evidence);
        let label = label(settlement, cost(steps, cost_usd, revisions), &RewardShaping::default());
        let json = serde_json::to_string(&label).unwrap();
        prop_assert!(
            !json.contains(reasoning.trim()),
            "judge reasoning leaked into {json}"
        );
        // And the label is still the right one — the airlock does not cost the
        // signal, only the prose.
        prop_assert_eq!(label.rung, Some(LadderRung::ModelJudge));
        prop_assert_eq!(label.outcome, Some(if passed { 0.5 } else { -0.5 }));
    }

    /// Labelling is total and never emits a non-finite scalar, for any rung,
    /// any polarity, and any finite cost.
    #[test]
    fn labelling_is_total(
        passed in any::<bool>(),
        steps in 0u32..10_000,
        cost_usd in 0.0f64..1_000.0,
        revisions in 0u32..1_000,
        which in 0usize..7,
    ) {
        const RUNGS: [LadderRung; 7] = [
            LadderRung::SubmitFast,
            LadderRung::Revise,
            LadderRung::NothingAttempted,
            LadderRung::Unverifiable,
            LadderRung::ModelJudge,
            LadderRung::HeuristicFallback,
            LadderRung::Waived,
        ];
        let label = label(
            settled(RUNGS[which], passed),
            cost(steps, cost_usd, revisions),
            &RewardShaping::default(),
        );
        if let Some(reward) = label.reward {
            prop_assert!(reward.is_finite());
            prop_assert!(label.discard.is_none());
        } else {
            prop_assert!(label.discard.is_some(), "an unscored label states why");
        }
    }

    /// Shaping only ever subtracts: a scored reward never exceeds its own
    /// outcome term.
    #[test]
    fn shaping_never_raises_a_reward(
        steps in 0u32..200,
        cost_usd in 0.0f64..50.0,
        revisions in 0u32..50,
        passed in any::<bool>(),
    ) {
        let label = label(
            settled(LadderRung::SubmitFast, passed),
            cost(steps, cost_usd, revisions),
            &RewardShaping::default(),
        );
        let (Some(outcome), Some(reward)) = (label.outcome, label.reward) else {
            return Ok(());
        };
        prop_assert!(reward <= outcome);
    }
}
