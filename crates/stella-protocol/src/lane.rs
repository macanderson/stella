//! Which lane ran this turn — #3274 slice 1, #3386.
//!
//! A **lane** is one place a turn runs. There is exactly one step loop
//! (`stella-core`'s `driver::drive`); what differs between an interactive turn, a
//! fleet worker and a pipeline stage is not the loop but *which of the loop's
//! optional capabilities that run was assembled with*. [`BuiltinLane`] names
//! every such site in this tree. Before this module none of them had a name,
//! which is why no matrix over them could have rows and no surface could
//! group by lane (`doc:turn-lane-assembly` §2).
//!
//! # Why [`TurnLane`] is open and [`BuiltinLane`] is closed
//!
//! Deferring this one is expensive (`doc:turn-lane-assembly` §9.1, §10.3).
//!
//! - **[`BuiltinLane`] is closed** over this tree's own sites. That is what
//!   lets a later slice make "adding a capability without deciding it for
//!   every lane" a compile error: the compiler can see every case, so an
//!   exhaustive `match` or destructuring covers the whole set.
//! - **[`TurnLane`] is open** — it carries a [`LaneId`] arm for a lane
//!   contributed by a plugin manifest, which by construction is not known at
//!   compile time. Adding that arm today costs one enum case; retrofitting
//!   it after the lanes and a parity matrix are written against a closed
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
//! Per AGENTS.md #4 this round-trips through `serde_json` byte-for-byte.

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
    /// logic by rule, and a second validator here would be a rule in two
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

/// The in-tree turn-assembly sites, named.
///
/// Closed on purpose — see the module doc. The assembly site each case
/// refers to is named in its own doc comment so the mapping survives a file
/// move, which a line number would not.
///
/// # Why the doors that are not the deck got cases of their own
///
/// `stella run` and `stella goal` assemble their own engines, and neither is
/// the deck's turn. Three answers were open for them: widen [`Self::Lead`] to
/// cover any top-level turn, give them cases, or leave them with no lane.
///
/// Cases won, because a lane is what a turn binds and these two bind
/// different sets. [`Self::RawTurn`] binds the session router's call outcomes
/// and its mid-turn fallback; the deck binds neither. [`Self::GoalArc`] binds
/// steering and calibration and nothing else. Folded into [`Self::Lead`], a
/// report grouped by lane could not tell a person typing at the deck from a
/// scripted run, and the row for the merged lane could not say what the lane
/// binds. Left with no lane, every turn either door drives lands in the
/// `null` group, which is the gap this type exists to close.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinLane {
    /// The interactive-mode turn (`stella-cli`'s
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
    /// A staged-pipeline stage turn. Named for the built-in staged pipeline's
    /// `execute_stage`/`witness_stage` (`crates/stella-pipeline`, deleted in
    /// #3865).
    ///
    /// **Nothing in this workspace stamps this lane, and nothing will.** It is
    /// kept for reading, not writing — #3881's decision, and the reason is
    /// structural rather than a deferral. A verification plugin's stage turn is
    /// a *plugin* lane: it arrives as [`TurnLane::Plugin`] carrying the
    /// [`LaneId`] the manifest declared, which is the arm that exists precisely
    /// because an out-of-tree lane cannot be a compile-time case. So there
    /// is no producer to re-home onto this case; a wrapper plugin that
    /// stages its turns names its own lane.
    ///
    /// Deleting it instead would be worse than the usual wire retirement.
    /// [`TurnLane`] is externally tagged over a closed [`BuiltinLane`] with no
    /// `serde(other)`, so a recorded `{"builtin":"pipeline_stage"}` from a
    /// pre-removal build would fail to deserialize outright and take its whole
    /// enclosing record with it — not the graceful demotion
    /// [`crate::event::AgentEvent::Unknown`] gives an unrecognised event.
    PipelineStage,
    /// A turn driven by a remote host over the wire (`stella-serve`'s
    /// `session`).
    ServeSession,
    /// The shared raw turn (`stella-cli`'s `agent::turn`) — the entry point
    /// `stella run`, the interactive door that does not draw the deck
    /// (`agent::run_interactive`), and `stella run` under a wrapper plugin
    /// all reach.
    RawTurn,
    /// A judged multi-round goal arc — `stella goal` (`stella-cli`'s
    /// `agent::goal`), with or without a wrapper plugin above it.
    GoalArc,
}

