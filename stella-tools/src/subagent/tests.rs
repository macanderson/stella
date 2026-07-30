// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `task` tool tests (#922) — the model-callable face of the sub-agent
//! primitive.

use std::sync::Mutex;

use stella_core::ports::{ReadOnlyTools, ToolExecutor};
use stella_core::subagent::SubAgentReport;

use super::*;

/// A dispatcher that returns a scripted outcome and records the spec it was
/// handed — the spec is the contract this tool is responsible for.
struct RecordingDispatcher {
    outcome: Mutex<Option<SubAgentOutcome>>,
    seen: Mutex<Option<SubAgentSpec>>,
}

impl RecordingDispatcher {
    fn new(outcome: SubAgentOutcome) -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(Some(outcome)),
            seen: Mutex::new(None),
        })
    }
    fn spec(&self) -> SubAgentSpec {
        self.seen.lock().unwrap().clone().expect("dispatch ran")
    }
    fn dispatched(&self) -> bool {
        self.seen.lock().unwrap().is_some()
    }
}

#[async_trait]
impl SubAgentDispatcher for RecordingDispatcher {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        *self.seen.lock().unwrap() = Some(spec);
        self.outcome
            .lock()
            .unwrap()
            .clone()
            .expect("outcome scripted once")
    }
}

fn report(summary: &str, cost_usd: f64, truncated: bool) -> SubAgentReport {
    SubAgentReport {
        summary: summary.to_string(),
        truncated,
        cost_usd,
        steps: 4,
        absorbed_messages: 9,
    }
}

fn tool_with(outcome: SubAgentOutcome) -> (SpawnSubAgent, Arc<RecordingDispatcher>) {
    let dispatcher = RecordingDispatcher::new(outcome);
    let slot: DispatcherSlot = Arc::default();
    *slot.write().unwrap() = Some(dispatcher.clone() as Arc<dyn SubAgentDispatcher>);
    (SpawnSubAgent::new(slot), dispatcher)
}

fn call(description: &str, prompt: &str) -> Value {
    json!({ "description": description, "prompt": prompt })
}

#[tokio::test]
async fn a_completed_child_returns_its_finding() {
    let (tool, dispatcher) = tool_with(SubAgentOutcome::Completed(report(
        "the retry policy is in stella-core/src/retry.rs",
        0.004,
        false,
    )));

    let out = tool
        .execute(
            &call("find retry policy", "Which file defines the retry policy?"),
            std::path::Path::new("."),
        )
        .await;

    match out {
        ToolOutput::Ok { content } => {
            assert_eq!(content, "the retry policy is in stella-core/src/retry.rs")
        }
        other => panic!("expected Ok, got {other:?}"),
    }
    let spec = dispatcher.spec();
    assert_eq!(spec.instruction, "Which file defines the retry policy?");
    assert_eq!(spec.agent_id, "find-retry-policy");
}

#[tokio::test]
async fn write_access_is_never_model_settable() {
    // This tool delegates *research*. A child that could edit would need the
    // parent's review gates around it, which is a different change — so the
    // model cannot ask for it, even explicitly.
    let (tool, dispatcher) = tool_with(SubAgentOutcome::Completed(report("done", 0.0, false)));
    let mut input = call("do a thing", "go");
    input["write_access"] = json!(true);
    input["max_report_chars"] = json!(10_000_000);

    tool.execute(&input, std::path::Path::new(".")).await;

    let spec = dispatcher.spec();
    assert!(!spec.write_access, "write access must stay off");
    assert_eq!(
        spec.max_report_chars, REPORT_CHARS,
        "the report cap is the point of the primitive — not model-settable"
    );
}

#[tokio::test]
async fn a_partial_child_returns_its_evidence_as_ok_not_as_an_error() {
    // The salvaged text is work the parent already paid for. Burying it in an
    // error invites the model to discard it and redo the search.
    let (tool, _) = tool_with(SubAgentOutcome::Incomplete {
        report: report("found three callers so far", 0.02, false),
        reason: "step cap reached".into(),
    });

    let out = tool
        .execute(
            &call("find callers", "Find every caller of X"),
            std::path::Path::new("."),
        )
        .await;

    match out {
        ToolOutput::Ok { content } => {
            assert!(content.contains("found three callers so far"));
            assert!(
                content.contains("partial") && content.contains("step cap reached"),
                "and it must be labelled partial, not passed off as final: {content}"
            );
        }
        other => panic!("expected Ok with a partial marker, got {other:?}"),
    }
}

#[tokio::test]
async fn a_child_that_produced_nothing_is_an_error() {
    let (tool, _) = tool_with(SubAgentOutcome::Incomplete {
        report: report("", 0.01, false),
        reason: "provider unreachable".into(),
    });

    let out = tool
        .execute(&call("x", "y"), std::path::Path::new("."))
        .await;
    assert!(matches!(out, ToolOutput::Error { .. }), "{out:?}");
}

