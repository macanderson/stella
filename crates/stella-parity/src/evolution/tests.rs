// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for the evolution-surface matrix (#2780).
//!
//! Totality is the compiler's job here — [`EvolutionSurface`] and
//! [`EVOLUTION_SURFACES`] come out of one macro, so a surface with no row is
//! not a case these tests have to catch. What they catch is everything the
//! generator cannot: a witness that has been renamed away, a gap citing
//! nothing, a declared grade drifting from the policy that defines it, and the
//! unwitnessed debt creeping upward.

use super::*;

/// A witness exists if the swept sources declare a function by that name —
/// the same substring check [`crate`]'s matrix and `provider_parity` use, and
/// not an AST walk: a moved witness should fail loudly rather than be quietly
/// re-resolved.
fn witness_exists(sources: &[&str], witness: &str) -> bool {
    let needle = format!("fn {witness}(");
    sources.iter().any(|source| source.contains(&needle))
}

/// **Every live row names a test, and that test still exists.**
#[test]
fn every_named_witness_exists_in_the_swept_sources() {
    let sources = evolution_sources();

    for row in EVOLUTION_SURFACES {
        let Some(witness) = row.posture.witness() else {
            continue;
        };
        assert!(
            witness_exists(&sources, witness),
            "the {} row names `{witness}`, which no swept source declares — either the \
             test was renamed (update the row) or the file holding it is missing from \
             evolution_sources()",
            row.surface.as_str()
        );
    }
}

/// The sweep can fail. A checker that cannot go red proves nothing about the
/// rows it passes, which is `content_free.rs`'s epistemics applied here.
#[test]
fn the_witness_check_rejects_a_name_that_does_not_exist() {
    let sources = evolution_sources();

    assert!(
        !witness_exists(&sources, "a_witness_nobody_has_ever_written"),
        "the witness check must be capable of failing"
    );
}

/// Every surface has exactly one row, and it is the row indexing finds.
///
/// [`EvolutionSurface::row`] indexes by discriminant, which is sound only
/// while the enum stays fieldless and the two lists stay in one order. Both
/// come out of one macro today; this is what notices if that stops being true.
#[test]
fn a_surfaces_discriminant_is_its_row_index() {
    assert_eq!(EvolutionSurface::ALL.len(), EVOLUTION_SURFACES.len());

    for (index, surface) in EvolutionSurface::ALL.iter().enumerate() {
        assert_eq!(
            *surface as usize,
            index,
            "{} sits at index {index} of ALL but its discriminant is {}",
            surface.as_str(),
            *surface as usize
        );
        assert_eq!(
            EVOLUTION_SURFACES[index].surface,
            *surface,
            "the ledger row at index {index} is not {}",
            surface.as_str()
        );
        assert_eq!(surface.row().surface, *surface);
    }
}

/// Tags are the ledger's external names, so a duplicate would make two rows
/// indistinguishable to anything reading them.
#[test]
fn every_surface_tag_is_unique_and_snake_case() {
    let mut seen = std::collections::BTreeSet::new();

    for surface in EvolutionSurface::ALL {
        let tag = surface.as_str();
        assert!(seen.insert(tag), "duplicate surface tag `{tag}`");
        assert!(
            !tag.is_empty() && tag.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "`{tag}` is not snake_case"
        );
    }
}

/// A `Planned` row cites the issue deciding it, and a `NotPursued` row cites
/// the issue where it was dropped — never prose standing in for a reference.
#[test]
fn every_parked_row_cites_an_issue() {
    for row in EVOLUTION_SURFACES {
        let cited = match row.posture {
            EvolutionPosture::Planned { issue, .. } => issue,
            EvolutionPosture::NotPursued { decided_in, .. } => decided_in,
            _ => continue,
        };
        assert!(
            cited.starts_with('#') && cited[1..].chars().all(|c| c.is_ascii_digit()),
            "the {} row cites `{cited}`, which is not a `#NNNN` issue reference",
            row.surface.as_str()
        );
    }
}

/// A `Prohibited` row cites a design document **by id**. AGENTS.md's rule is
/// that a document is cited by id and never by path, because a path moves and
/// nothing notices.
#[test]
fn every_prohibited_row_cites_a_design_document_by_id() {
    for row in EVOLUTION_SURFACES {
        let EvolutionPosture::Prohibited { design_doc, reason } = row.posture else {
            continue;
        };
        assert!(
            design_doc.starts_with("doc:"),
            "the {} row cites `{design_doc}`, which is not a `doc:<id>` reference",
            row.surface.as_str()
        );
        assert!(
            !design_doc.contains('/') && !design_doc.ends_with(".md"),
            "the {} row cites a path rather than an id: `{design_doc}`",
            row.surface.as_str()
        );
        assert!(
            !reason.trim().is_empty(),
            "the {} row prohibits without a reason",
            row.surface.as_str()
        );
    }
}