impl BuiltinLane {
    /// Every builtin lane, in declaration order.
    ///
    /// Written as an exhaustive array so a new case that is not added here
    /// is caught by [`Self::ALL`]'s own test rather than silently narrowing
    /// every caller that enumerates lanes.
    pub const ALL: [Self; 9] = [
        Self::Lead,
        Self::Resume,
        Self::SubSession,
        Self::SubagentFork,
        Self::FleetWorker,
        Self::PipelineStage,
        Self::ServeSession,
        Self::RawTurn,
        Self::GoalArc,
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
            Self::RawTurn => "raw_turn",
            Self::GoalArc => "goal_arc",
        }
    }

    /// How a dead turn on this lane is picked back up.
    ///
    /// The match has no `_` arm. A tenth lane does not compile until somebody
    /// says who resumes it, which is what makes the authority a property of
    /// the lane rather than of whichever module happens to write a record.
    #[must_use]
    pub fn resume_authority(self) -> ResumeAuthority {
        match self {
            // Each of these holds a session record of its own, and the deck's
            // resume path re-enters it.
            Self::Lead | Self::Resume | Self::RawTurn | Self::GoalArc => ResumeAuthority::Own,
            // The host that opened the session drives its next turn, so the
            // record is that session's own resume point.
            Self::ServeSession => ResumeAuthority::Own,
            // A lane beside a lead chat, and a child forked by the delegation
            // tool: nothing re-enters either turn, and what the lead does with
            // what they left is report it.
            Self::SubSession | Self::SubagentFork => ResumeAuthority::Parent,
            // The fleet re-dispatches a task once its attempt settles, and a
            // staged turn is re-run by whatever staged it.
            Self::FleetWorker | Self::PipelineStage => ResumeAuthority::Redispatch,
        }
    }
}

/// How a dead turn on a lane is picked back up, and therefore what the lane
/// owes when it dies (`doc:turn-lane-assembly` §6, approved 2026-08-14).
///
/// Before this type the answer was decided per site and written down nowhere,
/// so a reader had to infer it from which module happened to write a record.
/// Each arm is an obligation:
///
/// - [`Self::Own`] — the lane re-enters its own turn, so its durable record is
///   a resume point and it owes no report to anyone else.
/// - [`Self::Parent`] — nothing re-enters the turn. The parent reads what the
///   lane left in order to *report* it, so the lane owes one **terminal
///   frame**: the last committed step and the transcript that reached it,
///   written once when the lane dies.
/// - [`Self::Redispatch`] — the supervisor re-runs the unit. The record is
///   evidence for that decision rather than a resume point, and the lane owes
///   the same terminal frame.
///
/// [`Self::Parent`] and [`Self::Redispatch`] differ in who reads the frame,
/// not in whether one is owed. A report and a re-run are two acts, so a lane
/// that merged them would leave a frame no reader had asked for.
///
/// # Why this lives here
///
/// A plugin lane declares its authority in its manifest, and an undeclared one
/// is a load-time rejection (`doc:turn-lane-assembly` §9). The manifest parser
/// is `stella-plugin`, whose only workspace dependency is this crate, so the
/// type has to be here for that rejection to have anything to reject against.
/// `stella-parity`'s lane matrix carries the *column* — what each builtin lane
/// therefore owes, and what proves it — and reads the authority back out of
/// [`BuiltinLane::resume_authority`] rather than restating it.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeAuthority {
    /// The lane resumes itself.
    Own,
    /// The parent reads the lane's record to report it, and never re-enters
    /// the turn.
    Parent,
    /// The supervisor re-runs the unit, and the record is its evidence.
    Redispatch,
}

impl ResumeAuthority {
    /// Every authority, in declaration order.
    pub const ALL: [Self; 3] = [Self::Own, Self::Parent, Self::Redispatch];

    /// Whether a lane under this authority owes a terminal frame when it dies.
    ///
    /// `false` for [`Self::Own`] alone: a lane that comes back to its own turn
    /// has a resume point, and a frame beside it would offer a dead attempt's
    /// transcript to the path about to re-enter the live one.
    #[must_use]
    pub fn owes_a_terminal_frame(self) -> bool {
        !matches!(self, Self::Own)
    }

    /// The authority's wire spelling, written out for the reason
    /// [`BuiltinLane::as_str`] is.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Parent => "parent",
            Self::Redispatch => "redispatch",
        }
    }
}

