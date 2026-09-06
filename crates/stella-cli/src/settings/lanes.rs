//! `lanes.custom.<id>` — the operator's ceiling on a lane a plugin ships
//! (`doc:turn-lane-assembly` §9.4).
//!
//! A manifest **asks** for the turn-loop seams its lane wants. The host
//! decides what it holds: `granted = requested ∩ authorized`. The rung a
//! person accepted at install is one half of what is authorized, and
//! `stella-plugin` reads that half on its own. This block is the other: it is
//! where an operator writes the seams a lane may hold on this machine or in
//! this workspace.
//!
//! # Why it nests under a named key
//!
//! The scope merge walks a closed list of known keys. A map folded into the
//! root with `serde(flatten)` reads fine from one file and is dropped on the
//! way through the merge, so a project's answer would go missing with nothing
//! to notice it. `[lanes.custom.<id>]` is a named key holding a map, which is
//! the shape the merge can carry.
//!
//! # A scope may narrow a ceiling; it may never widen one
//!
//! Scopes fold by intersection, per lane. A lane no scope names has no
//! ceiling, and keeps what its install rung allows. A lane two scopes name
//! holds only what both allow.
//!
//! That is the `plugins` switch's rule in another shape. It is why this key
//! is safe to read from a repository you cloned: a project file can only take
//! a seam away.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use stella_protocol::{LaneCapability, LaneId};

/// The `lanes` block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaneSettings {
    /// `lanes.custom.<id>` — the ceiling for one lane a plugin ships, keyed
    /// by the lane id its manifest declared.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub custom: BTreeMap<String, LaneCeiling>,
}

/// What one lane may hold here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaneCeiling {
    /// The seams this lane may hold, by the names
    /// [`LaneCapability`] uses.
    ///
    /// Held as text so a name this build does not know can be reported
    /// rather than dropped. An empty list is a real answer: this lane holds
    /// nothing here.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

impl LaneCeiling {
    /// The seams this build knows among the names written.
    #[must_use]
    pub fn known(&self) -> BTreeSet<LaneCapability> {
        self.capabilities
            .iter()
            .filter_map(|name| LaneCapability::parse(name))
            .collect()
    }

    /// The names written that this build does not know.
    ///
    /// Reported by `stella plugin doctor` rather than refused at load. A
    /// settings file is read by every build of Stella a person runs, so a
    /// name from a newer build has to narrow rather than stop the session.
    #[must_use]
    pub fn unknown(&self) -> Vec<&str> {
        self.capabilities
            .iter()
            .filter(|name| LaneCapability::parse(name).is_none())
            .map(String::as_str)
            .collect()
    }
}

impl LaneSettings {
    /// Fold one scope in, narrowing each lane it names.
    ///
    /// A lane the scope does not name is left alone. A lane it does name
    /// keeps only the seams both sides allow, so no scope can hand back what
    /// another took away.
    pub fn narrow(&mut self, scope: &Self) {
        for (lane, ceiling) in &scope.custom {
            match self.custom.entry(lane.clone()) {
                Entry::Occupied(mut held) => {
                    let allowed = ceiling.known();
                    held.get_mut()
                        .capabilities
                        .retain(|name| match LaneCapability::parse(name) {
                            Some(capability) => allowed.contains(&capability),
                            // A name neither side reads cannot be part of an
                            // intersection, so it is dropped here rather than
                            // carried into a report as a grant.
                            None => false,
                        });
                }
                Entry::Vacant(slot) => {
                    slot.insert(ceiling.clone());
                }
            }
        }
    }

    /// The ceiling for one lane, or `None` when no scope named it.
    #[must_use]
    pub fn ceiling(&self, lane: &LaneId) -> Option<&LaneCeiling> {
        self.custom.get(lane.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> LaneSettings {
        serde_json::from_str(json).expect("the block parses")
    }

    /// **The witness for the scope merge.** A project scope's answer about a
    /// lane reaches the merged view. A lane only the user scope named
    /// survives beside it.
    ///
    /// `doc:turn-lane-assembly` §9.7 names the failure this rules out. A
    /// flattened map reads fine from one file. The overlay then drops it, so
    /// the project's line does nothing and says nothing.
    #[test]
    fn a_project_scope_answer_is_not_dropped() {
        let user = parse(
            r#"{"custom":{"acme.replay":{"capabilities":["bus","steering"]},
                          "acme.watch":{"capabilities":["bus"]}}}"#,
        );
        let project = parse(r#"{"custom":{"acme.replay":{"capabilities":["bus"]}}}"#);

        let mut merged = LaneSettings::default();
        merged.narrow(&user);
        merged.narrow(&project);

        let replay = merged
            .ceiling(&LaneId::new("acme.replay"))
            .expect("the lane both scopes named is in the merged view");
        assert_eq!(
            replay.known(),
            BTreeSet::from([LaneCapability::Bus]),
            "the project narrowed the lane and the narrowing survived the merge"
        );
        let watch = merged
            .ceiling(&LaneId::new("acme.watch"))
            .expect("a lane only the user scope named survives");
        assert_eq!(watch.known(), BTreeSet::from([LaneCapability::Bus]));
    }

    /// A later scope cannot hand back a seam an earlier one took away. That
    /// is what makes this key safe to read from a repository you cloned.
    #[test]
    fn a_later_scope_cannot_widen_an_earlier_ceiling() {
        let user = parse(r#"{"custom":{"acme.replay":{"capabilities":["bus"]}}}"#);
        let project = parse(r#"{"custom":{"acme.replay":{"capabilities":["bus","gate"]}}}"#);

        let mut merged = LaneSettings::default();
        merged.narrow(&user);
        merged.narrow(&project);

        assert_eq!(
            merged
                .ceiling(&LaneId::new("acme.replay"))
                .expect("the lane is held")
                .known(),
            BTreeSet::from([LaneCapability::Bus]),
            "the project asked for a seam the user scope had already withheld"
        );
    }

    /// A name this build does not know is reported, never read as a grant.
    #[test]
    fn a_name_this_build_does_not_know_grants_nothing() {
        let ceiling = parse(r#"{"custom":{"acme.replay":{"capabilities":["bus","teleport"]}}}"#);
        let lane = ceiling
            .ceiling(&LaneId::new("acme.replay"))
            .expect("the lane is held");
        assert_eq!(lane.known(), BTreeSet::from([LaneCapability::Bus]));
        assert_eq!(lane.unknown(), vec!["teleport"]);
    }

    /// An empty list is an answer: this lane holds nothing here.
    #[test]
    fn an_empty_list_withholds_every_seam() {
        let settings = parse(r#"{"custom":{"acme.replay":{"capabilities":[]}}}"#);
        let lane = settings
            .ceiling(&LaneId::new("acme.replay"))
            .expect("the lane is held");
        assert!(lane.known().is_empty());
        assert!(settings.ceiling(&LaneId::new("acme.other")).is_none());
    }
}
