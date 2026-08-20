//! `plugins/stella-witness` — the open verification plugin, graded against the
//! real `stella_plugin::wire` types (#4029 P3.2,
//! `doc:plugin-completion-plan` §4.1).
//!
//! # Why this plugin has to be checkable, specifically
//!
//! `AGENTS.md`'s first paragraph defines "verified done, not claimed done" as
//! *a property of the path that produced the evidence*. Since #3865 the only
//! such path is an installed verification plugin. If the only one of those
//! were private, the open project's headline claim would be unfalsifiable by
//! anyone who had not bought something — so `stella-witness` ships open, and
//! this file is the harness that makes "anyone can check it" true rather than
//! asserted.
//!
//! # What it grades
//!
//! The same shape as the three sibling conformance harnesses: every vector
//! goes through the host's own [`SubprocessWrapper`], and what comes back is
//! decoded by the host's own types and compared to a committed golden.
//! Refusal vectors are spawned directly, because a typed host cannot send a
//! request with a bad version or an unknown field.
//!
//! `test-duration-ms` is normalised out of the comparison — it is wall clock,
//! and asserting on it would make this fail on a slow runner rather than on a
//! real break. Every other measurement is compared exactly.
//!
//! # The property that matters most
//!
//! `a_pass_with_no_prior_failure_proves_nothing` is
//! `flip_requires_a_prior_failing_observation` — the property
//! `doc:pipeline-as-plugins` §8 names as the first thing to carry over from
//! the deleted built-in path, ported here and driven through the real plugin
//! process rather than against a Rust copy of its logic.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason (#3497): the plugin is spawned
//! as `python3`, and its vectors name POSIX programs.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use stella_plugin::{
    FlipObservation, FlipPolicy, HostCall, Participation, PluginManifest, WrapperPoint,
    WrapperRequest, WrapperResponse,
};
use stella_runtime::wrapper::{SubprocessWrapper, TurnWrapper};

fn plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/stella-witness")
        .canonicalize()
        .expect("the first-party plugin ships in this repository at plugins/stella-witness")
}

fn manifest() -> PluginManifest {
    let text = fs::read_to_string(plugin_dir().join("plugin.toml"))
        .expect("the manifest file is `plugin.toml`, exactly");
    PluginManifest::from_toml_str(&text).expect("the shipped manifest loads")
}

/// The program and arguments the host would spawn, with `${plugin_dir}`
/// interpolated the way the loader does it.
fn argv(manifest: &PluginManifest) -> Vec<String> {
    let dir = plugin_dir().display().to_string();
    manifest
        .runtime
        .as_ref()
        .expect("a plugin the host spawns declares [runtime]")
        .argv
        .iter()
        .map(|arg| arg.replace("${plugin_dir}", &dir))
        .collect()
}

/// Exactly the environment the manifest's allowlist admits — default-deny.
fn child_env(manifest: &PluginManifest) -> Vec<(String, String)> {
    manifest
        .runtime
        .as_ref()
        .expect("[runtime]")
        .child_env(|name| std::env::var(name).ok())
}

fn transport(manifest: &PluginManifest) -> SubprocessWrapper {
    let timeout = Duration::from_secs(manifest.runtime.as_ref().expect("[runtime]").timeout_secs);
    SubprocessWrapper::declare(argv(manifest), child_env(manifest), timeout)
        .expect("the manifest declares a program and a budget")
        .wrapper
}

fn vectors() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(plugin_dir().join("testdata"))
        .expect("the plugin ships its vectors beside it")
        .map(|entry| entry.expect("a readable vector").path())
        .filter(|path| path.to_string_lossy().ends_with(".request.json"))
        .collect();
    found.sort();
    assert!(!found.is_empty(), "the vector directory must not be empty");
    found
}

fn sibling(request: &Path, suffix: &str) -> PathBuf {
    let name = request
        .file_name()
        .expect("a vector has a file name")
        .to_string_lossy()
        .replace(".request.json", suffix);
    request.with_file_name(name)
}

