//! The third witness for `plugins/stella-goal`: it is what the host sequence
//! dispatches, and the arbiter machinery it declares — hold the round open
//! until the verifier's own child turn says met — is the **generic**
//! `WrapperDispatch`/`judge`/`again` loop every arbiter-grade plugin gets,
//! exercised here with a real process on the other end of the pipe.
//!
//! `goal_plugin_conformance.rs` grades the plugin against the socket alone;
//! `goal_plugin_hostcall.rs` grades one `after_turn`'s `child_turn`
//! conversation by hand. This file grades the WHOLE loop against
//! [`WrapperDispatch`], with a real [`ChildTurns`] plane over a fake
//! [`SubAgentDispatcher`] standing in for the session's own one — the same
//! shape `plan_plugin_dispatch.rs` uses to prove "who made the model call?"
//! for the generic mechanism, now driven for enough rounds to prove "who
//! decides done?" as well.
//!
//! # The one gap this file makes concrete rather than theoretical
//!
//! `ChildTurns::default_seats()` does not serve the `verifier` tier this
//! plugin's `[roles.verifier]` names (`child_turn.rs`'s own module doc names
//! the reason: attributing a plugin's child turn to `ModelCallRole::Verdict`
//! would put a call on the receipt the pipeline itself did not make). No
//! shipped host binds it today — see `plugins/stella-goal/README.md`'s
//! "Known gaps" table. So this file's main witness binds the seat by hand
//! with [`ChildTurns::with_seat`], exactly as `child_turn.rs`'s own
//! `an_unserved_tier_names_the_tiers_that_are_served` test does, and a second
//! test pins what happens on every host that does not: the plugin degrades
//! honestly and the loop ends `Undecided` rather than crediting or blaming an
//! assessment that was never made.
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
use stella_plugin::{Outcome, PluginManifest, SignalValues, StageName, TamperFinding, TurnOutcome};
use stella_protocol::completion::MessageRole;
use stella_protocol::event::ModelCallRole;
use stella_runtime::wrapper::{
    ChildTurns, DEFAULT_HOST_MAX_CALLS, DrivenTurn, HostCallGate, HostPlanes, RoundInput,
    SubprocessWrapper, TurnDriver, TurnPrelude, WrapperDispatch,
};

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

/// A host that records every turn it was asked to run and reports a fresh
/// completed turn each time — enough to answer this file's questions: how
/// many rounds ran, and what did each one see.
#[derive(Default)]
struct RecordingDriver {
    preludes: Vec<TurnPrelude>,
}

#[async_trait(?Send)]
impl TurnDriver for RecordingDriver {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        self.preludes.push(prelude);
        let round = self.preludes.len();
        DrivenTurn {
            outcome: TurnOutcome {
                completed: true,
                answer: format!("round {round}: fixed the retry loop"),
                tools: Some(vec!["edit_file".to_string()]),
                changed_files: Some(vec!["src/retry.rs".to_string()]),
            },
            tamper: TamperFinding::NotChecked,
        }
    }
}

/// The host's sub-agent dispatcher standing in for the session's own one.
/// Says "not yet met" the first time it is asked and "met" every time after,
/// so a test can drive exactly the number of rounds it wants by choosing how
/// many turns the driver above runs before the loop stops asking.
#[derive(Default, Clone)]
struct VerifierDispatcher {
    specs: Arc<Mutex<Vec<SubAgentSpec>>>,
}

