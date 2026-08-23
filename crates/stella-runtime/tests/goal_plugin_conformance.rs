//! The first witness for `plugins/stella-goal`, the goal-supervision
//! reference plugin (#3695 follow-on; `doc:pipeline-as-plugins` §3;
//! `doc:turn-loop-wrappers` §9.2): it answers a real `before_turn` request
//! over the wire, and what comes back is well-formed against the real
//! `stella_plugin::wire` types.
//!
//! # What this file grades, and what it deliberately does not
//!
//! `plugins/stella-goal` contributes at `before_turn` only when
//! `stage == "execute"`, and doing so needs no host call — unlike
//! `plugins/stella-plan`'s `plan` stage, the goal framing is a fixed,
//! byte-mirrored message with no conversation behind it. So this file's
//! vectors cover the whole `before_turn` half of the plugin honestly: the
//! `execute` stage's real contribution, every other declared stage's empty
//! one, and every malformed or unsupported request this plugin must refuse
//! outright. `after_turn` **always** spends its one `child_turn` call, so
//! there is no `after_turn` golden that produces a contribution without a
//! scripted host-call conversation behind it — that half is
//! `goal_plugin_hostcall.rs`'s witness, kept separate for the same reason
//! `plan_plugin_hostcall.rs` is: a host call is not a one-request-one-response
//! exchange, so it is driven line-by-line there rather than through
//! [`SubprocessWrapper`], which is exactly right for the shape this file
//! grades. This file's `after_turn` vectors are refusals only — a malformed
//! `after_turn` request is refused before any host call is ever attempted.
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

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use stella_plugin::{
    FlipPolicy, HookEvent, HostCall, HostStage, Participation, PluginManifest, StageName,
    WrapperPoint, WrapperRequest, WrapperResponse,
};
use stella_protocol::completion::MessageRole;
use stella_runtime::wrapper::{SubprocessWrapper, TurnWrapper};

fn plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/stella-goal")
        .canonicalize()
        .expect("the first-party plugin ships in this repository at plugins/stella-goal")
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
        .map(|arg| stella_plugin::expand_plugin_dir(arg, Path::new(&dir)))
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
/// types. Every one of today's goldens is a `before_turn` vector — see the
/// module doc for why `after_turn` never produces a golden here.
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

        let request: WrapperRequest =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} is not a request: {e}"));
        let WrapperRequest::BeforeTurn(request) = request else {
            panic!(
                "{name}: today's goldens are all before_turn — an after_turn golden needs a \
                 scripted host-call conversation and belongs in goal_plugin_hostcall.rs instead"
            );
        };

        let response = wrapper
            .before_turn(request)
            .await
            .unwrap_or_else(|e| panic!("{name}: the plugin did not answer: {e}"));

        common::bless_or_assert(
            &name,
            &golden_path,
            &WrapperResponse::BeforeTurn(response.clone()),
            // Nothing in a before_turn contribution is wall clock.
            |_| {},
        );

        assert!(
            response.role.is_none() && response.publish.is_empty(),
            "{name}: stella-goal names no role intent on the WORKING turn and publishes no \
             signal — its own role intent (`verifier`) is spent over the host-call channel, \
             never on BeforeTurnResponse::role"
        );
        assert!(
            response.scope.is_empty(),
            "{name}: no candidate grant, nothing structural to scope"
        );
        // Invariant 7, checked on the value rather than trusted.
        for context in response.context {
            assert_eq!(context.into_message().role, MessageRole::User, "{name}");
        }
        graded += 1;
    }
    assert!(graded >= 3, "only {graded} response vectors ran");
}

/// The other half of the contract: neither point's response has an error
/// variant, so a plugin that cannot answer **fails** — non-zero exit, one
/// line on stderr, nothing on stdout — and the host proceeds without it.
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

/// What the shipped manifest declares, asserted against the plugin's own
/// bar: both points, the strongest grant (`arbiter`), one declared
/// requirement decided entirely by a non-flip oracle check, one host call
/// (`child_turn`) and one role intent (`verifier`) — this plugin holds a
/// completion open on the evidence it gathers, and gathers nothing else.
#[test]
fn the_shipped_manifest_declares_both_points_and_the_arbiter_grant_they_need() {
    let manifest = manifest();
    assert_eq!(manifest.name, "stella-goal");
    assert_eq!(manifest.loop_grant.participation, Participation::Arbiter);
    assert_eq!(
        manifest.loop_grant.points,
        vec![WrapperPoint::BeforeTurn, WrapperPoint::AfterTurn]
    );
    assert_eq!(
        manifest.loop_grant.hooks,
        vec![HookEvent::Stop],
        "arbiter must declare the Stop gate (ManifestError::ArbiterMustDeclareStop)"
    );
    assert_eq!(
        manifest.loop_grant.max_holds,
        Some(7),
        "the honest mirror of GoalConfig::default().max_rounds - 1"
    );
    assert_eq!(manifest.loop_grant.calls, vec![HostCall::ChildTurn]);
    assert_eq!(
        manifest.loop_grant.max_calls,
        Some(1),
        "main.py asks the host for exactly one thing, once, every after_turn"
    );
    assert_eq!(
        manifest.loop_grant.max_child_turns,
        Some(8),
        "ChildTurns never resets between rounds, so an arbiter asking once in each of \
         max_holds + 1 rounds needs that many for the whole run (#3839)"
    );

    let requirements = manifest
        .requirements
        .as_ref()
        .expect("arbiter must declare its definition of done");
    assert_eq!(requirements.len(), 1);
    assert!(requirements.contains_key("goal-met"));

    let oracle = manifest
        .oracle
        .as_ref()
        .expect("arbiter must declare an oracle");
    assert_eq!(
        oracle.flip,
        FlipPolicy::NotApplicable,
        "a verifier's judgment is not a fail\u{2192}pass transition"
    );
    assert_eq!(oracle.measurements, vec!["met".to_string()]);
    assert_eq!(oracle.checks.len(), 1);
    assert_eq!(oracle.checks[0].requirement, "goal-met");

    let roles = manifest
        .roles
        .as_ref()
        .expect("child_turn needs a declared role intent to name");
    assert_eq!(roles.len(), 1, "exactly one role intent: verifier");
    assert_eq!(
        roles.get("verifier").map(|role| role.tier.as_str()),
        Some("verifier")
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
        wrapper.id, "goal-v1",
        "the variant id is the join key of any comparison against `classic`"
    );
    assert_eq!(
        wrapper.stages.len(),
        8,
        "the full classic stage order, every entry unconditional"
    );
    for stage in &wrapper.stages {
        assert!(
            stage.condition.is_none(),
            "goal supervision assesses whatever turn ran, unconditionally: {:?}",
            stage.name
        );
    }
    assert!(
        wrapper
            .stages
            .iter()
            .any(|s| s.name == StageName::Host(HostStage::Execute))
    );

    assert_eq!(
        manifest.runtime.as_ref().expect("[runtime]").env,
        vec!["PATH".to_string()],
        "every input this plugin needs arrives in the request or the host-call answer"
    );
    assert!(
        manifest.capabilities.is_empty(),
        "this plugin opens no file and searches no workspace"
    );
}
