//! `[lanes.custom.<id>]` — a lane a plugin ships, and what it asks to hold
//! (`doc:turn-lane-assembly` §9).
//!
//! A lane is one place a turn runs. The tree's own lanes are named in
//! [`stella_protocol::BuiltinLane`], and the compiler makes each of them
//! answer for every optional seam of the loop. A lane that arrives in a
//! manifest cannot be held to that, because no file can fail a build. So it
//! is held at load time instead, against
//! [`LaneCapability`], and this module is that check.
//!
//! # What the rules are, and why each one is here
//!
//! - **A lane names the seams it wants, one word each.** A word this build
//!   does not know is a load error naming the word. A grant nobody reads is a
//!   grant that quietly does nothing.
//! - **A lane may not take a builtin name.** Resolution is builtin first.
//!   `lead` is a lane that exists and is not yours, so naming it is refused
//!   rather than silently taken over (§9.7).
//! - **A lane says who resumes it.** An undeclared answer is refused, never
//!   read as `redispatch`. Who picks a dead turn back up decides what the
//!   lane owes when it dies, and a guess there is a report nobody asked for.
//! - **Every seam is answered, or it is reported.** A seam in neither list is
//!   a **defaulted slot**: the lane was written before the seam existed, so
//!   it holds nothing there and nobody decided that. `stella plugin doctor`
//!   prints those, which is the load-time stand-in for the compile error a
//!   builtin lane gets.
//!
//! # The set nests; it is never flattened
//!
//! `[lanes.custom.<id>]` is a map under a named key on purpose. The settings
//! merge walks a closed list of known keys, so a flattened map reads fine in
//! one scope and is dropped from the merge — a lost grant arriving through
//! the config plane (§9.7).
//!
//! # Asked, then granted
//!
//! What a manifest writes is a **request**. `granted = requested ∩
//! authorized`, and the two are kept apart so a report can show a lane that
//! asked for more than it holds. [`LaneAuthority`] is the second half, and
//! the answer this crate can give on its own is [`ConsentedGrade`]: a lane
//! may hold a seam whose risk sits at or under the rung a human accepted at
//! install. A host with a real gate narrows the grant this hands back — it
//! never widens it, and this crate stays unaware that a gate exists
//! (`stella_cli::plugin_authz::lanes`, `doc:turn-lane-assembly` §9.8).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use stella_protocol::{BuiltinLane, LaneCapability, LaneId, ResumeAuthority, RiskLevel, TurnLane};

use crate::error::ManifestError;
use crate::manifest::Participation;

/// The `[lanes]` block — the lanes this plugin ships.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lanes {
    /// `[lanes.custom.<id>]`, keyed by the lane's own id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom: BTreeMap<String, LaneDeclaration>,
}

/// One `[lanes.custom.<id>]` entry, as written.
///
/// The seam names are held as text rather than as
/// [`LaneCapability`] so a word this build does not know can be reported by
/// name. Read the checked form through [`Lanes::resolve`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneDeclaration {
    /// Who picks a dead turn on this lane back up. Required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeAuthority>,
    /// The seams this lane asks to hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// The seams this lane says it does not want.
    ///
    /// Not the same as leaving a seam out. A word here is an answer somebody
    /// wrote; a word in neither list is a seam nobody decided, and that is
    /// what `stella plugin doctor` reports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declined: Vec<String>,
}

/// A lane that passed every rule in this module.
///
/// Built by [`Lanes::resolve`] alone, so a value of this type has a known id,
/// a known resume authority, and only seam names this build knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredLane {
    id: LaneId,
    resume: ResumeAuthority,
    requested: BTreeSet<LaneCapability>,
    declined: BTreeSet<LaneCapability>,
}

impl DeclaredLane {
    /// The lane's id.
    #[must_use]
    pub fn id(&self) -> &LaneId {
        &self.id
    }

    /// The lane, in the shape a turn record carries.
    #[must_use]
    pub fn lane(&self) -> TurnLane {
        TurnLane::Plugin(self.id.clone())
    }

