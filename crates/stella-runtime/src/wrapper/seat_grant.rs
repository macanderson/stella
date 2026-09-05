// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which seats a plugin may spend at, read off the grant a human consented to.
//!
//! A seat is a [`ModelCallRole`]. It is the label a model call lands under on
//! the receipt, and this workspace owns the list. A plugin never names one. It
//! names a role of its own, in a word it chose, and the host decides where the
//! spend is booked.
//!
//! # Why the rule sits on the grant
//!
//! Core once held a table of four role words, and the table carried two rules
//! that have nothing to do with spelling:
//!
//! 1. a plugin may not spend at the seat the session's own turns are booked at,
//!    because a plugin that judges that work must not pay for the model that
//!    did it;
//! 2. a plugin may not spend at a seat that means "this call decided whether
//!    the work is done" unless it declared that job and a human agreed.
//!
//! Hiding those two rules in a word list made both of them accidents of
//! spelling. A plugin that needed a `reviewer` was refused, and a plugin that
//! got a seat bound for it bought a verdict nobody consented to. Both rules
//! read the manifest here instead (`doc:roleless-core` §6).
//!
//! # What counts as declaring the job
//!
//! An `[oracle]` block. A manifest may only carry one at arbiter grade, and an
//! arbiter is the plugin that says whether the turn's goal was met. So the
//! block is the plugin stating the job in the one document a person reads at
//! install.

use stella_plugin::PluginManifest;
use stella_protocol::event::ModelCallRole;

/// What a grant says about one seat.
///
/// Three answers, because two of them refuse for different reasons and send a
/// plugin author to different places. [`Self::Never`] means no manifest can buy
/// this. [`Self::Undeclared`] means this one did not ask for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatPermission {
    /// Spend here.
    Granted,
    /// No grant buys this seat. Reported as `Forbidden`.
    Never,
    /// This seat needs a job the manifest never declared. Reported as
    /// `Unavailable`.
    Undeclared,
}

/// The seats one installed plugin may spend at.
///
/// Built once, when a plane is declared, from the manifest a person read and
/// agreed to. It holds one fact, not the whole manifest. A plane is built from
/// a borrow and outlives it. A copy of the manifest in every plane would be a
/// second source of one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeatGrant {
    /// The manifest declares an `[oracle]`, so this plugin judges the turn.
    judges: bool,
}

impl SeatGrant {
    /// Read the grant out of a consented manifest.
    #[must_use]
    pub fn of(manifest: &PluginManifest) -> Self {
        Self {
            judges: manifest.oracle.is_some(),
        }
    }

    /// The seat this plugin's turns are booked at when the host binds none.
    ///
    /// A plugin that judges the turn gets the seat a verdict call is booked at,
    /// which is where `stella_core::goal` books its own verifier call, so the
    /// two read the same on one receipt. Every other plugin gets the seat a
    /// read-only sub-agent call is booked at, which is what a child turn is:
    /// the host runs it with no write arm and hands back an answer.
    ///
    /// Neither is a routing decision. Which model runs the turn comes from the
    /// user's own seat map, keyed on the plugin's word
    /// (`stella_cli::agent::seats`), and nothing here is consulted for it.
    ///
    /// The receipt is coarser than it should be while core has no word for "a
    /// call this plugin asked for". Slice 2 of `doc:roleless-core` gives a
    /// receipt the plugin's own name and retires the guess.
    #[must_use]
    pub fn default_seat(self) -> ModelCallRole {
        if self.judges {
            ModelCallRole::Verdict
        } else {
            ModelCallRole::Research
        }
    }

