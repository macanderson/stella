// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **The witness for `run_test`** (#3580): a plugin asks the host to run the
//! candidate's own tests again, and the host does it.
//!
//! Every test here fails before the change for one reason: **no host performed
//! the capability.** `run_test` crossed the wire, was gated exactly like
//! `recall` and `child_turn`, and then met the last `Unsupported` arm in the
//! only shipped `HostCapabilities` implementation. So a verification plugin
//! could observe the *first* run — the plan is in the grant, which is where
//! #3498 put it — and could not ask for a second one against the same opaque
//! handle, which is the whole reason `RunTestArgs` carries a `CandidateHandle`
//! and nothing else.
//!
//! The plugins below are `sh` scripts with no JSON library, for
//! `wrapper_child_turn.rs`'s reason: a capability only Rust can reach is a Rust
//! API with extra steps (`doc:pipeline-as-plugins` §5 commitment 2).
//!
//! `cfg(unix)` is the same declared gap the rest of this suite carries, tracked
//! in #3497.

#![cfg(unix)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use stella_plugin::{
    AfterTurnRequest, HostCallRefusal, PROTOCOL_VERSION, PluginManifest, TestBaseline, TurnOutcome,
};
use stella_protocol::candidate::CandidateHandle;
use stella_runtime::wrapper::{
    DEFAULT_HOST_MAX_CALLS, HostCallGate, HostPlanes, SubprocessWrapper, TestObservation,
    TestRunDenial, TestRunHost, TestRuns, TurnWrapper,
};

/// What a human consents to at install: this plugin answers `after_turn` and
/// may ask for `run_test`. No `[roles]`, and that is the point — re-running a
/// test spends CPU, not the user's model budget, so this capability needs no
/// seat to resolve and no dollar carve.
const VERIFYING_MANIFEST: &str = r#"
name = "re-runs-the-witness"
description = "asks the host to run the candidate's tests again"

[loop]
participation = "steering"
points = ["after_turn"]
calls = ["run_test"]

[wrapper]
id = "rerun-v1"

[[wrapper.stages]]
name = "verify"
"#;

/// The host's side: one workspace, and a record of every handle it was asked
/// about — which is how these tests answer the question that matters, **who
/// ran the tests?** A plugin that had shelled out itself would leave nothing
/// here.
struct Workspaces {
    holds: &'static str,
    answer: Result<TestObservation, TestRunDenial>,
    asked: Mutex<Vec<String>>,
}

impl Workspaces {
    fn green() -> Arc<Self> {
        Arc::new(Self {
            holds: "candidate-1",
            answer: Ok(TestObservation {
                assertions: TestBaseline::Passed,
                output: "running 1 test\ntest tests::flip ... ok".to_string(),
            }),
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// The port implementation, over a handle the test still holds.
///
/// A newtype rather than an impl on `Arc<Workspaces>` directly: the orphan rule
/// forbids the latter from an integration test, and sharing the recording is
/// how a test inspects what the host was asked *after* handing the host over.
#[derive(Clone)]
struct Holder(Arc<Workspaces>);

#[async_trait]
impl TestRunHost for Holder {
    async fn run_test(
        &self,
        candidate: &CandidateHandle,
    ) -> Result<TestObservation, TestRunDenial> {
        self.0
            .asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(candidate.as_str().to_string());
        if candidate.as_str() == self.0.holds {
            self.0.answer.clone()
        } else {
            // The re-resolution that is the whole security shape: a handle
            // this host does not hold names no directory here.
            Err(TestRunDenial::UnknownCandidate)
        }
    }
}

fn manifest() -> PluginManifest {
    PluginManifest::from_toml_str(VERIFYING_MANIFEST).expect("the manifest loads")
}

/// The host a driver would assemble: this plugin's grant, the test-run plane
/// over the host's own workspaces, behind the gate the manifest declares.
fn host(manifest: &PluginManifest, workspaces: Arc<Workspaces>) -> Arc<HostCallGate> {
    let plane = Arc::new(TestRuns::declare(manifest, Holder(workspaces)));
    Arc::new(HostCallGate::declare(
        manifest.loop_grant.clone(),
        DEFAULT_HOST_MAX_CALLS,
        Box::new(HostPlanes::none().with_test_runs(plane)),
    ))
}

/// A host with the capability and no plane behind it — the shape every driver
/// that has not wired its workspaces has.
fn host_without_a_plane(manifest: &PluginManifest) -> Arc<HostCallGate> {
    Arc::new(HostCallGate::declare(
        manifest.loop_grant.clone(),
        DEFAULT_HOST_MAX_CALLS,
        Box::new(HostPlanes::none()),
    ))
}

fn plugin(script: &str, gate: Arc<HostCallGate>) -> SubprocessWrapper {
    SubprocessWrapper::declare(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        Vec::new(),
        Duration::from_secs(10),
    )
    .expect("the transport is declared with a program and a budget")
    .wrapper
    .serving(gate)
}

fn after() -> AfterTurnRequest {
    AfterTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "rerun-v1".into(),
        stage: None,
        round: 1,
        goal: "make the flaky test deterministic".into(),
        candidate: None,
        turn: TurnOutcome {
            completed: true,
            answer: "replaced the sleep with a barrier".into(),
            tools: Some(vec!["edit_file".into()]),
            changed_files: Some(vec!["src/lib.rs".into()]),
        },
    }
}

/// The plugin asks about the handle it was given and reports the flip from the
/// answer it reads. `flip=achieved` only if the host said the assertions
/// passed, so a fabricated report cannot produce it.
const ASKS_ABOUT: &str = r#"
read -r request
printf '%s\n' '{"call":"run_test","id":7,"args":{"candidate":"CANDIDATE"}}'
read -r answer
case "$answer" in
  *'"result":7'*) ;;
  *) printf 'the answer did not carry the id this plugin chose\n' >&2 ; exit 1 ;;
esac
case "$answer" in
  *'"assertions":"passed"'*) flip=achieved ;;
  *'"err"'*)                 flip=unobservable ;;
  *)                         flip=not-achieved ;;