    /// Who picks a dead turn on this lane back up.
    #[must_use]
    pub fn resume(&self) -> ResumeAuthority {
        self.resume
    }

    /// The seams the lane asked for.
    #[must_use]
    pub fn requested(&self) -> &BTreeSet<LaneCapability> {
        &self.requested
    }

    /// The seams the lane said it does not want.
    #[must_use]
    pub fn declined(&self) -> &BTreeSet<LaneCapability> {
        &self.declined
    }

    /// The seams the lane answered for in neither list.
    ///
    /// These are the defaulted slots. The lane holds nothing there, and
    /// nobody said so — which is the state a report has to name rather than
    /// let pass.
    #[must_use]
    pub fn defaulted(&self) -> BTreeSet<LaneCapability> {
        LaneCapability::ALL
            .iter()
            .copied()
            .filter(|slot| !self.requested.contains(slot) && !self.declined.contains(slot))
            .collect()
    }

    /// The rung this lane sits on, read out of what it asked for.
    ///
    /// Derived, never written down twice. The ladder and the seam list are
    /// one statement at two grains (`doc:turn-lane-assembly` §9.3), so a
    /// second field for the rung would be a number in two places.
    #[must_use]
    pub fn participation(&self) -> Participation {
        participation_for(&self.requested)
    }

    /// What this lane holds once the host has had its say.
    ///
    /// `granted = requested ∩ authorized`. The two sets are both kept, so a
    /// reader can see a lane that asked for a seam it did not get.
    #[must_use]
    pub fn grant(&self, authority: &dyn LaneAuthority) -> LaneGrant {
        let granted = self
            .requested
            .iter()
            .copied()
            .filter(|capability| authority.authorizes(&self.id, *capability))
            .collect();
        LaneGrant {
            lane: self.id.clone(),
            requested: self.requested.clone(),
            granted,
        }
    }
}

/// What a lane asked for and what it holds, side by side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneGrant {
    /// The lane this is about.
    pub lane: LaneId,
    /// What the manifest asked for.
    pub requested: BTreeSet<LaneCapability>,
    /// What the host allowed, which is never more than it asked for.
    pub granted: BTreeSet<LaneCapability>,
}

impl LaneGrant {
    /// What the lane asked for and did not get.
    #[must_use]
    pub fn withheld(&self) -> BTreeSet<LaneCapability> {
        self.requested.difference(&self.granted).copied().collect()
    }
}

/// Whether a lane may hold a seam.
///
/// The host's half of `granted = requested ∩ authorized`. A host with an
/// authority plane answers from it; [`ConsentedGrade`] is what this crate can
/// answer on its own.
pub trait LaneAuthority {
    /// Whether `lane` may hold `capability`.
    fn authorizes(&self, lane: &LaneId, capability: LaneCapability) -> bool;
}

/// The rung a human accepted at install, read as a ceiling.
///
/// A lane may hold a seam whose risk sits at or under the rung's own. It
/// grants nothing a person did not read, and it needs no plane this crate
/// cannot see. A host that runs a gate wraps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsentedGrade(pub Participation);

impl LaneAuthority for ConsentedGrade {
    fn authorizes(&self, _lane: &LaneId, capability: LaneCapability) -> bool {
        match risk_ceiling(self.0) {
            Some(ceiling) => capability.risk().within(ceiling),
            None => false,
        }
    }
}

/// How far a rung of the ladder reaches, in the grade a gate reads.
///
/// `none` reaches nothing, so it has no ceiling at all rather than a low one.
#[must_use]
pub fn risk_ceiling(participation: Participation) -> Option<RiskLevel> {
    match participation {
        Participation::None => None,
        Participation::Observer => Some(RiskLevel::Low),
        Participation::Steering => Some(RiskLevel::Medium),
        Participation::Arbiter => Some(RiskLevel::High),
    }
}

