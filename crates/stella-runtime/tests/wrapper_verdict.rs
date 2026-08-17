//! `judge` and `again` are total, and this is where that is proved rather than
//! asserted.
//!
//! Totality is the load-bearing property, not a nicety: `judge` has no
//! `Result` to hand a caller and no `.await` to hang an escalation on, so every
//! shape of evidence must already have an answer — and every answer must be
//! one a host can act on. The evidence vocabulary is closed and small
//! (`FlipObservation` × `TamperFinding` × a measurement map), which is what
//! makes "for every input" an enumeration here instead of an argument.
//!
//! The one thing these tests refuse to do is restate the implementation. The
//! exhaustive pass asserts *properties* — `Met` is never returned without the
//! evidence that earns it, an abstention never hides a determinate failure —
//! and the exact answers are pinned as a **table**, which is data a reviewer
//! can read against `doc:wrapper-socket` §5 rather than a second copy of the
//! same `match`.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use stella_plugin::{
    Continuation, EvidenceSet, FlipObservation, FlipPolicy, LoopGrant, Oracle, OracleCheck,
    OracleCommand, Outcome, Participation, RoundState, StopReason, TamperFinding, TamperPolicy,
    UndecidedReason, UnmetBecause, Verdict, VerdictRule,
};
use stella_runtime::wrapper::{again, judge};

const REQUIREMENT: &str = "proven";
const STATEMENT: &str = "the change is proven by the witness";

fn oracle(flip: FlipPolicy, checks: Vec<OracleCheck>, measurements: Vec<String>) -> Oracle {
    Oracle {
        command: Some(OracleCommand {
            argv: vec!["oracle".into()],
            timeout_secs: 60,
        }),
        flip,
        tamper: TamperPolicy::ArtifactIdentity,
        measurements,
        checks,
    }
}

/// A rule whose single requirement is decided by the flip alone.
fn flip_rule(flip: FlipPolicy) -> VerdictRule {
    VerdictRule {
        requirements: BTreeMap::from([(REQUIREMENT.to_string(), STATEMENT.to_string())]),
        oracle: Some(oracle(flip, Vec::new(), Vec::new())),
    }
}

/// A rule whose single requirement is decided by a budget under the declared
/// `flip` policy — `not-applicable` for the falsifier's performance-budget
/// shape, `required` for the conjunction #3510 is about.
fn budget_rule_under(flip: FlipPolicy) -> VerdictRule {
    VerdictRule {
        requirements: BTreeMap::from([(REQUIREMENT.to_string(), STATEMENT.to_string())]),
        oracle: Some(oracle(
            flip,
            vec![OracleCheck {
                requirement: REQUIREMENT.into(),
                rule: "p50 <= 105".into(),
            }],
            vec!["p50".into()],
        )),
    }
}

/// A rule whose single requirement is decided by a budget alone.
fn budget_rule() -> VerdictRule {
    budget_rule_under(FlipPolicy::NotApplicable)
}

fn evidence(flip: FlipObservation, tamper: TamperFinding) -> EvidenceSet {
    EvidenceSet {
        flip,
        tamper,
        measurements: BTreeMap::new(),
    }
}

const FLIPS: [FlipObservation; 5] = [
    FlipObservation::NotAttempted,
    FlipObservation::Achieved,
    FlipObservation::NotAchieved,
    FlipObservation::Unsatisfiable,
    FlipObservation::Unobservable,
];

fn tampers() -> Vec<TamperFinding> {
    vec![
        TamperFinding::Clean,
        TamperFinding::NotChecked,
        TamperFinding::Tampered {
            artifact: "tests/witness.rs".into(),
        },
    ]
}

