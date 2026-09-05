// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for evidence grading and the pool that carries it (#2782).
//!
//! The hop these pin is observation → proposal, which is where the grade used
//! to be lost: a proposal keeps only its observations' ids, so anything not
//! carried across this boundary is unrecoverable downstream.

use stella_protocol::provenance::{ImpactClass, ProvenanceGrade, PublicationAuthority, authorises};

use super::*;
use crate::context_record::Confidence;
use crate::context_record::kind::{RecordProposalKind, RecordProposalStatus};
use crate::context_record::lifecycle::{ProposalRecord, ProposalScore};

fn observation(source: ObservationSource, task: &str, text: &str) -> ObservationRecord {
    ObservationRecord::new(
        source,
        format!("ref:{text}"),
        task,
        text,
        Vec::new(),
        false,
        "2026-08-25T00:00:00Z",
    )
    .expect("an observation hashes")
}

fn score(distinct_tasks: u32, occurrences: u32) -> ProposalScore {
    ProposalScore {
        occurrences,
        distinct_tasks,
        salient: false,
        rank: 1.0,
    }
}

fn proposal(evidence: Option<EvidencePool>) -> ProposalRecord {
    ProposalRecord::new(
        RecordProposalKind::Knowledge,
        RecordProposalStatus::Eligible,
        "candidate-abcd1234",
        "a title",
        "a body",
        Vec::new(),
        evidence,
        score(3, 3),
        Confidence::new(80).expect("a confidence"),
        "2026-08-25T00:00:00Z",
    )
    .expect("a proposal hashes")
}

/// Each source grades to exactly one thing, and the mapping is total — a new
/// `ObservationSource` fails to compile here rather than defaulting to
/// a grade nobody chose.
#[test]
fn every_observation_source_grades_to_one_named_thing() {
    assert_eq!(
        observation_grade(ObservationSource::ToolOutcome),
        ProvenanceGrade::EnvironmentObservation
    );
    assert_eq!(
        observation_grade(ObservationSource::ReflectionLesson),
        ProvenanceGrade::ModelCritique
    );
    assert_eq!(
        observation_grade(ObservationSource::MemoryCitation),
        ProvenanceGrade::ModelCritique
    );
}

/// **The hop.** A proposal built from real observations carries their folded
/// grade; nothing downstream has to reconstruct it.
#[test]
fn a_proposal_carries_the_grade_of_the_observations_behind_it() {
    let observations = [
        observation(ObservationSource::ToolOutcome, "task-a", "the build failed"),
        observation(
            ObservationSource::ToolOutcome,
            "task-b",
            "the build failed again",
        ),
    ];
    let pool = EvidencePool::from_observations(&observations)
        .expect("constructor-built observations hash clean")
        .expect("a non-empty pool");

    assert_eq!(pool.grade(), ProvenanceGrade::EnvironmentObservation);

    let proposal = proposal(Some(pool));
    assert_eq!(
        proposal.provenance,
        Some(ProvenanceGrade::EnvironmentObservation),
        "the grade must survive the hop from observations to a proposal"
    );
    assert_eq!(
        proposal.supporting_observations.len(),
        2,
        "the pool carries the observation ids across the same hop"
    );
}

/// **The laundering case, at the hop rather than in the abstract.** Three
/// reflection lessons agreeing across three separate tasks is the strongest
/// shape the mining path can produce from model critique alone — it clears the
/// distinct-task floor, so it is graded as the mined pattern it is, and it
/// still must not authorise a tool.
#[test]
fn three_agreeing_reflections_across_three_tasks_still_cannot_publish_a_tool() {
    let observations = [
        observation(ObservationSource::ReflectionLesson, "task-a", "prefer rg"),
        observation(
            ObservationSource::ReflectionLesson,
            "task-b",
            "prefer rg here too",
        ),
        observation(
            ObservationSource::ReflectionLesson,
            "task-c",
            "prefer rg again",
        ),
    ];
    let pool = EvidencePool::from_observations(&observations)
        .expect("constructor-built observations hash clean")
        .expect("a non-empty pool");
    let proposal = proposal(Some(pool));

    assert!(
        proposal.is_eligible(3, 3),
        "the fixture must clear the existing anti-poisoning floor, so the \
         refusal below is the grade's doing and not the floor's"
    );
    assert_eq!(
        proposal.provenance,
        Some(ProvenanceGrade::TrajectoryAbstraction),
        "a pattern across three distinct tasks is what that rung names"
    );

    let refusal = authorises(
        proposal.provenance,
        PublicationAuthority::LocalHuman,
        ImpactClass::ExecutableTool,
    )
    .expect_err("an eligible proposal mined from critique still cannot publish a tool");
    assert!(
        refusal.reason().contains("deterministic_proof"),
        "{}",
        refusal.reason()
    );

    authorises(
        proposal.provenance,
        PublicationAuthority::Agent,
        ImpactClass::PromptHint,
    )
    .expect("…and the hint it may be trialled as is now reachable");

    authorises(
        proposal.provenance,
        PublicationAuthority::LocalHuman,
        ImpactClass::SteeringDirective,
    )
    .expect_err("the lift caps below the grade a directive that steers requires");
}

