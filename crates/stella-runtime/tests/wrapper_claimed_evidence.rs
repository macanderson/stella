// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The witness for #3513: a `Met` a plugin asserted about its own work is
//! **labelled** as such, everywhere it is reported.
//!
//! #3511's Option 2 settled that a verification plugin reports its own
//! evidence and the host does not re-run it. That settlement is honest — the
//! install prompt says so — but it left a hole one layer down: the verdict it
//! produced was byte-identical to one the host had observed for itself. A
//! plugin that executed nothing and printed `flip: achieved` was credited
//! `Outcome::Met` and reported as "every declared requirement is met", with
//! nothing anywhere distinguishing an observation from a claim.
//!
//! The fixture is deliberately the one from `wrapper_decided_flip.rs`, because
//! that file is the *reference shape* an author copies: its plugin
//! string-matches the request and asserts a flip it never ran. That is not a
//! misuse to be scolded — it is what a self-reporting plugin legitimately
//! looks like — which is exactly why the label has to exist.
//!
//! What this does **not** claim: after this fix a plugin that executed nothing
//! still reaches `Met`. It just can no longer do so anonymously. The gate that
//! would refuse it needs a host that can observe a flip itself, and nothing in
//! this tree can — the one host call that would run a check answers
//! `Unsupported` (#3580).
//!
//! `cfg(unix)` for `wrapper_decided_flip.rs`'s reason: the plugin is a
//! `/bin/sh` script, so this file proves nothing on Windows until #3497
//! replaces it with a portable in-tree plugin.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use stella_plugin::{
    CandidateGrant, EvidenceProvenance, EvidenceSet, FlipObservation, ObservedEvidence, Outcome,
    PluginManifest, SignalValues, TamperFinding, TestBaseline, TestPlan, TurnOutcome,
};
use stella_protocol::CandidateHandle;
use stella_runtime::wrapper::{
    DEFAULT_WRAPPER_TIMEOUT, DispatchReport, DrivenTurn, RoundInput, SubprocessWrapper, TurnDriver,
    TurnPrelude, WrapperDispatch,
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

/// A plugin that runs **no test at all**: it string-matches the request and
/// prints the flip it wants to be believed about.
const ASSERTS_A_FLIP_IT_NEVER_RAN: &str = r#"
request=$(cat)
case "$request" in
  *'"root"'*'"program":"sh"'*) flip=achieved ;;
  *) flip=unobservable ;;
esac
printf '%s\n' "{\"point\":\"after_turn\",\"body\":{\"protocol_version\":1,\"evidence\":{\"flip\":\"$flip\"}}}"
"#;

fn dispatch() -> WrapperDispatch {
    let admitted = SubprocessWrapper::declare(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            ASSERTS_A_FLIP_IT_NEVER_RAN.into(),
        ],
        Vec::new(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");
    WrapperDispatch::bind(
        PluginManifest::from_toml_str(MANIFEST).expect("the manifest loads"),
        Arc::new(admitted.wrapper),
    )
    .expect("it declares a [wrapper]")
}

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

/// A host that vouches for the artifacts, so nothing but the flip's own
/// provenance is left to decide the label.
struct Host;

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
            tamper: TamperFinding::Clean,
        }
    }
}

async fn report() -> DispatchReport {
    let input = RoundInput {
        goal: "make the flaky test deterministic".into(),
        signals: signals(),
        candidate: Some(granted("/tmp/workspace")),
    };
    dispatch()
        .run(input, &mut Host)
        .await
        .expect("a validated manifest resolves")
}

/// **The witness.** The verdict still stands — this change withholds nothing —
/// but both the outcome and the line a user reads name whose word it rests on.
#[tokio::test]
async fn a_met_on_the_plugins_own_word_is_labelled_as_such() {
    let report = report().await;

    assert!(report.met(), "the verdict is not withheld, only attributed");
    assert_eq!(
        report.outcome,
        Outcome::Met {
            evidence: EvidenceProvenance::PluginReported,
        },
        "a flip the plugin asserted about its own work is plugin-reported"
    );
    assert!(
        report
            .summary()
            .contains("on the plugin's own reported evidence"),
        "a Met a plugin asserted about its own work must not print as an \
         observation, got: {}",
        report.summary()
    );
}

/// The falsifier, and the reason the field above is a distinction rather than
/// a decoration: the two sources stamp **different** values. If everything
/// were `PluginReported`, the label in the witness would carry no information.
#[test]
fn provenance_distinguishes_the_two_sources() {
    let reported = EvidenceSet::from_observed(
        ObservedEvidence {
            flip: FlipObservation::Achieved,
            measurements: BTreeMap::new(),
        },
        TamperFinding::Clean,
    );
    assert_eq!(reported.provenance, EvidenceProvenance::PluginReported);

    assert_eq!(
        EvidenceSet::unobserved().provenance,
        EvidenceProvenance::HostObserved,
        "an `Unobservable` flip is the host's own conclusion about a plugin \
         that did not answer, not a claim the plugin made"
    );
}
