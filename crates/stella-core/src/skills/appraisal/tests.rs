//! Unit and property tests for skill appraisal: both directions of the gate,
//! and the rule that hand-authored skills are never touched.

use proptest::prelude::*;

use super::*;

fn trial(task: &str, selected: bool, succeeded: bool) -> SkillTrial {
    SkillTrial {
        task: task.to_string(),
        selected,
        outcome: TaskOutcome {
            succeeded,
            cost_usd: 0.10,
            tokens: 1_000,
            retries: 0,
        },
        turns: 4,
    }
}

/// `n` trials of one task on one arm.
fn arm(task: &str, selected: bool, succeeded: bool, n: usize) -> Vec<SkillTrial> {
    (0..n).map(|_| trial(task, selected, succeeded)).collect()
}

/// A live window: one shared pairing key, `helped` successes on the with-skill
/// arm and `baseline` on the without-skill arm, each out of `n`.
fn window(n: usize, helped: usize, baseline: usize) -> Vec<SkillTrial> {
    let mut trials = Vec::new();
    for i in 0..n {
        trials.push(trial("live", true, i < helped));
        trials.push(trial("live", false, i < baseline));
    }
    trials
}

/// #1067's first fixture: a skill that measurably helps is promoted, and the
/// lift is on the record.
#[test]
fn a_skill_that_helps_is_promoted_with_its_lift_recorded() {
    let mut trials = arm("fix-git", false, false, 6);
    trials.extend(arm("fix-git", true, true, 6));

    let appraisal = appraise("witness-assertions", &trials, &AppraisalConfig::default());
    let SkillVerdict::Helps { lift } = appraisal.verdict else {
        panic!("expected Helps, got {:?}", appraisal.verdict);
    };
    assert!((lift - 1.0).abs() < 1e-9);
    assert!(appraisal.verdict.promotes());
    assert!(!appraisal.verdict.demotes());
    // The evidence a later reader needs is published whole: the arms, the
    // thresholds it cleared, and which task set it ran on.
    assert_eq!(appraisal.report.verdict.winner(), Some(WITH_SKILL_ARM));
    assert_eq!(appraisal.report.config.selection.confidence_z, 1.96);
    assert_eq!(appraisal.report.tasks.len(), 1);
    assert_eq!(appraisal.report.tasks[0].task, "fix-git");
}

/// #1067's second fixture: high observation frequency buys nothing. Thirty
/// trials a side with identical outcomes is a large sample and a zero lift,
/// and the verdict is that the skill contributes nothing — never a promotion.
#[test]
fn frequency_without_lift_never_promotes() {
    let trials = window(30, 20, 20);
    let appraisal = appraise("noise", &trials, &AppraisalConfig::default());
    assert!(!appraisal.verdict.promotes(), "{:?}", appraisal.verdict);
    assert!(
        matches!(appraisal.verdict, SkillVerdict::Inert { .. }),
        "a full window with no movement is inert: {:?}",
        appraisal.verdict
    );
}

