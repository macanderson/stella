// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A round the host judged leaves a record. The record says who judged it.
//!
//! `VerdictStamp` had no writer. So the list of claims was empty on every
//! run. A dispatch now hands the round back with one claim on it.
//!
//! Each test drives the whole dispatch. The name on the claim is the point
//! here. A test of the fold alone could pass while the host still sent the
//! wrong name.
//!
//! The plugin is `wrapper-plugin-fixture`, so this file runs on Windows too.

use std::sync::Arc;

use async_trait::async_trait;
use stella_core::ports::Clock;
use stella_plugin::{
    CandidateGrant, PluginManifest, SignalValues, TamperFinding, TestBaseline, TestPlan,
    TurnOutcome,
};
use stella_protocol::hash::record_hash;
use stella_protocol::{CandidateHandle, StampAssessment};
use stella_runtime::wrapper::{
    DEFAULT_WRAPPER_TIMEOUT, DispatchReport, DrivenTurn, RoundInput, SubprocessWrapper, TurnDriver,
    TurnPrelude, TurnWrapper, WrapperDispatch,
};

/// A plugin that holds the turn open until the test flips red to green.
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

# The manifest wants an argv. Nothing here spawns it: these tests build
# their own transport. The string is parsed and never run, so it stays
# portable on Windows.
[runtime]
argv = ["/bin/sh", "-c", "true"]
timeout_secs = 60

[wrapper]
id = "witness-v1"
[[wrapper.stages]]
name = "verify"
"#;

/// A second, steering member. It has no oracle and no say over the round.
/// It is here only to make the composed id longer than the arbiter's own id.
const STEERING_MANIFEST: &str = r#"
name = "witness-reader"
description = "reads the round and claims nothing"

[loop]
participation = "steering"
points = ["before_turn"]

[wrapper]
id = "reader-v1"
[[wrapper.stages]]
name = "recall"
"#;

const FIXTURE: &str = env!("CARGO_BIN_EXE_wrapper-plugin-fixture");

/// A clock this file owns. Both times on the claim are then values it names.
struct PinnedClock;

impl Clock for PinnedClock {
    fn now_ms(&self) -> u64 {
        1_767_225_600_000
    }
}

fn dispatch(mode: &str) -> WrapperDispatch {
    let admitted = SubprocessWrapper::declare(
        vec![FIXTURE.to_string(), mode.to_string()],
        Vec::new(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");
    WrapperDispatch::bind(
        PluginManifest::from_toml_str(MANIFEST).expect("the manifest loads"),
        Arc::new(admitted.wrapper),
    )
    .expect("it declares a [wrapper]")
    .with_clock(Arc::new(PinnedClock))
}

/// The same arbiter, now composed beside the steering member above. It
/// never answers `after_turn`, so the arbiter's rule still decides alone.
fn composed_dispatch(mode: &str) -> WrapperDispatch {
    let reader = SubprocessWrapper::declare(
        vec![
            FIXTURE.to_string(),
            "echo-stage".to_string(),
            "reader".to_string(),
        ],
        Vec::new(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");
    let arbiter = SubprocessWrapper::declare(
        vec![FIXTURE.to_string(), mode.to_string()],
        Vec::new(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");
    WrapperDispatch::bind_composed(vec![
        (
            PluginManifest::from_toml_str(STEERING_MANIFEST).expect("the manifest loads"),
            Arc::new(reader.wrapper) as Arc<dyn TurnWrapper>,
        ),
        (
            PluginManifest::from_toml_str(MANIFEST).expect("the manifest loads"),
            Arc::new(arbiter.wrapper) as Arc<dyn TurnWrapper>,
        ),
    ])
    .expect("a steering member and an arbiter member compose")
    .with_clock(Arc::new(PinnedClock))
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

/// A host that vouches for the files. The flip is then all that is left.
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

async fn report(mode: &str) -> DispatchReport {
    let input = RoundInput {
        goal: "make the flaky test deterministic".into(),
        signals: signals(),
        candidate: Some(granted("/tmp/workspace")),
    };
    dispatch(mode)
        .run(input, &mut Host)
        .await
        .expect("a validated manifest resolves")
}

/// **The witness.** A judged round comes back with one claim on it. The name
/// is the one the manifest gave. The hash adds up again from the record the
/// caller was handed.
#[tokio::test]
async fn a_decided_round_is_stamped_with_the_manifests_name() {
    let report = report("flip-if-granted").await;

    assert_eq!(
        report.snapshot.stamps.len(),
        1,
        "one round, one watcher, one claim: {:?}",
        report.snapshot.stamps
    );
    let stamp = &report.snapshot.stamps[0];
    assert_eq!(stamp.author, "witness-v1", "the name is the manifest's id");
    assert_eq!(stamp.assessment, StampAssessment::Done);
    assert_eq!(stamp.decided_at_ms, 1_767_225_600_000);
    assert_eq!(
        stamp.duration_ms, 0,
        "a pinned clock does not move, so the gap is zero"
    );
    assert!(!stamp.timed_out);

    let preimage = report
        .snapshot
        .stamp_preimage()
        .expect("the record serializes");
    assert_eq!(
        stamp.preimage_hash,
        record_hash(&preimage).expect("the record hashes"),
        "the hash drops the claims, so it adds up again from what came back"
    );
    assert!(
        report.faults.is_empty(),
        "nothing failed: {:?}",
        report.faults
    );
}

/// The plugin signs its note with another name. It gets the manifest name
/// anyway. Without that rule a plugin could sign in a name nobody installed.
#[tokio::test]
async fn a_plugin_cannot_name_itself() {
    let report = report("flip-and-claim-author").await;

    let stamp = &report.snapshot.stamps[0];
    assert_eq!(
        stamp.author, "witness-v1",
        "the name comes from the manifest, never from the payload"
    );
    assert_ne!(stamp.author, "vera");
}

/// The record points at the tree the proof came from. A reader can go look.
#[tokio::test]
async fn the_stamp_points_at_the_workspace_it_judged() {
    let report = report("flip-if-granted").await;

    assert_eq!(
        report.snapshot.stamps[0].evidence_refs,
        vec!["candidate:host-tree".to_string()]
    );
}

/// **The witness.** A composed run's stamp names the arbiter. It does not
/// name the whole composition.
///
/// Naming the whole composition would read `"reader-v1,witness-v1"`. That
/// name would not match the `ArbiterClaim` next to it on the same report.
/// The stamp below reads `"witness-v1"` instead.
#[tokio::test]
async fn a_composed_stamp_names_the_arbiter_not_the_whole_composition() {
    let input = RoundInput {
        goal: "make the flaky test deterministic".into(),
        signals: signals(),
        candidate: Some(granted("/tmp/workspace")),
    };
    let report = composed_dispatch("flip-if-granted")
        .run(input, &mut Host)
        .await
        .expect("a validated composition resolves");

    let stamp = &report.snapshot.stamps[0];
    assert_eq!(
        stamp.author, "witness-v1",
        "the stamp names the arbiter whose rule decided the round, not the whole composition"
    );

    let arbiter_claim = report
        .arbitration
        .rows
        .iter()
        .find(|row| row.assessment == StampAssessment::Done)
        .expect("the arbiter's own claim is in the fold");
    assert_eq!(
        stamp.author, arbiter_claim.author,
        "the stamp and the claim beside it on one report must name the same decider"
    );
}
