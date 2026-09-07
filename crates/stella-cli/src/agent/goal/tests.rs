// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The goal door's own wiring, read back from its source.
//!
//! The door binds a wrapper plugin and drives
//! [`crate::wrapper_plugin::run_wrapped`] (`#3911`), which builds a provider
//! from the resolved config, connects MCP and opens a store — nothing a unit
//! test can stand in for. Its predecessor built its own engine, so a
//! socket-level witness could hand it a scripted provider and watch a whistled
//! message reach round 2. The steering witness lives where the wiring does
//! now: [`crate::agent::turn::run_turn`] honours the
//! [`stella_core::ports::TurnControls`] its caller published, and this file
//! holds the door to publishing them.

use super::*;

/// Every door's own source, as `(crate-relative path, source)`.
///
/// Read back rather than driven, for the reason above. `runtime.rs` and
/// `memory/reflection/tests.rs` already hold `run_raw_one_shot` this way, and
/// the two doors are one shape now.
const GOAL: &str = include_str!("../goal.rs");

/// **The steering witness (`#4769`, re-homed by `#3911`).** `stella whistle`
/// reaches a `stella goal` arc because the door opens this session's whistle
/// and hands its controls to the driver that runs every round.
///
/// Neither assertion implies the other: a door that opens no whistle binds no
/// socket for `stella whistle` to connect to, and a door that opens one and
/// passes [`stella_core::ports::TurnControls::none`] leaves the socket bound
/// and the tap undrained.
#[test]
fn the_goal_door_publishes_this_sessions_steering_to_every_round() {
    let door = GOAL
        .find("pub async fn run_goal_cmd")
        .expect("the goal door must still be named this");
    let body = &GOAL[door..];

    assert!(
        body.contains("crate::whistle::SessionWhistle::open(Some(presence.id()))"),
        "the goal door must open this session's whistle, or `stella whistle` has no socket \
         to reach a goal arc through"
    );
    assert!(
        body.contains("controls: controls.clone()"),
        "the goal door must hand the whistle's controls to the driver, or the tap is bound \
         and never drained"
    );
}

/// The refusal a workspace with no goal plugin installed reads.
///
/// It is the whole user-facing half of the extraction. The verb needed no
/// install before it, so an error that reads as a typo would leave a user with
/// no way to get the verb back.
#[test]
fn the_missing_plugin_refusal_names_the_install_and_the_wrapper() {
    let message = goal_plugin_missing_message(
        DEFAULT_GOAL_WRAPPER,
        "no installed wrapper plugin declares `goal-v1`",
    );

    assert!(
        message.contains("stella plugin install ./plugins/stella-goal"),
        "the refusal must name the install that restores the verb: {message}"
    );
    assert!(
        message.contains("goal-v1"),
        "the refusal must name the wrapper this run asked for: {message}"
    );
    assert!(
        message.contains("no installed wrapper plugin declares `goal-v1`"),
        "the resolver's own account must survive, or a typo and an empty roster read alike: \
         {message}"
    );
}

/// The default is the reference plugin's declared id, read off the shipped
/// manifest rather than trusted to two hand-written copies.
#[test]
fn the_default_wrapper_is_the_shipped_goal_plugins_own_id() {
    const MANIFEST: &str = include_str!("../../../../../plugins/stella-goal/plugin.toml");

    assert!(
        MANIFEST.contains(&format!("id = \"{DEFAULT_GOAL_WRAPPER}\"")),
        "`stella goal` with no `--pipeline` must resolve the id `plugins/stella-goal` declares"
    );
}