/// Every point of the evidence vocabulary, against every rule shape. `judge`
/// answers for all of them, and never answers [`Verdict::Met`] without the
/// evidence that earns it.
#[test]
fn judge_is_total_over_the_evidence_vocabulary() {
    let rules = [
        ("flip required", flip_rule(FlipPolicy::Required)),
        ("flip not applicable", flip_rule(FlipPolicy::NotApplicable)),
        ("budget", budget_rule()),
        (
            "flip required + budget",
            budget_rule_under(FlipPolicy::Required),
        ),
    ];
    let measurement_sets = [
        ("absent", BTreeMap::new()),
        ("within budget", BTreeMap::from([("p50".to_string(), 100)])),
        ("over budget", BTreeMap::from([("p50".to_string(), 118)])),
        ("unrelated", BTreeMap::from([("p99".to_string(), 100)])),
    ];

    for (rule_name, rule) in &rules {
        for flip in FLIPS {
            for tamper in tampers() {
                for (set_name, measurements) in &measurement_sets {
                    let evidence = EvidenceSet {
                        flip,
                        tamper: tamper.clone(),
                        measurements: measurements.clone(),
                    };
                    let verdict = judge(rule, &evidence);
                    let case = format!("{rule_name} / {flip:?} / {tamper:?} / {set_name}");

                    if verdict == Verdict::Met {
                        // The two halves **conjoin** (#3510). A rule that
                        // declares both must satisfy both; declaring a check
                        // does not excuse the requirement from the flip.
                        let flip_required = rule
                            .oracle
                            .as_ref()
                            .is_some_and(|o| o.flip == FlipPolicy::Required);
                        let checked = rule.oracle.as_ref().is_some_and(|o| o.decides(REQUIREMENT));
                        let flip_earned =
                            flip == FlipObservation::Achieved && tamper == TamperFinding::Clean;
                        let budget_earned = measurements.get("p50").is_some_and(|&v| v <= 105);
                        let earned = match (flip_required, checked) {
                            (true, true) => flip_earned && budget_earned,
                            (true, false) => flip_earned,
                            (false, true) => budget_earned,
                            (false, false) => false,
                        };
                        assert!(earned, "Met without evidence that earns it: {case}");
                    }

                    // Whatever the answer, it must round-trip: a verdict is
                    // reported to a remote host and folded into the journal.
                    let json = serde_json::to_string(&verdict).expect("a verdict serializes");
                    assert_eq!(
                        serde_json::from_str::<Verdict>(&json).expect("and parses"),
                        verdict,
                        "{case}"
                    );
                }
            }
        }
    }
}

/// The exact answers for a flip-decided requirement, as a table. Fifteen rows,
/// one per point of the closed vocabulary — readable against
/// `doc:wrapper-socket` §5 rather than against the implementation.
#[test]
fn a_flip_decided_requirement_answers_exactly_this() {
    let rule = flip_rule(FlipPolicy::Required);
    let clean = TamperFinding::Clean;
    let unchecked = TamperFinding::NotChecked;
    let tampered = TamperFinding::Tampered {
        artifact: "tests/witness.rs".into(),
    };

    let unmet = |because: UnmetBecause| Verdict::Unmet {
        unmet: vec![stella_plugin::UnmetRequirement {
            requirement: REQUIREMENT.into(),
            statement: STATEMENT.into(),
            because,
        }],
    };
    let undecided = |reason: UndecidedReason| Verdict::Undecided { reason };
    let no_flip = |observed| unmet(UnmetBecause::NoFlip { observed });

    let table: Vec<(FlipObservation, TamperFinding, Verdict)> = vec![
        // A flip, and the artifacts are the ones that were authored.
        (FlipObservation::Achieved, clean.clone(), Verdict::Met),
        // A flip whose artifacts were rewritten is not a flip. This is the
        // whole of the tamper exclusion: a worker that edits the witness must
        // not be able to win by it.
        (
            FlipObservation::Achieved,
            tampered.clone(),
            unmet(UnmetBecause::Tampered {
                artifact: "tests/witness.rs".into(),
            }),
        ),
        // A flip nobody checked for tampering cannot be credited *or* refused
        // under a policy that demands the snapshot.
        (
            FlipObservation::Achieved,
            unchecked.clone(),
            undecided(UndecidedReason::TamperUnchecked),
        ),
        // No flip is a determinate failure whatever the tamper finding says —
        // the flip is read first, so a run whose witness never flipped is not
        // reported as a snapshot problem.
        (
            FlipObservation::NotAchieved,
            clean.clone(),
            no_flip(FlipObservation::NotAchieved),
        ),
        (
            FlipObservation::NotAchieved,
            unchecked.clone(),
            no_flip(FlipObservation::NotAchieved),
        ),
        (
            FlipObservation::NotAchieved,
            tampered.clone(),
            no_flip(FlipObservation::NotAchieved),
        ),
        // A witness that fails identically on both sides says nothing about
        // the work, and a worker cannot repair an instrument.
        (
            FlipObservation::Unsatisfiable,
            clean.clone(),
            undecided(UndecidedReason::WitnessUnsatisfiable),
        ),
        (
            FlipObservation::Unsatisfiable,
            unchecked.clone(),
            undecided(UndecidedReason::WitnessUnsatisfiable),
        ),
        (
            FlipObservation::Unsatisfiable,
            tampered.clone(),
            undecided(UndecidedReason::WitnessUnsatisfiable),
        ),
        // Nothing looked, so nothing is claimed.
        (
            FlipObservation::Unobservable,
            clean.clone(),
            undecided(UndecidedReason::FlipUnobservable),
        ),
        (
            FlipObservation::Unobservable,
            unchecked.clone(),
            undecided(UndecidedReason::FlipUnobservable),
        ),
        (
            FlipObservation::Unobservable,
            tampered.clone(),
            undecided(UndecidedReason::FlipUnobservable),
        ),
        (
            FlipObservation::NotAttempted,
            clean,
            undecided(UndecidedReason::FlipUnobservable),
        ),
        (
            FlipObservation::NotAttempted,
            unchecked,
            undecided(UndecidedReason::FlipUnobservable),
        ),
        (
            FlipObservation::NotAttempted,
            tampered,
            undecided(UndecidedReason::FlipUnobservable),
        ),
    ];

    assert_eq!(
        table.len(),
        FLIPS.len() * tampers().len(),
        "the table must cover every point of the vocabulary"
    );
    for (flip, tamper, expected) in table {
        assert_eq!(
            judge(&rule, &evidence(flip, tamper.clone())),
            expected,
            "{flip:?} / {tamper:?}"
        );
    }
}

