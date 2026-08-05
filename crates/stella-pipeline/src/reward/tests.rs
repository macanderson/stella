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
        diff_coverage: None,
    }
}

fn evidence(summary: &str, rung: Option<LadderRung>) -> VerdictEvidence {
    VerdictEvidence {
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
    let policy = RewardPolicy::default();
    for (rung, passed, expected) in [
        (LadderRung::SubmitFast, true, 1.0),
        (LadderRung::Revise, false, -1.0),
        (LadderRung::ModelVerdict, true, 0.5),
        (LadderRung::ModelVerdict, false, -0.5),
    ] {
        let label = label(settled(rung, passed), free, &policy);
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
        &RewardPolicy::default(),
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
    let policy = RewardPolicy::default();
    let base = label(
        settled(LadderRung::SubmitFast, true),
        cost(4, 0.1, 0),
        &policy,
    )
    .reward
    .unwrap();
    for dearer in [cost(5, 0.1, 0), cost(4, 0.2, 0), cost(4, 0.1, 1)] {
        let reward = label(settled(LadderRung::SubmitFast, true), dearer, &policy)
            .reward
            .unwrap();
        assert!(reward < base, "{dearer:?} should cost more than the base");
    }
}

/// The rule the module exists for: an abstain rung is discarded, never flipped
/// into a negative label. A legitimately-passing trial takes that path.
#[test]
fn the_abstain_rungs_are_discarded_not_punished() {
    let policy = RewardPolicy::default();
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
            let label = label(settled(rung, passed), cost(9, 1.0, 3), &policy);
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

/// A heuristic verdict describes the verifier's availability, not the work, and a
/// waived review determined nothing at all. Both are discarded — and the two
/// reasons stay distinct, because they are different facts.
#[test]
fn a_verdict_about_the_pipeline_is_not_a_verdict_about_the_work() {
    let policy = RewardPolicy::default();
    let heuristic = label(
        settled(LadderRung::HeuristicFallback, true),
        cost(3, 0.1, 0),
        &policy,
    );
    assert_eq!(heuristic.discard, Some(DiscardReason::VerifierUnavailable));
    let waived = label(settled(LadderRung::Waived, true), cost(3, 0.1, 0), &policy);
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
    let label = label(settlement, cost(2, 0.05, 0), &RewardPolicy::default());
    assert_eq!(label.reward, None);
    assert_eq!(label.discard, Some(DiscardReason::ReviewWaived));
}

/// A verdict recorded before the rung existed says so, rather than being
/// guessed at from the flags that cannot separate the rungs.
#[test]
fn a_rungless_verdict_is_labelled_unknown_not_guessed() {
    let settlement = Settlement::from_evidence(true, &evidence("PASS looks right", None));
    assert_eq!(settlement, Settlement::RungUnknown);
    let label = label(settlement, cost(4, 0.2, 1), &RewardPolicy::default());
    assert_eq!(label.rung, None);
    assert_eq!(label.discard, Some(DiscardReason::RungUnknown));
}

/// A trajectory that never reached the verify stage is its own discard reason.
#[test]
fn a_trajectory_with_no_verdict_says_so() {
    let label = label(
        Settlement::Absent,
        cost(1, 0.0, 0),
        &RewardPolicy::default(),
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
            &RewardPolicy::default(),
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

/// A policy with a helper for the common case: lower the verifier, keep the rest.
fn distrusting(judged: f64) -> RewardPolicy {
    RewardPolicy {
        outcome: OutcomeWeights {
            judged,
            ..OutcomeWeights::default()
        },
        ..RewardPolicy::default()
    }
}

/// The knob does what it says: a workspace that trusts its verifier less scores
/// judged turns lower, and leaves the deterministic rungs untouched. That
/// asymmetry is the whole point — the discount applies to the opinion, not to
/// the observation.
#[test]
fn a_lowered_verifier_weight_discounts_only_the_judged_rungs() {
    let policy = distrusting(0.2);
    let free = cost(0, 0.0, 0);
    for (rung, passed, expected) in [
        (LadderRung::ModelVerdict, true, 0.2),
        (LadderRung::ModelVerdict, false, -0.2),
        (LadderRung::SubmitFast, true, 1.0),
        (LadderRung::Revise, false, -1.0),
    ] {
        let label = label(settled(rung, passed), free, &policy);
        assert_eq!(
            label.outcome,
            Some(expected),
            "{rung:?} passed={passed} under judged=0.2"
        );
    }
}

/// A judged weight of zero is a refusal to label, not a label of zero.
///
/// The distinction is the module's founding rule applied to a knob: `0.0` is a
/// claim that a neutral outcome was observed, and "I do not trust this verifier"
/// is the opposite of a claim. A trainer filtering on `is_scored()` must not
/// see these rows at all.
#[test]
fn a_zero_verifier_weight_discards_rather_than_scoring_zero() {
    let policy = distrusting(0.0);
    for passed in [true, false] {
        let label = label(
            settled(LadderRung::ModelVerdict, passed),
            cost(3, 0.1, 0),
            &policy,
        );
        assert_eq!(label.reward, None, "a distrusted verifier scores nothing");
        assert_eq!(label.outcome, None);
        assert_eq!(label.discard, Some(DiscardReason::VerifierDistrusted));
        assert!(!label.is_scored());
        assert_eq!(
            label.rung,
            Some(LadderRung::ModelVerdict),
            "the rung survives so the row stays selectable"
        );
    }
    // And the deterministic rungs are unaffected: distrusting the verifier is not
    // distrusting the tests.
    let deterministic = label(
        settled(LadderRung::SubmitFast, true),
        cost(0, 0.0, 0),
        &policy,
    );
    assert_eq!(deterministic.outcome, Some(1.0));
}

/// The ceiling: a judged weight above the deterministic one is refused, at both
/// the places it can arrive. Above that line a model's opinion outranks a
/// test's observation, which inverts the ladder.
#[test]
fn a_verifier_weight_above_the_deterministic_one_is_refused() {
    let inverted = distrusting(1.5);
    assert_eq!(
        inverted.validate(),
        Err(WeightError::JudgedAboveDeterministic)
    );
    // ...and `label` refuses it too, rather than trusting that whoever built
    // the policy validated it first.
    let label = label(
        settled(LadderRung::ModelVerdict, true),
        cost(1, 0.0, 0),
        &inverted,
    );
    assert_eq!(label.reward, None);
    assert_eq!(label.discard, Some(DiscardReason::PolicyInvalid));
    assert_eq!(
        label.rung,
        Some(LadderRung::ModelVerdict),
        "the rung is still true even when the arithmetic is refused"
    );
}

/// Equal weights are legal — a workspace whose verifier is as good as its tests is
/// entitled to say so. The ceiling is `>`, not `>=`.
#[test]
fn a_verifier_weight_equal_to_the_deterministic_one_is_allowed() {
    let policy = distrusting(1.0);
    assert_eq!(policy.validate(), Ok(()));
    let label = label(
        settled(LadderRung::ModelVerdict, true),
        cost(0, 0.0, 0),
        &policy,
    );
    assert_eq!(label.outcome, Some(1.0));
}

/// Every other way a policy can be nonsense, and the distinct reason each one
/// earns. Distinct because they are different mistakes with different fixes.
#[test]
fn each_impossible_policy_names_its_own_rule() {
    let cases = [
        (distrusting(-0.1), WeightError::JudgedNegative),
        (distrusting(f64::NAN), WeightError::NotFinite),
        (
            RewardPolicy {
                outcome: OutcomeWeights {
                    deterministic: 0.0,
                    judged: 0.0,
                },
                ..RewardPolicy::default()
            },
            WeightError::DeterministicNotPositive,
        ),
        (
            RewardPolicy {
                shaping: RewardShaping {
                    per_step: -0.1,
                    ..RewardShaping::default()
                },
                ..RewardPolicy::default()
            },
            WeightError::ShapingNegative,
        ),
    ];
    for (policy, expected) in cases {
        assert_eq!(policy.validate(), Err(expected), "{policy:?}");
        // A refused policy produces a marked record, never a panic and never a
        // number.
        let label = label(
            settled(LadderRung::SubmitFast, true),
            cost(1, 0.0, 0),
            &policy,
        );
        assert_eq!(label.discard, Some(DiscardReason::PolicyInvalid));
        assert_eq!(label.reward, None);
    }
}

/// A negative shaping price would pay a trajectory for spending more, breaking
/// the "shaping only ever subtracts" invariant that
/// `shaping_never_raises_a_reward` pins. Validation is what keeps that property
/// true now that the prices are configurable.
#[test]
fn a_negative_shaping_price_cannot_pay_a_trajectory_to_spend_more() {
    let bribe = RewardPolicy {
        shaping: RewardShaping {
            per_usd: -1.0,
            ..RewardShaping::default()
        },
        ..RewardPolicy::default()
    };
    let label = label(
        settled(LadderRung::SubmitFast, true),
        cost(0, 10.0, 0),
        &bribe,
    );
    assert_eq!(
        label.reward, None,
        "a $10 turn must not out-score a free one"
    );
    assert_eq!(label.discard, Some(DiscardReason::PolicyInvalid));
}

/// The stamp: every label states the policy it was computed under, including
/// the discards. Without it two workspaces on different weights emit rows that
/// are arithmetically indistinguishable and silently incomparable.
#[test]
fn every_label_carries_the_policy_it_was_computed_under() {
    let policy = distrusting(0.25);
    let scored = label(
        settled(LadderRung::ModelVerdict, true),
        cost(2, 0.1, 0),
        &policy,
    );
    assert_eq!(scored.policy, policy);
    // A discard carries it too — a reader pooling records has to tell a
    // distrusted verifier from a workspace that simply never reached one.
    for settlement in [
        settled(LadderRung::Unverifiable, true),
        Settlement::Absent,
        Settlement::RungUnknown,
    ] {
        let label = label(settlement, cost(2, 0.1, 0), &policy);
        assert!(label.discard.is_some());
        assert_eq!(label.policy, policy, "a discard states its policy too");
    }
}

/// Two workspaces, the same turn, different verifier weights: the labels differ,
/// and each one carries the number that explains why. This is the property that
/// makes per-workspace weights safe to pool.
#[test]
fn the_same_turn_under_two_policies_is_told_apart_by_its_stamp() {
    let turn = || settled(LadderRung::ModelVerdict, true);
    let spent = cost(4, 0.1, 0);
    let trusting = label(turn(), spent, &RewardPolicy::default());
    let skeptical = label(turn(), spent, &distrusting(0.2));

    assert_ne!(trusting.reward, skeptical.reward);
    assert_ne!(trusting.policy, skeptical.policy);
    // And the difference is fully explained by the stamp: rescaling the
    // skeptical label by the ratio of the two judged weights recovers the
    // trusting one, with no access to the journal either produced it from.
    let ratio = trusting.policy.outcome.judged / skeptical.policy.outcome.judged;
    let recovered = skeptical.outcome.unwrap() * ratio;
    assert!((recovered - trusting.outcome.unwrap()).abs() < 1e-9);
}

/// The airlock, stated structurally: a serialized label contains no string
/// outside the closed enum vocabularies. Adding a `verifier_reasoning: String`
/// field to `RewardLabel` fails here, which is the point.
#[test]
fn a_label_has_no_free_text_leaves() {
    const TOKENS: &[&str] = &[
        // LadderRung
        "submit_fast",
        "revise",
        "nothing_attempted",
        "unverifiable",
        "model_verdict",
        "heuristic_fallback",
        "waived",
        // DiscardReason
        "abstained",
        "verifier_unavailable",
        "review_waived",
        "rung_unknown",
        "no_verdict",
        "cost_not_finite",
        "verifier_distrusted",
        "policy_invalid",
    ];
    let mut leaves = Vec::new();
    for rung in [
        LadderRung::SubmitFast,
        LadderRung::Revise,
        LadderRung::ModelVerdict,
        LadderRung::Unverifiable,
        LadderRung::NothingAttempted,
        LadderRung::HeuristicFallback,
        LadderRung::Waived,
    ] {
        for passed in [true, false] {
            // Every policy the stamp can carry, including the two that produce
            // the new discard reasons: the stamp must add numbers and nothing
            // else, whatever it holds.
            for policy in [
                RewardPolicy::default(),
                distrusting(0.0),
                distrusting(0.2),
                distrusting(f64::NAN),
            ] {
                let value =
                    serde_json::to_value(label(settled(rung, passed), cost(3, 0.2, 1), &policy))
                        .unwrap();
                collect_strings(&value, &mut leaves);
            }
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
        settled(LadderRung::ModelVerdict, false),
        cost(7, 0.33, 2),
        &RewardPolicy::default(),
    );
    let json = serde_json::to_string(&label).unwrap();
    let back: RewardLabel = serde_json::from_str(&json).unwrap();
    assert_eq!(label, back);
}

proptest! {
    /// The airlock as a property: however a verifier phrases itself, and whatever
    /// rung the verdict carries, none of that prose reaches the label.
    #[test]
    fn verifier_prose_never_reaches_a_label(
        reasoning in "[A-Za-z ]{12,60}",
        passed in any::<bool>(),
        steps in 0u32..50,
        cost_usd in 0.0f64..5.0,
        revisions in 0u32..8,
    ) {
        let evidence = evidence(&reasoning, Some(LadderRung::ModelVerdict));
        let settlement = Settlement::from_evidence(passed, &evidence);
        let label = label(settlement, cost(steps, cost_usd, revisions), &RewardPolicy::default());
        let json = serde_json::to_string(&label).unwrap();
        prop_assert!(
            !json.contains(reasoning.trim()),
            "verifier reasoning leaked into {json}"
        );
        // And the label is still the right one — the airlock does not cost the
        // signal, only the prose.
        prop_assert_eq!(label.rung, Some(LadderRung::ModelVerdict));
        prop_assert_eq!(label.outcome, Some(if passed { 0.5 } else { -0.5 }));
    }

    /// Labelling is total and never emits a non-finite scalar, for any rung,
    /// any polarity, any finite cost, and — since the weights became
    /// configurable — any policy at all, including the impossible ones.
    ///
    /// The policy is the least trustworthy of the three inputs: the rung comes
    /// from this pipeline and the cost from the store, but the weights come
    /// from a file a person edits. So the sweep deliberately ranges past every
    /// boundary `validate` enforces, in both directions.
    #[test]
    fn labelling_is_total(
        passed in any::<bool>(),
        steps in 0u32..10_000,
        cost_usd in 0.0f64..1_000.0,
        revisions in 0u32..1_000,
        which in 0usize..7,
        deterministic in -2.0f64..4.0,
        judged in -2.0f64..4.0,
        per_step in -1.0f64..1.0,
        per_usd in -1.0f64..1.0,
        per_revision in -1.0f64..1.0,
    ) {
        const RUNGS: [LadderRung; 7] = [
            LadderRung::SubmitFast,
            LadderRung::Revise,
            LadderRung::NothingAttempted,
            LadderRung::Unverifiable,
            LadderRung::ModelVerdict,
            LadderRung::HeuristicFallback,
            LadderRung::Waived,
        ];
        let policy = RewardPolicy {
            outcome: OutcomeWeights { deterministic, judged },
            shaping: RewardShaping { per_step, per_usd, per_revision },
        };
        let label = label(
            settled(RUNGS[which], passed),
            cost(steps, cost_usd, revisions),
            &policy,
        );
        prop_assert_eq!(label.policy, policy, "the stamp is always the policy applied");
        if let Some(reward) = label.reward {
            prop_assert!(reward.is_finite());
            prop_assert!(label.discard.is_none());
            // A scored label implies the policy cleared validation — no
            // refused weight may reach a number by any path.
            prop_assert_eq!(policy.validate(), Ok(()));
        } else {
            prop_assert!(label.discard.is_some(), "an unscored label states why");
        }
        // And the converse: an invalid policy scores nothing, whatever the rung.
        if policy.validate().is_err() {
            prop_assert_eq!(label.discard, Some(DiscardReason::PolicyInvalid));
        }
    }

    /// Shaping still only ever subtracts — now stated over every VALID policy
    /// rather than just the default one, which is what makes the invariant
    /// survive the weights becoming configurable.
    #[test]
    fn no_valid_policy_lets_shaping_raise_a_reward(
        steps in 0u32..200,
        cost_usd in 0.0f64..50.0,
        revisions in 0u32..50,
        passed in any::<bool>(),
        deterministic in 0.01f64..4.0,
        judged_fraction in 0.0f64..1.0,
        per_step in 0.0f64..1.0,
        per_usd in 0.0f64..1.0,
        per_revision in 0.0f64..1.0,
        which in 0usize..2,
    ) {
        let policy = RewardPolicy {
            // Constructed to be valid by shape: the judged weight is expressed
            // as a fraction of the deterministic one, which is the ordering
            // rule written as arithmetic instead of checked after the fact.
            outcome: OutcomeWeights { deterministic, judged: deterministic * judged_fraction },
            shaping: RewardShaping { per_step, per_usd, per_revision },
        };
        prop_assume!(policy.validate().is_ok());
        let rung = [LadderRung::SubmitFast, LadderRung::ModelVerdict][which];
        let label = label(
            settled(rung, passed),
            cost(steps, cost_usd, revisions),
            &policy,
        );
        let (Some(outcome), Some(reward)) = (label.outcome, label.reward) else {
            return Ok(());
        };
        prop_assert!(reward <= outcome, "{reward} > {outcome} under {policy:?}");
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
            &RewardPolicy::default(),
        );
        let (Some(outcome), Some(reward)) = (label.outcome, label.reward) else {
            return Ok(());
        };
        prop_assert!(reward <= outcome);
    }
}