/// The rung a set of seams sits on — the other reading of [`risk_ceiling`].
///
/// The lowest rung whose ceiling covers every seam in the set. An empty set
/// is `none`, since a lane that asks for nothing has no say in the turn.
#[must_use]
pub fn participation_for(capabilities: &BTreeSet<LaneCapability>) -> Participation {
    let Some(worst) = capabilities.iter().map(|c| c.risk()).max() else {
        return Participation::None;
    };
    match worst {
        RiskLevel::Low => Participation::Observer,
        RiskLevel::Medium => Participation::Steering,
        RiskLevel::High | RiskLevel::Destructive => Participation::Arbiter,
    }
}

/// Whether a lane id is one this workspace will print and store.
///
/// A closed set of characters rather than a list of things to turn away. It
/// keeps out whitespace, the seat separator, and every escape a name could
/// carry into chrome Stella draws, in one rule.
fn id_is_allowed(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

impl Lanes {
    /// Check every lane and hand back the checked form.
    ///
    /// The one door. Validation calls it and throws the result away; a reader
    /// calls it and keeps it. Two doors would be two sets of rules.
    ///
    /// # Errors
    ///
    /// Every rule in this module's docs, each as its own
    /// [`ManifestError`] case naming the lane it failed on.
    pub fn resolve(&self) -> Result<Vec<DeclaredLane>, ManifestError> {
        if self.custom.is_empty() {
            return Err(ManifestError::EmptyLanes);
        }
        let mut lanes = Vec::with_capacity(self.custom.len());
        for (id, declaration) in &self.custom {
            lanes.push(resolve_one(id, declaration)?);
        }
        Ok(lanes)
    }
}

fn resolve_one(id: &str, declaration: &LaneDeclaration) -> Result<DeclaredLane, ManifestError> {
    if !id_is_allowed(id) {
        return Err(ManifestError::LaneIdNotAllowed {
            lane: id.to_string(),
        });
    }
    // Builtin first, so a manifest naming a lane this tree already runs is
    // turned away rather than quietly taking it over.
    if BuiltinLane::ALL.iter().any(|lane| lane.as_str() == id) {
        return Err(ManifestError::LaneNamesABuiltin {
            lane: id.to_string(),
        });
    }
    let Some(resume) = declaration.resume else {
        return Err(ManifestError::LaneResumeUndeclared {
            lane: id.to_string(),
        });
    };

    let requested = named_set(id, &declaration.capabilities)?;
    let declined = named_set(id, &declaration.declined)?;
    if let Some(both) = requested.intersection(&declined).next() {
        return Err(ManifestError::LaneCapabilityBothWays {
            lane: id.to_string(),
            capability: *both,
        });
    }

    Ok(DeclaredLane {
        id: LaneId::new(id),
        resume,
        requested,
        declined,
    })
}

/// One list of seam names, checked into the set the register knows.
fn named_set(lane: &str, names: &[String]) -> Result<BTreeSet<LaneCapability>, ManifestError> {
    let mut set = BTreeSet::new();
    for name in names {
        let Some(capability) = LaneCapability::parse(name) else {
            return Err(ManifestError::UnknownLaneCapability {
                lane: lane.to_string(),
                name: name.clone(),
            });
        };
        if !set.insert(capability) {
            return Err(ManifestError::DuplicateLaneCapability {
                lane: lane.to_string(),
                capability,
            });
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(toml: &str) -> Result<Vec<DeclaredLane>, ManifestError> {
        let lanes: Lanes = toml::from_str(toml).expect("the block parses");
        lanes.resolve()
    }

    const REPLAY: &str = r#"
        [custom."acme.replay"]
        resume = "redispatch"
        capabilities = ["bus", "steering"]
        declined = ["gate"]
    "#;

    /// **The witness for a manifest-declared lane.** Nothing on the base
    /// commit can read a lane out of a manifest at all, so this whole shape
    /// is new.
    #[test]
    fn a_manifest_can_declare_a_lane() {
        let lanes = lane(REPLAY).expect("the lane loads");
        let [replay] = &lanes[..] else {
            panic!("one lane was declared");
        };
        assert_eq!(replay.id().as_str(), "acme.replay");
        assert_eq!(replay.resume(), ResumeAuthority::Redispatch);
        assert_eq!(
            replay.lane(),
            TurnLane::Plugin(LaneId::new("acme.replay")),
            "a declared lane is the open arm of `TurnLane`, never a builtin"
        );
        assert!(!replay.lane().totality_is_compile_enforced());
    }

    /// **The witness for the unknown name.** A word this build does not know
    /// is refused by name, so an author reads the word they mistyped.
    #[test]
    fn an_unknown_capability_name_is_refused_by_name() {
        let error = lane(
            r#"
            [custom."acme.replay"]
            resume = "own"
            capabilities = ["teleport"]
            "#,
        )
        .expect_err("an unknown seam name is a load error");
        assert!(
            matches!(&error, ManifestError::UnknownLaneCapability { lane, name }
                if lane == "acme.replay" && name == "teleport"),
            "got {error}"
        );
        assert!(error.to_string().contains("teleport"));
    }

    /// **The witness for builtin-first resolution.** A lane this tree already
    /// runs is not a name a manifest may take.
    #[test]
    fn a_lane_named_after_a_builtin_is_refused() {
        for builtin in BuiltinLane::ALL {
            let toml = format!(
                "[custom.{}]\nresume = \"own\"\ncapabilities = [\"bus\"]\n",
                builtin.as_str()
            );
            let error = lane(&toml).expect_err("a builtin name is a load error");
            assert!(
                matches!(&error, ManifestError::LaneNamesABuiltin { lane } if lane == builtin.as_str()),
                "got {error}"
            );
        }
    }

    /// **The witness for the resume rule.** Who resumes a lane is answered or
    /// the lane does not load; it is never read as a redispatch.
    #[test]
    fn an_undeclared_resume_authority_is_refused() {
        let error = lane(
            r#"
            [custom."acme.replay"]
            capabilities = ["bus"]
            "#,
        )
        .expect_err("an undeclared resume authority is a load error");
        assert!(
            matches!(&error, ManifestError::LaneResumeUndeclared { lane } if lane == "acme.replay"),
            "got {error}"
        );
    }

    /// **The witness for the defaulted slot.** A seam the lane answered for in
    /// neither list is named, so a report can print it.
    #[test]
    fn a_seam_in_neither_list_is_a_defaulted_slot() {
        let lanes = lane(REPLAY).expect("the lane loads");
        let defaulted = lanes[0].defaulted();
        assert!(
            !defaulted.contains(&LaneCapability::Bus),
            "bus was asked for"
        );
        assert!(
            !defaulted.contains(&LaneCapability::Gate),
            "gate was turned down in writing, which is an answer"
        );
        assert!(
            defaulted.contains(&LaneCapability::Requery),
            "requery is in neither list, so nobody decided it"
        );
        assert_eq!(
            defaulted.len(),
            LaneCapability::ALL.len() - 3,
            "every seam is either asked for, turned down, or defaulted"
        );
    }

    /// **The witness for the derived rung.** The ladder is read out of the
    /// seam list, so a manifest never writes it twice.
    #[test]
    fn the_rung_is_read_out_of_the_seam_list() {
        let cases = [
            (vec![], Participation::None),
            (vec![LaneCapability::Bus], Participation::Observer),
            (
                vec![LaneCapability::Bus, LaneCapability::Steering],
                Participation::Steering,
            ),
            (
                vec![LaneCapability::Bus, LaneCapability::Gate],
                Participation::Arbiter,
            ),
        ];
        for (capabilities, expected) in cases {
            let set: BTreeSet<_> = capabilities.into_iter().collect();
            assert_eq!(participation_for(&set), expected);
        }
    }

    /// **The witness for asked against held.** A lane that asks above the rung
    /// a human accepted keeps the ask on the record and holds less.
    #[test]
    fn a_lane_that_asks_above_its_rung_holds_less_than_it_asked_for() {
        let lanes = lane(REPLAY).expect("the lane loads");
        let grant = lanes[0].grant(&ConsentedGrade(Participation::Observer));

        assert_eq!(
            grant.requested,
            BTreeSet::from([LaneCapability::Bus, LaneCapability::Steering]),
            "the ask is kept as written"
        );
        assert_eq!(
            grant.granted,
            BTreeSet::from([LaneCapability::Bus]),
            "an observer holds the watching seam and no more"
        );
        assert_eq!(grant.withheld(), BTreeSet::from([LaneCapability::Steering]));

        let full = lanes[0].grant(&ConsentedGrade(Participation::Steering));
        assert_eq!(full.granted, full.requested, "a steering rung covers both");
        assert!(full.withheld().is_empty());
    }

    /// A rung with no say in the turn holds no seam, and `none` is that rung.
    #[test]
    fn a_content_bundle_holds_no_seam() {
        let lanes = lane(REPLAY).expect("the lane loads");
        let grant = lanes[0].grant(&ConsentedGrade(Participation::None));
        assert!(grant.granted.is_empty());
        assert_eq!(grant.withheld(), grant.requested);
    }

    #[test]
    fn a_name_in_both_lists_is_refused() {
        let error = lane(
            r#"
            [custom."acme.replay"]
            resume = "own"
            capabilities = ["bus"]
            declined = ["bus"]
            "#,
        )
        .expect_err("one seam, two answers");
        assert!(
            matches!(&error, ManifestError::LaneCapabilityBothWays { capability, .. }
                if *capability == LaneCapability::Bus),
            "got {error}"
        );
    }

    #[test]
    fn a_repeated_name_is_refused() {
        let error = lane(
            r#"
            [custom."acme.replay"]
            resume = "own"
            capabilities = ["bus", "bus"]
            "#,
        )
        .expect_err("a repeat is an editing mistake");
        assert!(
            matches!(&error, ManifestError::DuplicateLaneCapability { capability, .. }
                if *capability == LaneCapability::Bus),
            "got {error}"
        );
    }

    #[test]
    fn an_id_stella_cannot_print_is_refused() {
        for id in ["", " ", "Acme", "acme/replay", "acme replay", "acme\u{1b}"] {
            let mut custom = BTreeMap::new();
            custom.insert(
                id.to_string(),
                LaneDeclaration {
                    resume: Some(ResumeAuthority::Own),
                    capabilities: vec!["bus".to_string()],
                    declined: Vec::new(),
                },
            );
            let error = Lanes { custom }
                .resolve()
                .expect_err("an id outside the set is a load error");
            assert!(
                matches!(&error, ManifestError::LaneIdNotAllowed { .. }),
                "`{id}` got {error}"
            );
        }
    }

    #[test]
    fn a_lanes_block_with_no_lane_is_refused() {
        let error = Lanes::default()
            .resolve()
            .expect_err("an empty block asks for nothing and says something");
        assert!(matches!(error, ManifestError::EmptyLanes), "got {error}");
    }

    /// An unknown key inside the block is a load error, as it is in every
    /// other table here.
    #[test]
    fn an_unknown_key_in_the_block_is_refused() {
        let bad: Result<Lanes, _> = toml::from_str(
            r#"
            [custom."acme.replay"]
            resume = "own"
            participation = "arbiter"
            "#,
        );
        assert!(
            bad.is_err(),
            "the rung is derived, so writing it is a mistake"
        );
        let bad: Result<Lanes, _> = toml::from_str("[builtin]\n");
        assert!(bad.is_err(), "the block holds `custom` and nothing else");
    }

    /// The block round-trips, as everything crossing a crate line does
    /// (AGENTS.md #4).
    #[test]
    fn the_block_round_trips() {
        let lanes: Lanes = toml::from_str(REPLAY).expect("parses");
        let json = serde_json::to_string(&lanes).expect("serialises");
        let back: Lanes = serde_json::from_str(&json).expect("parses back");
        assert_eq!(back, lanes);
        assert_eq!(serde_json::to_string(&back).expect("re-serialises"), json);
    }
}