/// One weak observation weakens a pool that is otherwise measured — the fold
/// is a floor, not an average, so a critique cannot be outvoted.
///
/// Spread across three tasks the pool is also a mined pattern and is graded as
/// one, which is still short of the measurement two thirds of it came from.
/// Recurrence buys the rung below; it never buys back the one the critique
/// cost.
#[test]
fn a_single_critique_weakens_a_pool_of_measurements() {
    let observations = [
        observation(ObservationSource::ToolOutcome, "task-a", "exit 1"),
        observation(ObservationSource::ToolOutcome, "task-b", "exit 1"),
        observation(ObservationSource::ReflectionLesson, "task-c", "felt slow"),
    ];
    let pool = EvidencePool::from_observations(&observations)
        .expect("constructor-built observations hash clean")
        .expect("a non-empty pool");

    assert_eq!(pool.grade(), ProvenanceGrade::TrajectoryAbstraction);
    assert!(pool.grade() < ProvenanceGrade::EnvironmentObservation);
}

/// The same three lessons inside **one** task do not lift, so the lift reads
/// distinct tasks and never occurrences — spec §7 at the grade rather than at
/// the eligibility floor.
#[test]
fn three_restatements_inside_one_task_do_not_lift() {
    let observations = [
        observation(ObservationSource::ReflectionLesson, "task-a", "prefer rg"),
        observation(ObservationSource::ReflectionLesson, "task-a", "rg again"),
        observation(
            ObservationSource::ReflectionLesson,
            "task-a",
            "rg once more",
        ),
    ];
    let pool = EvidencePool::from_observations(&observations)
        .expect("constructor-built observations hash clean")
        .expect("a non-empty pool");

    assert_eq!(pool.distinct_tasks(), 1);
    assert_eq!(pool.grade(), ProvenanceGrade::ModelCritique);
}

/// A pool of measurements spanning three tasks keeps the stronger grade it
/// already had — the lift is a `max`, so it can never talk evidence down.
#[test]
fn the_lift_never_weakens_a_pool_of_measurements() {
    let observations = [
        observation(ObservationSource::ToolOutcome, "task-a", "exit 1"),
        observation(ObservationSource::ToolOutcome, "task-b", "exit 1"),
        observation(ObservationSource::ToolOutcome, "task-c", "exit 1"),
    ];
    let pool = EvidencePool::from_observations(&observations)
        .expect("constructor-built observations hash clean")
        .expect("a non-empty pool");

    assert_eq!(pool.distinct_tasks(), 3);
    assert_eq!(pool.grade(), ProvenanceGrade::EnvironmentObservation);
}

/// No observations is no grade, and that has to reach the record rather than
/// being smoothed into the weakest one.
#[test]
fn a_proposal_with_no_observations_stores_no_grade() {
    assert_eq!(EvidencePool::from_observations(&[]), Ok(None));

    let proposal = proposal(None);
    assert_eq!(proposal.provenance, None);
    assert!(proposal.supporting_observations.is_empty());

    assert!(
        authorises(
            proposal.provenance,
            PublicationAuthority::OrgPolicy,
            ImpactClass::PromptHint
        )
        .is_err(),
        "absent evidence must be refused, not rounded down to a weak grade"
    );
}

/// A record written before provenance was carried deserializes with no grade
/// and **hashes to the same value it always did** — the null-stripping
/// preimage is what makes adding this field safe for records already on disk.
#[test]
fn a_pre_provenance_proposal_still_verifies_its_stored_hash() {
    let mut proposal = proposal(None);
    // Re-seal so the fixture is a record whose stored hash was computed
    // without the field, exactly as one written before #2782 would be.
    let stored = proposal.record_hash.clone();

    let json = serde_json::to_string(&proposal).expect("a proposal serializes");
    assert!(
        !json.contains("provenance"),
        "an absent grade must not appear on the wire, or every stored record's \
         hash moves: {json}"
    );

    let round_tripped: ProposalRecord = serde_json::from_str(&json).expect("it deserializes");
    assert_eq!(round_tripped.provenance, None);
    assert_eq!(round_tripped.record_hash, stored);

    proposal.record_hash = String::new();
    let recomputed = crate::context_record::record_hash(&proposal).expect("it hashes");
    assert_eq!(
        recomputed, stored,
        "adding an absent optional field must not move an existing record's hash"
    );
}

/// The grade is on the wire when it is present, under the tag the protocol
/// crate writes — so the observatory and the CLI read one spelling.
#[test]
fn a_graded_proposal_serializes_its_grade_under_the_protocol_tag() {
    let observations = [observation(
        ObservationSource::ToolOutcome,
        "task-a",
        "exit 0",
    )];
    let pool = EvidencePool::from_observations(&observations)
        .expect("constructor-built observations hash clean")
        .expect("a non-empty pool");
    let proposal = proposal(Some(pool));

    let json = serde_json::to_value(&proposal).expect("a proposal serializes");
    assert_eq!(json["provenance"], "environment_observation");
}

