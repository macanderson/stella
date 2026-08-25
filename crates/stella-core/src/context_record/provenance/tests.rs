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
    let pool = EvidencePool::from_observations(&observations).expect("a non-empty pool");

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
/// distinct-task floor, and it still must not authorise a tool.
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
    let pool = EvidencePool::from_observations(&observations).expect("a non-empty pool");
    let proposal = proposal(Some(pool));

    assert!(
        proposal.is_eligible(3, 3),
        "the fixture must clear the existing anti-poisoning floor, so the \
         refusal below is the grade's doing and not the floor's"
    );
    assert_eq!(proposal.provenance, Some(ProvenanceGrade::ModelCritique));

    let refusal = authorises(
        proposal.provenance,
        PublicationAuthority::LocalHuman,
        ImpactClass::ExecutableTool,
    )
    .expect_err("an eligible proposal graded on critique alone still cannot publish a tool");
    assert!(
        refusal.reason().contains("deterministic_proof"),
        "{}",
        refusal.reason()
    );
}

/// One weak observation weakens a pool that is otherwise measured — the fold
/// is a floor, not an average, so a critique cannot be outvoted.
#[test]
fn a_single_critique_weakens_a_pool_of_measurements() {
    let observations = [
        observation(ObservationSource::ToolOutcome, "task-a", "exit 1"),
        observation(ObservationSource::ToolOutcome, "task-b", "exit 1"),
        observation(ObservationSource::ReflectionLesson, "task-c", "felt slow"),
    ];
    let pool = EvidencePool::from_observations(&observations).expect("a non-empty pool");

    assert_eq!(pool.grade(), ProvenanceGrade::ModelCritique);
}

/// No observations is no grade, and that has to reach the record rather than
/// being smoothed into the weakest one.
#[test]
fn a_proposal_with_no_observations_stores_no_grade() {
    assert_eq!(EvidencePool::from_observations(&[]), None);

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
    let pool = EvidencePool::from_observations(&observations).expect("a non-empty pool");
    let proposal = proposal(Some(pool));

    let json = serde_json::to_value(&proposal).expect("a proposal serializes");
    assert_eq!(json["provenance"], "environment_observation");
}
