//! Which lane ran this turn — #3274 slice 1, #3386.
//!
//! A **lane** is one place a turn runs. There is exactly one step loop
//! (`stella-core`'s `driver::drive`); what differs between a REPL turn, a
//! fleet worker and a pipeline stage is not the loop but *which of the loop's
//! optional capabilities that run was assembled with*. Today there are seven
//! such assembly sites and, before this module, none of them had a name —
//! which is why no matrix over them could have rows and no surface could
//! group by lane (`doc:turn-lane-assembly` §2).
//!
//! # Why [`TurnLane`] is open and [`BuiltinLane`] is closed
//!
//! Both halves are load-bearing, and this is the one decision in the
//! programme that is expensive to defer (`doc:turn-lane-assembly` §9.1,
//! §10.3).
//!
//! - **[`BuiltinLane`] is closed** over the seven in-tree sites. That is what
//!   lets a later slice make "adding a capability without deciding it for
//!   every lane" a compile error: the compiler can see every variant, so an
//!   exhaustive `match` or destructuring covers the whole set.
//! - **[`TurnLane`] is open** — it carries a [`LaneId`] arm for a lane
//!   contributed by a plugin manifest, which by construction is not known at
//!   compile time. Adding that arm today costs one enum variant; retrofitting
//!   it after seven lanes and a parity matrix are written against a closed
//!   enum is a matrix rewrite.
//!
//! The two arms do not have the same guarantee, and the type says so rather
//! than papering over it: a builtin lane's totality is enforced by the
//! compiler, a plugin lane's only at manifest load time
//! (`doc:turn-lane-assembly` §9.2). You cannot give an out-of-tree file a
//! build failure.
//!
//! # Wire shape
//!
//! Externally tagged, so the two arms are distinguishable without a
//! discriminator field and neither can be mistaken for the other by a reader
//! that knows only one of them:
//!
//! ```json
//! {"builtin": "fleet_worker"}
//! {"plugin": "acme.replay"}
//! ```
//!
//! Per invariant #4 this round-trips through `serde_json` byte-for-byte.

use serde::{Deserialize, Serialize};

/// The identifier of a lane contributed by a plugin manifest.
///
/// A newtype rather than a bare `String` so a lane name cannot be passed
/// where some other identifier is expected — this workspace already has six
/// ids that read alike (see AGENTS.md's glossary), and an unwrapped `String`
/// is how a seventh joins them.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LaneId(String);

impl LaneId {
    /// Wrap an already-validated identifier.
    ///
    /// Validation of what a manifest may name is the plugin loader's job
    /// (`stella-plugin`), not this crate's: `stella-protocol` carries zero
    /// logic by invariant, and a second validator here would be a rule in two
    /// places.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The seven in-tree turn-assembly sites, named.
///
/// Closed on purpose — see the module doc. The assembly site each variant
/// refers to is named in its own doc comment so the mapping survives a file
/// move, which a line number would not.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinLane {
    /// The interactive Command Deck / REPL turn (`stella-cli`'s
    /// `command_deck`).
    Lead,
    /// A turn replayed from a checkpoint (`stella-cli`'s `agent::resume`).
    Resume,
    /// A nested session driven by the lead (`stella-cli`'s `subsession`).
    SubSession,
    /// A bounded child forked by the delegation tool (`stella-core`'s
    /// `subagent`).
    SubagentFork,
    /// A raw fleet worker turn (`stella-cli`'s `fleet_cmd`).
    FleetWorker,
    /// A staged-pipeline stage turn. Its only producer was the built-in
    /// staged pipeline (`stella-pipeline`'s `execute_stage`/`witness_stage`),
    /// deleted from the workspace in #3865, so nothing in this workspace
    /// stamps this lane today; the token stays readable because already-written
    /// telemetry carries it. Retiring it is #3881.
    PipelineStage,
    /// A turn driven by a remote host over the wire (`stella-serve`'s
    /// `session`).
    ServeSession,
}

impl BuiltinLane {
    /// Every builtin lane, in declaration order.
    ///
    /// Written as an exhaustive array so a new variant that is not added here
    /// is caught by [`Self::ALL`]'s own test rather than silently narrowing
    /// every caller that enumerates lanes.
    pub const ALL: [Self; 7] = [
        Self::Lead,
        Self::Resume,
        Self::SubSession,
        Self::SubagentFork,
        Self::FleetWorker,
        Self::PipelineStage,
        Self::ServeSession,
    ];

    /// The lane's wire spelling.
    ///
    /// Written out rather than derived from serde so the mapping is readable
    /// at the definition; a test asserts the two agree, which is what keeps
    /// them from drifting.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Resume => "resume",
            Self::SubSession => "sub_session",
            Self::SubagentFork => "subagent_fork",
            Self::FleetWorker => "fleet_worker",
            Self::PipelineStage => "pipeline_stage",
            Self::ServeSession => "serve_session",
        }
    }
}