/// A number the oracle failed to report is *missing*, never a satisfied
/// budget. This is the one silent failure the whole apparatus exists to
/// refuse, and it is the arm a `Result`-returning `judge` would let a caller
/// ignore.
#[test]
fn a_missing_measurement_abstains_rather_than_passing() {
    let verdict = judge(&budget_rule(), &EvidenceSet::unobserved());
    assert_eq!(
        verdict,
        Verdict::Undecided {
            reason: UndecidedReason::MeasurementMissing {
                requirement: REQUIREMENT.into(),
                measurement: "p50".into(),
            }
        }
    );
}

/// The arms only a rule that skipped the manifest's own validation can reach.
/// Each is rejected at load — and each still abstains here, because "the
/// constructor was bypassed" must not mean "silently met".
#[test]
fn a_rule_that_never_passed_validation_still_cannot_be_silently_met() {
    // Requirements with no oracle: nothing could establish them.
    let orphaned = VerdictRule {
        requirements: BTreeMap::from([(REQUIREMENT.to_string(), STATEMENT.to_string())]),
        oracle: None,
    };
    assert_eq!(
        judge(&orphaned, &EvidenceSet::unobserved()),
        Verdict::Undecided {
            reason: UndecidedReason::NoOracle
        }
    );

    // `flip = "not-applicable"` with a requirement no check decides —
    // `ManifestError::UndecidableRequirement` at load.
    assert_eq!(
        judge(
            &flip_rule(FlipPolicy::NotApplicable),
            &evidence(FlipObservation::Achieved, TamperFinding::Clean)
        ),
        Verdict::Undecided {
            reason: UndecidedReason::Undecidable {
                requirement: REQUIREMENT.into()
            }
        }
    );

    // A check outside the closed grammar — `ManifestError::UnparsableCheck`.
    let unreadable = VerdictRule {
        requirements: BTreeMap::from([(REQUIREMENT.to_string(), STATEMENT.to_string())]),
        oracle: Some(oracle(
            FlipPolicy::NotApplicable,
            vec![OracleCheck {
                requirement: REQUIREMENT.into(),
                rule: "p50<=105".into(),
            }],
            vec!["p50".into()],
        )),
    };
    assert!(
        matches!(
            judge(&unreadable, &EvidenceSet::unobserved()),
            Verdict::Undecided {
                reason: UndecidedReason::UnreadableCheck { .. }
            }
        ),
        "an unparsable rule must not read as a satisfied one"
    );
}

/// A wrapper with nothing to prove is met. Inventing a requirement for a
/// steering-grade wrapper that only contributes context would be the host
/// deciding done.
#[test]
fn a_rule_with_no_requirements_is_met() {
    assert_eq!(
        judge(&VerdictRule::default(), &EvidenceSet::unobserved()),
        Verdict::Met
    );
}

