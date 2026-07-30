// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Witness tests for the step-scoped facade (#971), driven entirely through
//! the public API this crate re-exports — if a host can do it, these tests do
//! it the same way.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::{CompletionRequestRef, CompletionResult, CompletionUsage};
use tokio::sync::mpsc;

use crate::{
    AgentEvent, BudgetGuard, BudgetMode, CANCELLED_REASON, CancelToken, CompletionMessage, Engine,
    EngineConfig, EventSender, MessageRole, Provider, ProviderError, Sleeper, StepOutcome,
    ToolCall, ToolExecutor, ToolOutput, ToolSchema, TurnOutcome,
};

/// A `Sleeper` that records but never actually waits.
struct NoopSleeper;
#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// A scripted provider: one response per call, in order, looping the last
/// entry once exhausted so a runaway loop fails loudly rather than panicking.
struct ScriptedProvider {
    script: std::sync::Mutex<Vec<CompletionResult>>,
    calls: Arc<AtomicU32>,
}

impl ScriptedProvider {
    fn new(script: Vec<CompletionResult>) -> Self {
        Self {
            script: std::sync::Mutex::new(script),
            calls: Arc::new(AtomicU32::new(0)),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut script = self.script.lock().expect("script mutex");
        if script.len() > 1 {
            Ok(script.remove(0))
        } else {
            Ok(script[0].clone())
        }
    }
}

/// A tool executor that counts real invocations and can trip a
/// [`CancelToken`] from inside a call — the host cancelling while a tool is in
/// flight, which is exactly the window the boundary rule protects.
struct CountingTools {
    calls: Arc<AtomicU32>,
    cancel_on_call: Option<CancelToken>,
}

impl CountingTools {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicU32::new(0)),
            cancel_on_call: None,
        }
    }
}

#[async_trait]
impl ToolExecutor for CountingTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(cancel) = &self.cancel_on_call {
            cancel.cancel();
        }
        ToolOutput::Ok {
            content: "ok".into(),
        }
    }
}

fn text_result(text: &str) -> CompletionResult {
    CompletionResult {
        text: text.into(),
        tool_calls: vec![],
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: 0.0001,
        finish_reason: None,
    }
}

fn tool_call_result(call_id: &str) -> CompletionResult {
    CompletionResult {
        text: String::new(),
        tool_calls: vec![ToolCall {
            call_id: call_id.into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": "echo hi"}),
        }],
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: 0.0001,
        finish_reason: None,
    }
}

fn free_budget() -> BudgetGuard {
    BudgetGuard::new(BudgetMode::Off, None, None)
}

fn prompt() -> Vec<CompletionMessage> {
    vec![
        CompletionMessage::system("you are stella"),
        CompletionMessage::user("do the thing"),
    ]
}

/// One event, reduced to the parts that are a function of the script.
/// `duration_ms` is wall-clock and differs run to run; everything else — step
/// index, model, tokens, cost, text, call ids — is deterministic.
fn fingerprint(event: &AgentEvent) -> String {
    let mut value = serde_json::to_value(event).expect("AgentEvent serializes");
    if let Some(object) = value.as_object_mut() {
        object.remove("duration_ms");
    }
    value.to_string()
}

fn event_type(event: &AgentEvent) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default()
}

/// Context receipts are the one thing a resumed turn re-emits, because the
/// block registry is a per-turn memo a `Checkpoint` deliberately does not
/// carry (see `stella_core::step`). Compared separately, never mixed in.
fn is_receipt(event: &AgentEvent) -> bool {
    matches!(
        event_type(event).as_str(),
        "block_registered" | "step_manifest"
    )
}

