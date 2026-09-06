// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What each turn lane binds, seam by seam.
//!
//! [`super::LANES`] says who stamps a lane name. It stops there. The engine
//! carries eleven capability slots, and every lane answers all of them in a
//! struct literal in its own crate. Those literals are right, and the
//! compiler holds them there. What was missing is one table a reader can
//! open.
//!
//! This is that table. One row per lane and seam.
//!
//! Three asks, not two. A seam a lane refuses is a
//! [`SeamRequest::Declined`], and it carries the reason. A seam nobody has
//! reached is a [`SeamRequest::Deferred`], and it cites the issue that will
//! settle it. Before this table both of them read as a bare `None`.
//!
//! Two answers, not one. [`SeamRow::requested`] is what the lane asked for.
//! [`SeamRow::granted`] is what it got. Every lane here gets what it asks,
//! because it compiles its own literal: the ask and the grant are one edit.
//! A lane an installed plugin brings will not, and the column is here so the
//! gate that cuts one down has somewhere to write.
//!
//! **What it does not prove.** A witness named here is checked for
//! existence, never read. A row naming a test that proves something else
//! still passes, because the name of a test is not evidence for it. So a
//! seam with no test of its own says
//! [`Witness::Literal`] rather than borrow a neighbour's, and
//! [`UNWITNESSED_SEAMS`] counts those.

use stella_protocol::BuiltinLane;

/// Generate the seam enum and its field names from one table.
///
/// One place to edit. A slot added to the engine's capability struct needs a
/// case here, and a case here needs a line in every lane block below.
macro_rules! seams {
    ($(
        $(#[$meta:meta])*
        $case:ident => $field:literal;
    )*) => {
        /// One capability slot on the engine's `TurnCapabilities`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Seam {
            $($(#[$meta])* $case,)*
        }

        impl Seam {
            /// Every seam, in the order the engine declares them.
            pub const ALL: &'static [Self] = &[$(Self::$case,)*];

            /// The field name this seam stands for.
            #[must_use]
            pub const fn field(self) -> &'static str {
                match self {
                    $(Self::$case => $field,)*
                }
            }
        }
    };
}

/// Generate the matrix from one table, with every seam spelled per lane.
///
/// The seam names in the pattern are the totality gate. Adding a seam means
/// adding a line here, and that line then stops every lane block matching
/// until each one answers. A lane cannot skip a seam and compile.
macro_rules! lane_capabilities {
    ($(
        $lane:ident => $origin:expr, $site:expr,
        {
            hooks: $hooks:expr,
            hook_approvals: $hook_approvals:expr,
            calibration: $calibration:expr,
            gate: $gate:expr,
            steering: $steering:expr,
            requery: $requery:expr,
            bus: $bus:expr,
            outcomes: $outcomes:expr,
            fallback: $fallback:expr,
            call_role: $call_role:expr,
            lane: $lane_seam:expr,
        }
    )*) => {
        /// Every builtin lane, with every seam answered.
        ///
        /// The tests check both directions. Every lane gets one block, every
        /// block gets one row per seam, and every row is checked against the
        /// literal its lane compiles.
        pub static LANE_CAPABILITIES: &[LaneCapabilities] = &[
            $(LaneCapabilities {
                lane: BuiltinLane::$lane,
                origin: $origin,
                site: $site,
                seams: &[
                    SeamRow::new(Seam::Hooks, $hooks),
                    SeamRow::new(Seam::HookApprovals, $hook_approvals),
                    SeamRow::new(Seam::Calibration, $calibration),
                    SeamRow::new(Seam::Gate, $gate),
                    SeamRow::new(Seam::Steering, $steering),
                    SeamRow::new(Seam::Requery, $requery),
                    SeamRow::new(Seam::Bus, $bus),
                    SeamRow::new(Seam::Outcomes, $outcomes),
                    SeamRow::new(Seam::Fallback, $fallback),
                    SeamRow::new(Seam::CallRole, $call_role),
                    SeamRow::new(Seam::Lane, $lane_seam),
                ],
            },)*
        ];
    };
}

seams! {
    /// Lifecycle hooks and the port that runs them.
    Hooks => "hooks";
    /// Where a pre-tool approval decision parks.
    HookApprovals => "hook_approvals";
    /// The token-drift map the caller owns across turns.
    Calibration => "calibration";
    /// The boundary pause gate.
    Gate => "gate";
    /// Step-boundary steering and the soft stop.
    Steering => "steering";
    /// Step-boundary context re-query.
    Requery => "requery";
    /// The extension hook bus.
    Bus => "bus";
    /// Call-outcome feedback for a router's breaker.
    Outcomes => "outcomes";
    /// Mid-turn provider fallback.
    Fallback => "fallback";
    /// What this engine's model calls are billed to. Always answered.
    CallRole => "call_role";
    /// Which lane assembled the engine. Always answered.
    Lane => "lane";
}

/// What proves a bound seam arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Witness {
    /// A test with this name. Checked for existence in the swept sources.
    Test(&'static str),
    /// No test names this seam yet. The literal is the only evidence, and
    /// the source check reads it. Counted by [`UNWITNESSED_SEAMS`].
    Literal,
}

/// What a lane asks for at one seam.
#[derive(Debug, Clone, Copy)]
pub enum SeamRequest {
    /// The lane binds it.
    Bound {
        /// The text the lane's literal writes for this field. Checked
        /// against the source, so a lane that stops binding fails here.
        binds: &'static str,
        /// What proves the seam arrives.
        witness: Witness,
    },
    /// The lane will not bind it, and here is why. A decision somebody made.
    Declined {
        /// Why. Prose a reviewer can weigh.
        reason: &'static str,
    },
    /// Nobody has reached it. A gap, said out loud.
    Deferred {
        /// The issue that will settle it, cited as `Refs #NNNN`.
        issue: &'static str,
        /// What the answer is waiting on.
        waiting_on: &'static str,
    },
}

impl SeamRequest {
    /// The witness this ask names, when it names one.
    #[must_use]
    pub const fn witness(&self) -> Option<Witness> {
        match self {
            Self::Bound { witness, .. } => Some(*witness),
            Self::Declined { .. } | Self::Deferred { .. } => None,
        }
    }

    /// Whether the lane binds this seam.
    #[must_use]
    pub const fn is_bound(&self) -> bool {
        matches!(self, Self::Bound { .. })
    }
}

/// What the lane got.
#[derive(Debug, Clone, Copy)]
pub enum SeamGrant {
    /// The ask stands as written. Every lane in this table answers so: it
    /// compiles its own literal, and nothing sits between ask and answer.
    AsRequested,
    /// Something cut the ask down, and says who and why. No lane here needs
    /// it. A lane an installed plugin brings will, and adding the column
    /// later would be a schema change under a live table.
    Withheld {
        /// Who refused, in that gate's own words.
        authority: &'static str,
        /// Why.
        reason: &'static str,
    },
}

/// Where a lane comes from, and so what its row is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneOrigin {
    /// A lane this workspace compiles. The compiler holds its literal, so
    /// the ask and the grant are one edit.
    Builtin,
    /// A lane an installed plugin brings. Its ask arrives at load time and a
    /// gate answers it, so the two columns can differ.
    Plugin,
}