/// A determinate failure outranks an abstention. Reporting `Met` because one
/// clause went unmeasured is the false claim; reporting the failure is
/// actionable, and the undecidable clause is still not passed.
#[test]
fn a_determinate_failure_outranks_an_undecidable_clause() {
    let rule = VerdictRule {
        requirements: BTreeMap::from([
            (
                "budgeted".to_string(),
                "the p50 is inside budget".to_string(),
            ),
            (
                "measured".to_string(),
                "the p99 is inside budget".to_string(),
            ),
        ]),
        oracle: Some(oracle(
            FlipPolicy::NotApplicable,
            vec![
                OracleCheck {
                    requirement: "budgeted".into(),
                    rule: "p50 <= 105".into(),
                },
                OracleCheck {
                    requirement: "measured".into(),
                    rule: "p99 <= 130".into(),
                },
            ],
            vec!["p50".into(), "p99".into()],
        )),
    };
    // p50 is over budget; p99 was never reported.
    let verdict = judge(
        &rule,
        &EvidenceSet {
            flip: FlipObservation::NotAttempted,
            tamper: TamperFinding::NotChecked,
            measurements: BTreeMap::from([("p50".to_string(), 118)]),
        },
    );
    let Verdict::Unmet { unmet } = verdict else {
        panic!("a measured failure must be reported, got {verdict:?}");
    };
    assert_eq!(unmet.len(), 1);
    assert_eq!(unmet[0].requirement, "budgeted");
}

/// The allowance is the plugin's ask **clamped to the host's ceiling**, and a
/// spent allowance completes with the unmet clauses reported.
#[test]
fn the_hold_allowance_is_the_ask_clamped_to_the_hosts_ceiling() {
    let verdict = judge(
        &budget_rule(),
        &EvidenceSet {
            flip: FlipObservation::NotAttempted,
            tamper: TamperFinding::NotChecked,
            measurements: BTreeMap::from([("p50".to_string(), 118)]),
        },
    );
    let arbiter = |ask: Option<u32>| LoopGrant {
        participation: Participation::Arbiter,
        hooks: vec![stella_plugin::HookEvent::Stop],
        points: vec![stella_plugin::WrapperPoint::AfterTurn],
        calls: Vec::new(),
        max_calls: None,
        max_holds: ask,
    };
    let round = |spent: u32, ceiling: u32| RoundState {
        holds_spent: spent,
        host_max_holds: ceiling,
    };

    // An ask above the ceiling gets the ceiling, and the report says which
    // number bound it.
    let spent = again(&verdict, &round(2, 2), &arbiter(Some(9)));
    assert_eq!(
        spent,
        Continuation::Stop {
            outcome: Outcome::Unmet {
                unmet: match &verdict {
                    Verdict::Unmet { unmet } => unmet.clone(),
                    other => panic!("expected an unmet verdict, got {other:?}"),
                },
                stopped: StopReason::AllowanceSpent {
                    spent: 2,
                    allowed: 2
                },
            }
        }
    );

    // Below the clamped allowance, it asks again.
    assert!(matches!(
        again(&verdict, &round(1, 2), &arbiter(Some(9))),
        Continuation::Again { .. }
    ));

    // A manifest that asks for nothing inherits the ceiling rather than zero.
    assert!(matches!(
        again(&verdict, &round(0, 1), &arbiter(None)),
        Continuation::Again { .. }
    ));
    assert!(matches!(
        again(&verdict, &round(1, 1), &arbiter(None)),
        Continuation::Stop { .. }
    ));

    // A ceiling of zero means no wrapper holds anything, whatever it asked.
    assert!(matches!(
        again(&verdict, &round(0, 0), &arbiter(Some(9))),
        Continuation::Stop {
            outcome: Outcome::Unmet {
                stopped: StopReason::AllowanceSpent {
                    spent: 0,
                    allowed: 0
                },
                ..
            }
        }
    ));
}

/// An abstention is terminal. There is no correction to author from evidence
/// that decided nothing, and re-running against a broken instrument spends a
/// model call to learn the same non-answer.
#[test]
fn an_abstention_does_not_buy_another_turn() {
    let verdict = judge(
        &flip_rule(FlipPolicy::Required),
        &evidence(FlipObservation::Unsatisfiable, TamperFinding::Clean),
    );
    let continuation = again(
        &verdict,
        &RoundState {
            holds_spent: 0,
            host_max_holds: 8,
        },
        &LoopGrant {
            participation: Participation::Arbiter,
            hooks: vec![stella_plugin::HookEvent::Stop],
            points: vec![stella_plugin::WrapperPoint::AfterTurn],
            calls: Vec::new(),
            max_calls: None,
            max_holds: Some(8),
        },
    );
    assert_eq!(
        continuation,
        Continuation::Stop {
            outcome: Outcome::Undecided {
                reason: UndecidedReason::WitnessUnsatisfiable
            }
        }
    );
}

