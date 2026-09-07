// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a package adds to a turn, as data.
//!
//! A manifest's `[[wrapper.stages]]` entries decide what a turn does. They
//! say which stages run, what each is named, and when each runs. A person
//! must be told all of that before they say yes. Two surfaces owe the
//! answer. One is the install prompt. The other is `stella plugin list`.
//!
//! # Why a struct, and not words in each surface
//!
//! The two surfaces want different shapes. Consent is read once, before
//! anything runs. The listing reports on a package on disk, whose seats may
//! each hold a model. Writing the words twice leaves two copies. One of them
//! goes stale.
//!
//! It also settles the order of the work. A listing that a script can read
//! is asked for elsewhere. Words added first would be encoded again later.
//! [`TurnComposition`] is that encoding. It derives `Serialize`, so a JSON
//! report emits it, and the words stay a view of it.
//!
//! # This describes; it decides nothing
//!
//! Merging many packages into one order is `stella_runtime`'s job. It reads
//! bands across the whole active set. This module reads one manifest. A band
//! here is what the package asked for, not where it lands.

use serde::Serialize;

use crate::manifest::PluginManifest;
use crate::seat::seat_key;
use crate::wrapper::StageBand;

/// The stages and roles one package adds to a turn.
///
/// `None` when a manifest asks for neither. Such a package adds nothing to
/// merge. An empty block would read as a report about something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TurnComposition {
    /// The `[wrapper] id` this package runs under.
    ///
    /// Absent when the manifest has no `[wrapper]` block. It is what
    /// `--pipeline` matches, and what the store writes down for a run. A
    /// reader hunting the runs a package joined needs it here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<String>,
    /// The stages the `[wrapper]` block names, in the order it wrote them.
    pub stages: Vec<StageDisclosure>,
    /// The `[roles.<name>]` entries, ordered by role name.
    pub roles: Vec<RoleDisclosure>,
}

/// One `[[wrapper.stages]]` entry, as a reader needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StageDisclosure {
    /// The stage's name.
    pub name: String,
    /// Whether the package made this name up.
    ///
    /// The word list is open. So the name alone cannot tell a reader which
    /// kind of stage this is. That is the part a name does not carry.
    pub contributed: bool,
    /// Which band the stage asks to run in.
    pub band: StageBand,
    /// When the stage runs, as the author wrote it. Absent means every turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// One `[roles.<name>]` entry, joined to the seat a user assigns against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleDisclosure {
    /// The bare role name the package chose.
    pub role: String,
    /// The tier it asks to be routed at.
    pub tier: String,
    /// The seat key a `[seats]` line uses.
    ///
    /// It is the package name and the role, joined by the host. Carried here
    /// because the package never writes that first half. A reader who guesses
    /// the bare role name writes a line nothing looks up.
    pub seat: String,
}

impl TurnComposition {
    /// What `manifest` adds to a turn. `None` when it adds nothing.
    #[must_use]
    pub fn of(manifest: &PluginManifest) -> Option<Self> {
        let stages: Vec<StageDisclosure> = manifest
            .wrapper
            .iter()
            .flat_map(|wrapper| &wrapper.stages)
            .map(|stage| StageDisclosure {
                name: stage.name.as_str().to_owned(),
                contributed: stage.name.is_contributed(),
                band: stage.band,
                condition: stage.condition.clone(),
            })
            .collect();
        let roles: Vec<RoleDisclosure> = manifest
            .roles
            .iter()
            .flatten()
            .map(|(role, declared)| RoleDisclosure {
                role: role.clone(),
                tier: declared.tier.clone(),
                seat: seat_key(&manifest.name, role),
            })
            .collect();
        if stages.is_empty() && roles.is_empty() {
            return None;
        }
        Some(Self {
            pipeline: manifest.wrapper.as_ref().map(|wrapper| wrapper.id.clone()),
            stages,
            roles,
        })
    }
}

/// The sentence every surface says about a seat with no model.
///
/// Such a seat runs on the session's own model. Both surfaces have to say
/// so. Leaving the role out is the defect: the seat spends, and nothing
/// tells the user.
pub const UNASSIGNED_SEAT_SENTENCE: &str = "A seat with no `[seats]` line runs on your session's own model, so this package costs what a \
     single-model session costs until you assign one.";

/// The sentence every surface says about a tier.
///
/// A tier is a name the package asks for. What it maps to is the reader's
/// own setup. Naming a model here would be a claim this crate cannot keep.
pub const TIER_IS_AN_ASK_SENTENCE: &str = "A tier is the name this plugin asks for, never a model: your own provider configuration \
     decides what each one resolves to, and your key pays for it.";

/// The sentence every surface says about band order.
pub const BAND_ORDER_SENTENCE: &str = "Bands run in that order across every active plugin, not just this one: every `early` stage, \
     then every `normal` one, then every `late` one.";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> PluginManifest {
        PluginManifest::from_toml_str(toml).expect("fixture manifest parses")
    }

    #[test]
    fn a_manifest_that_adds_nothing_composes_nothing() {
        assert!(TurnComposition::of(&parse("name = \"p\"\n")).is_none());
    }

    #[test]
    fn a_stage_carries_its_band_and_its_condition() {
        let composed = TurnComposition::of(&parse(
            "name = \"p\"\n[loop]\nparticipation = \"steering\"\n\n\
             [wrapper]\nid = \"v\"\n\
             [[wrapper.stages]]\nname = \"triage-lite\"\nband = \"early\"\n\
             [[wrapper.stages]]\nname = \"plan\"\n",
        ))
        .expect("a wrapper composes something");
        assert_eq!(composed.pipeline.as_deref(), Some("v"));
        assert_eq!(composed.stages.len(), 2);
        assert_eq!(composed.stages[0].band, StageBand::Early);
        assert!(composed.stages[0].contributed);
        assert_eq!(composed.stages[1].band, StageBand::Normal);
        assert!(!composed.stages[1].contributed);
    }

    #[test]
    fn a_role_carries_the_seat_key_the_host_writes() {
        let composed = TurnComposition::of(&parse(
            "name = \"acme\"\n[loop]\nparticipation = \"steering\"\n\n\
             [subloop]\nstages = [\"triage\"]\n\n\
             [roles.triage]\ntier = \"cheap\"\n",
        ))
        .expect("a role composes something");
        assert_eq!(composed.roles.len(), 1);
        assert_eq!(composed.roles[0].seat, "acme/triage");
        assert_eq!(composed.roles[0].tier, "cheap");
    }
}