/// The creation gate: without a measured lift, a candidate is held rather than
/// written — and held is not dropped, so the reason distinguishes "measured
/// and found wanting" from "nobody looked".
#[test]
fn the_creation_gate_holds_an_unevaluated_candidate() {
    let candidate = crate::skills::SkillCandidate {
        name: "unproven".into(),
        description: "d".into(),
        domains: vec![],
        body: "b".into(),
        occurrences: 30,
        salient: true,
        evidence: vec![],
        score: 99.0,
    };
    // The gate ships OFF — see `AutoCreateConfig::require_measured_lift` for
    // why: the evidence source it would gate on does not exist yet, and a
    // default that no user action can satisfy is a feature-disabling default.
    // The mechanism is what ships complete, so this fixture arms it explicitly.
    assert!(!crate::skills::AutoCreateConfig::default().require_measured_lift);
    let config = crate::skills::AutoCreateConfig {
        require_measured_lift: true,
        ..crate::skills::AutoCreateConfig::default()
    };

    for evidence in [EvalEvidence::Unevaluated, EvalEvidence::NoLift] {
        assert_eq!(
            crate::skills::decide_auto_creation(
                &candidate,
                ".stella/skills",
                &[],
                0,
                evidence,
                &config,
            ),
            crate::skills::AutoCreateDecision::Skip {
                reason: crate::skills::AutoCreateSkip::AwaitingEvaluation { evidence }
            },
            "{evidence:?} must not mint a skill"
        );
    }
    // A measured lift is what opens it.
    assert_eq!(
        crate::skills::decide_auto_creation(
            &candidate,
            ".stella/skills",
            &[],
            0,
            EvalEvidence::MeasuredLift,
            &config,
        ),
        crate::skills::AutoCreateDecision::Create {
            path: ".stella/skills/unproven.md".into()
        }
    );
}

/// With the gate off — the shipped default — the loop behaves exactly as it
/// did before #1067. This is the compatibility half of the change: the
/// mechanism is present and the observable behavior has not moved.
#[test]
fn the_gate_off_is_the_pre_1067_loop() {
    let candidate = crate::skills::SkillCandidate {
        name: "unproven".into(),
        description: "d".into(),
        domains: vec![],
        body: "b".into(),
        occurrences: 3,
        salient: false,
        evidence: vec![],
        score: 1.0,
    };
    assert_eq!(
        crate::skills::decide_auto_creation(
            &candidate,
            ".stella/skills",
            &[],
            0,
            EvalEvidence::Unevaluated,
            &crate::skills::AutoCreateConfig::default(),
        ),
        crate::skills::AutoCreateDecision::Create {
            path: ".stella/skills/unproven.md".into()
        }
    );
}

/// #1068's fixture: a skill whose window regresses is demoted, and the
/// evidence window travels with the decision so the demotion is auditable.
#[test]
fn a_regressing_skill_is_demoted_with_its_evidence() {
    // The with-skill arm solves nothing; the without-skill arm solves
    // everything. Removing the skill confidently wins.
    let trials = window(8, 0, 8);
    let appraisal = appraise("stale-convention", &trials, &AppraisalConfig::default());

    let SkillVerdict::Harms { lift } = appraisal.verdict else {
        panic!("expected Harms, got {:?}", appraisal.verdict);
    };
    assert!((lift - 1.0).abs() < 1e-9);
    assert!(appraisal.verdict.demotes());
    let harm = appraisal.harm.as_ref().expect("the harm evidence is kept");
    assert_eq!(harm.arm, WITHOUT_SKILL_ARM);
    assert_eq!(harm.promotion.winner.n, 8);
    assert_eq!(harm.promotion.baseline.n, 8);

    assert_eq!(
        decide_demotion(SkillOrigin::AutoCreated, &appraisal),
        DemotionDecision::Demote {
            reason: DemotionReason::Harmful { lift }
        }
    );
}

/// #1068's scope guard: a hand-authored skill with an identically regressing
/// window is not demoted. Checked for every origin that is not `AutoCreated`,
/// so a new origin is protected by default rather than by remembering.
#[test]
fn a_hand_authored_skill_is_never_auto_demoted() {
    let trials = window(8, 0, 8);
    let appraisal = appraise("house-style", &trials, &AppraisalConfig::default());
    assert!(
        appraisal.verdict.demotes(),
        "the fixture's window really does regress"
    );

    for origin in [
        SkillOrigin::Workspace,
        SkillOrigin::User,
        SkillOrigin::Installed,
    ] {
        assert_eq!(
            decide_demotion(origin, &appraisal),
            DemotionDecision::Keep {
                reason: KeepSkillReason::HandAuthored
            },
            "{origin:?} must be left alone"
        );
    }
    // Only the auto-created one moves.
    assert!(decide_demotion(SkillOrigin::AutoCreated, &appraisal).is_demotion());
}

