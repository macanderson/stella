// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The dispatch half of #3587: what every stage of a round declared as its
//! witness reaches the host **before** the host runs the turn.
//!
//! The wire half is `stella-plugin`'s `wire_contract.rs`, and the filesystem
//! half is `stella-cli`'s `wrapper_candidate.rs` — the host's own comparison
//! over what it pinned. This file is the seam between them, and it is the level
//! every host has in common: `doc:wrapper-socket` §6 wants the same plugin
//! decided under `stella-serve` and an embedded host too, and all three read
//! the declaration off [`TurnPrelude`].
//!
//! The rule under test is the union, which is `scope`'s rule and is here for
//! the same reason: a wrapper declaring several stages contributes at each one,
//! two of them naming the same artifact are asking for one watch, and a list
//! that repeats it says nothing extra.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason and tracked in the same place
//! (#3497): the plugin is a `/bin/sh` script.

#![cfg(unix)]

use std::sync::Arc;

use async_trait::async_trait;
use stella_plugin::{PluginManifest, SignalValues, TamperFinding, TurnOutcome};
use stella_runtime::wrapper::{
    DEFAULT_WRAPPER_TIMEOUT, DrivenTurn, RoundInput, SubprocessWrapper, TurnDriver, TurnPrelude,
    WrapperDispatch,
};

/// A steering wrapper with two stages, so a round asks it twice.
const MANIFEST: &str = r#"
name = "declares-its-witness"
description = "names the artifacts its flip is measured against"

[loop]
participation = "steering"
points = ["before_turn"]

[wrapper]
id = "declaring-v1"
[[wrapper.stages]]
name = "research"
[[wrapper.stages]]
name = "verify"
"#;

/// Each stage declares one artifact, and both stages declare `tests/flip.rs` —
/// the overlap is the point, because the union has to fold it.
///
/// A plugin that writes JSON with `printf` and reads its request with `case`,
/// which is `doc:pipeline-as-plugins` §5 commitment 2 held to: a capability
/// only Rust can reach is a Rust API with extra steps.
const DECLARES: &str = r#"
request=$(cat)
case "$request" in
  *'"stage":"research"'*) list='"tests/flip.rs"' ;;
  *)                      list='"tests/flip.rs","tests/second.rs"' ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"witness":[%s]}}\n' "$list"
"#;

fn manifest() -> PluginManifest {
    PluginManifest::from_toml_str(MANIFEST).expect("the manifest loads")
}

fn signals() -> SignalValues {
    SignalValues {
        test_command: true,
        candidates: 1,
        budget_metered: false,
        conversational: false,
        questions: 0,
        plans: false,
        verifies: false,
        wants_witness: false,
        wants_verifier: false,
        mutating_actions: 0,
        diff_lines: 0,
        witness_authored: false,
        flip_achieved: false,
        tests_red: false,
        tests_green: false,
    }
}

/// A host that records what it was told to watch, in the order it was told —
/// which is exactly what `stella-cli`'s driver does before it pins anything.
#[derive(Default)]
struct Recording {
    declared: Vec<String>,
}

#[async_trait(?Send)]
impl TurnDriver for Recording {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        self.declared = prelude.witness().to_vec();
        DrivenTurn {
            outcome: TurnOutcome {
                completed: true,
                answer: "replaced the sleep with a barrier".into(),
                tools: Some(Vec::new()),
                changed_files: Some(Vec::new()),
            },
            tamper: TamperFinding::NotChecked,
        }
    }
}

/// **The witness.** Both stages' declarations reach the host as one list, in
/// first-seen order, with the artifact they share appearing once.
///
/// Before the change there was no field to declare on: a `before_turn` response
/// carrying `witness` did not parse (`deny_unknown_fields`), so the plugin's
/// whole contribution was dropped as a decode fault and the host had nothing
/// but the test invocation's own argv to watch.
#[tokio::test]
async fn every_stage_of_a_round_declares_its_witness_into_one_union() {
    let admitted = SubprocessWrapper::declare(
        vec!["/bin/sh".into(), "-c".into(), DECLARES.into()],
        Vec::new(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");
    let dispatch = WrapperDispatch::bind(manifest(), Arc::new(admitted.wrapper))
        .expect("it declares a [wrapper]");

    let mut host = Recording::default();
    dispatch
        .run(
            RoundInput {
                goal: "make the flaky test deterministic".into(),
                signals: signals(),
                candidate: None,
            },
            &mut host,
        )
        .await
        .expect("a validated manifest resolves");

    assert_eq!(
        host.declared,
        vec!["tests/flip.rs".to_string(), "tests/second.rs".to_string()],
        "the union across stages, in first-seen order, folding the shared artifact"
    );
}

/// A wrapper that declares nothing hands the host nothing, and the host's
/// finding stays `NotChecked` — which `judge` reads as an abstention rather
/// than a pass. The silence has to keep meaning what it always meant, or every
/// plugin written before this field would start being credited for a watch
/// nobody performed.
#[tokio::test]
async fn a_wrapper_that_declares_nothing_hands_the_host_an_empty_watch() {
    let admitted = SubprocessWrapper::declare(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "cat >/dev/null; printf '{\"point\":\"before_turn\",\"body\":{\"protocol_version\":1}}\
             \\n'"
                .into(),
        ],
        Vec::new(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");
    let dispatch = WrapperDispatch::bind(manifest(), Arc::new(admitted.wrapper))
        .expect("it declares a [wrapper]");

    let mut host = Recording::default();
    dispatch
        .run(
            RoundInput {
                goal: "make the flaky test deterministic".into(),
                signals: signals(),
                candidate: None,
            },
            &mut host,
        )
        .await
        .expect("a validated manifest resolves");

    assert!(host.declared.is_empty());
}