/// Unwitnessed debt is a down-only ratchet, checked for exact equality so that
/// raising it is an edit a reviewer sees rather than a threshold quietly
/// absorbing a new row.
#[test]
fn unwitnessed_rows_match_the_declared_baseline_exactly() {
    let count = EVOLUTION_SURFACES
        .iter()
        .filter(|row| matches!(row.posture, EvolutionPosture::ShippedUnwitnessed { .. }))
        .count();

    assert_eq!(
        count, UNWITNESSED_EVOLUTION_BASELINE,
        "write the missing witness and lower UNWITNESSED_EVOLUTION_BASELINE, or — if this \
         is genuinely new debt — raise it and say why in the PR"
    );
}

/// Every row says something a reviewer can act on: a live surface explains its
/// mechanism, and every surface says how a change to it is undone.
#[test]
fn every_row_explains_its_mechanism_and_its_rollback() {
    for row in EVOLUTION_SURFACES {
        assert!(
            !row.rollback.trim().is_empty(),
            "the {} row declares no rollback",
            row.surface.as_str()
        );

        let mechanism = match row.posture {
            EvolutionPosture::Shipped { mechanism, .. }
            | EvolutionPosture::ShippedUnwitnessed { mechanism, .. }
            | EvolutionPosture::Experimental { mechanism, .. } => Some(mechanism),
            EvolutionPosture::Planned { today, .. }
            | EvolutionPosture::NotPursued { today, .. } => Some(today),
            EvolutionPosture::Prohibited { .. } => None,
        };
        if let Some(mechanism) = mechanism {
            assert!(
                !mechanism.trim().is_empty(),
                "the {} row explains nothing",
                row.surface.as_str()
            );
        }
    }
}

/// **The two ledgers cannot drift**, because this one does not restate the
/// other. A row declares its impact and reads the grade out of #2782's policy,
/// so there is no second copy of the requirement to disagree with the first.
#[test]
fn required_evidence_is_read_from_the_provenance_policy() {
    for row in EVOLUTION_SURFACES {
        assert_eq!(row.required_evidence(), row.impact.required_grade());
        assert_eq!(row.required_authority(), row.impact.required_authority());
    }
}

/// The two surfaces that can run code in a teammate's session are held to
/// proof and to a human. If someone lowers either requirement, this is where
/// the decision surfaces rather than being absorbed by a row edit.
#[test]
fn the_executable_surfaces_require_proof_and_a_person() {
    for surface in [EvolutionSurface::Tool, EvolutionSurface::Model] {
        let row = surface.row();
        assert_eq!(row.impact, ImpactClass::ExecutableTool);
        assert_eq!(row.required_evidence(), ProvenanceGrade::DeterministicProof);
        assert_eq!(row.required_authority(), PublicationAuthority::LocalHuman);
    }
}

/// No live surface changes itself during the turn that is producing the
/// evidence for the change. If one ever does, this test is where that gets
/// argued rather than noticed later.
#[test]
fn no_live_surface_mutates_inside_the_running_turn() {
    for row in EVOLUTION_SURFACES {
        if row.posture.is_live() {
            assert_ne!(
                row.timing,
                EvolutionTiming::InTurn,
                "the {} row mutates in-turn: the loop would be altering the behaviour that \
                 is generating its own evidence. If that is intended, say so here",
                row.surface.as_str()
            );
        }
    }
}

/// A row that is not live claims no witness, and a live one either names a
/// witness or is counted as debt. Nothing sits between the two.
#[test]
fn liveness_and_witnesses_agree() {
    for row in EVOLUTION_SURFACES {
        match row.posture {
            EvolutionPosture::Shipped { .. } | EvolutionPosture::Experimental { .. } => {
                assert!(row.posture.is_live());
                assert!(row.posture.witness().is_some());
            }
            EvolutionPosture::ShippedUnwitnessed { .. } => {
                assert!(row.posture.is_live());
                assert!(row.posture.witness().is_none());
            }
            EvolutionPosture::Planned { .. }
            | EvolutionPosture::Prohibited { .. }
            | EvolutionPosture::NotPursued { .. } => {
                assert!(!row.posture.is_live());
                assert!(row.posture.witness().is_none());
            }
        }
    }
}
