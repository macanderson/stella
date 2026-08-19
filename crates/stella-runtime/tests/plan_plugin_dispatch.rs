//! The third witness for Track B's second extraction: `plugins/stella-plan`
//! is what the host sequence dispatches, and the child turn it asks for is
//! performed by the **host's own dispatcher**, never by the plugin.
//!
//! `plan_plugin_conformance.rs` grades the plugin against the socket alone;
//! `plan_plugin_hostcall.rs` grades the `child_turn` conversation by hand.
//! This file grades it against [`WrapperDispatch`], with a real [`ChildTurns`]
//! plane over a fake [`SubAgentDispatcher`] standing in for the session's own
//! one — the same shape `crates/stella-runtime/tests/wrapper_child_turn.rs`
//! uses to prove "who made the model call?" for the generic mechanism. This
//! file asks the same question with the shipped plugin's real process on the
//! other end of the pipe.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason, tracked in the same place
//! (#3497).

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use stella_core::{SubAgentDispatcher, SubAgentOutcome, SubAgentReport, SubAgentSpec};
use stella_plugin::{PluginManifest, SignalValues, StageName, TamperFinding, TurnOutcome};
use stella_protocol::completion::MessageRole;
use stella_protocol::event::ModelCallRole;
use stella_runtime::wrapper::{
    ChildTurns, DEFAULT_HOST_MAX_CALLS, DrivenTurn, HostCallGate, HostPlanes, RoundInput,
    SubprocessWrapper, TurnDriver, TurnPrelude, WrapperDispatch,
};

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

fn transport(manifest: &PluginManifest) -> SubprocessWrapper {
    let runtime = manifest.runtime.as_ref().expect("[runtime]");
    let dir = plugin_dir().display().to_string();
    let argv = runtime
        .argv
        .iter()
        .map(|arg| arg.replace("${plugin_dir}", &dir))
        .collect();
    SubprocessWrapper::declare(
        argv,
        runtime.child_env(|name| std::env::var(name).ok()),
        Duration::from_secs(runtime.timeout_secs),
    )
    .expect("the manifest declares a program and a budget")
    .wrapper
}

/// A host that records what it was asked to run and reports a turn that did
/// nothing. Enough to answer this file's only question: did the plugin's
/// contribution reach the turn, and in what shape.
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

/// The host's sub-agent dispatcher standing in for the session's own one —
/// the same port `task_assign` runs on, and the one [`ChildTurns`] spends
/// through. Records every spec it was handed, which is how this file answers
/// "who made the model call?", and answers with a report shaped as a plan so
/// the plugin's own `parse_plan` has something real to parse.
#[derive(Default, Clone)]
struct PlannerDispatcher {
    specs: Arc<Mutex<Vec<SubAgentSpec>>>,
}

impl PlannerDispatcher {
    fn specs(&self) -> Vec<SubAgentSpec> {
        self.specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl SubAgentDispatcher for PlannerDispatcher {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        self.specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(spec);
        SubAgentOutcome::Completed(SubAgentReport {
            summary: r#"["add a regression test for src/retry.rs:2","fix the retry loop to honour the budget"]"#
                .to_string(),
            truncated: false,
            cost_usd: 0.01,
            steps: 1,
            absorbed_messages: 2,
        })
    }
}

/// Signals for a turn triage classified as one that plans. Written out in
/// full because `SignalValues` derives no `Default` on purpose — a host must
/// answer every signal rather than let one read as `false`.
fn signals(plans: bool) -> SignalValues {
    SignalValues {
        test_command: false,
        candidates: 1,
        budget_metered: false,
        conversational: false,
        questions: 0,
        plans,
        verifies: true,
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

fn dispatch(manifest: PluginManifest, dispatcher: PlannerDispatcher) -> WrapperDispatch {
    let plane = ChildTurns::declare(&manifest, dispatcher);
    let gate = Arc::new(HostCallGate::declare(
        manifest.loop_grant.clone(),
        DEFAULT_HOST_MAX_CALLS,
        Box::new(HostPlanes::none().with_child_turns(plane)),
    ));
    let transport = transport(&manifest).serving(gate);
    WrapperDispatch::bind(manifest, Arc::new(transport))
        .expect("the shipped manifest binds to the host sequence that dispatches it")
}

/// **The witness that this plugin is what the dispatcher dispatches, and
/// that the host — not the plugin — makes the model call.** The whole
/// sequence runs, the plugin asks for a `planner` child turn, the host's own
/// dispatcher performs it, and what the driver is handed carries the parsed
/// plan as a user message.
#[tokio::test]
async fn the_host_sequence_drives_the_plugin_and_the_host_runs_its_child_turn() {
    let dispatcher = PlannerDispatcher::default();
    let dispatch = dispatch(manifest(), dispatcher.clone());
    let mut driver = RecordingDriver::default();

    let report = dispatch
        .run(
            RoundInput {
                goal: "retry_budget is not honoured".into(),
                signals: signals(true),
                candidate: None,
            },
            &mut driver,
        )
        .await
        .expect("the declared stage order resolves");

    assert_eq!(report.variant, "plan-v1");
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
        prelude.stages().contains(&StageName::Plan),
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
        "the child turn's real report reached the plugin and became the turn's \
         context: {messages:?}"
    );

    // The host made the call, not the plugin.
    let specs = dispatcher.specs();
    assert_eq!(
        specs.len(),
        1,
        "exactly one child turn, and the host ran it"
    );
    assert_eq!(
        specs[0].role,
        ModelCallRole::Plan,
        "the `planner` role intent resolved to the same responsibility the \
         built-in stage's own model call carries"
    );
    assert!(
        !specs[0].write_access,
        "a plugin's child turn is read-only, enforced at execution rather than by prompt"
    );
    assert!(
        specs[0]
            .instruction
            .contains("retry_budget is not honoured"),
        "the goal reached the child turn's instruction: {}",
        specs[0].instruction
    );
    assert!(
        specs[0].instruction.contains("You are the planner"),
        "the byte-mirrored PLANNER_INSTRUCTIONS opener reached the child turn"
    );
}

/// The declared condition is the host's, not the plugin's: with `plans =
/// false` the stage does not run, so the plugin is never asked and the turn
/// is byte-for-byte the one a host without it would have run — the same
/// advisory contract `research_plugin_dispatch.rs`'s sibling test holds for
/// `questions`.
#[tokio::test]
async fn a_turn_that_does_not_plan_gets_no_contribution() {
    let dispatcher = PlannerDispatcher::default();
    let dispatch = dispatch(manifest(), dispatcher.clone());
    let mut driver = RecordingDriver::default();

    let report = dispatch
        .run(
            RoundInput {
                goal: "retry_budget is not honoured".into(),
                signals: signals(false),
                candidate: None,
            },
            &mut driver,
        )
        .await
        .expect("the declared stage order resolves");

    assert!(report.faults.is_empty());
    let prelude = driver.prelude.expect("the host was asked to run a turn");
    assert!(!prelude.stages().contains(&StageName::Plan));
    assert!(
        prelude.into_messages().is_empty(),
        "no plan stage means no child turn and no contribution"
    );
    assert!(
        dispatcher.specs().is_empty(),
        "the host never ran a child turn the plugin never asked for"
    );
}