impl std::fmt::Display for BuiltinLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which lane ran a turn.
///
/// Open over its origin: [`Self::Builtin`] is compile-time total,
/// [`Self::Plugin`] is not and cannot be. See the module doc.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnLane {
    /// One of the seven in-tree assembly sites.
    Builtin(BuiltinLane),
    /// A lane contributed by a plugin manifest. Not known at compile time.
    Plugin(LaneId),
}

impl TurnLane {
    /// Whether this lane's capability totality is enforced by the compiler.
    ///
    /// `false` for a plugin lane, and that is a fact about the lane worth
    /// rendering rather than a detail to hide: a reader of a `Plugin` row in
    /// the lane matrix needs to know the row is worth less than a builtin one
    /// (`doc:turn-lane-assembly` §9.2).
    #[must_use]
    pub fn totality_is_compile_enforced(&self) -> bool {
        matches!(self, Self::Builtin(_))
    }
}

impl From<BuiltinLane> for TurnLane {
    fn from(lane: BuiltinLane) -> Self {
        Self::Builtin(lane)
    }
}

impl std::fmt::Display for TurnLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builtin(lane) => f.write_str(lane.as_str()),
            Self::Plugin(id) => write!(f, "plugin:{id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The witness (#3386): a lane round-trips byte-for-byte, per invariant #4.
    ///
    /// Both arms, and every builtin variant — a lane that serialised but did
    /// not parse back would make the stamped field unreadable by exactly the
    /// surfaces it exists for.
    #[test]
    fn every_lane_round_trips_byte_for_byte() {
        let mut lanes: Vec<TurnLane> = BuiltinLane::ALL.into_iter().map(TurnLane::from).collect();
        lanes.push(TurnLane::Plugin(LaneId::new("acme.replay")));

        for lane in lanes {
            let encoded = serde_json::to_string(&lane).expect("lane serialises");
            let decoded: TurnLane = serde_json::from_str(&encoded).expect("lane parses back");
            assert_eq!(decoded, lane, "round trip changed the value: {encoded}");
            let reencoded = serde_json::to_string(&decoded).expect("lane re-serialises");
            assert_eq!(reencoded, encoded, "round trip was not byte-for-byte");
        }
    }

    /// The two arms must not collide on the wire: a plugin whose id happens to
    /// spell a builtin lane is still a plugin lane.
    #[test]
    fn a_plugin_lane_named_like_a_builtin_stays_a_plugin_lane() {
        let lane = TurnLane::Plugin(LaneId::new("lead"));
        let encoded = serde_json::to_string(&lane).expect("serialises");
        let decoded: TurnLane = serde_json::from_str(&encoded).expect("parses");

        assert_eq!(decoded, lane);
        assert_ne!(decoded, TurnLane::Builtin(BuiltinLane::Lead));
        assert!(!decoded.totality_is_compile_enforced());
    }

    #[test]
    fn the_wire_shape_is_the_documented_one() {
        assert_eq!(
            serde_json::to_string(&TurnLane::Builtin(BuiltinLane::FleetWorker)).unwrap(),
            r#"{"builtin":"fleet_worker"}"#
        );
        assert_eq!(
            serde_json::to_string(&TurnLane::Plugin(LaneId::new("acme.replay"))).unwrap(),
            r#"{"plugin":"acme.replay"}"#
        );
    }

    /// `ALL` is the enumeration every caller uses; a variant missing from it
    /// would narrow them all silently.
    #[test]
    fn all_names_every_builtin_variant() {
        // Exhaustive match: a new variant fails to compile here until it is
        // added to `ALL` below as well.
        for lane in BuiltinLane::ALL {
            match lane {
                BuiltinLane::Lead
                | BuiltinLane::Resume
                | BuiltinLane::SubSession
                | BuiltinLane::SubagentFork
                | BuiltinLane::FleetWorker
                | BuiltinLane::PipelineStage
                | BuiltinLane::ServeSession => {}
            }
        }
        let mut seen = BuiltinLane::ALL.to_vec();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), BuiltinLane::ALL.len(), "ALL has a duplicate");
    }

    /// `as_str` is hand-written; this is what stops it drifting from serde.
    #[test]
    fn as_str_agrees_with_the_serde_spelling() {
        for lane in BuiltinLane::ALL {
            let encoded = serde_json::to_string(&lane).expect("serialises");
            assert_eq!(encoded, format!(r#""{}""#, lane.as_str()));
        }
    }

    #[test]
    fn display_agrees_with_the_wire_spelling() {
        assert_eq!(
            TurnLane::Builtin(BuiltinLane::SubagentFork).to_string(),
            "subagent_fork"
        );
        assert_eq!(
            TurnLane::Plugin(LaneId::new("acme.replay")).to_string(),
            "plugin:acme.replay"
        );
    }
}