/// #3510's witness. A manifest declaring `flip = "required"`, `tamper =
/// "artifact-identity"` **and** a `[[oracle.checks]]` entry on the same
/// requirement conjoins all three; the check does not stand in for the other
/// two.
///
/// Before the fix, `judge` treated any requirement a check named as decided by
/// that check alone, so this rule answered [`Verdict::Met`] for a run that
/// never went red→green and whose witness file had been rewritten — a plugin
/// declaring one trivial budget could buy itself a completion. That is the
/// worker grading its own work (`doc:pipeline-as-plugins` §6), reached without
/// a model call, which is why `judge`'s purity did not catch it.
#[test]
fn a_check_does_not_excuse_a_requirement_from_the_flip_or_the_tamper_exclusion() {
    let rule = budget_rule_under(FlipPolicy::Required);
    let within_budget = BTreeMap::from([("p50".to_string(), 100)]);
    let unmet = |because: UnmetBecause| Verdict::Unmet {
        unmet: vec![stella_plugin::UnmetRequirement {
            requirement: REQUIREMENT.into(),
            statement: STATEMENT.into(),
            because,
        }],
    };

    // The exact repro: a passing budget, no flip, and a rewritten witness.
    assert_eq!(
        judge(
            &rule,
            &EvidenceSet {
                flip: FlipObservation::NotAchieved,
                tamper: TamperFinding::Tampered {
                    artifact: "tests/witness.rs".into()
                },
                measurements: within_budget.clone(),
            }
        ),
        unmet(UnmetBecause::NoFlip {
            observed: FlipObservation::NotAchieved
        }),
        "a declared check must not void the flip requirement"
    );

    // Tampering alone, with the flip observed and the budget held.
    assert_eq!(
        judge(
            &rule,
            &EvidenceSet {
                flip: FlipObservation::Achieved,
                tamper: TamperFinding::Tampered {
                    artifact: "tests/witness.rs".into()
                },
                measurements: within_budget.clone(),
            }
        ),
        unmet(UnmetBecause::Tampered {
            artifact: "tests/witness.rs".into()
        }),
        "a declared check must not void the tamper exclusion"
    );

    // A tamper snapshot nobody took still abstains rather than crediting.
    assert_eq!(
        judge(
            &rule,
            &EvidenceSet {
                flip: FlipObservation::Achieved,
                tamper: TamperFinding::NotChecked,
                measurements: within_budget.clone(),
            }
        ),
        Verdict::Undecided {
            reason: UndecidedReason::TamperUnchecked
        }
    );

    // The conjunction narrows, it does not forbid: both halves satisfied is
    // still `Met`, so a plugin declaring a budget alongside a flip stays
    // expressible.
    assert_eq!(
        judge(
            &rule,
            &EvidenceSet {
                flip: FlipObservation::Achieved,
                tamper: TamperFinding::Clean,
                measurements: within_budget,
            }
        ),
        Verdict::Met
    );

    // ...and the check keeps its own force under a credited flip.
    assert_eq!(
        judge(
            &rule,
            &EvidenceSet {
                flip: FlipObservation::Achieved,
                tamper: TamperFinding::Clean,
                measurements: BTreeMap::from([("p50".to_string(), 118)]),
            }
        ),
        unmet(UnmetBecause::Budget {
            check: "p50 <= 105".into(),
            reported: 118
        })
    );
}

/// The requirements `judge` credits, read one at a time.
///
/// [`judge`] answers for a whole rule and folds the clauses together (a
/// determinate failure outranks an abstention), so there is no met *set* in
/// its return type. Asking the same question against a one-requirement rule
/// carrying the same oracle and the same evidence recovers it, and is faithful
/// because every clause is decided from the oracle and the evidence alone —
/// nothing about one requirement can change the answer for another.
fn met_requirements(rule: &VerdictRule, evidence: &EvidenceSet) -> BTreeSet<String> {
    rule.requirements
        .iter()
        .filter(|(name, statement)| {
            let one = VerdictRule {
                requirements: BTreeMap::from([((*name).clone(), (*statement).clone())]),
                oracle: rule.oracle.clone(),
            };
            judge(&one, evidence) == Verdict::Met
        })
        .map(|(name, _)| name.clone())
        .collect()
}