/// **The witness.** Every response vector goes through the host's own
/// transport and comes back equal to its golden, decoded by the host's own
/// types.
#[tokio::test]
async fn every_response_vector_answers_with_its_golden_evidence() {
    let manifest = manifest();
    let wrapper = transport(&manifest);

    let mut graded = 0;
    for vector in vectors() {
        let golden_path = sibling(&vector, ".expected.json");
        if !golden_path.exists() {
            continue;
        }
        let name = vector
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .to_string();

        let request: WrapperRequest =
            serde_json::from_str(&fs::read_to_string(&vector).expect("a readable vector"))
                .unwrap_or_else(|e| panic!("{name} is not a request: {e}"));
        let WrapperRequest::AfterTurn(request) = request else {
            panic!("{name}: this plugin answers only after_turn");
        };

        let response = wrapper
            .after_turn(request)
            .await
            .unwrap_or_else(|e| panic!("{name}: the plugin did not answer: {e}"));

        let golden: WrapperResponse =
            serde_json::from_str(&fs::read_to_string(&golden_path).expect("a readable golden"))
                .unwrap_or_else(|e| panic!("{name}'s golden is not a response: {e}"));
        let WrapperResponse::AfterTurn(golden) = golden else {
            panic!("{name}'s golden must be an after_turn response");
        };

        let mut observed = response.evidence;
        let mut expected = golden.evidence;
        // Wall clock, not a claim about the work.
        observed.measurements.remove("test-duration-ms");
        expected.measurements.remove("test-duration-ms");

        assert_eq!(observed.flip, expected.flip, "{name}: the flip changed");
        assert_eq!(
            observed.measurements, expected.measurements,
            "{name}: the measurements changed"
        );
        graded += 1;
    }
    assert!(graded >= 5, "only {graded} response vectors ran");
}

/// **The property (`doc:pipeline-as-plugins` §8's first ask).** A pass with no
/// prior failing observation of the same command proves nothing.
///
/// This is `flip_requires_a_prior_failing_observation`, carried over from the
/// deleted `crates/stella-pipeline/src/verify.rs` and driven through the real
/// plugin process rather than against a Rust copy of its logic — which is the
/// difference between testing the port and testing a second implementation of
/// the thing that was ported.
///
/// It is the invariant the whole design rests on: `Flipped` is reachable only
/// by passing through `Failing` for the same normalized command. A plugin that
/// credited a green test whose red was never observed would turn "verified" into
/// "the tests pass", which is what every honest verification story has to
/// refuse.
#[tokio::test]
async fn a_pass_with_no_prior_failure_proves_nothing() {
    let manifest = manifest();
    let wrapper = transport(&manifest);

    // The three baselines that are not red. Each pairs with a test that
    // passes, so the *only* thing standing between this and a credited flip is
    // the invariant.
    for baseline in ["not-run", "passed", "unobserved"] {
        let request: WrapperRequest = serde_json::from_str(&format!(
            r#"{{"point":"after_turn","body":{{"protocol_version":1,"wrapper":"witness-v1",
               "round":0,"goal":"g",
               "candidate":{{"handle":"c1","root":"/tmp",
                 "test":{{"program":"test","args":["1","=","1"],"baseline":"{baseline}"}}}},
               "turn":{{"completed":true,"answer":"done"}}}}}}"#
        ))
        .expect("a well-formed request");
        let WrapperRequest::AfterTurn(request) = request else {
            unreachable!("built as after_turn")
        };

        let response = wrapper
            .after_turn(request)
            .await
            .expect("the plugin answers");
        assert_eq!(
            response.evidence.flip,
            FlipObservation::Unobservable,
            "baseline `{baseline}` is not red, so a passing run has no failure to flip from — \
             the oracle must never have locked onto the command at all"
        );
    }

    // And the control, so the assertion above is not passing because the
    // plugin reports `unobservable` for everything: the same command, the same
    // pass, with a red baseline, IS a flip.
    let request: WrapperRequest = serde_json::from_str(
        r#"{"point":"after_turn","body":{"protocol_version":1,"wrapper":"witness-v1",
           "round":0,"goal":"g",
           "candidate":{"handle":"c1","root":"/tmp",
             "test":{"program":"test","args":["1","=","1"],"baseline":"failed"}},
           "turn":{"completed":true,"answer":"done"}}}"#,
    )
    .expect("a well-formed request");
    let WrapperRequest::AfterTurn(request) = request else {
        unreachable!("built as after_turn")
    };
    let response = wrapper
        .after_turn(request)
        .await
        .expect("the plugin answers");
    assert_eq!(
        response.evidence.flip,
        FlipObservation::Achieved,
        "a red baseline and a green run is the flip — if this fails the test above proves nothing"
    );
}

/// A still-red test is `not-achieved`, which is a different fact from
/// `unobservable` and must stay one: the first says the oracle tracked a
/// command and it did not flip, the second says it never had one to track.
#[tokio::test]
async fn a_test_that_is_still_red_is_not_achieved_rather_than_unobservable() {
    let manifest = manifest();
    let wrapper = transport(&manifest);
    let request: WrapperRequest = serde_json::from_str(
        r#"{"point":"after_turn","body":{"protocol_version":1,"wrapper":"witness-v1",
           "round":0,"goal":"g",
           "candidate":{"handle":"c1","root":"/tmp",
             "test":{"program":"false","args":[],"baseline":"failed"}},
           "turn":{"completed":true,"answer":"could not fix it"}}}"#,
    )
    .expect("a well-formed request");
    let WrapperRequest::AfterTurn(request) = request else {
        unreachable!("built as after_turn")
    };
    let response = wrapper
        .after_turn(request)
        .await
        .expect("the plugin answers");
    assert_eq!(response.evidence.flip, FlipObservation::NotAchieved);
    assert_eq!(
        response.evidence.measurements.get("test-command-exit-code"),
        Some(&1),
        "the exit status is reported, not inferred"
    );
}

