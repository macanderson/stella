// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The borrowed-history contract across the step extraction (#971).
//!
//! `Engine::run_turn` takes `&mut Vec<CompletionMessage>` and callers rely on
//! seeing every append the moment it happens — including the appends made by a
//! turn whose future is then DROPPED (the caller-side hard cancel). The driver
//! now moves that vector into a `TurnState` for the duration of the turn, so
//! "the caller sees the partial history" stopped being a property of the code
//! and became a property of one `Drop` impl.
//!
//! That is exactly the kind of invariant that breaks silently — nothing fails
//! to compile, the tests that inspect history after a *returned* turn keep
//! passing, and only a REPL that hard-cancels notices its transcript vanished.
//! So it is pinned here, from outside the crate, through the public API.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::budget::BudgetGuard;
use stella_core::event_sender::EventSender;
use stella_core::ports::ToolExecutor;
use stella_core::retry::Sleeper;
use stella_core::{Engine, EngineConfig};
use stella_protocol::{
    BudgetMode, CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage,
    MessageRole, Provider, ProviderError, ToolCall, ToolOutput, ToolSchema,
};

struct NoopSleeper;
#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// Always answers with the same single tool call, so the turn reaches tool
/// dispatch and parks there.
struct AlwaysCallsATool;

#[async_trait]
impl Provider for AlwaysCallsATool {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Ok(CompletionResult {
            text: String::new(),
            tool_calls: vec![ToolCall {
                call_id: "c1".into(),
                name: "bash".into(),
                input: serde_json::json!({"cmd": "sleep"}),
            }],
            usage: CompletionUsage::reported_zero(),
            model: "scripted".into(),
            cost_usd: 0.0001,
            finish_reason: None,
        })
    }
}

/// A tool that never returns within the test's lifetime — the in-flight window
/// a hard cancel lands in.
struct WedgedTool;

#[async_trait]
impl ToolExecutor for WedgedTool {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        ToolOutput::Ok {
            content: "unreachable".into(),
        }
    }
}

#[tokio::test]
async fn dropping_a_turn_mid_tool_still_leaves_the_partial_history_with_the_caller() {
    let provider = AlwaysCallsATool;
    let tools = WedgedTool;
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let events = EventSender::new(tx);
    let mut messages = vec![
        CompletionMessage::system("you are stella"),
        CompletionMessage::user("do the thing"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);

    // Drop the turn future while the tool is still in flight.
    let dropped = tokio::time::timeout(
        Duration::from_millis(250),
        engine.run_turn_with_sender(&mut messages, &mut budget, &events),
    )
    .await;
    assert!(dropped.is_err(), "the turn must still be running");

    // The assistant `tool_use` message was appended before dispatch, and the
    // caller must be able to see it — this is what the documented hard-drop
    // contract ("the caller owns truncating an unpaired tool_use") is about.
    assert_eq!(
        messages.len(),
        3,
        "the caller's history must carry the turn's appends: {messages:?}"
    );
    let assistant = &messages[2];
    assert_eq!(assistant.role, MessageRole::Assistant);
    assert_eq!(assistant.tool_calls.len(), 1);
    assert_eq!(assistant.tool_calls[0].call_id, "c1");
    assert!(
        messages.iter().all(|m| m.tool_results.is_empty()),
        "and it is the UNPAIRED shape the contract warns about, not a repaired one"
    );

    // The money the dropped turn spent is likewise the caller's, not lost with
    // the future: the model call settled before dispatch began.
    assert!(
        budget.spent_usd() > 0.0,
        "a dropped turn's committed spend must reach the caller's guard"
    );
}
