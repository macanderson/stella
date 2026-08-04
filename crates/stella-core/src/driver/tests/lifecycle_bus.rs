// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The turn/step/model-call lifecycle on the `HookBus` (#1133).
//!
//! The catalog and `emit_named` both already existed; what did not was any
//! production code path that called one with the other. Until this, the only
//! emitter on the bus was the tool registry, so an extension could see every
//! tool call and nothing about the turn that made them.
//!
//! These tests assert on the *pairing* as much as the presence: an observer
//! that counts `agent.step.started` against `agent.step.completed` must never
//! leak, on any exit path a step has.

use std::sync::{Arc, Mutex};

use super::super::*;
use crate::bus::{HookBus, HookEvent, names};
use serde_json::Value;
use stella_protocol::{CompletionResult, ToolSchema};

/// Collects every event the bus dispatches, in order.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<HookEvent>>,
}

impl Recorder {
    /// Register on a fresh bus and hand back both halves.
    fn attach() -> (HookBus, Arc<Recorder>) {
        let bus = HookBus::new("lifecycle-test");
        let recorder = Arc::new(Recorder::default());
        let sink = Arc::clone(&recorder);
        bus.on("*", move |event| {
            sink.events.lock().unwrap().push(event.clone());
            Ok(())
        })
        .detach();
        (bus, recorder)
    }

    fn names(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.name.clone())
            .collect()
    }

    fn count(&self, name: &str) -> usize {
        self.names().iter().filter(|n| *n == name).count()
    }

    fn first(&self, name: &str) -> Option<HookEvent> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.name == name)
            .cloned()
    }
}

/// A provider that answers once with text and then keeps answering — enough
/// to drive a turn that completes on its first step.
struct OneShotProvider;

#[async_trait::async_trait]
impl Provider for OneShotProvider {
    fn id(&self) -> &str {
        "lifecycle-mock"
    }

    async fn complete_ref(
        &self,
        _req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Ok(CompletionResult {
            text: "the answer".to_string(),
            tool_calls: Vec::new(),
            usage: stella_protocol::CompletionUsage {
                reported: true,
                input_tokens: 11,
                output_tokens: 7,
                cached_input_tokens: 0,
                cache_write_tokens: 0,
            },
            model: "mock-1".to_string(),
            cost_usd: 0.25,
            finish_reason: None,
        })
    }
}

/// A provider that always fails unretryably, so the model-call boundary's
/// failure arm is reached.
struct AlwaysFailsProvider;

#[async_trait::async_trait]
impl Provider for AlwaysFailsProvider {
    fn id(&self) -> &str {
        "lifecycle-fail"
    }

    async fn complete_ref(
        &self,
        _req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Err(ProviderError::Auth("no credential".to_string()))
    }
}

struct NoTools;

#[async_trait::async_trait]
impl ToolExecutor for NoTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: String::new(),
        }
    }
}

struct NoSleep;

#[async_trait::async_trait]
impl crate::retry::Sleeper for NoSleep {
    async fn sleep(&self, _duration_ms: u64) {}
}

async fn run_one_turn(provider: &dyn Provider, bus: &HookBus) -> TurnOutcome {
    let tools = NoTools;
    let sleeper = NoSleep;
    let engine =
        Engine::with_sleeper(provider, &tools, EngineConfig::default(), &sleeper).with_bus(bus);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut messages = vec![CompletionMessage::user("hi")];
    let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
    engine.run_turn(&mut messages, &mut budget, &tx).await
}

/// The #1133 acceptance for the turn and step boundaries: a registered
/// observer sees the turn open, the step open and close, and the turn close.
#[tokio::test]
async fn a_completed_turn_emits_the_whole_lifecycle_in_order() {
    let (bus, recorder) = Recorder::attach();
    let outcome = run_one_turn(&OneShotProvider, &bus).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    let names = recorder.names();
    let ordered: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| n.starts_with("agent.") || n.starts_with("model."))
        .collect();
    assert_eq!(
        ordered,
        vec![
            names::AGENT_TURN_STARTED,
            names::AGENT_STEP_STARTED,
            names::MODEL_REQUEST_STARTED,
            names::MODEL_REQUEST_COMPLETED,
            names::AGENT_STEP_COMPLETED,
            names::AGENT_TURN_COMPLETED,
        ],
        "the lifecycle nests: turn wraps step wraps model call",
    );
}

