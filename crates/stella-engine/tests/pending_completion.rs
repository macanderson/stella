// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A model call can wait a day for its answer.
//!
//! A batch API can take up to 24 hours to answer. Stella does not hold a live
//! call open that long. The host saves the turn at the last step boundary and
//! may then stop. When the batch answer comes, the host loads the saved turn
//! in a new engine. The new engine asks for the same call again, and the host
//! hands it the batch answer.
//!
//! This test runs that path end to end on a paused clock.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use stella_time::test_util::PausedSleeper;
use tokio::sync::{Notify, mpsc};

use stella_engine::{
    AgentEvent, BudgetGuard, BudgetMode, CheckpointSink, CompletionMessage, CompletionRequestRef,
    CompletionResult, CompletionUsage, Engine, EngineConfig, EventSender, Provider, ProviderError,
    StepOutcome, ToolCall, ToolExecutor, ToolOutput, ToolSchema, TurnCapabilities,
    decode_checkpoint,
};

const TOOL: &str = "read_node";
const BATCH_ANSWER: &str = "link spec to handler";
const ONE_DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// What a provider saw on one call: the messages and the tool names.
type SeenRequest = (serde_json::Value, Vec<String>);

fn seen(req: &CompletionRequestRef<'_>) -> SeenRequest {
    let messages = serde_json::to_value(req.messages).expect("messages serialize");
    let tools = req.tools.iter().map(|tool| tool.name.clone()).collect();
    (messages, tools)
}

fn answer(text: &str, tool_calls: Vec<ToolCall>) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: text.to_string(),
        tool_calls,
        usage: CompletionUsage {
            reported: true,
            input_tokens: 1,
            output_tokens: 1,
            ..CompletionUsage::default()
        },
        model: "batch-model".to_string(),
        cost_usd: 0.0,
        finish_reason: None,
    }
}

/// The first host. Its first call asks for a tool. Its second call is sent
/// to the batch and never comes back in this process.
struct SubmitsToBatch {
    calls: Mutex<u32>,
    submitted: Mutex<Option<SeenRequest>>,
    in_flight: Arc<Notify>,
}

#[async_trait]
impl Provider for SubmitsToBatch {
    fn id(&self) -> &str {
        "batch"
    }

    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let index = {
            let mut calls = self.calls.lock().unwrap_or_else(|p| p.into_inner());
            *calls += 1;
            *calls
        };
        if index == 1 {
            return Ok(answer(
                "",
                vec![ToolCall {
                    call_id: "call_1".to_string(),
                    name: TOOL.to_string(),
                    input: serde_json::json!({}),
                }],
            ));
        }
        *self.submitted.lock().unwrap_or_else(|p| p.into_inner()) = Some(seen(&req));
        self.in_flight.notify_one();
        std::future::pending::<Result<CompletionResult, ProviderError>>().await
    }
}

/// The second host. It holds the batch answer and gives it to the first
/// call it gets.
#[derive(Default)]
struct AnswersFromBatch {
    requests: Mutex<Vec<SeenRequest>>,
}

#[async_trait]
impl Provider for AnswersFromBatch {
    fn id(&self) -> &str {
        "batch"
    }

    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.requests
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(seen(&req));
        Ok(answer(BATCH_ANSWER, Vec::new()))
    }
}

#[derive(Default)]
struct HostTools {
    runs: Mutex<u32>,
}

#[async_trait]
impl ToolExecutor for HostTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: TOOL.to_string(),
            description: "read one graph node".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            read_only: true,
            speculation_safe: false,
        }]
    }

    async fn execute(&self, _name: &str, _input: &serde_json::Value) -> ToolOutput {
        *self.runs.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        ToolOutput::ok("node body".to_string())
    }
}

/// The host's store for saved turns.
#[derive(Default)]
struct SavedTurns {
    saved: Mutex<Vec<String>>,
}

impl CheckpointSink for SavedTurns {
    fn persist(&self, json: &str) {
        self.saved
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(json.to_string());
    }

    fn discard(&self) {
        self.saved.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
}

fn config(store: &Arc<SavedTurns>) -> EngineConfig {
    EngineConfig {
        checkpoint_sink: Some(Arc::clone(store) as Arc<dyn CheckpointSink>),
        ..EngineConfig::default()
    }
}

#[tokio::test(start_paused = true)]
async fn a_turn_saved_during_a_pending_call_resumes_with_the_answer_a_day_later() {
    let store = Arc::new(SavedTurns::default());
    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let events: EventSender = tx.into();
    let sleeper = PausedSleeper;

    // The first host runs one step, saves, and sends the next call to the
    // batch. Then it stops while that call is still out.
    let in_flight = Arc::new(Notify::new());
    let first = SubmitsToBatch {
        calls: Mutex::new(0),
        submitted: Mutex::new(None),
        in_flight: Arc::clone(&in_flight),
    };
    let first_tools = HostTools::default();
    {
        let engine = Engine::assemble(
            &first,
            &first_tools,
            config(&store),
            &sleeper,
            TurnCapabilities::none(),
        );
        let budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let mut state = engine.new_turn(vec![CompletionMessage::user("link this spec")], budget);

        match engine.run_step(&mut state, &events).await {
            StepOutcome::Continue => engine.persist_checkpoint(&state),
            other => panic!("the tool step should commit: {other:?}"),
        }

        tokio::select! {
            outcome = engine.run_step(&mut state, &events) => {
                panic!("the batch call should still be out when the host stops: {outcome:?}")
            }
            () = in_flight.notified() => {}
        }
    }
    assert_eq!(*first_tools.runs.lock().unwrap_or_else(|p| p.into_inner()), 1);
    let submitted = first
        .submitted
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .expect("the second call reached the batch");

    let saved = store.saved.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(saved.len(), 1, "one step boundary gives one saved turn");

    // A day goes by with no process holding the turn.
    tokio::time::advance(ONE_DAY).await;

    // The second host loads the saved turn and gets the same call again.
    let second = AnswersFromBatch::default();
    let second_tools = HostTools::default();
    let engine = Engine::assemble(
        &second,
        &second_tools,
        config(&store),
        &sleeper,
        TurnCapabilities::none(),
    );
    let checkpoint = decode_checkpoint(&saved[0]).expect("the saved turn decodes");
    let mut state = engine.resume_turn(checkpoint);
    assert_eq!(state.step(), 1, "the turn picks up at the step that was out");

    let text = match engine.run_step(&mut state, &events).await {
        StepOutcome::Done { text, .. } => text,
        other => panic!("the batch answer should end the turn: {other:?}"),
    };
    assert_eq!(text, BATCH_ANSWER);

    let requests = second.requests.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(requests.len(), 1, "the second host gets one call");
    assert_eq!(
        requests[0], submitted,
        "the call after resume is the call that went to the batch"
    );
    assert_eq!(
        *second_tools.runs.lock().unwrap_or_else(|p| p.into_inner()),
        0,
        "the tool step done before the stop does not run again"
    );
}
