// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The host a **round-driving** door assembles: `stella goal` per judged
//! round, `stella fleet` per worker attempt (#3833, #3882).
//!
//! A sibling file rather than more lines in the parent suite, for the reason
//! AGENTS.md § "God files" gives: that file is within fifteen lines of the
//! 1500-line ratchet, and this is a new property rather than a fix to one
//! already covered there. It reads the parent's fixtures through `use
//! super::*` — the grading manifest, the recording dispatcher and the roster
//! helpers are the same ones the `stella run` host is tested against, which is
//! the point: the doors differ in what they assemble, not in what a plugin
//! asks for.

use std::sync::Arc;

use stella_core::subagent::SubAgentDispatcher;
use stella_plugin::PluginManifest;

use super::*;

/// **Witness (#3833, #3882).** A door that drives several rounds under one
/// execution row serves `child_turn`, and every child turn it runs lands on a
/// slot no round of that door will ever claim.
///
/// Both halves fail before this change, for the two reasons the issues name.
/// `stella goal` and `stella fleet` each built `WrapperHost::recalling(..)`
/// alone, so this call could only be answered `Unavailable` — there was no
/// assembly to reach a plane through. And the plane pinned every child turn to
/// one fixed slot, so a second child turn reused the first's receipt key and
/// the first round of a goal loop already owned the slot it sat on. The lane
/// rule ([`stella_core::turn_slots`]) is what makes both statable at once:
/// the plane counts only its own calls, and residue keeps it clear of a
/// counter it cannot see.
#[tokio::test]
async fn a_round_driving_door_serves_child_turns_clear_of_its_own_rounds() {
    use stella_plugin::{HostCallOk, HostCallOutcome};
    use stella_runtime::wrapper::HostCallChannel;

    let workspace = tempfile::tempdir().expect("workspace");
    let roster = roster(vec![installed(
        GRADING_MANIFEST,
        "/plugins/grading-wrapper",
    )]);
    let manifest = PluginManifest::from_toml_str(GRADING_MANIFEST).expect("fixture must load");
    let dispatcher = RecordingDispatcher::default();
    let wrapper = bind_installed(&roster, "grading-v1", &mut |_| {})
        .expect("the installed plugin declares this variant")
        .serving(round_driver_host(
            workspace.path(),
            &manifest,
            Arc::new(dispatcher.clone()) as Arc<dyn SubAgentDispatcher>,
        ))
        .expect("a found variant binds");

    let channel = wrapper.gate.open();
    for _ in 0..2 {
        match channel.call(child_turn("reviewer")).await {
            HostCallOutcome::Ok(HostCallOk::ChildTurn(result)) => assert!(result.completed),
            other => panic!("this door answered a declared child turn with {other:?}"),
        }
    }

    let slots: Vec<u32> = dispatcher
        .specs()
        .iter()
        .map(|spec| spec.turn_instance)
        .collect();
    assert_eq!(slots.len(), 2, "two child turns ran");
    assert_ne!(
        slots[0], slots[1],
        "two child turns sharing a slot overwrite each other's step manifests"
    );
    for slot in &slots {
        assert_eq!(
            stella_core::turn_slots::lane_of(*slot),
            stella_core::turn_slots::CHILD_TURN_LANE
        );
    }
    // Against the door's own rounds, at the arithmetic `Engine::run_goal` and
    // `AttemptDriver::run_turn` actually use — the collision that kept this
    // capability off both doors.
    for round in 1..=8usize {
        let worker = stella_core::goal::goal_round_turn_offset(round);
        for slot in &slots {
            assert_ne!(*slot, worker, "child turn landed on round {round}'s worker");
            assert_ne!(
                *slot,
                worker + 1,
                "child turn landed on round {round}'s verifier"
            );
        }
    }
}
