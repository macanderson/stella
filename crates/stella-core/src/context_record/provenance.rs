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
///   into its observed and judged parts would promote it honestly; counting
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

/// The observations behind a proposal, together with the grade they fold to.
///
/// Construction is the enforcement: [`EvidencePool::from_observations`] is the
/// only public way to make one, so a proposal's grade is always a fold over
/// records that exist rather than a value chosen by whoever is promoting.
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
    /// Fold real observations into a pool, or [`None`] if there are none.
    ///
    /// The grade is [`ProvenanceGrade::weakest`] over the pool, so adding a
    /// weak observation to a strong pool weakens it and adding a strong one to
    /// a weak pool does not lift it.
    #[must_use]
    pub fn from_observations<'a>(
        observations: impl IntoIterator<Item = &'a ObservationRecord>,
    ) -> Option<Self> {
        let mut grade: Option<ProvenanceGrade> = None;
        let mut observation_ids = Vec::new();
        for observation in observations {
            let observed = observation_grade(observation.source);
            grade = Some(match grade {
                Some(current) => current.min(observed),
                None => observed,
            });
            observation_ids.push(observation.record_id.clone());
        }
        grade.map(|grade| Self {
            grade,
            observation_ids,
        })
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
