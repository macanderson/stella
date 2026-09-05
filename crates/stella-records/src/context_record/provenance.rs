// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where an observation's evidence grade comes from, and the pool that carries
//! it across the hop to a proposal (#2782).
//!
//! [`stella_protocol::provenance`] owns the vocabulary and the gate. This
//! module owns the one thing that vocabulary cannot own from another crate:
//! the mapping out of *this* crate's [`ObservationSource`], and the container
//! that makes a proposal's grade impossible to assert.
//!
//! # Derived, never asserted
//!
//! #2782 asks that a grade be "derived from the source record, never asserted
//! by the promoting code". Two mechanisms hold that here, and neither is a
//! review question:
//!
//! - An observation does not *store* a grade. [`observation_grade`] is a total
//!   function of [`ObservationSource`], so an observation's grade cannot drift
//!   from its source — there is no second copy to disagree.
//! - A proposal must store one, because it keeps only the `record_id`s of the
//!   observations behind it and so cannot re-derive later. [`EvidencePool`] is
//!   the only way to build that field, and its only public constructor takes
//!   real [`ObservationRecord`]s. Promoting code cannot hand a proposal a
//!   grade it likes; it can only hand it the evidence it has.
//!
//! The pool folds with [`ProvenanceGrade::weakest`], so the hop from
//! observations to a proposal can lose strength and never gain it.

use stella_protocol::provenance::ProvenanceGrade;

use super::hash::record_hash;
use super::lifecycle::{ObservationRecord, ObservationSource};

/// The evidence grade an observation of this source carries.
///
/// Graded down when the answer is arguable, because the cost of the two
/// directions is not symmetric: under-grading parks an artifact until someone
/// re-derives it against a stronger source, and over-grading publishes on
/// evidence that was never there.
///
/// - [`ObservationSource::ToolOutcome`] is the environment answering — an exit
///   status, a build result — which is
///   [`ProvenanceGrade::EnvironmentObservation`] exactly.
/// - [`ObservationSource::ReflectionLesson`] is a model's judgement about a
///   run it just took, which is [`ProvenanceGrade::ModelCritique`] however
///   confident the prose sounds.
/// - [`ObservationSource::MemoryCitation`] pairs an observed retrieval with a
///   model's usefulness judgement. The retrieval is observed and the judgement
///   is not, so the pair is graded at the weaker half. Splitting the citation
///   into its observed and judged parts would promote it; counting
///   citations would not, and that is the door this grading closes.
#[must_use]
pub fn observation_grade(source: ObservationSource) -> ProvenanceGrade {
    match source {
        ObservationSource::ToolOutcome => ProvenanceGrade::EnvironmentObservation,
        ObservationSource::ReflectionLesson | ObservationSource::MemoryCitation => {
            ProvenanceGrade::ModelCritique
        }
    }
}

/// An observation whose stored `record_hash` does not match its own content.
///
/// [`ObservationRecord`]'s fields are public (the CLI and the observatory read
/// them), so nothing stops a caller literal-constructing one instead of going
/// through the hashing constructor. The pool re-derives the hash and refuses a
/// mismatch, because folding such a record would mint a grade from evidence
/// whose identity nothing vouches for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "observation {record_id} carries record_hash \"{stored}\", but its content \
     hashes to {recomputed} — a grade cannot fold over evidence whose identity \
     nothing vouches for"
)]
pub struct EvidenceIntegrityError {
    /// The observation's claimed id.
    pub record_id: String,
    /// The hash the record carried.
    pub stored: String,
    /// The hash its content actually has.
    pub recomputed: String,
}

/// The observations behind a proposal, together with the grade they fold to.
///
/// Construction is the enforcement: [`EvidencePool::from_observations`] is the
/// only public way to make one, and it re-verifies each observation's
/// `record_hash`, so a proposal's grade is always a fold over records that
/// exist rather than a value chosen by whoever is promoting.
///
/// An empty pool is [`None`] rather than a pool with a weak grade — no
/// evidence is not weak evidence, and rounding the two together is what lets a
/// proposal with nothing behind it look merely unconvincing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidencePool {
    grade: ProvenanceGrade,
    observation_ids: Vec<String>,
}

impl EvidencePool {
    /// Fold real observations into a pool, or `Ok(None)` if there are none.
    ///
    /// The grade is [`ProvenanceGrade::weakest`] over the pool, so adding a
    /// weak observation to a strong pool weakens it and adding a strong one to
    /// a weak pool does not lift it. Each observation's `record_hash` is
    /// re-derived from its content first — see [`EvidenceIntegrityError`] —
    /// and an unhashable record fails the same way, since a record the
    /// canonical hash cannot cover is one nothing can vouch for either.
    pub fn from_observations<'a>(
        observations: impl IntoIterator<Item = &'a ObservationRecord>,
    ) -> Result<Option<Self>, EvidenceIntegrityError> {
        let mut grade: Option<ProvenanceGrade> = None;
        let mut observation_ids = Vec::new();
        for observation in observations {
            let recomputed = record_hash(observation).map_err(|err| EvidenceIntegrityError {
                record_id: observation.record_id.clone(),
                stored: observation.record_hash.clone(),
                recomputed: format!("(unhashable: {err})"),
            })?;
            if recomputed != observation.record_hash {
                return Err(EvidenceIntegrityError {
                    record_id: observation.record_id.clone(),
                    stored: observation.record_hash.clone(),
                    recomputed,
                });
            }
            let observed = observation_grade(observation.source);
            grade = Some(match grade {
                Some(current) => current.min(observed),
                None => observed,
            });
            observation_ids.push(observation.record_id.clone());
        }
        Ok(grade.map(|grade| Self {
            grade,
            observation_ids,
        }))
    }

    /// The grade this evidence folds to.
    #[must_use]
    pub fn grade(&self) -> ProvenanceGrade {
        self.grade
    }

    /// The `record_id`s of the observations behind the pool, in the order they
    /// were folded.
    #[must_use]
    pub fn observation_ids(&self) -> &[String] {
        &self.observation_ids
    }

    /// Split into the two fields a proposal stores.
    #[must_use]
    pub fn into_parts(self) -> (ProvenanceGrade, Vec<String>) {
        (self.grade, self.observation_ids)
    }
}

#[cfg(test)]
mod tests;
