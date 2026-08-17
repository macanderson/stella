//! Slice A's acceptance, verbatim from #3245 §6: fixture manifests at all
//! four grades round-trip, and a hook not named in the manifest is not
//! callable even if the process registers for it (the manifest side of that
//! rule — [`LoopGrant::permits_hook`] is the filter a host must consult).
//!
//! Round-tripping is checked through both wire shapes: TOML (the manifest's
//! own format) and `serde_json` (invariant 4 — these types will cross the
//! host boundary as JSON when install consent and `stella app list` render
//! them).

use stella_plugin::{HookEvent, OracleProcessSource, Participation, PluginManifest, WrapperPoint};

const FIXTURES: [(&str, Participation); 4] = [
    (include_str!("fixtures/none.toml"), Participation::None),
    (
        include_str!("fixtures/observer.toml"),
        Participation::Observer,
    ),
    (
        include_str!("fixtures/steering.toml"),
        Participation::Steering,
    ),
    (
        include_str!("fixtures/arbiter.toml"),
        Participation::Arbiter,
    ),
];

#[test]
fn every_grade_fixture_parses_to_its_declared_grade() {
    for (text, grade) in FIXTURES {
        let manifest = PluginManifest::from_toml_str(text).unwrap();
        assert_eq!(manifest.loop_grant.participation, grade);
    }
}

#[test]
fn every_grade_fixture_round_trips_through_toml() {
    for (text, grade) in FIXTURES {
        let parsed = PluginManifest::from_toml_str(text).unwrap();
        let reserialized = toml::to_string(&parsed).unwrap();
        let reparsed = PluginManifest::from_toml_str(&reserialized).unwrap();
        assert_eq!(
            parsed, reparsed,
            "TOML round-trip diverged at grade {grade}"
        );
    }
}

#[test]
fn every_grade_fixture_round_trips_through_serde_json() {
    for (text, grade) in FIXTURES {
        let parsed = PluginManifest::from_toml_str(text).unwrap();
        let json = serde_json::to_string(&parsed).unwrap();
        let reparsed: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed, reparsed,
            "JSON round-trip diverged at grade {grade}"
        );
    }
}

#[test]
fn a_hook_not_named_in_the_manifest_is_not_permitted() {
    // The steering fixture declares SessionStart and PreToolUse. A process
    // registering for PostToolUse anyway gets nothing: the filter answers
    // from the manifest alone.
    let steering = PluginManifest::from_toml_str(FIXTURES[2].0).unwrap();
    assert!(steering.loop_grant.permits_hook(HookEvent::SessionStart));
    assert!(steering.loop_grant.permits_hook(HookEvent::PreToolUse));
    assert!(!steering.loop_grant.permits_hook(HookEvent::PostToolUse));
    assert!(!steering.loop_grant.permits_hook(HookEvent::PreCompact));
    assert!(!steering.loop_grant.permits_hook(HookEvent::Stop));

    // Below steering no hook is ever permitted, declared or not.
    for (text, _) in &FIXTURES[..2] {
        let manifest = PluginManifest::from_toml_str(text).unwrap();
        for hook in [
            HookEvent::SessionStart,
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::PreCompact,
        ] {
            assert!(!manifest.loop_grant.permits_hook(hook));
        }
    }
}

#[test]
fn the_arbiter_fixture_carries_the_full_grant() {
    let arbiter = PluginManifest::from_toml_str(FIXTURES[3].0).unwrap();
    assert!(arbiter.loop_grant.permits_hook(HookEvent::Stop));
    assert_eq!(arbiter.loop_grant.max_holds, Some(3));

    let requirements = arbiter.requirements.as_ref().unwrap();
    assert!(requirements.contains_key("tests-flip"));
    assert!(requirements.contains_key("no-tamper"));

    // Resolved rather than read off the block: the oracle may be the plugin's
    // own `[runtime]` process, and a caller that only wants "what does the host
    // run" must not have to know which of the two named it (#3501).
    let oracle = arbiter
        .oracle_process()
        .expect("the arbiter fixture declares an oracle");
    assert_eq!(oracle.timeout_secs, 120);
    assert_eq!(oracle.source, OracleProcessSource::OracleCommand);
    assert!(
        arbiter.loop_grant.permits_point(WrapperPoint::AfterTurn),
        "an oracle's evidence arrives at after_turn, so the fixture declares it"
    );

    let subloop = arbiter.subloop.as_ref().unwrap();
    assert_eq!(
        subloop.stages,
        ["triage", "plan", "execute", "witness", "verify"]
    );
}

/// The manifest's hook vocabulary IS the engine's, not a copy of it (#3310).
///
/// Before the shared home this assertion could not have been written: the two
/// enums were distinct types in crates that may not depend on each other, so
/// "the sets are identical" was a sentence in a doc comment, and a sixth
/// engine event would have been undeclarable in a manifest with nothing red.
/// `HookEvent::ALL` is what makes the *whole* set the subject, rather than
/// the five a test author happened to spell out.
#[test]
fn hook_vocabulary_is_the_shared_one() {
    let granted: Vec<HookEvent> = HookEvent::ALL.into_iter().collect();
    let shared: Vec<stella_protocol::hook::HookEvent> =
        stella_protocol::hook::HookEvent::ALL.into_iter().collect();
    assert_eq!(granted, shared);
}