fn drain(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

/// Every assistant `tool_use` in `messages` is answered by a `tool_result`.
/// The shape a provider rejects outright when it is not true.
fn every_call_is_paired(messages: &[CompletionMessage]) -> bool {
    let mut open: Vec<&str> = Vec::new();
    for message in messages {
        match message.role {
            MessageRole::Assistant => {
                open.extend(message.tool_calls.iter().map(|c| c.call_id.as_str()));
            }
            MessageRole::Tool => {
                for result in &message.tool_results {
                    if let Some(position) = open.iter().rposition(|id| *id == result.call_id) {
                        open.remove(position);
                    }
                }
            }
            MessageRole::System | MessageRole::User => {}
        }
    }
    open.is_empty()
}

#[tokio::test]
async fn new_turn_then_run_step_drives_a_turn_to_done() {
    // Step 0 calls a tool, step 1 answers. The host owns the loop.
    let provider = ScriptedProvider::new(vec![tool_call_result("c1"), text_result("all done")]);
    let tools = CountingTools::new();
    let tool_calls = tools.calls.clone();
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = EventSender::new(tx);
    let mut state = engine.new_turn(prompt(), free_budget());
    assert_eq!(state.step(), 0, "a fresh turn starts at step 0");

    assert_eq!(
        engine.run_step(&mut state, &events).await,
        StepOutcome::Continue
    );
    assert_eq!(state.step(), 1, "a committed step advances the index");
    assert!(
        every_call_is_paired(state.messages()),
        "Continue is the checkpointable boundary, so the transcript must be paired there"
    );

    let outcome = engine.run_step(&mut state, &events).await;
    assert_eq!(
        outcome,
        StepOutcome::Done {
            text: "all done".into(),
            cost_usd: 0.0002,
        }
    );
    assert_eq!(
        state.step(),
        1,
        "a terminal step leaves the index naming the step that ended the turn"
    );
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1, "the tool ran once");

    // The whole turn is in the state the host owns, not in a borrowed vec.
    let messages = state.into_messages();
    assert!(every_call_is_paired(&messages));
    assert_eq!(messages.last().expect("an answer").content, "all done");

    let types: Vec<String> = drain(&mut rx).iter().map(event_type).collect();
    assert!(types.contains(&"tool_start".to_string()));
    assert!(types.contains(&"complete".to_string()));
}

#[tokio::test]
async fn run_turn_is_exactly_a_loop_over_run_step() {
    // The "one code path" claim, as a witness: the same script driven both
    // ways emits the same events in the same order. `run_turn` adds only its
    // one turn-level `Stage` event, which is why it is filtered here.
    let script = || vec![tool_call_result("c1"), text_result("all done")];

    let stepwise = {
        let provider = ScriptedProvider::new(script());
        let tools = CountingTools::new();
        let sleeper = NoopSleeper;
        let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let mut state = engine.new_turn(prompt(), free_budget());
        let mut outcome = engine.run_step(&mut state, &events).await;
        while outcome == StepOutcome::Continue {
            outcome = engine.run_step(&mut state, &events).await;
        }
        (outcome, drain(&mut rx), state.into_messages())
    };

    let whole_turn = {
        let provider = ScriptedProvider::new(script());
        let tools = CountingTools::new();
        let sleeper = NoopSleeper;
        let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let mut messages = prompt();
        let mut budget = free_budget();
        let outcome = engine
            .run_turn_with_sender(&mut messages, &mut budget, &events)
            .await;
        (outcome, drain(&mut rx), messages)
    };

    assert_eq!(
        stepwise.0,
        StepOutcome::Done {
            text: "all done".into(),
            cost_usd: 0.0002
        }
    );
    assert!(matches!(whole_turn.0, TurnOutcome::Completed { .. }));
    assert_eq!(
        stepwise.2, whole_turn.2,
        "the same history, appended in the same order"
    );

    // `run_turn` brackets the loop with turn-level `Stage` events a single step
    // does not emit; everything else is the step stream, verbatim.
    let is_stage = |event: &&AgentEvent| event_type(event) != "stage";
    let stepwise_events: Vec<String> = stepwise
        .1
        .iter()
        .filter(is_stage)
        .map(fingerprint)
        .collect();
    let turn_events: Vec<String> = whole_turn
        .1
        .iter()
        .filter(is_stage)
        .map(fingerprint)
        .collect();
    assert_eq!(
        stepwise_events, turn_events,
        "run_step and run_turn must emit the identical event stream"
    );
}

