// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for the gate.
//!
//! This table must agree with the ledger. The ledger is the copy a reviewer
//! reads. The gate must also say no to the proof the loop holds now, and yes
//! to the proof it is meant to earn.

use super::*;

/// The ledger source, read as text.
///
/// `stella-parity` has no dependents, so the binary does not link it. The
/// ledger reads source text the same way, to check that a named test is
/// still there.
const EVOLUTION_LEDGER: &str = include_str!("../../../stella-parity/src/evolution.rs");

/// The impact class the row tagged `tag` names.
fn ledger_impact(tag: &str) -> String {
    let needle = format!("=> \"{tag}\",");
    let start = EVOLUTION_LEDGER
        .find(&needle)
        .unwrap_or_else(|| panic!("no ledger row is tagged `{tag}`"));
    let row = &EVOLUTION_LEDGER[start..];
    let at = row
        .find("ImpactClass::")
        .expect("every row names an impact class");
    row[at + "ImpactClass::".len()..]
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect()
}

/// **The two copies agree.** This table names a class per file kind. The
/// ledger names one per surface. A rule is the `framework` surface. So both
/// must say the same word.
#[test]
fn an_artifact_carries_the_impact_its_ledger_row_declares() {
    assert_eq!(
        ledger_impact("framework"),
        format!("{:?}", Published::Rule.impact()),
        "the framework row and this table do not agree"
    );
}

/// The reader can fail. A check that cannot go red proves nothing.
#[test]
fn the_ledger_reader_finds_a_class_that_is_really_there() {
    assert_eq!(ledger_impact("tool"), "ExecutableTool");
}

/// **The no the loop meets today.** A lesson grades `ModelCritique`. A rule
/// steers. So the gate says no, and names both grades.
#[test]
fn a_rule_on_model_critique_is_refused_and_names_both_grades() {
    let refusal = admits(
        Published::Rule,
        Some(ProvenanceGrade::ModelCritique),
        PublicationAuthority::LocalHuman,
    )
    .expect_err("a model critique must not write a rule");

    assert_eq!(
        refusal,
        PromotionRefusal::EvidenceTooWeak {
            impact: ImpactClass::SteeringDirective,
            required: ProvenanceGrade::EnvironmentObservation,
            actual: ProvenanceGrade::ModelCritique,
        }
    );
    let line = refusal_line(Published::Rule, "some-lesson-abcd1234", &refusal);
    assert!(line.contains("environment_observation"), "{line}");
    assert!(line.contains("model_critique"), "{line}");
    assert!(line.contains("some-lesson-abcd1234"), "{line}");
}

/// A person typing the command does not lift weak proof. Who signs and what
/// was seen are two axes. This is the one a name cannot buy.
#[test]
fn a_human_signature_does_not_lift_a_weak_grade() {
    for authority in PublicationAuthority::ALL {
        assert!(
            admits(
                Published::Rule,
                Some(ProvenanceGrade::ModelCritique),
                *authority,
            )
            .is_err(),
            "{} wrote a rule on a model critique",
            authority.as_str()
        );
    }
}

/// A measured run writes. A holdout is the producer being built for this
/// grade. This arm shows the gate is not a flat no.
#[test]
fn a_rule_on_an_environment_observation_is_admitted() {
    admits(
        Published::Rule,
        Some(ProvenanceGrade::EnvironmentObservation),
        PublicationAuthority::Agent,
    )
    .expect("a measured run writes a rule");
}

/// No proof is a no of its own. It is not weak proof. An old record has to
/// read as "nothing behind it".
#[test]
fn a_rule_with_no_evidence_is_refused_as_absent() {
    let refusal = admits(Published::Rule, None, PublicationAuthority::LocalHuman)
        .expect_err("a record with no grade must not write");
    assert_eq!(
        refusal,
        PromotionRefusal::NoEvidence {
            impact: ImpactClass::SteeringDirective,
        }
    );
}