esac
printf '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"%s"}}}\n' "$flip"
"#;

fn asks_about(handle: &str) -> String {
    ASKS_ABOUT.replace("CANDIDATE", handle)
}

/// **The witness.** The plugin asks, the host runs, and the plugin's evidence
/// is built from the answer it read.
///
/// Three claims, and the third is the one the capability exists for:
///
/// 1. the plugin got a real observation back through the wire;
/// 2. the **host** ran it — the workspace holder records the handle it was
///    asked about, and the plugin never named a directory;
/// 3. the run is on the host's ledger, so a user can be told what a plugin made
///    their machine do.
#[tokio::test]
async fn a_plugin_asks_the_host_to_re_run_the_candidates_tests_and_the_host_does() {
    let manifest = manifest();
    let workspaces = Workspaces::green();
    let gate = host(&manifest, Arc::clone(&workspaces));

    let evidence = plugin(&asks_about("candidate-1"), gate)
        .after_turn(after())
        .await
        .expect("the plugin answers the point")
        .evidence;

    assert_eq!(
        evidence.flip,
        stella_plugin::FlipObservation::Achieved,
        "the plugin read a passing re-run off the wire"
    );
    assert_eq!(
        workspaces.asked(),
        vec!["candidate-1".to_string()],
        "the host was asked about the handle, and the plugin named no path"
    );
}

/// **The code that sends a plugin author to the right place.** A handle from
/// another run is refused `unavailable`, never `unsupported`.
///
/// The two words are the difference between "this host has no workspace for
/// you" and "stop asking, this capability does not exist here", and only the
/// first is true once a plane is installed. Before the change every host said
/// the second, about every handle.
#[tokio::test]
async fn a_handle_from_another_run_is_refused_unavailable_not_unsupported() {
    let manifest = manifest();
    let workspaces = Workspaces::green();
    let gate = host(&manifest, Arc::clone(&workspaces));

    let evidence = plugin(&asks_about("candidate-from-another-run"), Arc::clone(&gate))
        .after_turn(after())
        .await
        .expect("a refused call is a value, not a death")
        .evidence;
    assert_eq!(
        evidence.flip,
        stella_plugin::FlipObservation::Unobservable,
        "the plugin degraded honestly rather than claiming a flip"
    );

    let refusals = gate.refusals();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].refusal, HostCallRefusal::Unavailable);
    assert_ne!(refusals[0].refusal, HostCallRefusal::Unsupported);
    assert_eq!(
        workspaces.asked(),
        vec!["candidate-from-another-run".to_string()],
        "the host re-resolved the handle rather than trusting it"
    );
}

/// A driver that has not wired its workspaces still answers `unavailable`, and
/// the plugin degrades. That the arm exists at all is the change: the same ask
/// used to be `unsupported` from every host including one that could serve it.
#[tokio::test]
async fn a_host_with_no_plane_is_unavailable_and_the_plugin_degrades() {
    let manifest = manifest();
    let gate = host_without_a_plane(&manifest);

    let evidence = plugin(&asks_about("candidate-1"), Arc::clone(&gate))
        .after_turn(after())
        .await
        .expect("a refused call is a value")
        .evidence;
    assert_eq!(evidence.flip, stella_plugin::FlipObservation::Unobservable);

    let refusals = gate.refusals();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].refusal, HostCallRefusal::Unavailable);
    assert!(
        refusals[0].detail.contains("no test-run plane"),
        "{}",
        refusals[0].detail
    );
}

/// The manifest is where "may it ask?" is answered, and the plugin's process is
/// never consulted: a plugin whose `[loop] calls` omits `run_test` is refused
/// by the gate before any plane is reached.
#[tokio::test]
async fn a_plugin_that_did_not_declare_the_call_never_reaches_the_plane() {
    let undeclared = PluginManifest::from_toml_str(
        "name = \"quiet\"\n\n[loop]\nparticipation = \"steering\"\npoints = \
         [\"after_turn\"]\ncalls = [\"recall\"]\n\n[wrapper]\nid = \
         \"quiet-v1\"\n\n[[wrapper.stages]]\nname = \"verify\"",
    )
    .expect("the manifest loads");
    let workspaces = Workspaces::green();
    let gate = host(&undeclared, Arc::clone(&workspaces));

    let evidence = plugin(&asks_about("candidate-1"), Arc::clone(&gate))
        .after_turn(after())
        .await
        .expect("a refused call is a value")
        .evidence;
    assert_eq!(evidence.flip, stella_plugin::FlipObservation::Unobservable);
    assert_eq!(gate.refusals()[0].refusal, HostCallRefusal::Undeclared);
    assert!(
        workspaces.asked().is_empty(),
        "an ungranted call must not reach the host's workspaces at all"
    );
}
