// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What each of this crate's turn lanes binds, and which lane it says it is.
//!
//! A lane is one place a turn runs. `stella_protocol::BuiltinLane` names them
//! all. Built here: the deck's lead turn, a resumed turn, a deck worker lane,
//! a fleet attempt, the shared raw turn, and a judged goal arc.
//!
//! Each one goes to the engine through `Engine::assemble`. Its
//! `TurnCapabilities` carries the lane name. The other way in,
//! `Engine::with_sleeper` and a chain of `with_*` calls, cannot: it always
//! writes `lane: None`. A turn with no lane lands in the `null` group with
//! every other one, and a report keyed by lane then has nothing to show.
//!
//! Each function below answers every seam. Every literal is written out in
//! full. So a new slot on [`TurnCapabilities`] breaks this file until someone
//! picks an answer for each lane. That is why the type has no `Default`.
//!
//! **One file, not a literal at each call site.** Side by side, the lanes are
//! easy to compare. And `subsession.rs` and `fleet_cmd.rs` both sit near the
//! file-size ceiling, with no room for a literal of their own.
//!
//! `stella_core`'s `driver::drive` puts the lane on `agent.turn.started`.
//! `crates/stella-parity/src/lane.rs` holds every lane to having a producer
//! or a written reason for having none.

use stella_core::hooks::decision::ApprovalRoute;
use stella_core::ports::{
    FallbackResolver, ProviderOutcomes, SteeringRequery, TurnControls, TurnGate, TurnSteering,
};
use stella_core::{CalibrationMap, HookRunner, Hooks, TurnCapabilities};
use stella_protocol::{BuiltinLane, ModelCallRole, TurnLane};

/// The deck's lead turn — `BuiltinLane::Lead`.
///
/// `hooks` and `requery` are `Option`s because the deck binds them only
/// sometimes. Hooks exist when the config declares them. The re-query plane
/// exists when the session has a memory to query.
pub(crate) fn lead<'a>(
    hooks: Option<&'a Hooks>,
    runner: &'a dyn HookRunner,
    calibration: &'a CalibrationMap,
    gate: &'a dyn TurnGate,
    steering: &'a dyn TurnSteering,
    requery: Option<&'a dyn SteeringRequery>,
) -> TurnCapabilities<'a> {
    TurnCapabilities {
        hooks: hooks.map(|hooks| (hooks, runner)),
        // The deck parks a `PreToolUse` approval on its own responder. The
        // broker route is the one-shot door's answer, not the deck's.
        hook_approvals: None,
        calibration: Some(calibration),
        gate: Some(gate),
        steering: Some(steering),
        requery,
        // The deck's observers ride the event stream, not the bus.
        bus: None,
        // The deck picks its provider once per session. There is no router
        // breaker to feed, and nothing to re-resolve mid-turn.
        outcomes: None,
        fallback: None,
        call_role: ModelCallRole::Worker,
        lane: Some(TurnLane::Builtin(BuiltinLane::Lead)),
    }
}

/// A turn replayed from a checkpoint — `BuiltinLane::Resume`.
pub(crate) fn resume<'a>(
    hooks: Option<&'a Hooks>,
    runner: &'a dyn HookRunner,
    calibration: &'a CalibrationMap,
) -> TurnCapabilities<'a> {
    TurnCapabilities {
        hooks: hooks.map(|hooks| (hooks, runner)),
        hook_approvals: None,
        calibration: Some(calibration),
        // A resumed turn has no live operator channel. It replays a
        // checkpoint to its end and returns. Nothing to park on, and nothing
        // to steer with.
        gate: None,
        steering: None,
        requery: None,
        bus: None,
        outcomes: None,
        fallback: None,
        call_role: ModelCallRole::Worker,
        lane: Some(TurnLane::Builtin(BuiltinLane::Resume)),
    }
}

