//! The second half of the witness: `plugins/stella-research` is what the host
//! sequence dispatches, not merely a program that speaks the wire.
//!
//! `research_plugin_conformance.rs` grades the plugin against the socket —
//! vectors in, goldens out. This file grades it against
//! [`WrapperDispatch`], the caller #3494 added: the declared stage program
//! resolves, `before_turn` runs at the stages it says run, the contribution
//! passes `admissible` and reaches the turn as user messages, and `judge`
//! and `again?` conclude a wrapper that decides nothing.
//!
//! **It depends on that caller**, which is why it is its own file: the wire
//! contract's witness must stand without a host sequence, and this one exists
//! precisely to prove the two fit together.
//!
//! Fails before the extraction for the same reason its sibling does — there was
//! no `plugins/stella-research` to bind.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason, tracked in the same place
//! (#3497).

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use stella_plugin::{
    CandidateGrant, HostStage, PluginManifest, RecallFrame, SignalValues, StageName, TamperFinding,
    TurnOutcome,
};
use stella_protocol::CandidateHandle;
use stella_protocol::completion::MessageRole;
use stella_runtime::wrapper::{
    DEFAULT_HOST_MAX_CALLS, DrivenTurn, HostCallGate, HostPlanes, RecallHost, RoundInput,
    SubprocessWrapper, TurnDriver, TurnPrelude, WrapperDispatch,
};
use tempfile::TempDir;

fn plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/stella-research")
        .canonicalize()
        .expect("the first-party plugin ships in this repository at plugins/stella-research")
}

fn manifest_text() -> String {
    fs::read_to_string(plugin_dir().join("plugin.toml"))
        .expect("the manifest file is `plugin.toml`, exactly")
}

fn manifest() -> PluginManifest {
    PluginManifest::from_toml_str(&manifest_text()).expect("the shipped manifest loads")
}

/// The shipped manifest with one condition put back on its `research` stage.
///
/// The stage order under test stays the shipped one — only the condition is
/// added — so what this grades is the resolver's treatment of a condition,
/// not a fixture pipeline of its own.
fn manifest_gated_on(condition: &str) -> PluginManifest {
    let text = manifest_text().replace(
        "name = \"research\"\n",
        &format!("name = \"research\"\nif = \"{condition}\"\n"),
    );
    PluginManifest::from_toml_str(&text).expect("a gated stage order loads")
}

/// The transport the host would build from this manifest: the declared argv
/// with `${plugin_dir}` interpolated as the loader does it, the declared
/// environment allowlist and nothing else, the declared budget.
fn transport(manifest: &PluginManifest) -> SubprocessWrapper {
    let runtime = manifest.runtime.as_ref().expect("[runtime]");
    let dir = plugin_dir().display().to_string();
    let argv = runtime
        .argv
        .iter()
        .map(|arg| stella_plugin::expand_plugin_dir(arg, Path::new(&dir)))
        .collect();
    SubprocessWrapper::declare(
        argv,
        runtime.child_env(|name| std::env::var(name).ok()),
        Duration::from_secs(runtime.timeout_secs),
    )
    .expect("the manifest declares a program and a budget")
    .wrapper
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("a fixture path has a parent"))
        .expect("fixture directory");
    fs::write(path, contents).expect("fixture file");
}

/// The workspace the turn is granted. Small on purpose — the two files that
/// must *not* be reported are what it is really for.
fn fixture() -> TempDir {
    let dir = TempDir::new().expect("a fixture workspace");
    let root = dir.path();
    write(
        root,
        "AGENTS.md",
        "The fixture workspace's orientation file.\n",
    );
    write(
        root,
        "src/retry.rs",
        "pub struct RetryBudget;\n// retry_budget is honoured here\n",
    );
    write(root, "src/scheduler.rs", "let b = retry_budget();\n");
    write(root, "target/generated.rs", "retry_budget, generated\n");
    write(root, ".hidden/notes.md", "retry_budget, hidden\n");
    dir
}

/// A host that records what it was asked to run and reports a turn that did
/// nothing. Enough to answer the only question this test asks: did the
/// plugin's contribution reach the turn, and in what shape.
#[derive(Default)]
struct RecordingDriver {
    prelude: Option<TurnPrelude>,
}

#[async_trait(?Send)]
impl TurnDriver for RecordingDriver {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        self.prelude = Some(prelude);
        DrivenTurn {
            outcome: TurnOutcome {
                completed: true,
                ..TurnOutcome::default()
            },
            tamper: TamperFinding::NotChecked,
        }
    }
}

