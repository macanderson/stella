// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for the provenance grade (#2782).
//!
//! The first test in this file is the one the issue asks for by name: a
//! fixture of several agreeing model critiques must not reach a grade that
//! authorises publishing an executable tool. Everything else exists to keep
//! that answer from being reachable by accident through another door.

use super::*;

/// **#2782's named witness.** Laundering by aggregation, refused.
///
/// Fifty model critiques agreeing with each other is the exact shape a
/// vote-counting gate reads as strong evidence. It stays a model critique, and
/// the tool gate refuses it — with a refusal that names the witness it wanted.
#[test]
fn agreeing_model_critiques_never_authorise_an_executable_tool() {
    let pool = std::iter::repeat_n(ProvenanceGrade::ModelCritique, 50);

    let grade = ProvenanceGrade::weakest(pool);
    assert_eq!(
        grade,
        Some(ProvenanceGrade::ModelCritique),
        "agreement between critiques must not promote the grade"
    );

    // Even with the strongest authority available, the evidence half is short.
    let refusal = authorises(
        grade,
        PublicationAuthority::OrgPolicy,
        ImpactClass::ExecutableTool,
    )
    .expect_err("a tool published on model critique alone must be refused");

    assert_eq!(
        refusal,
        PromotionRefusal::EvidenceTooWeak {
            impact: ImpactClass::ExecutableTool,
            required: ProvenanceGrade::DeterministicProof,
            actual: ProvenanceGrade::ModelCritique,
        }
    );
    assert!(
        refusal.reason().contains("deterministic_proof"),
        "the refusal must name the evidence that would satisfy it: {}",
        refusal.reason()
    );
}

/// The same shape one rung down: a blocking guard is refused too, because it
/// can fail a gate in someone else's session.
#[test]
fn agreeing_model_critiques_never_authorise_a_blocking_guard() {
    let grade = ProvenanceGrade::weakest(std::iter::repeat_n(ProvenanceGrade::ModelCritique, 12));

    let refusal = authorises(
        grade,
        PublicationAuthority::LocalHuman,
        ImpactClass::BlockingGuard,
    )
    .expect_err("a blocking guard published on model critique alone must be refused");

    assert!(matches!(
        refusal,
        PromotionRefusal::EvidenceTooWeak { .. }
    ));
}

/// Rule 1, stated over every grade rather than only the dangerous one: no
/// amount of repetition moves any grade.
#[test]
fn repetition_never_promotes_any_grade() {
    for &grade in ProvenanceGrade::ALL {
        for count in [1_usize, 2, 3, 5, 100] {
            assert_eq!(
                ProvenanceGrade::weakest(std::iter::repeat_n(grade, count)),
                Some(grade),
                "{} repeated {count} times must remain {}",
                grade.as_str(),
                grade.as_str()
            );
        }
    }
}

/// A pool is only as strong as its weakest member — one deterministic proof
/// does not carry the critiques standing beside it.
#[test]
fn one_strong_source_does_not_lift_the_weak_ones_beside_it() {
    let mixed = [
        ProvenanceGrade::DeterministicProof,
        ProvenanceGrade::EnvironmentObservation,
        ProvenanceGrade::ModelCritique,
    ];

    assert_eq!(
        ProvenanceGrade::weakest(mixed),
        Some(ProvenanceGrade::ModelCritique)
    );
}

/// No evidence is not weak evidence, and must not round up to it.
#[test]
fn an_empty_pool_is_none_rather_than_the_weakest_grade() {
    assert_eq!(ProvenanceGrade::weakest(std::iter::empty()), None);

    let refusal = authorises(None, PublicationAuthority::OrgPolicy, ImpactClass::PromptHint)
        .expect_err("promoting with no evidence at all must be refused");
    assert_eq!(
        refusal,
        PromotionRefusal::NoEvidence {
            impact: ImpactClass::PromptHint
        }
    );
}

/// The strength order is the declaration order, and it is strict — a tie would
/// make `weakest` pick arbitrarily between two grades a reviewer reads as
/// different.
#[test]
fn the_grade_order_is_strictly_ascending() {
    assert!(
        ProvenanceGrade::ALL.windows(2).all(|pair| pair[0] < pair[1]),
        "ProvenanceGrade::ALL must be strictly weakest-first"
    );
    assert_eq!(ProvenanceGrade::ALL.first(), Some(&ProvenanceGrade::ModelCritique));
    assert_eq!(
        ProvenanceGrade::ALL.last(),
        Some(&ProvenanceGrade::DeterministicProof)
    );
    assert!(PublicationAuthority::ALL.windows(2).all(|p| p[0] < p[1]));
    assert!(ImpactClass::ALL.windows(2).all(|p| p[0] < p[1]));
}

/// Trajectory evidence trials a hint and nothing heavier — the two ends #2782
/// states, checked as a boundary rather than a spot value.
#[test]
fn trajectory_evidence_trials_a_hint_and_stops_below_a_guard() {
    let trajectory = Some(ProvenanceGrade::TrajectoryAbstraction);

    for impact in [
        ImpactClass::PromptHint,
        ImpactClass::RecallBias,
        ImpactClass::AdvisoryRecord,
    ] {
        assert!(
            authorises(trajectory, PublicationAuthority::Agent, impact).is_ok(),
            "{} may be trialled from trajectory evidence",
            impact.as_str()
        );
    }

    for impact in [ImpactClass::BlockingGuard, ImpactClass::ExecutableTool] {
        assert!(
            authorises(trajectory, PublicationAuthority::OrgPolicy, impact).is_err(),
            "{} must not be reachable from trajectory evidence",
            impact.as_str()
        );
    }
}