/// A deck worker lane — `BuiltinLane::SubSession`.
pub(crate) fn sub_session<'a>(
    calibration: &'a CalibrationMap,
    gate: &'a dyn TurnGate,
    steering: &'a dyn TurnSteering,
) -> TurnCapabilities<'a> {
    TurnCapabilities {
        // Not bound, and not a new gap. A lane's hooks are the session's,
        // and the lead turn that spawned it runs them. Wiring them onto a
        // worker lane is its own decision.
        hooks: None,
        hook_approvals: None,
        calibration: Some(calibration),
        gate: Some(gate),
        steering: Some(steering),
        requery: None,
        bus: None,
        outcomes: None,
        fallback: None,
        call_role: ModelCallRole::Worker,
        lane: Some(TurnLane::Builtin(BuiltinLane::SubSession)),
    }
}

/// One fleet attempt — `BuiltinLane::FleetWorker`.
pub(crate) fn fleet_attempt<'a>(
    hooks: Option<&'a Hooks>,
    runner: &'a dyn HookRunner,
    calibration: &'a CalibrationMap,
    gate: &'a dyn TurnGate,
) -> TurnCapabilities<'a> {
    TurnCapabilities {
        hooks: hooks.map(|hooks| (hooks, runner)),
        hook_approvals: None,
        calibration: Some(calibration),
        gate: Some(gate),
        // A fleet worker has no input channel. Nobody is at a keyboard to
        // steer it, so this stays unbound.
        steering: None,
        requery: None,
        bus: None,
        outcomes: None,
        fallback: None,
        call_role: ModelCallRole::Worker,
        lane: Some(TurnLane::Builtin(BuiltinLane::FleetWorker)),
    }
}

/// The shared raw turn — `BuiltinLane::RawTurn`.
///
/// The hooks arrive as one triple because this door binds all three together
/// or none of them: the runner exists only where the config declares hooks,
/// and the approval route is the broker surface those hooks park a
/// `PreToolUse` decision on. Under process-free authority the door strips the
/// hook layer outright and hands `None`.
///
/// `controls` is the pause gate and steering tap this turn's caller
/// published, read here so the lane binds what the caller has rather than
/// what a chain of optional calls happened to attach.
pub(crate) fn raw_turn<'a>(
    hooks: Option<(&'a Hooks, &'a dyn HookRunner, &'a dyn ApprovalRoute)>,
    calibration: &'a CalibrationMap,
    outcomes: &'a dyn ProviderOutcomes,
    fallback: &'a dyn FallbackResolver,
    requery: Option<&'a dyn SteeringRequery>,
    controls: &'a TurnControls,
) -> TurnCapabilities<'a> {
    TurnCapabilities {
        hooks: hooks.map(|(hooks, runner, _)| (hooks, runner)),
        hook_approvals: hooks.map(|(_, _, route)| route),
        calibration: Some(calibration),
        gate: controls.gate.as_deref(),
        steering: controls.steering.as_deref(),
        requery,
        // This door's observers ride the event stream, not the bus.
        bus: None,
        // The session router picks the worker model and keeps a breaker over
        // it, so this door feeds outcomes back and re-resolves through the
        // same router when a retry ladder runs out.
        outcomes: Some(outcomes),
        fallback: Some(fallback),
        call_role: ModelCallRole::Worker,
        lane: Some(TurnLane::Builtin(BuiltinLane::RawTurn)),
    }
}