    /// Whether this plugin may spend at `seat`.
    ///
    /// The match names every case on purpose. A wildcard would grant a new
    /// seat with nobody deciding to. A new seat is most likely to look like
    /// one of the two this refuses.
    #[must_use]
    pub fn permits(self, seat: ModelCallRole) -> SeatPermission {
        match seat {
            // The session's own turns land here. A plugin's turn is evidence
            // about that work, so paying for the model that did the work would
            // let the plugin grade itself.
            ModelCallRole::Worker => SeatPermission::Never,

            // These three say "a call decided whether the work is done". Only a
            // plugin that declared that job may spend at one.
            ModelCallRole::WitnessAuthor
            | ModelCallRole::WitnessRepair
            | ModelCallRole::Verdict => {
                if self.judges {
                    SeatPermission::Granted
                } else {
                    SeatPermission::Undeclared
                }
            }

            // Reading, planning, summarising and the rest. A child turn is
            // read-only and bounded, so a wrong label here costs a line on a
            // cost report and nothing else.
            ModelCallRole::Unknown
            | ModelCallRole::Triage
            | ModelCallRole::Research
            | ModelCallRole::Plan
            | ModelCallRole::PlanRepair
            | ModelCallRole::DistressGuidance
            | ModelCallRole::AgentAuthor
            | ModelCallRole::SkillAuthor
            | ModelCallRole::DomainInference
            | ModelCallRole::Reflection
            | ModelCallRole::Summarization => SeatPermission::Granted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A steering plugin: it contributes to the turn and judges nothing.
    fn steering() -> PluginManifest {
        PluginManifest::from_toml_str(
            "name = \"reader\"\n\n[loop]\nparticipation = \"steering\"\npoints = \
             [\"after_turn\"]\ncalls = [\"child_turn\"]",
        )
        .expect("the manifest loads")
    }

    /// An arbiter with an `[oracle]`: it says whether the goal was met.
    fn judging() -> PluginManifest {
        PluginManifest::from_toml_str(
            "name = \"arbiter\"\n\n[loop]\nparticipation = \"arbiter\"\nhooks = \
             [\"Stop\"]\npoints = [\"after_turn\"]\ncalls = \
             [\"child_turn\"]\n\n[requirements]\ndone = \"the goal is met\"\n\n[oracle]\nflip = \
             \"required\"\n\n[oracle.command]\nargv = [\"true\"]\ntimeout_secs = 5",
        )
        .expect("the manifest loads")
    }

    #[test]
    fn the_seat_the_sessions_own_turns_use_is_never_granted() {
        assert_eq!(
            SeatGrant::of(&steering()).permits(ModelCallRole::Worker),
            SeatPermission::Never
        );
        assert_eq!(
            SeatGrant::of(&judging()).permits(ModelCallRole::Worker),
            SeatPermission::Never,
            "declaring an oracle buys the one seat no grant buys"
        );
    }

    #[test]
    fn a_verdict_seat_needs_a_declared_oracle() {
        for seat in [
            ModelCallRole::Verdict,
            ModelCallRole::WitnessAuthor,
            ModelCallRole::WitnessRepair,
        ] {
            assert_eq!(
                SeatGrant::of(&steering()).permits(seat),
                SeatPermission::Undeclared,
                "{seat:?}"
            );
            assert_eq!(
                SeatGrant::of(&judging()).permits(seat),
                SeatPermission::Granted,
                "{seat:?}"
            );
        }
    }

    #[test]
    fn every_other_seat_is_granted() {
        let grant = SeatGrant::of(&steering());
        let refused = [
            ModelCallRole::Worker,
            ModelCallRole::Verdict,
            ModelCallRole::WitnessAuthor,
            ModelCallRole::WitnessRepair,
        ];
        for &seat in ModelCallRole::ALL {
            if refused.contains(&seat) {
                continue;
            }
            assert_eq!(grant.permits(seat), SeatPermission::Granted, "{seat:?}");
        }
    }

    #[test]
    fn the_default_seat_follows_the_declared_job() {
        assert_eq!(
            SeatGrant::of(&steering()).default_seat(),
            ModelCallRole::Research
        );
        assert_eq!(
            SeatGrant::of(&judging()).default_seat(),
            ModelCallRole::Verdict
        );
    }

    /// Whatever the default is, the grant must permit it. A default the rule
    /// refuses would make every plugin's first ask a refusal.
    #[test]
    fn the_default_seat_is_one_the_grant_permits() {
        for manifest in [steering(), judging()] {
            let grant = SeatGrant::of(&manifest);
            assert_eq!(
                grant.permits(grant.default_seat()),
                SeatPermission::Granted,
                "{grant:?}"
            );
        }
    }
}
