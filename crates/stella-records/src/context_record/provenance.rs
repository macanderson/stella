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
//!
//! Three of the five grades are derived here: [`observation_grade`] for one
//! observation, [`EvidencePool::from_observations`] for a pattern across
//! tasks, and [`decision_grade`] for a person's sign-off.

use std::collections::BTreeSet;

use stella_protocol::provenance::ProvenanceGrade;

use super::hash::record_hash;
use super::kind::PromotionAction;
use super::lifecycle::{ObservationRecord, ObservationSource, PromotionActor};

/// How many distinct tasks a pool must span before it is a mined pattern
/// rather than a set of opinions.
///
/// Three, matching spec §7's distinct-task floor and the number both miners
/// already use to call a proposal eligible. It lives here rather than in
/// settings because `context.promotion.inferred_directive.min_distinct_tasks`
/// is a file a person edits, and a project that lowers its own promotion bar
/// must not thereby lower what its evidence is worth.
pub const TRAJECTORY_ABSTRACTION_MIN_DISTINCT_TASKS: u32 = 3;

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
///
/// A third source, `MemoryCitation`, paired an observed retrieval with a
/// model's usefulness judgement here. It is retired — see
/// [`ObservationSource`]'s own doc comment for why.
#[must_use]
pub fn observation_grade(source: ObservationSource) -> ProvenanceGrade {
    match source {
        ObservationSource::ToolOutcome => ProvenanceGrade::EnvironmentObservation,
        ObservationSource::ReflectionLesson => ProvenanceGrade::ModelCritique,
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
    ///
    /// The fold is then lifted to [`ProvenanceGrade::TrajectoryAbstraction`]
    /// when the observations span
    /// [`TRAJECTORY_ABSTRACTION_MIN_DISTINCT_TASKS`] distinct `task_id`s and
    /// folded weaker than that.
    ///
    /// # Why the lift is not laundering
    ///
    /// Combining evidence can only weaken it, and the lift is not a
    /// combination. It is a second claim: the observations each say what
    /// happened in one task, and the pool says the same thing recurred across
    /// several. That claim is derived against the trajectory corpus, and
    /// re-deriving against another source is the one move that promotes a
    /// grade.
    ///
    /// Four properties bound it, and all four are mechanical:
    ///
    /// - The lift is a `max`, so a pool of tool outcomes keeps the stronger
    ///   grade it already had.
    /// - It **caps at** [`ProvenanceGrade::TrajectoryAbstraction`]. However
    ///   often a pattern recurs, it can never authorise a steering directive,
    ///   a blocking guard, or an executable tool.
    /// - Its floor is a constant here, not the `min_distinct_tasks` setting,
    ///   so lowering a project's promotion bar cannot lower this one.
    /// - It counts the observations' own `task_id`s, never a caller's
    ///   `ProposalScore`. Thirty restatements inside one task fold to
    ///   [`ProvenanceGrade::ModelCritique`] and stay there, which is spec §7.
    pub fn from_observations<'a>(
        observations: impl IntoIterator<Item = &'a ObservationRecord>,
    ) -> Result<Option<Self>, EvidenceIntegrityError> {
        let mut grade: Option<ProvenanceGrade> = None;
        let mut observation_ids = Vec::new();
        // Borrowed rather than cloned, and never stored: only the size
        // outlives the fold, and only to decide the lift.
        let mut tasks: BTreeSet<&str> = BTreeSet::new();
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
            tasks.insert(observation.task_id.as_str());
        }
        let distinct_tasks = tasks.len() as u32;
        Ok(grade.map(|folded| Self {
            grade: if distinct_tasks >= TRAJECTORY_ABSTRACTION_MIN_DISTINCT_TASKS {
                folded.max(ProvenanceGrade::TrajectoryAbstraction)
            } else {
                folded
            },
            observation_ids,
        }))
    }

    /// The grade this evidence stands on.
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

/// The evidence grade a governance decision itself supplies.
///
/// A total function of the pair, for the reason [`observation_grade`] is a
/// total function of its source: nothing stores this, so there is no second
/// copy to drift from the event that decided it.
///
/// Exactly one pair answers with a grade. A person confirming a proposal has
/// read the claim and put their name to it, which is
/// [`ProvenanceGrade::HumanReview`] as that rung defines itself. Every other
/// pair answers `None`:
///
/// - The system acting under policy is not a person, whatever it decides.
/// - Rejecting, retiring or reverting is a decision *against* the claim, and
///   supplies no evidence for it.
/// - Proposing and auto-activating happen before or without a reading.
/// - Publishing moves a record that some earlier decision already graded.
#[must_use]
pub fn decision_grade(actor: PromotionActor, action: PromotionAction) -> Option<ProvenanceGrade> {
    match (actor, action) {
        (PromotionActor::User, PromotionAction::Confirmed) => Some(ProvenanceGrade::HumanReview),
        (PromotionActor::System, _) => None,
        (
            PromotionActor::User,
            PromotionAction::Proposed
            | PromotionAction::AutoActivated
            | PromotionAction::Published
            | PromotionAction::Rejected
            | PromotionAction::Retired
            | PromotionAction::Reverted,
        ) => None,
    }
}

/// The grade a published artifact stands on: the stronger of what its
/// evidence folded to and what the decision to publish it supplies.
///
/// This is a `max` where [`ProvenanceGrade::weakest`] is a `min`, and the
/// difference is what the two are ranging over. `weakest` folds one pool of
/// evidence for one claim, where agreement is correlation and must not add up.
/// Here there are two derivations against two sources — a trajectory corpus
/// and a person — and re-deriving a claim against another source is the one
/// move that promotes a grade. Refusing to see the second source would leave
/// [`ProvenanceGrade::HumanReview`] a rung nothing can ever stand on.
///
/// A missing half is not a weak half: `None` on either side leaves the other
/// unchanged, and `None` on both stays `None`.
#[must_use]
pub fn published_grade(
    evidence: Option<ProvenanceGrade>,
    decision: Option<ProvenanceGrade>,
) -> Option<ProvenanceGrade> {
    match (evidence, decision) {
        (Some(evidence), Some(decision)) => Some(evidence.max(decision)),
        (only, None) | (None, only) => only,
    }
}

#[cfg(test)]
mod tests;
