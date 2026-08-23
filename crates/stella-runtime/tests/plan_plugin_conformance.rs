//! The witness for Track B's second extraction: `plugins/stella-plan`
//! answers a real `before_turn` request over the wire, and what comes back is
//! well-formed against the real `stella_plugin::wire` types.
//!
//! Failing before this landed for the plainest possible reason: the plugin
//! did not exist. `doc:pipeline-as-plugins` §7 puts `stella-plan` second, and
//! #3562 recorded why it could not start before `child_turn` reached a real
//! host — that gap closed on the `stella run` door
//! (`crates/stella-cli/src/wrapper_plugin.rs::bind_installed`).
//!
//! # What this file grades, and what it deliberately does not
//!
//! `plugins/stella-plan` contributes at exactly one stage (`plan`), and doing
//! so always spends its one `child_turn` call — there is no "plan" vector
//! that produces a golden without a scripted host-call conversation behind
//! it. So this file's vectors are every OTHER shape: non-`plan` stages (which
//! contribute nothing and need no conversation) and every malformed or
//! unsupported request this plugin must refuse outright. The `plan` stage's
//! real behaviour — the `child_turn` conversation and its degradations — is
//! `plan_plugin_hostcall.rs`'s witness, kept separate for the same reason
//! `research_plugin_recall.rs` is: a host call is not a one-request-one-response
//! exchange, so it is driven line-by-line there rather than through
//! [`SubprocessWrapper`], which is exactly right for the shape this file
//! grades.
//!
//! Response vectors go through [`SubprocessWrapper`] — the same code
//! `stella-cli` dispatches with — built from the plugin's own `plugin.toml`,
//! with `${plugin_dir}` interpolated exactly as
//! `stella_cli::plugin_cmd::roster` does it. Refusal vectors are spawned
//! directly: a typed host cannot send an unknown field or a malformed body,
//! which is the point of grading them.
//!
//! `cfg(unix)` for the reason `wrapper_socket.rs` states and tracked in the
//! same place (#3497): the child is spawned with a POSIX `PATH` and named
//! `python3`.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use stella_plugin::{
    HostStage, Participation, PluginManifest, StageName, WrapperPoint, WrapperRequest,
    WrapperResponse,
};
use stella_protocol::completion::MessageRole;
use stella_runtime::wrapper::{SubprocessWrapper, TurnWrapper};

fn plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/stella-plan")
        .canonicalize()
        .expect("the first-party plugin ships in this repository at plugins/stella-plan")
}

fn manifest() -> PluginManifest {
    let text = fs::read_to_string(plugin_dir().join("plugin.toml"))
        .expect("the manifest file is `plugin.toml`, exactly");
    PluginManifest::from_toml_str(&text).expect("the shipped manifest loads")
}

/// The program and arguments the host would spawn, with `${plugin_dir}`
/// interpolated the way the loader does it
/// (`stella_cli::plugin_cmd::roster`) — the plugin's own declaration, never a
/// path this test knows separately.
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

/// Exactly the environment the manifest's allowlist admits — default-deny, so
/// a plugin that quietly read an inherited variable would pass on a
/// developer's machine and fail on a host that withheld it.
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
async fn every_response_vector_answers_with_its_golden_contribution() {
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
        let text = fs::read_to_string(&vector).expect("a readable vector");

        // Decoded before it is sent: a vector that is not a well-formed
        // request is a bug in the fixture, and finding that out here rather
        // than from the plugin's refusal is the difference between grading
        // the plugin and grading the vector.
        let request: WrapperRequest =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} is not a request: {e}"));
        let WrapperRequest::BeforeTurn(request) = request else {
            panic!("{name} addresses a point this plugin does not declare");
        };

        let response = wrapper
            .before_turn(request)
            .await
            .unwrap_or_else(|e| panic!("{name}: the plugin did not answer: {e}"));

        let golden: WrapperResponse =
            serde_json::from_str(&fs::read_to_string(&golden_path).expect("a readable golden"))
                .unwrap_or_else(|e| panic!("{name}'s golden is not a response: {e}"));
        assert_eq!(
            WrapperResponse::BeforeTurn(response.clone()),
            golden,
            "{name} did not answer with its golden contribution"
        );

        assert!(
            response.role.is_none() && response.publish.is_empty(),
            "{name}: stella-plan names no role intent and publishes no signal — \
             `StageName::Host(HostStage::Plan).publishes()` is empty in the host, so there is \
             none it could honestly publish"
        );
        assert!(
            response.scope.is_empty(),
            "{name}: no candidate grant, nothing structural to scope — see \
             main.py's module doc for why zero scope entries is a decision"
        );
        // Invariant 7, checked on the value rather than trusted.
        for context in response.context {
            assert_eq!(context.into_message().role, MessageRole::User, "{name}");
        }
        graded += 1;
    }
    assert!(graded >= 3, "only {graded} response vectors ran");
}