/// A judged multi-round goal arc — `BuiltinLane::GoalArc`.
///
/// Both arms of `stella goal` bind the same set, so both take this one
/// literal: the raw arm, which drives its rounds inside `Engine::run_goal`,
/// and the wrapped arm, which drives them itself under a plugin.
pub(crate) fn goal_arc<'a>(
    hooks: Option<&'a Hooks>,
    runner: &'a dyn HookRunner,
    calibration: &'a CalibrationMap,
    steering: &'a dyn TurnSteering,
) -> TurnCapabilities<'a> {
    TurnCapabilities {
        hooks: hooks.map(|hooks| (hooks, runner)),
        // No approval route. A `PreToolUse` hook asking for approval gets the
        // grant-path refusal instead of a prompt, which is what this door
        // answers with or without the seam named here.
        hook_approvals: None,
        calibration: Some(calibration),
        // Nobody can pause an arc. The whistle steers it and never stops it
        // at a step boundary.
        gate: None,
        steering: Some(steering),
        requery: None,
        bus: None,
        // An arc resolves its provider once and drives every round through
        // it. There is no breaker to feed and nothing to re-resolve mid-turn.
        outcomes: None,
        fallback: None,
        call_role: ModelCallRole::Worker,
        lane: Some(TurnLane::Builtin(BuiltinLane::GoalArc)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use stella_core::ports::ResolvedFallback;
    use stella_tools::hook_runner::HostHookRunner;

    struct OpenGate;

    #[async_trait::async_trait]
    impl TurnGate for OpenGate {
        async fn wait_if_paused(&self) {}
    }

    struct QuietSteering;

    impl TurnSteering for QuietSteering {
        fn drain_steering(&self) -> Vec<String> {
            Vec::new()
        }

        fn soft_stop_requested(&self) -> bool {
            false
        }
    }

    struct NoRequery;

    #[async_trait::async_trait]
    impl SteeringRequery for NoRequery {
        async fn requery(&self, _signal: &stella_core::steering::TurnSignal<'_>) -> Option<String> {
            None
        }
    }

    struct QuietOutcomes;

    impl ProviderOutcomes for QuietOutcomes {
        fn record_success(&self, _provider_id: &str) {}

        fn record_failure(&self, _provider_id: &str) {}
    }

    struct RefusingApprovals;

    #[async_trait::async_trait]
    impl ApprovalRoute for RefusingApprovals {
        async fn resolve(
            &self,
            _request: &stella_core::hooks::decision::ApprovalRouteRequest,
        ) -> stella_core::hooks::decision::ApprovalRouteResolution {
            stella_core::hooks::decision::ApprovalRouteResolution::Denied {
                reason: "no human is at this test".to_string(),
            }
        }
    }

    struct NoFallback;

    impl FallbackResolver for NoFallback {
        fn resolve_fallback(&self, _failed_provider_id: &str) -> Option<ResolvedFallback<'_>> {
            None
        }
    }

    /// **The lane witnesses.** Every lane this crate assembles says which
    /// lane it is.
    ///
    /// This fails on a tree whose sites use `Engine::with_sleeper`. The
    /// reason is structural, not behavioural. That call takes no lane and
    /// always writes `lane: None`. There is no function to call, and nothing
    /// that could answer.
    ///
    /// `stella-core`'s `a_turn_stamps_the_lane_that_assembled_it` covers what
    /// a named lane then does: it reaches `agent.turn.started` on the wire.
    #[test]
    fn every_lane_this_crate_assembles_declares_itself() {
        let runner = HostHookRunner;
        let calibration = CalibrationMap::default();
        let gate = OpenGate;
        let steering = QuietSteering;
        let unbound = TurnControls::none();

        let rows = [
            (
                "lead",
                lead(None, &runner, &calibration, &gate, &steering, None).lane,
                BuiltinLane::Lead,
            ),
            (
                "resume",
                resume(None, &runner, &calibration).lane,
                BuiltinLane::Resume,
            ),
            (
                "sub_session",
                sub_session(&calibration, &gate, &steering).lane,
                BuiltinLane::SubSession,
            ),
            (
                "fleet_attempt",
                fleet_attempt(None, &runner, &calibration, &gate).lane,
                BuiltinLane::FleetWorker,
            ),
            (
                "raw_turn",
                raw_turn(
                    None,
                    &calibration,
                    &QuietOutcomes,
                    &NoFallback,
                    None,
                    &unbound,
                )
                .lane,
                BuiltinLane::RawTurn,
            ),
            (
                "goal_arc",
                goal_arc(None, &runner, &calibration, &steering).lane,
                BuiltinLane::GoalArc,
            ),
        ];

        for (name, declared, expected) in rows {
            assert_eq!(
                declared,
                Some(TurnLane::Builtin(expected)),
                "the `{name}` lane must name itself, or its turns land in \
                 the `null` group with every door that names none",
            );
        }
    }

    /// A lane must not quietly gain or lose a seam. Moving a site onto
    /// `Engine::assemble` may not change one of these. An engine that gained
    /// a gate would park where nothing parked before. One that lost its
    /// calibration map would start each turn from a raw estimate.
    ///
    /// Both directions, for every seam a caller may or may not hand in: a
    /// lane must pass on what it is given and must not invent what it is
    /// not. `stella-parity`'s lane matrix names this test as the witness for
    /// each of these rows, and a row is only worth its name if the test
    /// exercises the seam it claims.
    #[test]
    fn each_lane_binds_what_its_call_site_bound_before() {
        let runner = HostHookRunner;
        let calibration = CalibrationMap::default();
        let gate = OpenGate;
        let steering = QuietSteering;
        let hooks = Hooks::default();
        let plane = NoRequery;

        let deck = lead(None, &runner, &calibration, &gate, &steering, None);
        assert!(deck.calibration.is_some() && deck.gate.is_some() && deck.steering.is_some());
        assert!(deck.requery.is_none(), "no plane was handed in");
        assert!(deck.hooks.is_none(), "no hooks were given");
        assert_eq!(deck.call_role, ModelCallRole::Worker);

        let deck_with_both = lead(
            Some(&hooks),
            &runner,
            &calibration,
            &gate,
            &steering,
            Some(&plane),
        );
        assert!(
            deck_with_both.hooks.is_some() && deck_with_both.requery.is_some(),
            "the lead turn must pass on the seams its caller handed it",
        );

        let replayed = resume(None, &runner, &calibration);
        assert!(replayed.calibration.is_some());
        assert!(
            replayed.gate.is_none() && replayed.steering.is_none(),
            "a replayed turn binds neither, and must not grow one here",
        );
        assert_eq!(replayed.call_role, ModelCallRole::Worker);
        assert!(
            resume(Some(&hooks), &runner, &calibration).hooks.is_some(),
            "a replayed turn must run the hooks its caller handed it",
        );

        let worker = sub_session(&calibration, &gate, &steering);
        assert!(worker.calibration.is_some() && worker.gate.is_some() && worker.steering.is_some(),);
        assert_eq!(worker.call_role, ModelCallRole::Worker);

        let fleet = fleet_attempt(None, &runner, &calibration, &gate);
        assert!(fleet.calibration.is_some() && fleet.gate.is_some());
        assert!(
            fleet.steering.is_none(),
            "a fleet attempt has no input channel to steer from",
        );
        assert_eq!(fleet.call_role, ModelCallRole::Worker);
        assert!(
            fleet_attempt(Some(&hooks), &runner, &calibration, &gate)
                .hooks
                .is_some(),
            "a fleet attempt must run the hooks its caller handed it",
        );

        let controls = TurnControls::none()
            .with_gate(Arc::new(OpenGate))
            .with_steering(Arc::new(QuietSteering));
        let raw = raw_turn(
            None,
            &calibration,
            &QuietOutcomes,
            &NoFallback,
            None,
            &controls,
        );
        assert!(raw.calibration.is_some());
        assert!(
            raw.outcomes.is_some() && raw.fallback.is_some(),
            "the raw turn reports call outcomes to its session router and \
             re-resolves through it when a retry ladder runs out",
        );
        assert!(
            raw.gate.is_some() && raw.steering.is_some(),
            "the gate and the tap are whatever the caller published",
        );
        assert!(raw.hooks.is_none() && raw.hook_approvals.is_none());
        assert_eq!(raw.call_role, ModelCallRole::Worker);

        let with_hooks = raw_turn(
            Some((&hooks, &runner, &RefusingApprovals)),
            &calibration,
            &QuietOutcomes,
            &NoFallback,
            Some(&plane),
            &controls,
        );
        assert!(
            with_hooks.hooks.is_some()
                && with_hooks.hook_approvals.is_some()
                && with_hooks.requery.is_some(),
            "the raw turn must pass on the seams its caller handed it",
        );

        let unbound = TurnControls::none();
        let bare = raw_turn(
            None,
            &calibration,
            &QuietOutcomes,
            &NoFallback,
            None,
            &unbound,
        );
        assert!(
            bare.gate.is_none() && bare.steering.is_none(),
            "a caller that published neither must not grow one here",
        );

        let arc = goal_arc(None, &runner, &calibration, &steering);
        assert!(arc.calibration.is_some() && arc.steering.is_some());
        assert!(
            arc.gate.is_none(),
            "an arc is steered and never paused, so a gate here would park \
             where nothing parked before",
        );
        assert!(
            arc.outcomes.is_none() && arc.fallback.is_none(),
            "an arc resolves its provider once and drives every round on it",
        );
        assert_eq!(arc.call_role, ModelCallRole::Worker);
        assert!(
            goal_arc(Some(&hooks), &runner, &calibration, &steering)
                .hooks
                .is_some(),
            "an arc must run the hooks its caller handed it",
        );
    }

    /// Every call site this crate reads back, as
    /// `(crate-relative path, source)`.
    fn door_sources() -> [(&'static str, &'static str); 3] {
        [
            ("agent/turn.rs", include_str!("agent/turn.rs")),
            ("agent/goal.rs", include_str!("agent/goal.rs")),
            (
                "agent/goal/goal_wrapped.rs",
                include_str!("agent/goal/goal_wrapped.rs"),
            ),
        ]
    }

    /// **The call-site witnesses.** Each door that is not the deck reaches
    /// the engine through `Engine::assemble` and its own seams.
    ///
    /// Fails on a tree where the three files build with the sleeper
    /// constructor: that call takes no lane, so it writes `lane: None` and no
    /// argument can say otherwise. The needles are built rather than written
    /// out, so `turn_files`' driver fence reads this file as prose about a
    /// constructor instead of a door that builds one.
    #[test]
    fn each_door_that_is_not_the_deck_assembles_through_its_lane() {
        let blessed = format!("Engine::{}(", "assemble");
        let builder = format!("Engine::with_{}(", "sleeper");
        let seams = [
            ("agent/turn.rs", "lane_capabilities::raw_turn("),
            ("agent/goal.rs", "lane_capabilities::goal_arc("),
            ("agent/goal/goal_wrapped.rs", "lane_capabilities::goal_arc("),
        ];

        for (path, source) in door_sources() {
            assert!(
                source.contains(&blessed),
                "{path} drives a turn and must assemble it, or its turns \
                 report no lane at all",
            );
            assert!(
                !source.contains(&builder),
                "{path} is back on the builder path, which takes no lane and \
                 lands every turn it drives in the `null` group",
            );
            let seam = seams
                .iter()
                .find(|(name, _)| *name == path)
                .map(|(_, seam)| *seam)
                .expect("every door source names its seams");
            assert!(
                source.contains(seam),
                "{path} must build its seams through `{seam}`, so what it \
                 binds is one written literal rather than a chain",
            );
        }
    }

    /// `agent/turn.rs` assembles twice — once with the hook layer stripped by
    /// process-free authority and once with it. Both arms are the same door
    /// and must name the same lane.
    #[test]
    fn both_arms_of_the_raw_door_assemble() {
        let source = door_sources()
            .into_iter()
            .find(|(path, _)| *path == "agent/turn.rs")
            .map(|(_, source)| source)
            .expect("the raw door is one of the sources");
        let arms = source.matches("lane_capabilities::raw_turn(").count();
        assert_eq!(
            arms, 2,
            "the raw door has a process-free arm and an ordinary one; both \
             assemble, and an arm that stopped would report no lane",
        );
    }
}