/// The model-call event carries what an observer applying policy at that
/// boundary needs — which model, what it cost, how big the answer was — and
/// no transcript content.
#[tokio::test]
async fn the_model_call_events_carry_counts_and_identifiers_not_content() {
    let (bus, recorder) = Recorder::attach();
    run_one_turn(&OneShotProvider, &bus).await;

    let started = recorder
        .first(names::MODEL_REQUEST_STARTED)
        .expect("the model call is announced");
    assert_eq!(started.payload["message_count"], 1);
    assert_eq!(started.payload["step"], 0);

    let completed = recorder
        .first(names::MODEL_REQUEST_COMPLETED)
        .expect("the model call reports its result");
    assert_eq!(completed.payload["model"], "mock-1");
    assert_eq!(completed.payload["cost_usd"], 0.25);
    assert_eq!(completed.payload["input_tokens"], 11);
    assert_eq!(completed.payload["output_tokens"], 7);

    // The answer text is transcript content and has no business here.
    let rendered = serde_json::to_string(&completed.payload).unwrap();
    assert!(
        !rendered.contains("the answer"),
        "payload must not carry transcript content: {rendered}"
    );
}

/// A turn whose model call fails still closes both the step and the turn it
/// opened. This is the path the wrapper exists for: `run_step_inner` returns
/// early here, and a `completed` emit distributed across the step's exits
/// would be one refactor away from missing it.
#[tokio::test]
async fn a_failed_model_call_still_closes_its_step_and_turn() {
    let (bus, recorder) = Recorder::attach();
    let outcome = run_one_turn(&AlwaysFailsProvider, &bus).await;
    assert!(matches!(outcome, TurnOutcome::Aborted { .. }));

    assert_eq!(recorder.count(names::MODEL_REQUEST_FAILED), 1);
    assert_eq!(recorder.count(names::MODEL_REQUEST_COMPLETED), 0);
    assert_eq!(
        recorder.count(names::AGENT_STEP_STARTED),
        recorder.count(names::AGENT_STEP_COMPLETED),
        "every started step must be closed, on the failure path too",
    );
    assert_eq!(recorder.count(names::AGENT_TURN_STARTED), 1);
    assert_eq!(recorder.count(names::AGENT_TURN_COMPLETED), 1);

    let completed = recorder.first(names::AGENT_TURN_COMPLETED).unwrap();
    assert_eq!(completed.payload["outcome"], "aborted");
    assert!(completed.payload["reason"].is_string());
}

/// The step-completed event names whether the turn continues, which is what
/// an observer pairing starts with completions actually reads.
#[tokio::test]
async fn a_step_reports_how_it_ended() {
    let (bus, recorder) = Recorder::attach();
    run_one_turn(&OneShotProvider, &bus).await;

    let completed = recorder
        .first(names::AGENT_STEP_COMPLETED)
        .expect("the step closes");
    assert_eq!(completed.payload["outcome"], "done");
    assert_eq!(completed.payload["step"], 0);
    assert_eq!(completed.payload["cost_usd"], 0.25);
}

/// No bus attached, no behaviour change: this is what makes the emitters
/// free for every CLI turn, and it is worth pinning because the wrapper adds
/// a branch on the hottest path in the engine.
#[tokio::test]
async fn an_engine_without_a_bus_runs_identically() {
    let tools = NoTools;
    let sleeper = NoSleep;
    let engine = Engine::with_sleeper(&OneShotProvider, &tools, EngineConfig::default(), &sleeper);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut messages = vec![CompletionMessage::user("hi")];
    let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
}

/// `session.started` is emitted by whoever mints the bus, after subscribing —
/// the ordering matters, because an observer registered later would miss the
/// one event whose job is to open the stream.
#[test]
fn session_started_reaches_an_observer_registered_before_it() {
    let (bus, recorder) = Recorder::attach();
    bus.session_started();
    assert_eq!(recorder.count(names::SESSION_STARTED), 1);
    let event = recorder.first(names::SESSION_STARTED).unwrap();
    assert_eq!(event.payload["session_id"], "lifecycle-test");
    assert_eq!(event.session_id, "lifecycle-test");
}

/// Every lifecycle name this engine emits is in the catalog. The catalog is
/// the contract for what the host emits, so a name that drifted out of it
/// would be a private extension masquerading as a documented event.
#[tokio::test]
async fn every_emitted_lifecycle_name_is_in_the_catalog() {
    let (bus, recorder) = Recorder::attach();
    bus.session_started();
    run_one_turn(&OneShotProvider, &bus).await;
    run_one_turn(&AlwaysFailsProvider, &bus).await;

    for name in recorder.names() {
        assert!(
            names::ALL.contains(&name.as_str()),
            "{name} is emitted but not declared in the catalog",
        );
    }
}