/// The authority half is real: deterministic proof alone does not publish the
/// two classes that can break a teammate's session.
#[test]
fn proof_without_authority_does_not_publish_a_tool_or_a_guard() {
    let proof = Some(ProvenanceGrade::DeterministicProof);

    for impact in [ImpactClass::BlockingGuard, ImpactClass::ExecutableTool] {
        let refusal = authorises(proof, PublicationAuthority::Agent, impact)
            .expect_err("the agent may not publish this on its own");
        assert_eq!(
            refusal,
            PromotionRefusal::AuthorityTooLow {
                impact,
                required: PublicationAuthority::LocalHuman,
                actual: PublicationAuthority::Agent,
            }
        );

        assert!(
            authorises(proof, PublicationAuthority::LocalHuman, impact).is_ok(),
            "a person signing off on proven evidence publishes {}",
            impact.as_str()
        );
    }
}

/// Every impact class is satisfiable by *something*, so no row is a dead end a
/// caller can never clear.
#[test]
fn every_impact_class_is_reachable_at_its_own_requirements() {
    for &impact in ImpactClass::ALL {
        assert!(
            authorises(
                Some(impact.required_grade()),
                impact.required_authority(),
                impact
            )
            .is_ok(),
            "{} must be satisfiable by exactly what it requires",
            impact.as_str()
        );
    }
}

/// One rung below the requirement always fails — the gate is a boundary, not a
/// suggestion. Checked for every class that has a rung below it.
#[test]
fn one_grade_below_the_requirement_is_always_refused() {
    for &impact in ImpactClass::ALL {
        let required = impact.required_grade();
        let Some(weaker) = ProvenanceGrade::ALL
            .iter()
            .copied()
            .filter(|grade| *grade < required)
            .next_back()
        else {
            continue;
        };

        assert!(
            authorises(Some(weaker), PublicationAuthority::OrgPolicy, impact).is_err(),
            "{} must refuse {}, one rung below its requirement",
            impact.as_str(),
            weaker.as_str()
        );
    }
}

/// Invariant #4: every type crossing a crate boundary round-trips through
/// `serde_json` byte-for-byte.
#[test]
fn every_tag_round_trips_byte_for_byte() {
    for &grade in ProvenanceGrade::ALL {
        let json = serde_json::to_string(&grade).expect("a grade serializes");
        assert_eq!(json, format!("\"{}\"", grade.as_str()));
        let back: ProvenanceGrade = serde_json::from_str(&json).expect("a grade deserializes");
        assert_eq!(back, grade);
    }

    for &authority in PublicationAuthority::ALL {
        let json = serde_json::to_string(&authority).expect("an authority serializes");
        assert_eq!(json, format!("\"{}\"", authority.as_str()));
        let back: PublicationAuthority = serde_json::from_str(&json).expect("it deserializes");
        assert_eq!(back, authority);
    }

    for &impact in ImpactClass::ALL {
        let json = serde_json::to_string(&impact).expect("an impact serializes");
        assert_eq!(json, format!("\"{}\"", impact.as_str()));
        let back: ImpactClass = serde_json::from_str(&json).expect("it deserializes");
        assert_eq!(back, impact);
    }

    let refusal = PromotionRefusal::EvidenceTooWeak {
        impact: ImpactClass::ExecutableTool,
        required: ProvenanceGrade::DeterministicProof,
        actual: ProvenanceGrade::ModelCritique,
    };
    let json = serde_json::to_string(&refusal).expect("a refusal serializes");
    let back: PromotionRefusal = serde_json::from_str(&json).expect("a refusal deserializes");
    assert_eq!(back, refusal);
}

/// A refusal a human cannot act on is a dead end: both sides of the comparison
/// have to appear in the prose.
#[test]
fn a_refusal_names_what_was_required_and_what_was_offered() {
    let weak = PromotionRefusal::EvidenceTooWeak {
        impact: ImpactClass::BlockingGuard,
        required: ProvenanceGrade::DeterministicProof,
        actual: ProvenanceGrade::TrajectoryAbstraction,
    };
    let reason = weak.reason();
    assert!(reason.contains("blocking_guard"), "{reason}");
    assert!(reason.contains("deterministic_proof"), "{reason}");
    assert!(reason.contains("trajectory_abstraction"), "{reason}");

    let low = PromotionRefusal::AuthorityTooLow {
        impact: ImpactClass::ExecutableTool,
        required: PublicationAuthority::LocalHuman,
        actual: PublicationAuthority::Agent,
    };
    let reason = low.reason();
    assert!(reason.contains("local_human"), "{reason}");
    assert!(reason.contains("agent"), "{reason}");
}

/// Human review is accountable and is still not a measurement — it must not
/// clear a bar that trajectory evidence clears.
#[test]
fn human_review_does_not_outrank_a_measured_pattern() {
    assert!(ProvenanceGrade::HumanReview < ProvenanceGrade::TrajectoryAbstraction);
    assert!(ProvenanceGrade::HumanReview > ProvenanceGrade::ModelCritique);

    assert!(
        authorises(
            Some(ProvenanceGrade::HumanReview),
            PublicationAuthority::LocalHuman,
            ImpactClass::PromptHint
        )
        .is_err(),
        "a sign-off is not evidence a hint helps"
    );
}
