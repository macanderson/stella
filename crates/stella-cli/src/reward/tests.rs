//! Unit and property tests for reward extraction — the tiers, the discards,
//! and the airlock that keeps model prose out of a training label.

use proptest::prelude::*;
use stella_protocol::{FlipOutcome, LadderSnapshot};

use super::*;

fn cost(steps: u32, cost_usd: f64, revisions: u32) -> TrajectoryCost {
    TrajectoryCost {
        steps: Some(steps),
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
        flip: FlipOutcome::NotAchieved,
        unstable_flip: false,
        flip_refused_different_failure: false,
        touched_tests_passed: None,
        test_infra: None,
        diff_lines: 0,
        diff_budget: 0,
        diff_available: false,
        mutating_actions: 0,
        new_diag_errors: 0,
        new_diag_warnings: 0,
        witness_intact: None,
        witness_mutation: None,
        diff_coverage: None,
        verify_done_flip: false,
        no_test_surface: false,
        errored_commands: 0,
        verifier_independent: None,
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

/// The scored tiers, at their stated magnitudes. Zero cost so the outcome
/// term is the whole reward.
///
/// There are two of them, and there used to be four. The judged tier is gone
/// with the call that produced it: no rung's magnitude now comes from a model's
/// opinion, so every non-deterministic rung is a named discard rather than a
/// number — see `every_unscored_rung_names_its_reason`.
#[test]
fn only_the_deterministic_rungs_score_at_their_stated_weights() {
    let free = cost(0, 0.0, 0);
    let policy = RewardPolicy::default();
    for (rung, passed, expected) in [
        (LadderRung::SubmitFast, true, 1.0),
        (LadderRung::Revise, false, -1.0),
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
    assert_eq!(label.cost.steps, Some(10));
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

/// Evidence that fell short and a review nobody bought are both discarded —
/// and the two reasons stay distinct, because they are different facts about
/// the turn. One ran the checks and did not get a proof; the other never
/// checked, because the warrant found nothing worth proving.
#[test]
fn an_unproven_turn_is_told_apart_from_an_unreviewed_one() {
    let policy = RewardPolicy::default();
    let unproven = label(
        settled(LadderRung::Unverified, true),
        cost(3, 0.1, 0),
        &policy,
    );
    assert_eq!(unproven.discard, Some(DiscardReason::Unproven));
    let waived = label(settled(LadderRung::Waived, true), cost(3, 0.1, 0), &policy);
    assert_eq!(waived.discard, Some(DiscardReason::ReviewWaived));
    assert_ne!(unproven.discard, waived.discard);
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

/// An unrecorded step count is refused, not read as zero.
///
/// The direction is what makes this a defect rather than an imprecision:
/// shaping only ever subtracts, so a missing count does not shrink the reward,
/// it *raises* it above what the same trajectory earns once its calls are
/// counted — and it raises it on exactly the rows whose provenance is
/// weakest, which then pool with the trustworthy ones as though they had been
/// cheaper.
#[test]
fn an_unrecorded_step_count_is_discarded_rather_than_priced_as_zero() {
    let policy = RewardPolicy::default();
    let unknown = label(
        settled(LadderRung::SubmitFast, true),
        TrajectoryCost {
            steps: None,
            cost_usd: 0.10,
            revisions: 0,
        },
        &policy,
    );
    assert_eq!(unknown.reward, None, "no scalar is claimed");
    assert_eq!(unknown.discard, Some(DiscardReason::StepsUnknown));
    assert_eq!(
        unknown.outcome,
        Some(1.0),
        "the rung's own term is still true, so the row stays selectable"
    );
    assert_eq!(unknown.cost.steps, None, "the gap travels with the label");

    // Priced as zero it would have out-scored the same trajectory with its
    // calls counted, which is the failure this refuses.
    let counted = label(
        settled(LadderRung::SubmitFast, true),
        cost(5, 0.10, 0),
        &policy,
    );
    assert!(
        counted.reward.expect("scored") < 1.0 - 0.5 * 0.10,
        "the counted trajectory pays its step penalty"
    );
}

/// The receipts plane's empty answer is "nobody wrote it down", never "zero
/// calls" — one rule, so `stella trace` and `stella dataset export` cannot
/// drift on the labels they are documented to agree about.
#[test]
fn an_empty_receipts_plane_is_an_unknown_step_count_not_a_zero_one() {
    assert_eq!(TrajectoryCost::recorded_steps(0), None);
    assert_eq!(TrajectoryCost::recorded_steps(1), Some(1));
    assert_eq!(TrajectoryCost::recorded_steps(7), Some(7));
    assert_eq!(
        TrajectoryCost::recorded_steps(usize::MAX),
        Some(u32::MAX),
        "a count past the field saturates rather than panicking or wrapping"
    );
}

/// A policy with a helper for the common case: restate the unit, keep the rest.
fn scaled(deterministic: f64) -> RewardPolicy {
    RewardPolicy {
        outcome: OutcomeWeights { deterministic },
        ..RewardPolicy::default()
    }
}

/// A policy a stored label wrote before the judged tier was retired.
///
/// The extra key is the whole point: the field is gone from [`OutcomeWeights`],
/// so a label already on disk must still decode — silently dropping the weight
/// it no longer has a home for, never failing the read. Every other field has
/// to survive intact, because a stored row that decodes to the WRONG policy is
/// worse than one that will not decode at all.
#[test]
fn a_label_written_under_the_retired_judged_weight_still_decodes() {
    let stored = serde_json::json!({
        "rung": "submit_fast",
        "outcome": 0.5,
        "reward": 0.48,
        "cost": {"steps": 1, "cost_usd": 0.0, "revisions": 0},
        "policy": {
            "outcome": {"deterministic": 1.0, "judged": 0.5},
            "shaping": {"per_step": 0.02, "per_usd": 0.5, "per_revision": 0.1},
        },
    });
    let label: RewardLabel = serde_json::from_value(stored).expect("a stored label must decode");
    assert_eq!(label.policy.outcome, OutcomeWeights { deterministic: 1.0 });
    assert_eq!(label.policy.shaping, RewardShaping::default());
    assert_eq!(
        label.outcome,
        Some(0.5),
        "the term the retired weight produced is still the term that was recorded"
    );
}

/// Every way a policy can be nonsense, and the distinct reason each one earns.
/// Distinct because they are different mistakes with different fixes.
#[test]
fn each_impossible_policy_names_its_own_rule() {
    let cases = [
        (scaled(f64::NAN), WeightError::NotFinite),
        (scaled(0.0), WeightError::DeterministicNotPositive),
        (scaled(-1.0), WeightError::DeterministicNotPositive),
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
    let policy = scaled(0.25);
    let scored = label(
        settled(LadderRung::Unverified, true),
        cost(2, 0.1, 0),
        &policy,
    );
    assert_eq!(scored.policy, policy);
    // A discard carries it too — a reader pooling records has to tell a
    // discard from a scored row computed on a different scale.
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

/// Two workspaces, the same turn, different shaping: the labels differ, and
/// each one carries the numbers that explain why. This is the property that
/// makes per-workspace policies safe to pool.
///
/// Demonstrated on the cost term rather than the outcome weights, because
/// after the judged tier's removal a scored label's outcome is the same
/// magnitude under every valid policy — the weights that still vary a scored
/// row are the shaping ones.
#[test]
fn the_same_turn_under_two_policies_is_told_apart_by_its_stamp() {
    let turn = || settled(LadderRung::SubmitFast, true);
    let spent = cost(4, 0.1, 0);
    let thrifty = RewardPolicy {
        shaping: RewardShaping {
            per_usd: 4.0,
            ..RewardShaping::default()
        },
        ..RewardPolicy::default()
    };
    let ordinary = label(turn(), spent, &RewardPolicy::default());
    let penny_pinching = label(turn(), spent, &thrifty);

    assert_ne!(ordinary.reward, penny_pinching.reward);
    assert_ne!(ordinary.policy, penny_pinching.policy);
    // And the difference is fully explained by the stamp: the two rewards
    // differ by exactly the extra dollar-price the thrifty policy declares,
    // recoverable without access to the journal either came from.
    let extra = (thrifty.shaping.per_usd - RewardPolicy::default().shaping.per_usd) * 0.1;
    assert!((penny_pinching.reward.unwrap() + extra - ordinary.reward.unwrap()).abs() < 1e-9);
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
        "unverified",
        "waived",
        // DiscardReason
        "unproven",
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
        LadderRung::Unverified,
        LadderRung::Unverifiable,
        LadderRung::NothingAttempted,
        LadderRung::Unverified,
        LadderRung::Waived,
    ] {
        for passed in [true, false] {
            // Every policy the stamp can carry, including the invalid one
            // that produces `PolicyInvalid`: the stamp must add numbers and
            // nothing else, whatever it holds.
            for policy in [
                RewardPolicy::default(),
                scaled(0.2),
                scaled(0.0),
                scaled(f64::NAN),
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
        settled(LadderRung::Unverified, false),
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
        let evidence = evidence(&reasoning, Some(LadderRung::Unverified));
        let settlement = Settlement::from_evidence(passed, &evidence);
        let label = label(settlement, cost(steps, cost_usd, revisions), &RewardPolicy::default());
        let json = serde_json::to_string(&label).unwrap();
        prop_assert!(
            !json.contains(reasoning.trim()),
            "verifier reasoning leaked into {json}"
        );
        // And the label is still the right one — the airlock does not cost the
        // signal, only the prose. An unproven rung carries no outcome term by
        // construction, so the surviving signal is the rung and the named
        // reason it was not scored.
        prop_assert_eq!(label.rung, Some(LadderRung::Unverified));
        prop_assert_eq!(label.outcome, None);
        prop_assert_eq!(label.discard, Some(DiscardReason::Unproven));
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
        per_step in -1.0f64..1.0,
        per_usd in -1.0f64..1.0,
        per_revision in -1.0f64..1.0,
    ) {
        const RUNGS: [LadderRung; 7] = [
            LadderRung::SubmitFast,
            LadderRung::Revise,
            LadderRung::NothingAttempted,
            LadderRung::Unverifiable,
            LadderRung::Unverified,
            LadderRung::Unverified,
            LadderRung::Waived,
        ];
        let policy = RewardPolicy {
            outcome: OutcomeWeights { deterministic },
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
        per_step in 0.0f64..1.0,
        per_usd in 0.0f64..1.0,
        per_revision in 0.0f64..1.0,
        which in 0usize..2,
    ) {
        let policy = RewardPolicy {
            outcome: OutcomeWeights { deterministic },
            shaping: RewardShaping { per_step, per_usd, per_revision },
        };
        prop_assume!(policy.validate().is_ok());
        let rung = [LadderRung::SubmitFast, LadderRung::Unverified][which];
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