impl VerifierDispatcher {
    fn specs(&self) -> Vec<SubAgentSpec> {
        self.specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl SubAgentDispatcher for VerifierDispatcher {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        let call = {
            let mut specs = self
                .specs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            specs.push(spec);
            specs.len()
        };
        let report = if call == 1 {
            r#"{"met": false, "reasoning": "no regression test guards the fix", "feedback": "add a regression test for the retry budget"}"#
        } else {
            r#"{"met": true, "reasoning": "a regression test now covers the fix", "feedback": ""}"#
        };
        SubAgentOutcome::Completed(SubAgentReport {
            summary: report.to_string(),
            truncated: false,
            cost_usd: 0.01,
            steps: 1,
            absorbed_messages: 2,
        })
    }
}

/// Signals for a plain working turn. Written out in full because
/// `SignalValues` derives no `Default` on purpose — a host must answer every
/// signal rather than let one read as `false`. This plugin's own stage order
/// is entirely unconditional, so the values here do not change which stages
/// run; they exist only because `RoundInput` requires a complete answer.
fn signals() -> SignalValues {
    SignalValues {
        test_command: false,
        candidates: 1,
        budget_metered: false,
        conversational: false,
        questions: 0,
        plans: false,
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

fn dispatch_with_seats(
    manifest: PluginManifest,
    dispatcher: VerifierDispatcher,
    bind_verifier: bool,
) -> WrapperDispatch {
    let mut plane = ChildTurns::declare(&manifest, dispatcher);
    if bind_verifier {
        // No shipped host does this today — see the module doc. This test
        // exercises the loop as it would run once a host does.
        plane = plane.with_seat("verifier", ModelCallRole::Verdict);
    }
    let gate = Arc::new(HostCallGate::declare(
        manifest.loop_grant.clone(),
        DEFAULT_HOST_MAX_CALLS,
        Box::new(HostPlanes::none().with_child_turns(plane)),
    ));
    let transport = transport(&manifest).serving(gate);
    WrapperDispatch::bind(manifest, Arc::new(transport))
        .expect("the shipped manifest binds to the host sequence that dispatches it")
}

/// **The witness that this plugin's arbiter grant is the generic loop, and
/// that the host — not the plugin — makes the verifier's model call.** Round
/// 1's verifier says not-yet-met; the declared `[requirements]`/`[oracle]`
/// hold the completion open for a correction round; round 2's verifier says
/// met; the loop stops there.
#[tokio::test]
async fn a_round_the_verifier_marks_unmet_holds_open_for_one_correction_round() {
    let dispatcher = VerifierDispatcher::default();
    let dispatch = dispatch_with_seats(manifest(), dispatcher.clone(), true);
    let mut driver = RecordingDriver::default();

    let report = dispatch
        .run(
            RoundInput {
                goal: "fix the retry loop".into(),
                signals: signals(),
                candidate: None,
            },
            &mut driver,
        )
        .await
        .expect("the declared stage order resolves");

    assert_eq!(report.variant, "goal-v1");
    assert_eq!(
        report.rounds, 2,
        "round 1's unmet verdict must hold the turn open for exactly one correction round"
    );
    assert!(
        report.met(),
        "round 2's verifier said met, so the loop must stop there: {:?}",
        report.outcome
    );
    assert!(
        matches!(report.outcome, Outcome::Met),
        "got {:?}",
        report.outcome
    );
    assert!(
        report.faults.is_empty(),
        "the plugin answered every point it declared: {:?}",
        report.faults
    );

    assert_eq!(
        driver.preludes.len(),
        2,
        "the host must have run exactly two working turns, one per round"
    );

    // Every round's `execute` stage contributes the goal framing — the
    // repetition is deliberate, see plugin.toml's header: `stella run`'s
    // door runs no separate "goal kickoff" step the way `stella goal`'s own
    // loop does, so a worker turn started fresh each round needs the frame
    // restated.
    for (index, prelude) in driver.preludes.iter().enumerate() {
        assert!(
            prelude.stages().contains(&StageName::Execute),
            "round {}: the resolved program runs the stage this plugin frames",
            index + 1
        );
    }
    let round_one_messages: Vec<_> = driver.preludes[0].clone().into_messages();
    assert!(
        round_one_messages
            .iter()
            .all(|message| message.role == MessageRole::User),
        "contributed context and the correction both ride after the byte-stable prefix"
    );
    assert!(
        round_one_messages
            .iter()
            .any(|message| message.content.contains("GOAL: fix the retry loop")),
        "round 1 sees the goal framing: {round_one_messages:?}"
    );
    assert!(
        !round_one_messages
            .iter()
            .any(|message| message.content.contains("not met yet")),
        "round 1 has no prior verdict to correct: {round_one_messages:?}"
    );

    let round_two_messages: Vec<_> = driver.preludes[1].clone().into_messages();
    assert!(
        round_two_messages
            .iter()
            .any(|message| message.content.contains("GOAL: fix the retry loop")),
        "round 2 is framed again: {round_two_messages:?}"
    );
    assert!(
        round_two_messages.iter().any(|message| message
            .content
            .contains("declared requirements are not met yet")),
        "round 2 must carry the host-rendered correction from round 1's unmet verdict: \
         {round_two_messages:?}"
    );
    assert!(
        round_two_messages
            .iter()
            .any(|message| message.content.contains("goal-met")),
        "the correction names the requirement by its declared key: {round_two_messages:?}"
    );

    // The host made both calls, not the plugin.
    let specs = dispatcher.specs();
    assert_eq!(
        specs.len(),
        2,
        "exactly two verifier child turns, and the host ran them"
    );
    for spec in &specs {
        assert_eq!(
            spec.role,
            ModelCallRole::Verdict,
            "the `verifier` role intent resolved to the seat this host bound it to"
        );
        assert!(
            !spec.write_access,
            "a plugin's child turn is read-only, enforced at execution rather than by prompt"
        );
        assert!(
            spec.instruction.contains("fix the retry loop"),
            "the goal reached the child turn's instruction: {}",
            spec.instruction
        );
        assert!(
            spec.instruction.contains("You are an impartial verifier"),
            "the byte-mirrored VERIFIER_SYSTEM_PROMPT opener reached the child turn"
        );
        assert!(
            spec.instruction.contains("round: fixed the retry loop")
                || spec.instruction.contains("Final answer"),
            "the working turn's own answer reached the verifier's instruction: {}",
            spec.instruction
        );
    }
}

/// **The gap made concrete.** No shipped host binds a `verifier` seat today
/// (`ChildTurns::default_seats()` does not serve it), so this is the loop as
/// it actually runs in the field: the plugin's `child_turn` ask degrades to
/// `Unavailable`, `after_turn` reports no `met` measurement, `judge` abstains
/// (`UndecidedReason::MeasurementMissing`) rather than crediting or blaming
/// an assessment that was never made, and an abstention is terminal — `again`
/// never holds an `Undecided` round open (`crates/stella-runtime/src/wrapper/verdict.rs`'s
/// own doc comment: "There is no correction to author from evidence that
/// decided nothing").
#[tokio::test]
async fn without_a_bound_verifier_seat_the_loop_ends_undecided_after_one_round() {
    let dispatcher = VerifierDispatcher::default();
    let dispatch = dispatch_with_seats(manifest(), dispatcher.clone(), false);
    let mut driver = RecordingDriver::default();

    let report = dispatch
        .run(
            RoundInput {
                goal: "fix the retry loop".into(),
                signals: signals(),
                candidate: None,
            },
            &mut driver,
        )
        .await
        .expect("the declared stage order resolves");

    assert_eq!(
        report.rounds, 1,
        "an abstention is terminal — it never holds a round open"
    );
    assert!(
        matches!(report.outcome, Outcome::Undecided { .. }),
        "got {:?}",
        report.outcome
    );
    assert!(
        !report.met(),
        "an undecided run must never read as met: {:?}",
        report.outcome
    );
    assert!(
        dispatcher.specs().is_empty(),
        "the host never ran a child turn for an ask it could not resolve"
    );
}