/// A short window never demotes: "we have not measured anything" must not be
/// read as "it does not help". This is the difference between a retirement
/// mechanism and a random one.
#[test]
fn a_short_window_is_insufficient_not_inert() {
    // Six a side clears `min_samples_per_arm` but not the 20-trial window.
    let trials = window(6, 4, 4);
    let appraisal = appraise("quiet", &trials, &AppraisalConfig::default());
    assert!(
        matches!(appraisal.verdict, SkillVerdict::Insufficient { .. }),
        "got {:?}",
        appraisal.verdict
    );
    assert!(!appraisal.verdict.demotes());
    assert_eq!(
        decide_demotion(SkillOrigin::AutoCreated, &appraisal),
        DemotionDecision::Keep {
            reason: KeepSkillReason::NotEnoughEvidence
        }
    );
}

/// A one-sided window — a skill selected on every turn, or on none — measures
/// nothing, and says so as an absent *control* rather than as a missing
/// baseline arm, which would read as a caller error.
///
/// Both arms report zero, and that is the pairing rule doing its job: with no
/// control turn for the task, there is nothing to compare the treated turns
/// against, so the task is excluded and named. A skill selected on literally
/// every turn is unmeasurable, and this is the shape of that fact.
#[test]
fn a_one_sided_window_measures_nothing() {
    for selected in [true, false] {
        let trials = arm("live", selected, true, 10);
        let appraisal = appraise("always-on", &trials, &AppraisalConfig::default());
        let SkillVerdict::Insufficient {
            reason: KeepReason::InsufficientSamples { n, .. },
        } = &appraisal.verdict
        else {
            panic!("expected InsufficientSamples, got {:?}", appraisal.verdict);
        };
        assert_eq!(*n, 0, "no trial is paired without a control");
        assert_eq!(
            appraisal.report.unpaired_tasks,
            vec!["live".to_string()],
            "the task with no control is named, never silently dropped"
        );
        assert!(
            !appraisal.verdict.demotes(),
            "unmeasurable is not unhelpful"
        );
    }
}

/// A skill that helps but regresses a configured guard is held, not promoted —
/// and not demoted either. The lift is real; the price is the operator's call.
#[test]
fn a_guard_regression_holds_a_helpful_skill() {
    let mut trials = Vec::new();
    for _ in 0..8 {
        trials.push(SkillTrial {
            outcome: TaskOutcome {
                succeeded: false,
                cost_usd: 0.10,
                tokens: 1_000,
                retries: 0,
            },
            ..trial("live", false, false)
        });
        trials.push(SkillTrial {
            outcome: TaskOutcome {
                succeeded: true,
                cost_usd: 0.90,
                tokens: 1_000,
                retries: 0,
            },
            ..trial("live", true, true)
        });
    }
    let config = AppraisalConfig {
        guards: vec![Guard::strict(Metric::CostUsd)],
        ..AppraisalConfig::default()
    };
    let appraisal = appraise("expensive", &trials, &config);
    assert!(
        matches!(appraisal.verdict, SkillVerdict::GuardBlocked { .. }),
        "got {:?}",
        appraisal.verdict
    );
    assert!(!appraisal.verdict.promotes());
    assert!(!appraisal.verdict.demotes());
    assert_eq!(
        decide_demotion(SkillOrigin::AutoCreated, &appraisal),
        DemotionDecision::Keep {
            reason: KeepSkillReason::Helping
        }
    );
    // The gate reads the verdict, and a blocked lift does not open it.
    assert_eq!(
        EvalEvidence::from_verdict(&appraisal.verdict),
        EvalEvidence::NoLift
    );
}