/// A lane's ask and answer at one seam.
#[derive(Debug, Clone, Copy)]
pub struct SeamClaim {
    /// What the lane asked for.
    pub requested: SeamRequest,
    /// What it got.
    pub granted: SeamGrant,
}

impl SeamClaim {
    /// The lane binds the seam, and gets it.
    #[must_use]
    pub const fn bound(binds: &'static str, witness: Witness) -> Self {
        Self {
            requested: SeamRequest::Bound { binds, witness },
            granted: SeamGrant::AsRequested,
        }
    }

    /// The lane will not bind the seam, and says why.
    #[must_use]
    pub const fn declined(reason: &'static str) -> Self {
        Self {
            requested: SeamRequest::Declined { reason },
            granted: SeamGrant::AsRequested,
        }
    }

    /// Nobody has decided, and the issue that will is named.
    #[must_use]
    pub const fn deferred(issue: &'static str, waiting_on: &'static str) -> Self {
        Self {
            requested: SeamRequest::Deferred { issue, waiting_on },
            granted: SeamGrant::AsRequested,
        }
    }

    /// The lane asked and a gate refused. Nothing builds one of these yet.
    /// It is the shape a plugin lane's row takes.
    #[must_use]
    pub const fn withheld(
        requested: SeamRequest,
        authority: &'static str,
        reason: &'static str,
    ) -> Self {
        Self {
            requested,
            granted: SeamGrant::Withheld { authority, reason },
        }
    }
}

