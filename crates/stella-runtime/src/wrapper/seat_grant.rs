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
//! Both rules read the manifest here rather than a word list, so neither is an
//! accident of spelling (`doc:roleless-core` §6).
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
    /// Core has a word for this now: [`ModelCallRole::Plugin`] says a plugin
    /// asked for the call, and the plugin's own name for the job rides beside
    /// it on the child's `sub_agent` bracket. So every plugin gets it, judging
    /// or not, and no receipt claims a job core did.
    ///
    /// This used to guess — a verdict seat for a judging plugin and a research
    /// seat for the rest — and the guess is what `doc:roleless-core` slice 2
    /// retires.
    ///
    /// Not a routing decision. Which model runs the turn comes from the user's
    /// own seat map, keyed on the plugin's word
    /// (`stella_cli::agent::seats`), and nothing here is consulted for it.
    #[must_use]
    pub fn default_seat(self) -> ModelCallRole {
        ModelCallRole::Plugin
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

            // This one says "a call decided whether the work is done". Only a
            // plugin that declared that job may spend at it.
            ModelCallRole::Verdict => {
                if self.judges {
                    SeatPermission::Granted
                } else {
                    SeatPermission::Undeclared
                }
            }

            // A plugin's own seat, and the one every child turn lands at when
            // the host binds nothing.
            ModelCallRole::Plugin => SeatPermission::Granted,

            // Summarising, reflection and the rest. A child turn is read-only
            // and bounded, so a wrong label here costs a line on a cost report
            // and nothing else.
            ModelCallRole::Unknown
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
        assert_eq!(
            SeatGrant::of(&steering()).permits(ModelCallRole::Verdict),
            SeatPermission::Undeclared
        );
        assert_eq!(
            SeatGrant::of(&judging()).permits(ModelCallRole::Verdict),
            SeatPermission::Granted
        );
    }

    #[test]
    fn every_other_seat_is_granted() {
        let grant = SeatGrant::of(&steering());
        let refused = [ModelCallRole::Worker, ModelCallRole::Verdict];
        for &seat in ModelCallRole::ALL {
            if refused.contains(&seat) {
                continue;
            }
            assert_eq!(grant.permits(seat), SeatPermission::Granted, "{seat:?}");
        }
    }

    /// A plugin's call is booked to the seat that says a plugin asked for it,
    /// whatever job the manifest declared. Reading `judges` here would pick a
    /// seat that names a job core does.
    #[test]
    fn every_grant_books_a_plugins_own_seat() {
        assert_eq!(
            SeatGrant::of(&steering()).default_seat(),
            ModelCallRole::Plugin
        );
        assert_eq!(
            SeatGrant::of(&judging()).default_seat(),
            ModelCallRole::Plugin
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