#[tokio::test]
async fn a_turn_resumed_from_a_checkpoint_emits_the_same_downstream_events() {
    // Reference run: every step in one process, events captured per step.
    let mut reference: Vec<Vec<AgentEvent>> = Vec::new();
    let reference_messages = {
        let provider = ScriptedProvider::new(vec![tool_call_result("c1"), text_result("all done")]);
        let tools = CountingTools::new();
        let sleeper = NoopSleeper;
        let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let mut state = engine.new_turn(prompt(), free_budget());
        loop {
            let outcome = engine.run_step(&mut state, &events).await;
            reference.push(drain(&mut rx));
            if outcome != StepOutcome::Continue {
                break;
            }
        }
        state.into_messages()
    };
    assert_eq!(reference.len(), 2, "one tool step, one answer step");

    // Run one step, snapshot, and throw the process away.
    let checkpoint = {
        let provider = ScriptedProvider::new(vec![tool_call_result("c1")]);
        let tools = CountingTools::new();
        let sleeper = NoopSleeper;
        let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
        let (tx, _rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let mut state = engine.new_turn(prompt(), free_budget());
        assert_eq!(
            engine.run_step(&mut state, &events).await,
            StepOutcome::Continue
        );
        // Through the wire form, not the in-memory value: a real host stores
        // bytes, so the bytes are what must resume.
        let json = crate::encode_checkpoint(&state.to_checkpoint()).expect("encode");
        crate::decode_checkpoint(&json).expect("decode")
    };
    assert_eq!(
        checkpoint.step, 1,
        "resume lands on the step that never ran"
    );

    // Resume in a fresh engine with only the remaining script.
    let provider = ScriptedProvider::new(vec![text_result("all done")]);
    let tools = CountingTools::new();
    let resumed_tool_calls = tools.calls.clone();
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = EventSender::new(tx);
    let mut state = engine.resume_turn(checkpoint);

    let outcome = engine.run_step(&mut state, &events).await;
    let resumed_events = drain(&mut rx);
    assert_eq!(
        outcome,
        StepOutcome::Done {
            text: "all done".into(),
            cost_usd: 0.0002,
        },
        "a resumed turn carries the spend it already made"
    );
    assert_eq!(
        resumed_tool_calls.load(Ordering::SeqCst),
        0,
        "resuming must not re-run the step that already committed"
    );
    assert_eq!(
        state.messages(),
        reference_messages.as_slice(),
        "and lands on the identical transcript"
    );

    let expected: Vec<String> = reference[1]
        .iter()
        .filter(|e| !is_receipt(e))
        .map(fingerprint)
        .collect();
    let actual: Vec<String> = resumed_events
        .iter()
        .filter(|e| !is_receipt(e))
        .map(fingerprint)
        .collect();
    assert_eq!(
        actual, expected,
        "the downstream event stream must be indistinguishable from the un-interrupted run"
    );

    // The one documented difference, asserted rather than assumed: the block
    // registry is a per-turn memo the checkpoint drops, so a resumed turn
    // re-registers the blocks the reference run had already seen.
    let reference_receipts = reference[1].iter().filter(|e| is_receipt(e)).count();
    let resumed_receipts = resumed_events.iter().filter(|e| is_receipt(e)).count();
    assert!(
        resumed_receipts > reference_receipts,
        "resuming re-registers blocks ({resumed_receipts} vs {reference_receipts}) — that is the \
         documented cost of not serializing the receipt ledger"
    );
}

#[tokio::test]
async fn a_mid_turn_cancel_stops_at_the_next_boundary_with_a_valid_transcript() {
    // The token trips from INSIDE the tool call — the host cancelling while a
    // tool is in flight. The step must still finish and commit.
    let provider =
        ScriptedProvider::new(vec![tool_call_result("c1"), text_result("never reached")]);
    let cancel = CancelToken::new();
    let mut tools = CountingTools::new();
    tools.cancel_on_call = Some(cancel.clone());
    let tool_calls = tools.calls.clone();
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let provider_calls = provider.calls.clone();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = EventSender::new(tx);
    let mut state = engine
        .new_turn(prompt(), free_budget())
        .with_cancel_token(cancel.clone());

    // Step 0 runs to completion despite the cancel arriving mid-tool.
    assert_eq!(
        engine.run_step(&mut state, &events).await,
        StepOutcome::Continue,
        "cancellation must never interrupt a step already in flight"
    );
    assert!(cancel.is_cancelled());
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1, "the tool ran, once");

    // Step 1 never reaches the provider.
    let outcome = engine.run_step(&mut state, &events).await;
    match outcome {
        StepOutcome::Aborted { reason, cost_usd } => {
            assert_eq!(reason, CANCELLED_REASON);
            assert!(
                (cost_usd - 0.0001).abs() < 1e-12,
                "the committed step's spend is reported, not discarded"
            );
        }
        other => panic!("expected a cancellation abort, got {other:?}"),
    }
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        1,
        "the cancelled step must not pay for a model call"
    );

    // The whole point: the history is still valid to hand to the next turn.
    let messages = state.into_messages();
    assert!(
        every_call_is_paired(&messages),
        "a cancelled turn must not leave an unpaired tool_use: {messages:?}"
    );
    assert_eq!(
        messages.last().expect("a tool message").role,
        MessageRole::Tool,
        "step 0's real tool result is the tail — nothing synthetic was appended"
    );

    // A cancellation is a decision, not a failure: no Error event, exactly as
    // for the soft stop.
    let types: Vec<String> = drain(&mut rx).iter().map(event_type).collect();
    assert!(
        !types.contains(&"error".to_string()),
        "a cancel must not read as a failure: {types:?}"
    );
}