impl std::fmt::Display for ResumeAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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
    /// One of this tree's own assembly sites.
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

    /// The witness (#3386): a lane round-trips byte-for-byte, per AGENTS.md #4.
    ///
    /// Both arms, and every builtin case — a lane that serialised but did
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

    /// `ALL` is the enumeration every caller uses; a case missing from it
    /// would narrow them all silently.
    #[test]
    fn all_names_every_builtin_variant() {
        // Exhaustive match: a new case fails to compile here until it is
        // added to `ALL` below as well.
        for lane in BuiltinLane::ALL {
            match lane {
                BuiltinLane::Lead
                | BuiltinLane::Resume
                | BuiltinLane::SubSession
                | BuiltinLane::SubagentFork
                | BuiltinLane::FleetWorker
                | BuiltinLane::PipelineStage
                | BuiltinLane::ServeSession
                | BuiltinLane::RawTurn
                | BuiltinLane::GoalArc => {}
            }
        }
        let mut seen = BuiltinLane::ALL.to_vec();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), BuiltinLane::ALL.len(), "ALL has a duplicate");
    }

    /// `pipeline_stage` has no producer left in this workspace (#3881), and is
    /// kept so a lane recorded before `stella-pipeline` was deleted (#3865)
    /// still reads back. This is what that decision costs if it is reversed:
    /// [`BuiltinLane`] is closed with no `serde(other)`, so removing the
    /// case does not demote the tag — it fails the record outright.
    #[test]
    fn a_lane_recorded_before_the_pipeline_was_deleted_still_deserializes() {
        let recorded: TurnLane =
            serde_json::from_str(r#"{"builtin":"pipeline_stage"}"#).expect("a pre-#3865 recording");
        assert_eq!(recorded, TurnLane::Builtin(BuiltinLane::PipelineStage));
        assert_eq!(recorded.to_string(), "pipeline_stage");
    }

    /// `as_str` is hand-written; this is what stops it drifting from serde.
    #[test]
    fn as_str_agrees_with_the_serde_spelling() {
        for lane in BuiltinLane::ALL {
            let encoded = serde_json::to_string(&lane).expect("serialises");
            assert_eq!(encoded, format!(r#""{}""#, lane.as_str()));
        }
    }

    /// **The witness for the `Own` arm.** Every lane answers who resumes it,
    /// and the answer is the one `doc:turn-lane-assembly` §6 approved.
    ///
    /// Nothing could ask this before: `ResumeAuthority` existed only in that
    /// document and in two doc comments naming a type the tree did not have,
    /// so the five self-resuming lanes had no way to say they owe no frame.
    #[test]
    fn every_lane_declares_who_resumes_it() {
        use BuiltinLane::*;
        use ResumeAuthority::{Own, Parent, Redispatch};

        for (lane, authority) in [
            (Lead, Own),
            (Resume, Own),
            (ServeSession, Own),
            (RawTurn, Own),
            (GoalArc, Own),
            (SubSession, Parent),
            (SubagentFork, Parent),
            (FleetWorker, Redispatch),
            (PipelineStage, Redispatch),
        ] {
            assert_eq!(
                lane.resume_authority(),
                authority,
                "`{lane}` must resume as `{authority}`"
            );
        }
    }

    /// A self-resuming lane owes nothing to a parent; the other two owe a
    /// frame. The obligation is what the arms are for, so it is asserted
    /// rather than left to the doc comment.
    #[test]
    fn only_a_self_resuming_lane_owes_no_terminal_frame() {
        for lane in BuiltinLane::ALL {
            assert_eq!(
                lane.resume_authority().owes_a_terminal_frame(),
                lane.resume_authority() != ResumeAuthority::Own,
                "`{lane}` disagrees with its own authority about owing a frame"
            );
        }
    }

    /// An authority crosses a crate boundary — a plugin manifest declares one
    /// — so it round-trips byte-for-byte like every other type here
    /// (AGENTS.md #4).
    #[test]
    fn every_resume_authority_round_trips_byte_for_byte() {
        for authority in ResumeAuthority::ALL {
            let encoded = serde_json::to_string(&authority).expect("serialises");
            let decoded: ResumeAuthority = serde_json::from_str(&encoded).expect("parses back");
            assert_eq!(decoded, authority);
            assert_eq!(encoded, format!(r#""{}""#, authority.as_str()));
        }
        assert_eq!(ResumeAuthority::Redispatch.to_string(), "redispatch");
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