/// Guards are not applied in reverse: a skill that is making turns worse does
/// not keep its place because removing it would also cost tokens.
#[test]
fn a_harmful_skill_is_demoted_even_under_a_guard_set() {
    let trials = window(8, 0, 8);
    let config = AppraisalConfig {
        guards: vec![
            Guard::strict(Metric::CostUsd),
            Guard::strict(Metric::Tokens),
        ],
        ..AppraisalConfig::default()
    };
    let appraisal = appraise("stale", &trials, &config);
    assert!(matches!(appraisal.verdict, SkillVerdict::Harms { .. }));
}

/// Invariant #4: the appraisal round-trips, so it can be persisted as the
/// promoted skill's provenance.
#[test]
fn an_appraisal_round_trips() {
    let trials = window(8, 8, 0);
    let appraisal = appraise("helper", &trials, &AppraisalConfig::default());
    let json = serde_json::to_string(&appraisal).unwrap();
    let back: SkillAppraisal = serde_json::from_str(&json).unwrap();
    assert_eq!(appraisal, back);
    assert!(json.contains("\"verdict\":\"helps\""));
}

/// The gate reads a verdict rather than a scalar, so there is only ever one
/// threshold in play.
#[test]
fn the_gate_reads_the_verdict_not_a_second_threshold() {
    assert_eq!(
        EvalEvidence::from_verdict(&SkillVerdict::Helps { lift: 0.000_1 }),
        EvalEvidence::MeasuredLift,
        "the comparison's own min_effect already decided this"
    );
    for verdict in [
        SkillVerdict::Harms { lift: 1.0 },
        SkillVerdict::Inert { trials: 40 },
        SkillVerdict::GuardBlocked { lift: 1.0 },
    ] {
        assert_eq!(EvalEvidence::from_verdict(&verdict), EvalEvidence::NoLift);
    }
}

fn arb_trial() -> impl Strategy<Value = SkillTrial> {
    (
        prop::sample::select(vec!["alpha", "beta"]),
        any::<bool>(),
        any::<bool>(),
        0.0f64..2.0,
        0u32..20,
    )
        .prop_map(|(task, selected, succeeded, cost_usd, turns)| SkillTrial {
            task: task.to_string(),
            selected,
            outcome: TaskOutcome {
                succeeded,
                cost_usd,
                tokens: 1_000,
                retries: 0,
            },
            turns,
        })
}

proptest! {
    /// Appraisal is total for any window, including empty and one-sided ones.
    #[test]
    fn appraisal_never_panics(trials in proptest::collection::vec(arb_trial(), 0..40)) {
        let _ = appraise("s", &trials, &AppraisalConfig::default());
    }

    /// A skill is never both promotable and demotable — the two directions of
    /// one gate cannot fire together.
    #[test]
    fn a_verdict_never_points_both_ways(
        trials in proptest::collection::vec(arb_trial(), 0..60),
    ) {
        let appraisal = appraise("s", &trials, &AppraisalConfig::default());
        prop_assert!(!(appraisal.verdict.promotes() && appraisal.verdict.demotes()));
    }

    /// Whatever the numbers say, a skill nobody auto-created is kept.
    #[test]
    fn no_window_can_demote_a_hand_authored_skill(
        trials in proptest::collection::vec(arb_trial(), 0..60),
    ) {
        let appraisal = appraise("s", &trials, &AppraisalConfig::default());
        for origin in [SkillOrigin::Workspace, SkillOrigin::User, SkillOrigin::Installed] {
            prop_assert_eq!(
                decide_demotion(origin, &appraisal),
                DemotionDecision::Keep { reason: KeepSkillReason::HandAuthored }
            );
        }
    }

    /// Same window, same appraisal.
    #[test]
    fn appraisal_is_deterministic(
        trials in proptest::collection::vec(arb_trial(), 0..40),
    ) {
        let config = AppraisalConfig::default();
        prop_assert_eq!(
            serde_json::to_string(&appraise("s", &trials, &config)).unwrap(),
            serde_json::to_string(&appraise("s", &trials, &config)).unwrap()
        );
    }
}
