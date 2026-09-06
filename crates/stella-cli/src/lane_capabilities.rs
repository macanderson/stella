// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What each of this crate's turn lanes binds, and which lane it says it is.
//!
//! A lane is one place a turn runs. `stella_protocol::BuiltinLane` names
//! seven. Four of them are built here: the deck's lead turn, a resumed turn,
//! a deck worker lane, and a fleet attempt.
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
//! **One file, not four call sites.** Side by side, the four are easy to
//! compare. And `subsession.rs` and `fleet_cmd.rs` both sit near the
//! file-size ceiling, with no room for a literal of their own.
//!
//! `stella_core`'s `driver::drive` puts the lane on `agent.turn.started`.
//! `crates/stella-parity/src/lane.rs` holds every lane to having a producer
//! or a written reason for having none.

use stella_core::ports::{SteeringRequery, TurnGate, TurnSteering};
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

#[cfg(test)]
mod tests {
    use super::*;
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

    /// **The lane witnesses.** Each of this crate's four lanes says which
    /// lane it is.
    ///
    /// This fails on a tree where the four sites use `Engine::with_sleeper`.
    /// The reason is structural, not behavioural. That call takes no lane and
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
    }
}