/// One row of the matrix: a seam, an ask, and an answer.
#[derive(Debug, Clone, Copy)]
pub struct SeamRow {
    /// Which slot this row is about.
    pub seam: Seam,
    /// What the lane asked for.
    pub requested: SeamRequest,
    /// What it got.
    pub granted: SeamGrant,
}

impl SeamRow {
    /// One row from a seam and a claim.
    #[must_use]
    pub const fn new(seam: Seam, claim: SeamClaim) -> Self {
        Self {
            seam,
            requested: claim.requested,
            granted: claim.granted,
        }
    }
}

/// Where a lane's capability literal lives.
#[derive(Debug, Clone, Copy)]
pub struct LaneSite {
    /// The workspace-relative file.
    pub file: &'static str,
    /// Text just above the literal. The source check starts here, takes the
    /// next opening of the capability struct, and reads to its close.
    pub anchor: &'static str,
}

/// One lane, with every seam answered.
#[derive(Debug, Clone, Copy)]
pub struct LaneCapabilities {
    /// The lane this block is about.
    pub lane: BuiltinLane,
    /// Where the lane comes from.
    pub origin: LaneOrigin,
    /// The literal to check the rows against. `None` for a lane nothing
    /// assembles.
    pub site: Option<LaneSite>,
    /// One row per seam, in the order the engine declares them.
    pub seams: &'static [SeamRow],
}

impl LaneCapabilities {
    /// This lane's row for `seam`, or `None`.
    #[must_use]
    pub fn seam(&self, seam: Seam) -> Option<&SeamRow> {
        self.seams.iter().find(|row| row.seam == seam)
    }
}

/// The block for `lane`, or `None`. The tests make every builtin lane
/// resolve, so a `None` means the caller named something that is not a lane.
#[must_use]
pub fn row(lane: BuiltinLane) -> Option<&'static LaneCapabilities> {
    LANE_CAPABILITIES.iter().find(|block| block.lane == lane)
}

/// How many bound seams name no test.
///
/// A ratchet that only goes down, checked for exact equality like
/// [`crate::evolution::UNWITNESSED_EVOLUTION_BASELINE`]. Writing a missing
/// test lowers this number in the same change. Raising it to turn a red gate
/// green is the expedient CLAUDE.md forbids.
///
/// All five are seams the forked child takes from its parent.
/// Refs #6163
pub const UNWITNESSED_SEAMS: usize = 5;

/// The test that pins what each of the CLI's four lanes binds.
const CLI_SEAMS: Witness = Witness::Test("each_lane_binds_what_its_call_site_bound_before");
/// The test that pins the lane name each of the CLI's four lanes writes.
const CLI_LANE: Witness = Witness::Test("every_lane_this_crate_assembles_declares_itself");
/// The test that pins what a served turn binds.
const SERVE_SEAMS: Witness = Witness::Test("a_served_turn_binds_what_its_call_site_bound_before");

/// Why none of the CLI's lanes routes an approval to the broker.
const DECK_APPROVALS: &str = "the deck parks a pre-tool approval on its own responder. The broker \
                              route is the one-shot door's answer";
/// Why none of the CLI's lanes takes the bus.
const DECK_BUS: &str = "the deck's observers ride the event stream, so nothing here reads a bus";
/// Why none of the CLI's lanes feeds a breaker.
const DECK_OUTCOMES: &str = "the deck picks its provider once per session, so there is no router \
                             breaker to feed";
/// Why none of the CLI's lanes re-routes mid-turn.
const DECK_FALLBACK: &str = "the deck picks its provider once per session, so there is nothing to \
                             re-resolve mid-turn";
/// Why a replayed turn binds no live seam.
const REPLAY_IS_DEAF: &str = "a replayed turn has no live operator channel. It replays a \
                              checkpoint to its end and returns";
/// Why the pipeline-stage lane binds nothing.
const NO_PIPELINE_PRODUCER: &str = "nothing assembles this lane and nothing will. A verification \
                                    plugin's stage turn is a plugin lane, so there is no literal \
                                    here to bind a seam in. Refs #3881";
/// Why neither of the two non-deck doors takes the bus.
const DOOR_BUS: &str = "this door's observers ride the event stream, so nothing here reads a bus";
/// Why a goal arc owns no router seam.
const ARC_ONE_PROVIDER: &str = "an arc resolves its provider once and drives every round through \
                                it, so there is no breaker to feed and nothing to re-resolve";