/// The other half of the contract: `BeforeTurnResponse` has no error variant,
/// so a plugin that cannot answer **fails** — non-zero exit, one line on
/// stderr, nothing on stdout — and the host runs the turn without the
/// contribution.
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
            "{name}: a refusal must exit non-zero, got {:?} with stdout {:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout)
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
    assert!(graded >= 6, "only {graded} refusal vectors ran");
}

/// A vector carries a golden **or** a refusal, never both and never neither —
/// the hygiene rule that stops a vector from silently grading nothing when a
/// rename drops its sibling.
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

/// What the shipped manifest declares, asserted against the extraction's own
/// bar: `before_turn` **only**, no arbiter powers, one host call
/// (`child_turn`) and one role intent (`planner`, resolving to the `plan`
/// tier) — this plugin participates in a turn, it does not decide anything
/// about it.
#[test]
fn the_shipped_manifest_declares_before_turn_and_one_bounded_child_turn() {
    let manifest = manifest();
    assert_eq!(manifest.name, "stella-plan");
    assert_eq!(manifest.loop_grant.participation, Participation::Steering);
    assert_eq!(manifest.loop_grant.points, vec![WrapperPoint::BeforeTurn]);
    assert!(
        manifest.loop_grant.hooks.is_empty(),
        "stella-plan decides nothing, so it binds no gate"
    );
    assert!(manifest.loop_grant.max_holds.is_none());
    assert!(manifest.requirements.is_none() && manifest.oracle.is_none());
    assert_eq!(
        manifest.loop_grant.calls,
        vec![stella_plugin::HostCall::ChildTurn],
        "a planning stage asks for one bounded model call and nothing else"
    );
    assert_eq!(
        manifest.loop_grant.max_calls,
        Some(1),
        "one child turn per point is the ask this plugin actually makes"
    );
    let roles = manifest
        .roles
        .as_ref()
        .expect("child_turn needs a declared role intent to name");
    assert_eq!(roles.len(), 1, "exactly one role intent: planner");
    assert_eq!(
        roles.get("planner").map(|role| role.tier.as_str()),
        Some("plan"),
        "the planner role intent resolves to the built-in stage's own responsibility"
    );
    assert!(
        manifest.subloop.is_none(),
        "this plugin spends its child turn entirely over the host-call channel, so it \
         declares no `[subloop]` — since #3496 the `[wrapper]` below is what resolves the \
         role intent above, and the `[subloop]` that used to sit here only to satisfy the \
         validator is gone"
    );

    let wrapper = manifest.wrapper.as_ref().expect(
        "a dispatchable wrapper declares its stage order — `WrapperDispatch::bind` \
                 refuses a manifest without one",
    );
    assert_eq!(
        wrapper.id, "plan-v1",
        "the variant id is the join key of any comparison against `classic`"
    );
    let plan_stage = wrapper
        .stages
        .iter()
        .find(|stage| stage.name == StageName::Host(HostStage::Plan))
        .expect("the stage this plugin exists for");
    assert_eq!(
        plan_stage.condition.as_deref(),
        Some("plans"),
        "the built-in stage only runs when triage's Signal::Plans is true, \
         and this is that boolean read exactly"
    );
    assert_eq!(
        manifest.runtime.as_ref().expect("[runtime]").env,
        vec!["PATH".to_string()],
        "every input this plugin needs arrives in the request or the host-call \
         answer, so PATH is the whole allowlist"
    );
    assert!(
        manifest.capabilities.is_empty(),
        "this plugin opens no file and searches no workspace"
    );
}