/// The other half of the contract: `AfterTurnResponse` has no error variant,
/// so a plugin that cannot answer **fails** — non-zero exit, one line on
/// stderr, nothing on stdout — and the host reads the silence as
/// `EvidenceSet::unobserved`.
///
/// Spawned directly rather than through [`SubprocessWrapper`], because the
/// host's typed transport cannot express any of these requests.
#[test]
fn every_refusal_vector_refuses_with_the_reason_it_names() {
    let manifest = manifest();
    let argv = argv(&manifest);

    let mut graded = 0;
    for vector in vectors() {
        let refusal_path = sibling(&vector, ".refusal.txt");
        if !refusal_path.exists() {
            continue;
        }
        let name = vector
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .to_string();
        let (program, args) = argv.split_first().expect("a program");
        let mut child = Command::new(program)
            .args(args)
            .env_clear()
            .envs(child_env(&manifest))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{name}: the plugin did not start: {e}"));
        child
            .stdin
            .take()
            .expect("a stdin pipe")
            .write_all(
                fs::read_to_string(&vector)
                    .expect("a readable vector")
                    .as_bytes(),
            )
            .expect("the request is written");
        let output = child.wait_with_output().expect("the plugin exits");

        assert!(
            !output.status.success(),
            "{name}: a refusal must exit non-zero, got {:?}",
            output.status.code()
        );
        assert!(
            output.stdout.is_empty(),
            "{name}: a refusing plugin must print nothing on stdout, got {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            fs::read_to_string(&refusal_path)
                .expect("a readable refusal")
                .trim(),
            "{name}: the refusal reason changed"
        );
        graded += 1;
    }
    assert!(graded >= 3, "only {graded} refusal vectors ran");
}

/// A vector carries a golden **or** a refusal, never both and never neither.
#[test]
fn every_vector_is_graded_by_exactly_one_sibling() {
    for vector in vectors() {
        let golden = sibling(&vector, ".expected.json").exists();
        let refusal = sibling(&vector, ".refusal.txt").exists();
        assert!(
            golden ^ refusal,
            "{} must carry exactly one of .expected.json / .refusal.txt",
            vector.display()
        );
    }
}

/// What the shipped manifest declares, asserted rather than described.
///
/// The restraint is the interesting part and is easy to lose in a later edit:
/// this plugin asks for **no host calls at all**. Everything it needs — the
/// invocation, its arguments, and the red half of the flip — arrives in the
/// grant, so a `[loop] calls` entry here would be a capability nothing spends.
#[test]
fn the_shipped_manifest_declares_exactly_what_this_plugin_does() {
    let manifest = manifest();

    assert_eq!(manifest.name, "stella-witness");
    assert_eq!(
        manifest.loop_grant.participation,
        Participation::Arbiter,
        "only an arbiter may hold a turn open until the flip happens"
    );
    assert_eq!(
        manifest.loop_grant.points,
        vec![WrapperPoint::AfterTurn],
        "the flip needs the work to have happened, and the red half is already pinned in the \
         grant — so there is nothing to do before a turn runs, and nothing is declared"
    );
    assert!(
        manifest.loop_grant.calls.is_empty(),
        "this plugin asks the host for nothing: got {:?}",
        manifest.loop_grant.calls
    );
    assert!(
        !manifest.loop_grant.calls.contains(&HostCall::ChildTurn),
        "and specifically spends no model call — there is no arm in this plugin that could"
    );

    let oracle = manifest.oracle.as_ref().expect("[oracle]");
    assert_eq!(
        oracle.flip,
        FlipPolicy::Required,
        "the flip is what decides `witness-flips`"
    );
    assert!(
        oracle.command.is_none(),
        "with [runtime] declared, the oracle is this plugin's own process (#3501)"
    );

    let requirements = manifest.requirements.as_ref().expect("[requirements]");
    assert!(requirements.contains_key("witness-flips"));
    assert!(requirements.contains_key("witness-stable"));
    assert_eq!(
        requirements.len(),
        2,
        "this plugin declares only what it decides — #867's same-failure rule is NOT declared, \
         because its fingerprint guard is not ported and a requirement it could only ever \
         report as satisfied would be a vacuous check reading as a guarantee"
    );

    // Every measurement a check reads must be declared; a check may read
    // nothing else.
    for check in &oracle.checks {
        assert!(
            requirements.contains_key(&check.requirement),
            "a check decides a requirement that exists: {}",
            check.requirement
        );
    }
}
