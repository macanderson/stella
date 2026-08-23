// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `--pipeline a,b`: the CLI half of #3801, and the privilege boundary that
//! makes it more than a loop over `bind`.
//!
//! A submodule of `wrapper_plugin::tests` for its siblings' reason — that file
//! sits under the 1500-line ratchet and these belong together.

use super::*;

/// Two wrappers that declare **different** role intents and the same stage
/// order, so a composition of them binds and the only thing separating their
/// child-turn planes is which manifest each was built from.
const GROUNDER_MANIFEST: &str = r#"
name = "grounder"
[loop]
participation = "steering"
points = ["before_turn"]
calls = ["child_turn"]
[roles.researcher]
tier = "research"
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
[wrapper]
id = "ground-v1"
[[wrapper.stages]]
name = "research"
[[wrapper.stages]]
name = "execute"
"#;

const PLANNER_MANIFEST: &str = r#"
name = "planner"
[loop]
participation = "steering"
points = ["before_turn"]
calls = ["child_turn"]
[roles.strategist]
tier = "plan"
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
[wrapper]
id = "plan-fixture-v1"
[[wrapper.stages]]
name = "research"
[[wrapper.stages]]
name = "execute"
"#;

fn both_installed() -> PluginRoster {
    roster(vec![
        installed(GROUNDER_MANIFEST, "/plugins/grounder"),
        installed(PLANNER_MANIFEST, "/plugins/planner"),
    ])
}

/// A composition bound the way `stella run` binds one: a child-turn plane per
/// member, built from that member's own manifest.
fn composed(selection: &str) -> BoundWrapper {
    let dispatcher = Arc::new(RecordingDispatcher::default()) as Arc<dyn SubAgentDispatcher>;
    bind_installed(&both_installed(), selection, &mut |_| {})
        .expect("both fixtures are installed")
        .serving(|manifest| {
            WrapperHost::recalling(no_recall()).with_child_turns(Arc::new(child_turn_plane(
                manifest,
                Arc::clone(&dispatcher),
            )))
        })
        .expect("the two stage orders reconcile")
}

/// **Witness for #4094.** `--pipeline a,b` resolves both plugins and binds
/// them as one selection.
///
/// `WrapperDispatch::bind_composed` landed with #3801 and nothing in the
/// shipping binary could reach it: `bind_installed` scanned for the *first*
/// manifest whose `wrapper.id == variant` and bound that one. The capability
/// existed and no user could ask for it.
#[test]
fn a_comma_separated_selection_binds_every_member_in_the_order_given() {
    let bound = composed("ground-v1,plan-fixture-v1");
    assert_eq!(
        bound.variant(),
        "ground-v1,plan-fixture-v1",
        "the composition's id names both members, in the order the selection did"
    );
    assert_eq!(
        bound.gates.len(),
        2,
        "a gate per member: the `[loop]` grant is one plugin's"
    );

    let reversed = composed("plan-fixture-v1,ground-v1");
    assert_eq!(
        reversed.variant(),
        "plan-fixture-v1,ground-v1",
        "the order is the selection's — nothing else in the system knows it"
    );
}

/// **The privilege witness, and the reason `serving` takes a closure.**
///
/// `child_turn_plane` reads the manifest's `[roles]` and `[loop] max_calls`,
/// so a single `WrapperHost` cloned across members would hand the second
/// plugin a plane built from the first's manifest — and it could then spend a
/// child turn at a role intent no human consented to on its install. That is
/// a privilege leak, not a cosmetic problem.
///
/// Each member names the *other's* role and is refused; each names its own
/// and is served. The second half is the anti-vacuity: a build that refused
/// everything would pass the first assertion alone.
#[tokio::test]
async fn a_member_cannot_name_another_members_role_intent() {
    use stella_plugin::{HostCallArgs, HostCallOk, HostCallOutcome};
    use stella_runtime::wrapper::HostCallChannel;

    let bound = composed("ground-v1,plan-fixture-v1");
    let ask = |role: &str| {
        HostCallArgs::ChildTurn(stella_plugin::ChildTurnArgs {
            role: role.to_string(),
            instruction: "say something about the diff".to_string(),
        })
    };

    for (member, plugin, own, other) in [
        (0, "grounder", "researcher", "strategist"),
        (1, "planner", "strategist", "researcher"),
    ] {
        let gate = &bound.gates[member];
        match gate.open().call(ask(other)).await {
            HostCallOutcome::Err(failure) => {
                assert_eq!(failure.refusal, stella_plugin::HostCallRefusal::Undeclared);
                // The detail names the *asking* plugin, which is the proof
                // that the plane was built from its own manifest and not its
                // neighbour's.
                assert!(
                    failure
                        .detail
                        .contains(&format!("plugin \"{plugin}\" declares no [roles.{other}]")),
                    "member {member} was refused for the wrong reason: {}",
                    failure.detail
                );
            }
            other_outcome => panic!(
                "member {member} reached `{other}` — its plane was built from another \
                 plugin's manifest: {other_outcome:?}"
            ),
        }
        match gate.open().call(ask(own)).await {
            HostCallOutcome::Ok(HostCallOk::ChildTurn(result)) => {
                assert_eq!(result.role, own);
            }
            outcome => panic!("member {member} must still reach its own role: {outcome:?}"),
        }
    }
}

/// A repeated id is a typo, not a request to run a plugin twice — a second
/// copy would compose against itself, spend a second gate's allowance and
/// print every refusal twice.
#[test]
fn a_repeated_id_is_refused_as_a_typo() {
    let refusal = bind_installed(&both_installed(), "ground-v1,ground-v1", &mut |_| {})
        .expect_err("a selection runs each plugin once");
    assert!(refusal.contains("ground-v1"), "{refusal}");
    assert!(refusal.contains("more than once"), "{refusal}");
}

/// The first id that names nothing installed is what the refusal names, and
/// it still lists what *is* installed — a selection does not make the
/// single-id message worse.
#[test]
fn a_member_that_is_not_installed_is_named_and_the_installed_ones_listed() {
    let refusal = bind_installed(&both_installed(), "ground-v1,typo-v1", &mut |_| {})
        .expect_err("the second id names nothing");
    assert!(refusal.contains("typo-v1"), "{refusal}");
    assert!(
        refusal.contains("ground-v1") && refusal.contains("plan-fixture-v1"),
        "the installed variants are offered: {refusal}"
    );
}

/// An empty entry — a trailing comma, or `a,,b` — is refused rather than
/// silently skipped, because a skipped member is a plugin the user asked for
/// and did not get.
#[test]
fn an_empty_entry_is_refused() {
    for selection in ["ground-v1,", ",ground-v1", "ground-v1,,plan-fixture-v1"] {
        let refusal = bind_installed(&both_installed(), selection, &mut |_| {})
            .err()
            .unwrap_or_else(|| panic!("`{selection}` must be refused"));
        assert!(refusal.contains("empty entry"), "{refusal}");
    }
}
