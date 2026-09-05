// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a write asks the gate first.
//!
//! The rules live in [`stella_protocol::provenance`]. What a bad change can
//! break sets the proof it costs. `authorises` is the gate. Until now, no
//! path asked it at the point a file gets written.
//!
//! # One place, one table
//!
//! The binary asks here and nowhere else. So the map from "what is written"
//! to "what a bad one breaks" is one small table. [`Published::impact`] is
//! that table. Each arm names the ledger row it must match.
//!
//! The ledger is not read at run time. `stella-parity` has no dependents.
//! Linking it in to fetch one value would cost an edge for a constant. So
//! each arm cites its row, and a test reads the ledger text to check both
//! still say the same word.
//!
//! # What a no looks like
//!
//! A [`PromotionRefusal`], handed back whole. It is never folded into a flat
//! string. The caller must tell weak proof from a missing sign-off.
//! `PromotionRefusal::reason` is the line a person reads. It names what was
//! needed and what was there.

use stella_protocol::provenance::{
    ImpactClass, PromotionRefusal, ProvenanceGrade, PublicationAuthority, authorises,
};

/// A thing the loop writes about itself.
///
/// One arm per kind of file, not one per command. Two commands that write
/// the same kind of file meet the same bar. That is the point of grading by
/// blast radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Published {
    /// A rule under `.stella/rules/`. It is mined from what the loop saw, or
    /// promoted from a cited memory. Either way it lands in the system
    /// prefix as an instruction, so it steers the agent.
    ///
    /// The ledger's `framework` row says
    /// [`ImpactClass::SteeringDirective`].
    Rule,
}

impl Published {
    /// What a bad one of these can break.
    #[must_use]
    pub(crate) fn impact(self) -> ImpactClass {
        match self {
            Self::Rule => ImpactClass::SteeringDirective,
        }
    }

    /// The word a refusal calls this file.
    #[must_use]
    pub(crate) fn noun(self) -> &'static str {
        match self {
            Self::Rule => "rule",
        }
    }
}

/// **Ask the gate.** May proof of this grade, from this actor, become this
/// kind of file?
///
/// # Errors
///
/// The typed [`PromotionRefusal`], naming the half that fell short.
pub(crate) fn admits(
    artifact: Published,
    grade: Option<ProvenanceGrade>,
    authority: PublicationAuthority,
) -> Result<(), PromotionRefusal> {
    authorises(grade, authority, artifact.impact())
}

/// One line for a person: what was not written, and what it would have cost.
///
/// `PromotionRefusal::reason` names the grade that was needed beside the one
/// on offer. This adds the two things it cannot know. Which kind of file,
/// and which one.
#[must_use]
pub(crate) fn refusal_line(artifact: Published, id: &str, refusal: &PromotionRefusal) -> String {
    format!(
        "{} `{id}` was not published: {}",
        artifact.noun(),
        refusal.reason()
    )
}

#[cfg(test)]
mod tests;