/// Why a served turn runs no hooks.
const SERVE_HOOKS: &str = "the host runs its own hooks on its own side of the wire. This engine \
                           holds no authority to run a command with";
/// Why a served turn owns no model-call seam.
const SERVE_HOST_OWNS_CALLS: &str = "the host owns the model calls, so this is the host's choice \
                                     rather than this engine's";

lane_capabilities! {
    // The deck's lead turn. The one lane that binds the re-query plane.
    Lead => LaneOrigin::Builtin,
        Some(LaneSite {
            file: "crates/stella-cli/src/lane_capabilities.rs",
            anchor: "pub(crate) fn lead<'a>(",
        }),
    {
        hooks: SeamClaim::bound("hooks: hooks.map(", CLI_SEAMS),
        hook_approvals: SeamClaim::declined(DECK_APPROVALS),
        calibration: SeamClaim::bound("calibration: Some(calibration)", CLI_SEAMS),
        gate: SeamClaim::bound("gate: Some(gate)", CLI_SEAMS),
        steering: SeamClaim::bound("steering: Some(steering)", CLI_SEAMS),
        requery: SeamClaim::bound("requery,", CLI_SEAMS),
        bus: SeamClaim::declined(DECK_BUS),
        outcomes: SeamClaim::declined(DECK_OUTCOMES),
        fallback: SeamClaim::declined(DECK_FALLBACK),
        call_role: SeamClaim::bound("call_role: ModelCallRole::Worker", CLI_SEAMS),
        lane: SeamClaim::bound("lane: Some(TurnLane::Builtin(BuiltinLane::Lead))", CLI_LANE),
    }

    // A turn replayed from a checkpoint. Nobody is watching it.
    Resume => LaneOrigin::Builtin,
        Some(LaneSite {
            file: "crates/stella-cli/src/lane_capabilities.rs",
            anchor: "pub(crate) fn resume<'a>(",
        }),
    {
        hooks: SeamClaim::bound("hooks: hooks.map(", CLI_SEAMS),
        hook_approvals: SeamClaim::declined(DECK_APPROVALS),
        calibration: SeamClaim::bound("calibration: Some(calibration)", CLI_SEAMS),
        gate: SeamClaim::declined(REPLAY_IS_DEAF),
        steering: SeamClaim::declined(REPLAY_IS_DEAF),
        requery: SeamClaim::declined(REPLAY_IS_DEAF),
        bus: SeamClaim::declined(DECK_BUS),
        outcomes: SeamClaim::declined(DECK_OUTCOMES),
        fallback: SeamClaim::declined(DECK_FALLBACK),
        call_role: SeamClaim::bound("call_role: ModelCallRole::Worker", CLI_SEAMS),
        lane: SeamClaim::bound("lane: Some(TurnLane::Builtin(BuiltinLane::Resume))", CLI_LANE),
    }

    // A deck worker lane. Two of its seams are open questions.
    SubSession => LaneOrigin::Builtin,
        Some(LaneSite {
            file: "crates/stella-cli/src/lane_capabilities.rs",
            anchor: "pub(crate) fn sub_session<'a>(",
        }),
    {
        hooks: SeamClaim::deferred(
            "Refs #6157",
            "a decision on whether a worker lane runs the session's hooks. The lead turn that \
             spawned it runs them today",
        ),
        hook_approvals: SeamClaim::declined(DECK_APPROVALS),
        calibration: SeamClaim::bound("calibration: Some(calibration)", CLI_SEAMS),
        gate: SeamClaim::bound("gate: Some(gate)", CLI_SEAMS),
        steering: SeamClaim::bound("steering: Some(steering)", CLI_SEAMS),
        requery: SeamClaim::deferred(
            "Refs #6158",
            "a decision on whether a worker lane may ask the session's context plane again",
        ),
        bus: SeamClaim::declined(DECK_BUS),
        outcomes: SeamClaim::declined(DECK_OUTCOMES),
        fallback: SeamClaim::declined(DECK_FALLBACK),
        call_role: SeamClaim::bound("call_role: ModelCallRole::Worker", CLI_SEAMS),
        lane: SeamClaim::bound(
            "lane: Some(TurnLane::Builtin(BuiltinLane::SubSession))",
            CLI_LANE,
        ),
    }

    // The child the delegation tool forks. It takes almost everything the
    // parent holds, which is why so many rows here read from the parent.
    SubagentFork => LaneOrigin::Builtin,
        Some(LaneSite {
            file: "crates/stella-core/src/subagent.rs",
            anchor: "pub async fn run_sub_agent_with_sender(",
        }),
    {
        hooks: SeamClaim::bound(
            "hooks: self.hooks.map(HooksHandle::parts)",
            Witness::Test("subagent_start_and_stop_hooks_fire_around_a_child_turn"),
        ),
        hook_approvals: SeamClaim::bound("hook_approvals: self.hook_approvals", Witness::Literal),
        calibration: SeamClaim::bound("calibration: self.calibration", Witness::Literal),
        gate: SeamClaim::bound(
            "gate: self.gate",
            Witness::Test("a_child_polls_the_parents_pause_gate"),
        ),
        steering: SeamClaim::bound(
            "steering,",
            Witness::Test("a_child_honors_the_soft_stop_but_never_eats_the_parents_steering"),
        ),
        requery: SeamClaim::declined(
            "the re-query plane belongs to the session's own turn. A parent-scoped plane would \
             inject the parent's context into a child's transcript",
        ),
        bus: SeamClaim::bound("bus: self.bus", Witness::Literal),
        outcomes: SeamClaim::bound("outcomes: self.outcomes", Witness::Literal),
        fallback: SeamClaim::declined(
            "the child's provider is the spec's own choice, so a mid-turn re-route is not this \
             fork's call to make",
        ),
        call_role: SeamClaim::bound("call_role: spec.role", Witness::Literal),
        lane: SeamClaim::bound(
            "lane: Some(TurnLane::Builtin(BuiltinLane::SubagentFork))",
            Witness::Test("a_forked_child_stamps_the_subagent_fork_lane"),
        ),
    }

    // One fleet attempt. Nobody is at a keyboard.
    FleetWorker => LaneOrigin::Builtin,
        Some(LaneSite {
            file: "crates/stella-cli/src/lane_capabilities.rs",
            anchor: "pub(crate) fn fleet_attempt<'a>(",
        }),
    {
        hooks: SeamClaim::bound("hooks: hooks.map(", CLI_SEAMS),
        hook_approvals: SeamClaim::declined(DECK_APPROVALS),
        calibration: SeamClaim::bound("calibration: Some(calibration)", CLI_SEAMS),
        gate: SeamClaim::bound("gate: Some(gate)", CLI_SEAMS),
        steering: SeamClaim::declined(
            "a fleet worker has no input channel. Nobody is at a keyboard to steer it",
        ),
        requery: SeamClaim::deferred(
            "Refs #6158",
            "a decision on whether a fleet attempt may ask the workspace context plane again",
        ),
        bus: SeamClaim::declined(DECK_BUS),
        outcomes: SeamClaim::declined(DECK_OUTCOMES),
        fallback: SeamClaim::declined(DECK_FALLBACK),
        call_role: SeamClaim::bound("call_role: ModelCallRole::Worker", CLI_SEAMS),
        lane: SeamClaim::bound(
            "lane: Some(TurnLane::Builtin(BuiltinLane::FleetWorker))",
            CLI_LANE,
        ),
    }

    // The staged pipeline's lane. It has no site, so every seam is refused
    // for one reason: there is no literal here to bind one in.
    PipelineStage => LaneOrigin::Builtin, None,
    {
        hooks: SeamClaim::declined(NO_PIPELINE_PRODUCER),
        hook_approvals: SeamClaim::declined(NO_PIPELINE_PRODUCER),
        calibration: SeamClaim::declined(NO_PIPELINE_PRODUCER),
        gate: SeamClaim::declined(NO_PIPELINE_PRODUCER),
        steering: SeamClaim::declined(NO_PIPELINE_PRODUCER),
        requery: SeamClaim::declined(NO_PIPELINE_PRODUCER),
        bus: SeamClaim::declined(NO_PIPELINE_PRODUCER),
        outcomes: SeamClaim::declined(NO_PIPELINE_PRODUCER),
        fallback: SeamClaim::declined(NO_PIPELINE_PRODUCER),
        call_role: SeamClaim::declined(NO_PIPELINE_PRODUCER),
        lane: SeamClaim::declined(NO_PIPELINE_PRODUCER),
    }

    // A turn a remote host drives over the wire. The host keeps the seams
    // that need authority this engine does not hold.
    ServeSession => LaneOrigin::Builtin,
        Some(LaneSite {
            file: "crates/stella-serve/src/session.rs",
            anchor: "fn served_capabilities<'a>(",
        }),
    {
        hooks: SeamClaim::declined(SERVE_HOOKS),
        hook_approvals: SeamClaim::declined(
            "this engine runs no hooks, so no approval can be parked",
        ),
        calibration: SeamClaim::bound("calibration,", SERVE_SEAMS),
        gate: SeamClaim::bound("gate: Some(gate)", SERVE_SEAMS),
        steering: SeamClaim::bound("steering: Some(steering)", SERVE_SEAMS),
        requery: SeamClaim::bound("requery,", SERVE_SEAMS),
        bus: SeamClaim::bound("bus,", SERVE_SEAMS),
        outcomes: SeamClaim::declined(SERVE_HOST_OWNS_CALLS),
        fallback: SeamClaim::declined(SERVE_HOST_OWNS_CALLS),
        call_role: SeamClaim::bound(
            "call_role: stella_protocol::ModelCallRole::Worker",
            SERVE_SEAMS,
        ),
        lane: SeamClaim::bound(
            "lane: Some(stella_protocol::TurnLane::Builtin(",
            Witness::Test("a_served_turn_declares_the_serve_session_lane"),
        ),
    }

    // The shared raw turn. The one lane that binds the session router's
    // breaker and its mid-turn fallback.
    RawTurn => LaneOrigin::Builtin,
        Some(LaneSite {
            file: "crates/stella-cli/src/lane_capabilities.rs",
            anchor: "pub(crate) fn raw_turn<'a>(",
        }),
    {
        hooks: SeamClaim::bound("hooks: hooks.map(", CLI_SEAMS),
        hook_approvals: SeamClaim::bound("hook_approvals: hooks.map(", CLI_SEAMS),
        calibration: SeamClaim::bound("calibration: Some(calibration)", CLI_SEAMS),
        gate: SeamClaim::bound("gate: controls.gate.as_deref()", CLI_SEAMS),
        steering: SeamClaim::bound("steering: controls.steering.as_deref()", CLI_SEAMS),
        requery: SeamClaim::bound("requery,", CLI_SEAMS),
        bus: SeamClaim::declined(DOOR_BUS),
        outcomes: SeamClaim::bound("outcomes: Some(outcomes)", CLI_SEAMS),
        fallback: SeamClaim::bound("fallback: Some(fallback)", CLI_SEAMS),
        call_role: SeamClaim::bound("call_role: ModelCallRole::Worker", CLI_SEAMS),
        lane: SeamClaim::bound(
            "lane: Some(TurnLane::Builtin(BuiltinLane::RawTurn))",
            CLI_LANE,
        ),
    }

    // A judged goal arc. Steered, and never paused.
    GoalArc => LaneOrigin::Builtin,
        Some(LaneSite {
            file: "crates/stella-cli/src/lane_capabilities.rs",
            anchor: "pub(crate) fn goal_arc<'a>(",
        }),
    {
        hooks: SeamClaim::bound("hooks: hooks.map(", CLI_SEAMS),
        hook_approvals: SeamClaim::declined(
            "a pre-tool hook asking for approval here gets the grant-path refusal rather than a \
             prompt, whether or not a route is named",
        ),
        calibration: SeamClaim::bound("calibration: Some(calibration)", CLI_SEAMS),
        gate: SeamClaim::declined(
            "nobody can pause an arc. The whistle steers it and never stops it at a step \
             boundary",
        ),
        steering: SeamClaim::bound("steering: Some(steering)", CLI_SEAMS),
        requery: SeamClaim::deferred(
            "Refs #6158",
            "a decision on whether a goal arc may ask the workspace context plane again",
        ),
        bus: SeamClaim::declined(DOOR_BUS),
        outcomes: SeamClaim::declined(ARC_ONE_PROVIDER),
        fallback: SeamClaim::declined(ARC_ONE_PROVIDER),
        call_role: SeamClaim::bound("call_role: ModelCallRole::Worker", CLI_SEAMS),
        lane: SeamClaim::bound(
            "lane: Some(TurnLane::Builtin(BuiltinLane::GoalArc))",
            CLI_LANE,
        ),
    }
}

#[cfg(test)]
mod tests;
