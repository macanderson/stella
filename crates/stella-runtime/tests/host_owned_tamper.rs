//! The witness for #3499: a plugin that declares the oracle contract can reach
//! a **decided** verdict.
//!
//! Before this, it could not, and the reason was a contract defect rather than
//! a bug in any plugin — Track C hit it identically in Rust, Python and
//! TypeScript (macanderson/stella-examples#1). `[oracle] tamper` was mandatory
//! with one value, tamper snapshotting is host-side by design
//! (`doc:pipeline-as-plugins` §4 A10), and yet the `tamper` field sat on the
//! evidence the *plugin* returns. So an honest plugin declared a policy it
//! could not execute, answered `not-checked` because that was the only true
//! thing it could say, and `judge` — rightly refusing to credit a flip whose
//! artifacts nobody compared — abstained. Every run. Forever.
//!
//! Every test below fails on the old code for a mechanical reason: the plugin
//! here answers `{"flip":"achieved"}` with no tamper key, which the old
//! `EvidenceSet` refused as a missing field, and `EvidenceSet::from_observed`
//! did not exist for the host to merge its own finding through.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason and tracked in the same place:
//! the plugin is a `/bin/sh` script, so this file proves nothing on Windows
//! until #3497 replaces it with a portable in-tree plugin.

#![cfg(unix)]

use stella_plugin::{
    AfterTurnRequest, EvidenceSet, FlipObservation, ObservedEvidence, PROTOCOL_VERSION,
    PluginManifest, TamperFinding, TurnOutcome, UnmetBecause, Verdict, VerdictRule,
};
use stella_runtime::wrapper::{
    DEFAULT_WRAPPER_TIMEOUT, SubprocessWrapper, TurnWrapper, WrapperError, judge,
};

/// An arbiter whose definition of done is the witness contract itself: a
/// fail→pass flip the host credits only against artifacts it snapshotted.
///
/// It declares no `tamper` line, which is the second half of #3499 — the
/// policy names what the *host* does, and there is one thing a host does, so
/// restating it said nothing. It also declares no `[oracle] command`: the
/// oracle is this plugin's own process (#3501), named once.
const MANIFEST: &str = r#"
name = "witness-arbiter"
description = "holds the turn open until the witness flips red to green"

[loop]
participation = "arbiter"
hooks = ["Stop"]
points = ["after_turn"]
max_holds = 2

[requirements]
proven = "a witness test failed before the change and passes after it"

[oracle]
flip = "required"

[runtime]
argv = ["/bin/sh", "-c", "true"]
timeout_secs = 60
"#;

/// The plugin: it reports the flip it watched, and nothing about a snapshot it
/// never took.
const PLUGIN: &str = r#"
cat >/dev/null
printf '%s\n' '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"achieved"}}}'
"#;

fn manifest() -> PluginManifest {
    PluginManifest::from_toml_str(MANIFEST).expect("the manifest loads")
}

fn plugin(script: &str) -> SubprocessWrapper {
    SubprocessWrapper::new(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        Vec::new(),
        DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget")
}

fn after() -> AfterTurnRequest {
    AfterTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "witness-arbiter".into(),
        round: 0,
        goal: "make the flaky test deterministic".into(),
        candidate: None,
        turn: TurnOutcome {
            completed: true,
            answer: "replaced the sleep with a barrier".into(),
            tools: vec!["edit_file".into()],
            changed_files: vec!["crates/stella-core/src/driver.rs".into()],
        },
    }
}

/// **The witness.** Flip achieved, host's snapshot clean: the verdict is
/// `Met` — decided, on a run where the work was really done.
#[tokio::test]
async fn a_flip_the_host_vouches_for_reaches_a_decided_verdict() {
    let observed = plugin(PLUGIN)
        .after_turn(after())
        .await
        .expect("the plugin answers after_turn")
        .evidence;
    assert_eq!(
        observed,
        ObservedEvidence {
            flip: FlipObservation::Achieved,
            measurements: std::collections::BTreeMap::new(),
        },
        "the plugin reports the flip it watched and nothing it did not"
    );

    // The host's own snapshot comparison — the half a plugin never had and
    // could never have.
    let evidence = EvidenceSet::from_observed(observed, TamperFinding::Clean);

    assert_eq!(
        judge(&VerdictRule::from_manifest(&manifest()), &evidence),
        Verdict::Met,
        "an achieved flip over artifacts the host vouches for is done"
    );
}

/// The exclusion still bites, and it is now the *host's* finding that bites —
/// which is the point. A worker that rewrote the witness does not win the flip.
#[tokio::test]
async fn the_hosts_finding_is_what_refuses_a_tampered_flip() {
    let observed = plugin(PLUGIN)
        .after_turn(after())
        .await
        .expect("the plugin answers after_turn")
        .evidence;

    let tampered = EvidenceSet::from_observed(
        observed.clone(),
        TamperFinding::Tampered {
            artifact: "tests/witness.rs".into(),
        },
    );
    let rule = VerdictRule::from_manifest(&manifest());
    match judge(&rule, &tampered) {
        Verdict::Unmet { unmet } => assert!(
            matches!(&unmet[..], [clause] if matches!(
                &clause.because,
                UnmetBecause::Tampered { artifact } if artifact == "tests/witness.rs"
            )),
            "got {unmet:?}"
        ),
        other => panic!("a tampered witness must be refused, got {other:?}"),
    }

    // And a host that took no snapshot still abstains — the answer that used to
    // be the *only* reachable one is now the honest report of a host that did
    // not check, rather than a plugin's forced confession.
    assert!(matches!(
        judge(
            &rule,
            &EvidenceSet::from_observed(observed, TamperFinding::NotChecked)
        ),
        Verdict::Undecided { .. }
    ));
}

/// The structural half, end to end: `tamper` is not a word a plugin can say on
/// the wire. A plugin claiming the host's finding is refused by name, exactly
/// as an unknown manifest key is.
#[tokio::test]
async fn a_plugin_claiming_the_hosts_tamper_finding_is_refused() {
    const LIAR: &str = r#"
cat >/dev/null
printf '%s\n' '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"achieved","tamper":"clean"}}}'
"#;
    let error = plugin(LIAR)
        .after_turn(after())
        .await
        .expect_err("a plugin cannot vouch for the host's own check");
    assert!(
        matches!(&error, WrapperError::Decode { .. }) && error.to_string().contains("tamper"),
        "the refusal must name the field, got {error:?}"
    );
}