/// The signals a **shipping** host publishes, which is the only kind there is:
/// the host facts are real and every triage signal is false or zero, because
/// no host runs a triage stage
/// (`crates/stella-cli/src/wrapper_plugin.rs::pre_turn_signals`).
///
/// Written out in full because [`SignalValues`] derives no `Default` on
/// purpose — a host must answer every signal rather than let one read as
/// `false`. The `questions` parameter is what a triage producer would publish
/// if one existed; the tests below drive `0`, which is what a real run does.
fn signals(questions: u64) -> SignalValues {
    SignalValues {
        test_command: false,
        candidates: 1,
        budget_metered: false,
        conversational: false,
        questions,
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

/// A context plane with nothing to recall.
///
/// The plugin asks for `recall` mid-`before_turn` (`doc:wrapper-socket` §6b), so
/// a host driving it must serve the channel or the ask has nowhere to go. What
/// this host answers is the ordinary empty case — nothing was relevant — which
/// is what keeps these two tests about the thing they were written to grade:
/// the *research* findings the plugin reads out of the granted workspace. That
/// a real plane's frames reach the plugin and become contributed context is
/// `wrapper_host_call.rs`'s witness, not this file's.
struct NothingToRecall;

#[async_trait]
impl RecallHost for NothingToRecall {
    async fn recall(&self, _goal: &str) -> Vec<RecallFrame> {
        Vec::new()
    }
}

fn dispatch(manifest: PluginManifest) -> WrapperDispatch {
    let gate = Arc::new(HostCallGate::declare(
        manifest.loop_grant.clone(),
        DEFAULT_HOST_MAX_CALLS,
        Box::new(HostPlanes::recalling(NothingToRecall)),
    ));
    let transport = transport(&manifest).serving(gate);
    WrapperDispatch::bind(manifest, Arc::new(transport))
        .expect("the shipped manifest binds to the host sequence that dispatches it")
}

/// **The witness that this plugin is what the dispatcher dispatches.** The
/// whole sequence runs — the declared stage program, `before_turn` per stage,
/// the turn, `judge`, `again?` — and what the driver is handed carries this
/// plugin's findings as user messages.
///
/// **Driven on `signals(0)` since #3547**, which is what a real run publishes:
/// no host performs a triage assessment, so `questions` is zero on every one.
/// The manifest gated `research` on `questions > 0` until then, so this same
/// sequence contributed nothing on the only signals that ever reach it — the
/// plugin was installed, selected, dispatched and useless, and this test
/// passed anyway because it supplied a count no host produces.
#[tokio::test]
async fn the_host_sequence_drives_the_plugin_and_the_turn_gets_its_findings() {
    let workspace = fixture();
    let dispatch = dispatch(manifest());
    let mut driver = RecordingDriver::default();

    let report = dispatch
        .run(
            RoundInput {
                goal: "retry_budget is not honoured".into(),
                signals: signals(0),
                candidate: Some(CandidateGrant::new(
                    CandidateHandle::new("candidate-1"),
                    workspace.path().display().to_string(),
                )),
            },
            &mut driver,
        )
        .await
        .expect("the declared stage order resolves");

    assert_eq!(report.variant, "research-v1");
    assert_eq!(
        report.rounds, 1,
        "a wrapper that decides nothing holds nothing open"
    );
    assert!(
        report.faults.is_empty(),
        "the plugin answered every point it declared: {:?}",
        report.faults
    );

    let prelude = driver.prelude.expect("the host was asked to run a turn");
    assert!(
        prelude
            .stages()
            .contains(&StageName::Host(HostStage::Research)),
        "the resolved program runs the stage this plugin exists for"
    );
    let messages = prelude.into_messages();
    assert!(
        messages
            .iter()
            .all(|message| message.role == MessageRole::User),
        "contributed context rides after the byte-stable prefix, never inside it"
    );
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains("src/retry.rs:2")),
        "the turn was handed a line that exists in the granted workspace, \
         cited by path: {messages:?}"
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.content.contains("target/generated.rs")),
        "a match in a generated tree reads exactly like a finding and is never one"
    );
}

/// The declared condition is the host's, not the plugin's: a stage whose
/// condition is false does not run, so the plugin is never asked and the turn
/// is byte-for-byte the one a host without it would have run.
///
/// Driven against a manifest patched to gate `research` on a **host** signal
/// rather than the shipped file, since #3547 took the shipped condition off.
/// The property is worth keeping and the shipped manifest can no longer
/// witness it: `test-command` is a fact this host answers for, which is
/// exactly what `doc:wrapper-socket` §5 now points a manifest author at.
#[tokio::test]
async fn a_stage_whose_declared_condition_is_false_gets_no_contribution() {
    let workspace = fixture();
    let dispatch = dispatch(manifest_gated_on("test-command"));
    let mut driver = RecordingDriver::default();

    let report = dispatch
        .run(
            RoundInput {
                goal: "retry_budget is not honoured".into(),
                // `test_command: false`, so the patched condition is false.
                signals: signals(0),
                candidate: Some(CandidateGrant::new(
                    CandidateHandle::new("candidate-1"),
                    workspace.path().display().to_string(),
                )),
            },
            &mut driver,
        )
        .await
        .expect("the declared stage order resolves");

    assert!(report.faults.is_empty());
    let prelude = driver.prelude.expect("the host was asked to run a turn");
    assert!(
        !prelude
            .stages()
            .contains(&StageName::Host(HostStage::Research))
    );
    assert!(
        prelude.into_messages().is_empty(),
        "zero findings must leave the prompt exactly as it was — the advisory \
         contract the built-in stage holds too"
    );
}
