// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What `executions.pipeline_variant` records, per door.
//!
//! A sibling file rather than an inline `mod` block, for the reason AGENTS.md
//! § "God files" gives: [`super`] sits against the 1500-line ratchet, and
//! this suite grows with every door that gains a wrapper driver (#3695 added
//! the third).

use super::*;

/// **The A/B witness.** The wrapper that ran is recorded on the executions
/// row — the actual variant id, not a constant.
///
/// Failing before #3494 because the only writer was
/// [`begin_pipeline_execution`], which passed
/// [`PIPELINE_VARIANT_CLASSIC`] unconditionally: two variants produced the
/// same value in the column both are supposed to be distinguished by, so
/// the comparison `doc:pipeline-as-plugins` §7 justifies the whole
/// extraction with could not be made. The `TurnDoor` this asserts through
/// did not exist either.
#[test]
fn the_wrapper_that_ran_is_the_variant_the_row_records() {
    let store = stella_store::Store::in_memory().expect("store");

    let wrapped = store
        .begin_execution("run", "make it faster", "anthropic", "claude-opus-5")
        .expect("begin");
    let door = TurnDoor::new("run").wrapped_by("budget-v1");
    assert_eq!(door.kind, "run", "the door is the command, not the wrapper");
    store
        .set_pipeline_variant(wrapped, door.variant.expect("a wrapper ran"))
        .expect("record the variant");
    assert_eq!(
        store.pipeline_variant(wrapped).expect("read"),
        Some("budget-v1".to_string()),
        "the installed plugin's own id reaches the column, not the built-in's"
    );

    // ...and the built-in staged pipeline still records its own id rather
    // than a blank, which is the rule the column shipped with.
    let classic = store
        .begin_execution("run", "make it faster", "anthropic", "claude-opus-5")
        .expect("begin");
    store
        .set_pipeline_variant(classic, PIPELINE_VARIANT_CLASSIC)
        .expect("record");
    assert_eq!(
        store.pipeline_variant(classic).expect("read"),
        Some("classic".to_string())
    );

    // ...and a raw turn with nothing over it leaves NULL, which is the
    // positive statement "no wrapper ran", not a missing measurement.
    let bare = store
        .begin_execution("run", "make it faster", "anthropic", "claude-opus-5")
        .expect("begin");
    assert!(TurnDoor::new("run").variant.is_none());
    assert_eq!(store.pipeline_variant(bare).expect("read"), None);
}

/// **Pins the goal/fleet honesty fix's call shape (#3381, #3388, #3684).**
/// `goal.rs`'s `run_goal_pipeline_turn` and `fleet_cmd.rs`'s `run_task`
/// used to call `begin_execution(..., None)` even while the staged
/// pipeline drove the round, so `NULL` meant "raw OR classic" there —
/// unlike `run`/`deck`. The real fix is the one-line argument each site
/// now passes, verified by reading those two lines directly (neither is
/// reachable from a unit test without a live provider); this pins that
/// `begin_execution`/`pipeline_variant` round-trip both call shapes
/// correctly for `"goal"`/`"fleet"` — see `wrapper_plugin::tests` for the
/// `resolve`-level witnesses that DO fail on old code.
#[test]
fn goal_and_fleet_write_classic_honestly_never_a_false_null() {
    let provider = crate::config::PROVIDERS[0].clone();
    let cfg = crate::config::Config::for_tests(provider, "test-model".to_string());
    let store = Some(Arc::new(Store::in_memory().expect("in-memory store")));
    // What `begin_execution` writes and reads back for `door`/`variant`.
    let written = |door: &str, v| {
        let (s, id) = begin_execution(&store, door, "prompt", &cfg, None, v).expect("begin");
        s.pipeline_variant(id).expect("read")
    };

    for door in ["goal", "fleet"] {
        // Classic arm: the wrapper that ran is named, not dropped. Raw
        // arm: NULL is the honest answer here, not a gap.
        let msg = format!("{door}: honesty rule");
        assert_eq!(
            written(door, Some(PIPELINE_VARIANT_CLASSIC)),
            Some(PIPELINE_VARIANT_CLASSIC.to_string()),
            "{msg}"
        );
        assert_eq!(written(door, None), None, "{msg}");
    }
}

/// **Witness (#3695).** A wrapped `goal` or `fleet` attempt records the
/// *installed plugin's own* variant id, the same rule
/// [`the_wrapper_that_ran_is_the_variant_the_row_records`] already held
/// the `run` door to. It fails on code where those two doors could only
/// ever write `classic` or NULL — which is what
/// `wrapper_plugin::reject_plugin_variant_for_door` guaranteed for
/// `fleet` by refusing a named variant outright before the run started,
/// so no `fleet` row in that build can carry a plugin id at all.
///
/// The call shape it pins is the one argument each door now passes:
/// `crate::agent::goal::goal_wrapped::run_goal_wrapped_turn`'s
/// `Some(bound.variant())` and `crate::fleet_cmd::run_task`'s
/// `wrapped.as_ref().map(AttemptWrapper::variant)`. Neither is reachable
/// from a unit test without a live provider; the end-to-end witnesses that
/// drive a real binary are `goal_wrapped_dispatch_cli.rs` and
/// `fleet_wrapped_dispatch_cli.rs`.
#[test]
fn a_wrapped_goal_or_fleet_row_records_the_plugins_own_id() {
    let provider = crate::config::PROVIDERS[0].clone();
    let cfg = crate::config::Config::for_tests(provider, "test-model".to_string());
    let store = Some(Arc::new(Store::in_memory().expect("in-memory store")));

    for door in ["goal", "fleet"] {
        let (s, id) =
            begin_execution(&store, door, "prompt", &cfg, None, Some("budget-v1")).expect("begin");
        assert_eq!(
            s.pipeline_variant(id).expect("read"),
            Some("budget-v1".to_string()),
            "{door}: the installed plugin's own id reaches the column, not the built-in's"
        );
        assert_ne!(
            s.pipeline_variant(id).expect("read").as_deref(),
            Some(PIPELINE_VARIANT_CLASSIC),
            "{door}: a wrapped attempt must never be recorded as the built-in pipeline"
        );
    }
}

/// `PIPELINE_VARIANT_CLASSIC` is now a pure historical literal (the crate
/// it used to cross-check against, `stella_pipeline::variant`, is gone —
/// see the constant's own doc comment). This only pins the literal's
/// spelling so an edit to it is a deliberate, reviewed change to a join
/// key already-written rows depend on.
#[test]
fn the_classic_id_literal_is_unchanged() {
    assert_eq!(PIPELINE_VARIANT_CLASSIC, "classic");
}
