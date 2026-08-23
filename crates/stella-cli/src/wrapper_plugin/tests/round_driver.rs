// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The host a **round-driving** door assembles: `stella goal` per judged
//! round, `stella fleet` per worker attempt (#3833, #3882).
//!
//! A sibling file rather than more lines in the parent suite, for the reason
//! AGENTS.md § "God files" gives: that file is the largest in this module and
//! this is a new property rather than a fix to one already covered there. It
//! reads the parent's fixtures through `use super::*` — the grading manifest,
//! the recording dispatcher and the roster helpers are the same ones the
//! `stella run` host is tested against, which is the point: the doors differ in
//! what they assemble, not in what a plugin asks for.

use std::sync::Arc;

use stella_core::subagent::SubAgentDispatcher;

use super::*;

/// A wrapper that asks the host to re-run the candidate's tests, which is what
/// `GRADING_MANIFEST` beside it does not declare.
///
/// Steering rather than arbiter, because what is under test is the plane a
/// round-driving door installs and not the hold loop above it — and `stella
/// goal` refuses an arbiter on this door anyway
/// (`reject_arbiter_wrapper_on_goal`).
const VERIFYING_MANIFEST: &str = r#"
name = "verifying-wrapper"
[loop]
participation = "steering"
points = ["before_turn"]
calls = ["run_test"]
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
[wrapper]
id = "verifying-v1"
[[wrapper.stages]]
name = "execute"
"#;

fn run_test(handle: &str) -> stella_plugin::HostCallArgs {
    stella_plugin::HostCallArgs::RunTest(stella_plugin::RunTestArgs {
        candidate: stella_protocol::candidate::CandidateHandle::new(handle),
    })
}

/// A workspace whose one test invocation reports `exit`.
fn tree_with_a_test(exit: u8) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(dir.path().join("tests")).expect("tests dir");
    std::fs::write(
        dir.path().join("tests/witness_flip.sh"),
        format!("#!/bin/sh\necho 'the suite ran'\nexit {exit}\n"),
    )
    .expect("the witness");
    dir
}

/// **Witness (#4536).** A round-driving door performs a declared `run_test`:
/// the invocation its own grant carries runs in the tree that grant names, and
/// the plugin is told what the assertions said.
///
/// Fails before this change for the reason #3580 left behind: the `TestRuns`
/// plane shipped with no [`stella_runtime::wrapper::TestRunHost`] behind it on
/// any real door, so this call could only ever be answered `Unavailable` —
/// true, and still a refusal, on every door a user has.
#[tokio::test]
async fn a_round_driving_door_runs_the_tests_its_own_grant_names() {
    use stella_plugin::{HostCallOk, HostCallOutcome, TestBaseline};
    use stella_runtime::wrapper::HostCallChannel;

    let tree = tree_with_a_test(0);
    let granted =
        crate::wrapper_candidate::grant_shared_tree(tree.path(), Some("sh tests/witness_flip.sh"))
            .expect("the root resolves and the command parses");
    let roster = roster(vec![installed(
        VERIFYING_MANIFEST,
        "/plugins/verifying-wrapper",
    )]);
    let dispatcher = RecordingDispatcher::default();
    let wrapper = bind_installed(&roster, "verifying-v1", &mut |_| {})
        .expect("the installed plugin declares this variant")
        .serving(|manifest| {
            round_driver_host(
                tree.path(),
                manifest,
                Arc::new(dispatcher.clone()) as Arc<dyn SubAgentDispatcher>,
                Some(&granted.grant),
            )
        })
        .expect("a found variant binds");

    let channel = wrapper.gate().open();
    match channel.call(run_test(granted.grant.handle.as_str())).await {
        HostCallOutcome::Ok(HostCallOk::RunTest(result)) => {
            assert_eq!(result.assertions, TestBaseline::Passed);
            assert!(result.output.contains("the suite ran"), "{result:?}");
            assert_eq!(result.candidate.as_str(), granted.grant.handle.as_str());
        }
        other => panic!("this door answered a declared run_test with {other:?}"),
    }
}

/// The grant is the fence. A handle this door did not mint reaches no
/// filesystem, and is `Unavailable` rather than `Unsupported`: the capability
/// is here, and what is missing is a workspace for that handle.
#[tokio::test]
async fn a_handle_this_door_never_granted_reaches_no_tree() {
    use stella_plugin::{HostCallOutcome, HostCallRefusal};
    use stella_runtime::wrapper::HostCallChannel;

    let tree = tree_with_a_test(0);
    let granted =
        crate::wrapper_candidate::grant_shared_tree(tree.path(), Some("sh tests/witness_flip.sh"))
            .expect("the grant mints");
    let roster = roster(vec![installed(
        VERIFYING_MANIFEST,
        "/plugins/verifying-wrapper",
    )]);
    let dispatcher = RecordingDispatcher::default();
    let wrapper = bind_installed(&roster, "verifying-v1", &mut |_| {})
        .expect("the variant resolves")
        .serving(|manifest| {
            round_driver_host(
                tree.path(),
                manifest,
                Arc::new(dispatcher.clone()) as Arc<dyn SubAgentDispatcher>,
                Some(&granted.grant),
            )
        })
        .expect("a found variant binds");

    match wrapper
        .gate()
        .open()
        .call(run_test("candidate-from-another-run"))
        .await
    {
        HostCallOutcome::Err(failure) => {
            assert_eq!(failure.refusal, HostCallRefusal::Unavailable);
            assert_ne!(failure.refusal, HostCallRefusal::Unsupported);
        }
        other => panic!("a handle this door never minted answered {other:?}"),
    }
}

/// A door that minted no grant installs **no plane**, which is a different
/// sentence to a plugin's author than "your handle is wrong": there is nothing
/// to re-run because the user named no `--test-command`.
#[tokio::test]
async fn a_door_with_no_grant_installs_no_plane() {
    use stella_plugin::HostCallOutcome;
    use stella_runtime::wrapper::HostCallChannel;

    let workspace = tempfile::tempdir().expect("workspace");
    let roster = roster(vec![installed(
        VERIFYING_MANIFEST,
        "/plugins/verifying-wrapper",
    )]);
    let dispatcher = RecordingDispatcher::default();
    let wrapper = bind_installed(&roster, "verifying-v1", &mut |_| {})
        .expect("the variant resolves")
        .serving(|manifest| {
            round_driver_host(
                workspace.path(),
                manifest,
                Arc::new(dispatcher.clone()) as Arc<dyn SubAgentDispatcher>,
                None,
            )
        })
        .expect("a found variant binds");

    match wrapper.gate().open().call(run_test("candidate-1")).await {
        HostCallOutcome::Err(failure) => {
            assert!(
                failure.detail.contains("test-run plane"),
                "a door with no grant says so: {failure}"
            );
        }
        other => panic!("a door with no plane answered {other:?}"),
    }
}

/// **Witness (#3833, #3882).** A door that drives several rounds under one
/// execution row serves `child_turn`, and every child turn it runs lands on a
/// slot no round of that door will ever claim.
///
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
    // The manifest reaches the plane through `serving`'s closure, which is
    // handed each member's own (#4094) — so this test no longer parses one of
    // its own to pass in.
    let dispatcher = RecordingDispatcher::default();
    let wrapper = bind_installed(&roster, "grading-v1", &mut |_| {})
        .expect("the installed plugin declares this variant")
        .serving(|manifest| {
            round_driver_host(
                workspace.path(),
                manifest,
                Arc::new(dispatcher.clone()) as Arc<dyn SubAgentDispatcher>,
                None,
            )
        })
        .expect("a found variant binds");

    let channel = wrapper.gate().open();
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