#[tokio::test]
async fn a_truncated_report_says_so_to_the_model() {
    let (tool, _) = tool_with(SubAgentOutcome::Completed(report(
        "a long finding",
        0.0,
        true,
    )));
    let out = tool
        .execute(&call("x", "y"), std::path::Path::new("."))
        .await;
    match out {
        ToolOutput::Ok { content } => assert!(
            content.contains("truncated"),
            "a clipped answer must never look exhaustive: {content}"
        ),
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unattached_dispatcher_tells_the_model_to_do_the_work_itself() {
    let tool = SpawnSubAgent::new(Arc::default());
    let out = tool
        .execute(&call("x", "y"), std::path::Path::new("."))
        .await;
    match out {
        ToolOutput::Error { message } => assert!(
            message.contains("unavailable") && message.contains("directly"),
            "the model needs a next action, not just a failure: {message}"
        ),
        other => panic!("expected a named error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_missing_prompt_is_a_self_correcting_error_and_dispatches_nothing() {
    let (tool, dispatcher) = tool_with(SubAgentOutcome::Completed(report("x", 1.0, false)));
    let out = tool
        .execute(&json!({ "description": "x" }), std::path::Path::new("."))
        .await;
    assert!(matches!(out, ToolOutput::Error { .. }), "{out:?}");
    assert!(
        !dispatcher.dispatched(),
        "a rejected call must not reach the dispatcher at all — that, not a          zero ledger, is what makes it cost nothing"
    );
}

/// A sub-agent runs behind `ReadOnlyTools`, and this tool is truthfully NOT
/// read-only (it spends money). So a child cannot see `task` in its schema
/// list and cannot execute it by guessing the name — nesting is capped at one
/// level by construction, with no depth counter to thread through concurrent
/// sibling spawns and get wrong.
#[tokio::test]
async fn a_child_structurally_cannot_spawn_a_grandchild() {
    struct OneTool(SpawnSubAgent);
    #[async_trait]
    impl ToolExecutor for OneTool {
        fn schemas(&self) -> Vec<ToolSchema> {
            vec![self.0.schema()]
        }
        async fn execute(&self, _name: &str, input: &Value) -> ToolOutput {
            self.0.execute(input, std::path::Path::new(".")).await
        }
    }

    let (tool, _) = tool_with(SubAgentOutcome::Completed(report("nope", 0.0, false)));
    let parent = OneTool(tool);
    assert_eq!(parent.schemas().len(), 1, "the parent can see `task`");

    let child_view = ReadOnlyTools::new(&parent);
    assert!(
        child_view.schemas().is_empty(),
        "a child must not be offered `task`"
    );
    let refused = child_view.execute("task", &call("x", "y")).await;
    match refused {
        ToolOutput::Error { message } => assert!(
            message.contains("read-only"),
            "and guessing the name must be refused at execution time: {message}"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn slug_is_stable_short_and_never_empty() {
    assert_eq!(slug("find the retry policy"), "find-the-retry-policy");
    assert_eq!(slug("Trace WHO calls X()"), "trace-who-calls-x");
    assert_eq!(slug("   "), "sub-agent");
    assert_eq!(slug("!!!"), "sub-agent");
    assert!(slug(&"very long description ".repeat(10)).len() <= 32);
}

#[test]
fn the_schema_is_not_read_only_which_is_what_makes_nesting_structural() {
    let tool = SpawnSubAgent::new(Arc::default());
    let schema = tool.schema();
    assert_eq!(schema.name, "task");
    assert!(
        !schema.read_only,
        "marking this read_only would let children spawn children"
    );
    // The description must tell the model WHEN, not just what — an
    // undiscoverable tool is the failure mode this codebase already shipped
    // once with graph_query.
    let description = schema.description.to_lowercase();
    assert!(description.contains("read-only"), "{description}");
    assert!(
        description.contains("not for") || description.contains("prefer it"),
        "the description must scope the tool, not just advertise it"
    );
}

// ---- turn controls ----------------------------------------------------

/// A steering tap whose stop flag latches, exactly like the deck's.
struct LatchingStop(std::sync::atomic::AtomicBool);

impl stella_core::ports::TurnSteering for LatchingStop {
    fn drain_steering(&self) -> Vec<String> {
        Vec::new()
    }
    fn soft_stop_requested(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The reason this is a guard and not a setter.
///
/// A soft stop LATCHES — that is the port's contract, so it survives until
/// the tap does. A tap left published past its turn would therefore stop
/// every child of the next turn at its first boundary, and the user would
/// see delegation that silently stopped working after one Esc. Coming down
/// on drop is also what covers the cancel and panic paths, where no detach
/// call would ever run.
#[test]
fn a_latched_stop_cannot_outlive_the_turn_that_published_it() {
    let slot: TurnControlsSlot = Arc::default();
    let stopped = Arc::new(LatchingStop(std::sync::atomic::AtomicBool::new(true)));

    {
        let _guard =
            TurnControlsGuard::attach(&slot, TurnControls::none().with_steering(stopped.clone()));
        let published = slot.read().unwrap().clone();
        assert!(
            published
                .steering
                .expect("published for the turn")
                .soft_stop_requested(),
            "a child dispatched during this turn must see the stop"
        );
    }

    assert!(
        slot.read().unwrap().is_empty(),
        "and must not see it once the turn is over"
    );
}

#[test]
fn attaching_replaces_rather_than_stacking() {
    let slot: TurnControlsSlot = Arc::default();
    let running = Arc::new(LatchingStop(std::sync::atomic::AtomicBool::new(false)));

    let first = TurnControlsGuard::attach(&slot, TurnControls::none().with_steering(running));
    let second = TurnControlsGuard::attach(&slot, TurnControls::none());
    assert!(
        slot.read().unwrap().is_empty(),
        "the second turn's (absent) controls win — turns do not nest on one \
         registry"
    );

    // Dropping in either order leaves the slot empty rather than resurrecting
    // the first turn's seams.
    drop(first);
    assert!(slot.read().unwrap().is_empty());
    drop(second);
    assert!(slot.read().unwrap().is_empty());
}
