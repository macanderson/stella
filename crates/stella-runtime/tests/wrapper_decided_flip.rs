// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The witness for #3553: a plugin whose `[oracle]` declares `flip =
//! "required"` reaches a **decided** verdict through the real dispatch.
//!
//! # Why this could not be written before
//!
//! Two host-side facts were missing on every shipping driver at once, and each
//! one alone is enough to force an abstention:
//!
//! - `RoundInput::candidate` was `None`, so a plugin had no root to read and no
//!   test to run. With nothing to observe, the honest report is
//!   [`FlipObservation::Unobservable`], and under
//!   [`FlipPolicy::Required`](stella_plugin::FlipPolicy) `judge` answers
//!   [`UndecidedReason::FlipUnobservable`].
//! - the driver reported [`TamperFinding::NotChecked`] about its own check, and
//!   `TamperPolicy::ArtifactIdentity` refuses to credit a flip whose artifacts
//!   nobody compared — [`UndecidedReason::TamperUnchecked`].
//!
//! So the verdict was `Undecided` on **every** run of **every** such plugin,
//! and the third assertion below — the falsifier — is the one that proves the
//! first two are about the fix rather than about the fixture: with the tamper
//! finding put back to `NotChecked`, the identical plugin and the identical
//! flip abstain exactly as they used to.
//!
//! `stella-cli`'s driver supplies both facts now
//! (`crates/stella-cli/src/wrapper_candidate.rs`, whose own tests pin the grant
//! and the identity comparison against a real filesystem). This file drives the
//! shared sequence, which is where the two facts turn into a verdict, and it is
//! the level every host has in common — §6 wants the same plugin decided under
//! `stella-serve` and an embedded host too.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason and tracked in the same place:
//! the plugin is a `/bin/sh` script, so this file proves nothing on Windows
//! until #3497 replaces it with a portable in-tree plugin.

#![cfg(unix)]

use std::sync::Arc;

use async_trait::async_trait;
use stella_plugin::{
    CandidateGrant, Outcome, PluginManifest, SignalValues, TamperFinding, TestBaseline, TestPlan,
    TurnOutcome, UndecidedReason, UnmetBecause,
};
use stella_protocol::CandidateHandle;
use stella_runtime::wrapper::{
    DEFAULT_WRAPPER_TIMEOUT, DrivenTurn, RoundInput, SubprocessWrapper, TurnDriver, TurnPrelude,
    WrapperDispatch,
};

/// An arbiter whose definition of done is the witness contract itself.
const MANIFEST: &str = r#"
name = "witness-arbiter"
description = "holds the turn open until the witness flips red to green"

[loop]
participation = "arbiter"
hooks = ["Stop"]
points = ["after_turn"]
max_holds = 1

[requirements]
proven = "a witness test failed before the change and passes after it"

[oracle]
flip = "required"

[runtime]
argv = ["/bin/sh", "-c", "true"]
timeout_secs = 60

[wrapper]
id = "witness-v1"
[[wrapper.stages]]
name = "verify"
"#;

/// The plugin reads the grant it was handed and reports the flip it watched.
///
/// It answers from the request rather than from ambient state, which is the
/// property the grant exists to make possible: `root` and `test` are the only
/// things it needs, and both arrived in the message.
const OBSERVES_THE_FLIP: &str = r#"
request=$(cat)
case "$request" in
  *'"root"'*'"program":"sh"'*) flip=achieved ;;
  *) flip=unobservable ;;
esac
printf '%s\n' "{\"point\":\"after_turn\",\"body\":{\"protocol_version\":1,\"evidence\":{\"flip\":\"$flip\"}}}"
"#;

fn manifest() -> PluginManifest {
    PluginManifest::from_toml_str(MANIFEST).expect("the manifest loads")
}

fn dispatch() -> WrapperDispatch {
    let admitted = SubprocessWrapper::declare(
        vec!["/bin/sh".into(), "-c".into(), OBSERVES_THE_FLIP.into()],
        Vec::new(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");
    WrapperDispatch::bind(manifest(), Arc::new(admitted.wrapper)).expect("it declares a [wrapper]")
}

/// The grant a driver that runs in the tree it already has mints — the shape
/// `stella_plugin::host_tree_grant` produces (the minting logic was the staged
/// pipeline's `ports::host_tree_grant` until #3865 deleted that crate).
fn granted(root: &str) -> CandidateGrant {
    CandidateGrant::new(CandidateHandle::new("host-tree"), root).with_test(
        TestPlan::new("sh", vec!["tests/witness_flip.sh".to_string()])
            .with_baseline(TestBaseline::NotRun),
    )
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

/// A host that reports one fixed tamper finding, as a real driver reports what
/// its own comparison found.
struct Host(TamperFinding);

#[async_trait(?Send)]
impl TurnDriver for Host {
    async fn run_turn(&mut self, _prelude: TurnPrelude) -> DrivenTurn {
        DrivenTurn {
            outcome: TurnOutcome {
                completed: true,
                answer: "replaced the sleep with a barrier".into(),
                tools: Some(vec!["edit_file".into()]),
                changed_files: Some(vec!["crates/stella-core/src/driver.rs".into()]),
            },
            tamper: self.0.clone(),
        }
    }
}

async fn run(candidate: Option<CandidateGrant>, tamper: TamperFinding) -> Outcome {
    let input = RoundInput {
        goal: "make the flaky test deterministic".into(),
        signals: signals(),
        candidate,
    };
    dispatch()
        .run(input, &mut Host(tamper))
        .await
        .expect("a validated manifest resolves")
        .outcome
}

/// **The witness.** A grant the plugin can act on, plus a host that vouched for
/// the artifacts, is a decided `Met`.
#[tokio::test]
async fn a_granted_candidate_and_a_clean_snapshot_decide_the_verdict() {
    assert_eq!(
        run(Some(granted("/tmp/workspace")), TamperFinding::Clean).await,
        Outcome::Met,
        "a flip the plugin observed over artifacts the host vouches for is done"
    );
}

/// The other decided answer: the host's own finding refuses a flip whose
/// witness the worker rewrote. `Unmet`, not `Undecided` — the evidence settled
/// it, and it settled it against the work.
#[tokio::test]
async fn a_tampered_witness_is_refused_rather_than_abstained_over() {
    let outcome = run(
        Some(granted("/tmp/workspace")),
        TamperFinding::Tampered {
            artifact: "tests/witness_flip.sh".into(),
        },
    )
    .await;
    match outcome {
        Outcome::Unmet { unmet, .. } => {
            assert_eq!(unmet.len(), 1);
            assert_eq!(
                unmet[0].because,
                UnmetBecause::Tampered {
                    artifact: "tests/witness_flip.sh".into(),
                }
            );
        }
        other => panic!("a host finding of tampering is determinate, got {other:?}"),
    }
}

/// **The falsifier.** Take either host-side fact away and the identical plugin
/// abstains — which is what every run of every witness plugin did before #3553,
/// and what makes the two assertions above about the fix rather than about this
/// fixture.
#[tokio::test]
async fn without_the_grant_or_the_snapshot_the_verdict_is_undecided() {
    assert_eq!(
        run(None, TamperFinding::Clean).await,
        Outcome::Undecided {
            reason: UndecidedReason::FlipUnobservable,
        },
        "with no candidate to act on, the plugin can observe no flip"
    );
    assert_eq!(
        run(Some(granted("/tmp/workspace")), TamperFinding::NotChecked).await,
        Outcome::Undecided {
            reason: UndecidedReason::TamperUnchecked,
        },
        "a flip whose artifacts nobody compared is not credited"
    );
}