const NAMES: [&str; 3] = ["proven", "fast", "small"];
const MEASUREMENTS: [&str; 3] = ["p50", "p99", "ghost"];
const OPS: [&str; 6] = [">", ">=", "<", "<=", "==", "!="];

fn any_requirements() -> impl Strategy<Value = BTreeMap<String, String>> {
    prop::collection::btree_set(prop::sample::select(NAMES.as_slice()), 1..=NAMES.len()).prop_map(
        |names| {
            names
                .into_iter()
                .map(|n| (n.to_string(), format!("{n} is established")))
                .collect()
        },
    )
}

/// One check, occasionally written outside the closed grammar — an unreadable
/// rule is an arm of the input space too, and it must abstain rather than
/// credit.
fn any_check() -> impl Strategy<Value = OracleCheck> {
    (
        prop::sample::select(NAMES.as_slice()),
        prop::sample::select(MEASUREMENTS.as_slice()),
        prop::sample::select(OPS.as_slice()),
        0u64..200,
        prop::bool::weighted(0.1),
    )
        .prop_map(
            |(requirement, measurement, op, value, unreadable)| OracleCheck {
                requirement: requirement.to_string(),
                rule: if unreadable {
                    format!("{measurement}{op}{value}")
                } else {
                    format!("{measurement} {op} {value}")
                },
            },
        )
}

fn any_evidence() -> impl Strategy<Value = EvidenceSet> {
    let tamper = prop_oneof![
        Just(TamperFinding::Clean),
        Just(TamperFinding::NotChecked),
        Just(TamperFinding::Tampered {
            artifact: "tests/witness.rs".into()
        }),
    ];
    let measurements = prop::collection::btree_map(
        prop::sample::select(MEASUREMENTS.as_slice()).prop_map(str::to_string),
        0u64..200,
        0..MEASUREMENTS.len(),
    );
    (prop::sample::select(FLIPS.as_slice()), tamper, measurements).prop_map(
        |(flip, tamper, measurements)| EvidenceSet {
            flip,
            tamper,
            measurements,
        },
    )
}

proptest! {
    /// **A check narrows done; it never widens it.** This is the durable
    /// statement of #3510's invariant, and the reason it is a property rather
    /// than a case: it is quantified over every oracle a manifest can express,
    /// not over the one shape that happened to be reported.
    ///
    /// Adding an `[[oracle.checks]]` entry to any oracle, against any
    /// evidence, may only *remove* requirements from the met set. The single
    /// exemption is named and bounded: an oracle declaring
    /// `flip = "not-applicable"` contributes no conjunct at all, so a check is
    /// the sole decider of the requirement it names — and a manifest that
    /// leaves one undecided is refused at load
    /// (`ManifestError::UndecidableRequirement`), so that exemption cannot be
    /// reached through a validated manifest either. Under
    /// [`FlipPolicy::Required`] the exemption is empty, which is the defect
    /// exactly.
    #[test]
    fn a_check_can_only_narrow_the_met_set(
        requirements in any_requirements(),
        base in prop::collection::vec(any_check(), 0..3),
        extra in any_check(),
        evidence in any_evidence(),
        policy in prop::sample::select(vec![FlipPolicy::Required, FlipPolicy::NotApplicable]),
    ) {
        let declared: Vec<String> = MEASUREMENTS.iter().map(|m| (*m).to_string()).collect();
        let narrow = VerdictRule {
            requirements: requirements.clone(),
            oracle: Some(oracle(policy, base.clone(), declared.clone())),
        };
        let widened = VerdictRule {
            requirements,
            oracle: Some(oracle(
                policy,
                base.iter().cloned().chain(std::iter::once(extra)).collect(),
                declared,
            )),
        };

        let before = met_requirements(&narrow, &evidence);
        let after = met_requirements(&widened, &evidence);

        let exempt: BTreeSet<String> = if policy == FlipPolicy::NotApplicable {
            narrow
                .requirements
                .keys()
                .filter(|name| {
                    !narrow.oracle.as_ref().is_some_and(|o| o.decides(name))
                })
                .cloned()
                .collect()
        } else {
            BTreeSet::new()
        };

        let added: BTreeSet<&String> = after.difference(&before).collect();
        prop_assert!(
            added.iter().all(|name| exempt.contains(*name)),
            "a check widened done: {added:?} became met only once it was added \
             (policy {policy:?}, evidence {evidence:?})"
        );
    }
}