/// The pool re-derives every observation's `record_hash` before folding, so a
/// literal-constructed record — the fields are public — cannot mint an
/// environment-observation grade the hashing constructor never sealed.
#[test]
fn a_forged_observation_cannot_mint_a_grade() {
    let mut forged = observation(ObservationSource::ToolOutcome, "task-a", "exit 0");
    forged.text = "exit 0, with edits the hash never saw".to_string();
    let err = EvidencePool::from_observations(std::slice::from_ref(&forged))
        .expect_err("a hash that does not cover this content must refuse to fold");
    assert_eq!(err.record_id, forged.record_id);
    assert_eq!(err.stored, forged.record_hash);
}

/// One person confirming one proposal is the whole of `human_review` — the
/// system deciding is not a person, and a decision *against* a claim supplies
/// no evidence for it.
#[test]
fn only_a_person_confirming_supplies_a_review_grade() {
    assert_eq!(
        decision_grade(PromotionActor::User, PromotionAction::Confirmed),
        Some(ProvenanceGrade::HumanReview)
    );
    assert_eq!(
        decision_grade(PromotionActor::System, PromotionAction::Confirmed),
        None,
        "the loop acting under policy is not a person reading a claim"
    );
    assert_eq!(
        decision_grade(PromotionActor::User, PromotionAction::Rejected),
        None,
        "a decision against a claim is not evidence for it"
    );
    assert_eq!(
        decision_grade(PromotionActor::System, PromotionAction::AutoActivated),
        None
    );
}

/// The published grade keeps the stronger of the two derivations, and a
/// missing half leaves the other alone rather than pulling it down.
#[test]
fn a_review_lifts_a_critique_and_never_talks_a_pattern_down() {
    assert_eq!(
        published_grade(
            Some(ProvenanceGrade::ModelCritique),
            Some(ProvenanceGrade::HumanReview)
        ),
        Some(ProvenanceGrade::HumanReview),
        "a person read a claim that had only a model's opinion behind it"
    );
    assert_eq!(
        published_grade(
            Some(ProvenanceGrade::TrajectoryAbstraction),
            Some(ProvenanceGrade::HumanReview)
        ),
        Some(ProvenanceGrade::TrajectoryAbstraction),
        "being read must not cost a mined pattern the rung it earned"
    );
    assert_eq!(
        published_grade(Some(ProvenanceGrade::ModelCritique), None),
        Some(ProvenanceGrade::ModelCritique)
    );
    assert_eq!(
        published_grade(None, Some(ProvenanceGrade::HumanReview)),
        Some(ProvenanceGrade::HumanReview)
    );
    assert_eq!(published_grade(None, None), None, "absent stays absent");
}

/// **Every rung is reachable, or the gap is declared.**
///
/// A grade nothing constructs neither authorises nor blocks anything; it only
/// moves where the real boundary sits, and three `ImpactClass` floors sit on
/// rungs this sweep is what keeps reachable. Each arm below calls a real
/// production derivation and asserts what it yields, so a rung that stops
/// being produced fails here rather than going quiet.
///
/// The `match` is exhaustive on purpose: a sixth grade is a compile error in
/// this file, which is the question its author has to answer before the enum
/// can grow.
#[test]
fn every_grade_is_produced_or_its_gap_is_declared() {
    let mined = [
        observation(ObservationSource::ReflectionLesson, "task-a", "prefer rg"),
        observation(ObservationSource::ReflectionLesson, "task-b", "rg again"),
        observation(
            ObservationSource::ReflectionLesson,
            "task-c",
            "rg once more",
        ),
    ];

    for &grade in ProvenanceGrade::ALL {
        let produced = match grade {
            ProvenanceGrade::ModelCritique => {
                observation_grade(ObservationSource::ReflectionLesson)
            }
            ProvenanceGrade::HumanReview => {
                decision_grade(PromotionActor::User, PromotionAction::Confirmed)
                    .expect("a user confirmation is graded")
            }
            ProvenanceGrade::TrajectoryAbstraction => EvidencePool::from_observations(&mined)
                .expect("constructor-built observations hash clean")
                .expect("a non-empty pool")
                .grade(),
            ProvenanceGrade::EnvironmentObservation => {
                observation_grade(ObservationSource::ToolOutcome)
            }
            // Declared gap (`#5955`). The fail-to-pass witness that earns
            // this rung is run by a verification plugin, and none ships here,
            // so producing it in this workspace would mean asserting it —
            // the one move the provenance policy forbids.
            ProvenanceGrade::DeterministicProof => continue,
        };
        assert_eq!(
            produced,
            grade,
            "the production path named for {} yields {} instead",
            grade.as_str(),
            produced.as_str()
        );
    }
}