#[tokio::test]
async fn a_cancel_before_the_first_step_costs_nothing() {
    let provider = ScriptedProvider::new(vec![text_result("never reached")]);
    let provider_calls = provider.calls.clone();
    let tools = CountingTools::new();
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);

    let (tx, _rx) = mpsc::unbounded_channel();
    let events = EventSender::new(tx);
    let cancel = CancelToken::new();
    cancel.cancel();
    let mut state = engine
        .new_turn(prompt(), free_budget())
        .with_cancel_token(cancel);

    let outcome = engine.run_step(&mut state, &events).await;
    assert!(matches!(
        outcome,
        StepOutcome::Aborted { ref reason, cost_usd } if reason == CANCELLED_REASON && cost_usd == 0.0
    ));
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    assert_eq!(state.step(), 0);
}

#[tokio::test]
async fn a_checkpoint_taken_between_steps_round_trips_through_the_facade() {
    let provider = ScriptedProvider::new(vec![tool_call_result("c1"), text_result("done")]);
    let tools = CountingTools::new();
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let (tx, _rx) = mpsc::unbounded_channel();
    let events = EventSender::new(tx);

    let mut budget = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), Some(5.0));
    budget.reseed_session_spend(0.75);
    let mut state = engine.new_turn(prompt(), budget);
    assert_eq!(
        engine.run_step(&mut state, &events).await,
        StepOutcome::Continue
    );

    let json = crate::encode_checkpoint(&state.to_checkpoint()).expect("encode");
    let again = crate::encode_checkpoint(&crate::decode_checkpoint(&json).expect("decode"))
        .expect("re-encode");
    assert_eq!(json, again, "the wire form must be byte-stable");

    let restored = engine.resume_turn(crate::decode_checkpoint(&json).expect("decode"));
    assert_eq!(restored.step(), state.step());
    assert_eq!(restored.messages(), state.messages());
    assert!((restored.budget().spent_usd() - state.budget().spent_usd()).abs() < 1e-12);
    assert!(
        (restored.budget().session_spent_usd() - state.budget().session_spent_usd()).abs() < 1e-12,
        "session spend survives the hop, so a --budget stays a hard ceiling across processes"
    );
    assert_eq!(restored.budget().mode(), BudgetMode::Enforced);
}
