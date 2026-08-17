// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `StageKind` is the run owner's vocabulary, and the engine does not speak it
//! (#3416).
//!
//! `Engine::drive` used to open every turn with `Stage(Execute)`. Eight of
//! `StageKind`'s eleven values name stages the engine knows nothing about, and
//! the two it emitted were a claim about a caller it cannot see: the same
//! `run_engine_turn` seam drives the pipeline's *witness* stage, so the
//! engine's `Execute` arrived while the run was in `Witness` — a backwards
//! transition `replay::stage_transition_legal` correctly rejects. The staged
//! path could only survive it by dropping the event, which is the two-way
//! coupling #3379 set out to remove.
//!
//! Pinned from outside the crate, through the public API, because nothing
//! fails to compile when a boundary emission comes back: a run owner that also
//! emits one would simply produce two, and the duplicate is invisible until a
//! stream validator or a HUD reads it.

use async_trait::async_trait;
use serde_json::Value;
use stella_core::budget::BudgetGuard;
use stella_core::event_sender::EventSender;
use stella_core::ports::ToolExecutor;
use stella_core::retry::Sleeper;
use stella_core::{Engine, EngineConfig};
use stella_protocol::{
    AgentEvent, BudgetMode, CompletionMessage, CompletionRequestRef, CompletionResult,
    CompletionUsage, Provider, ProviderError, ToolOutput, ToolSchema,
};

struct NoopSleeper;
#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// Answers once, with text and no tool calls, so the turn completes in one
/// step — the shortest turn that still passes through the whole framing.
struct AnswersOnce;

#[async_trait]
impl Provider for AnswersOnce {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Ok(CompletionResult {
            upstream_provider: None,
            text: "done".into(),
            tool_calls: Vec::new(),
            usage: CompletionUsage::reported_zero(),
            model: "scripted".into(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

struct NoTools;

#[async_trait]
impl ToolExecutor for NoTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: "unreachable".into(),
            data: None,
        }
    }
}

#[tokio::test]
async fn a_turn_emits_no_stage_boundary_of_its_own() {
    let provider = AnswersOnce;
    let tools = NoTools;
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let events = EventSender::new(tx);
    let mut messages = vec![
        CompletionMessage::system("you are stella"),
        CompletionMessage::user("do the thing"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);

    engine
        .run_turn_with_sender(&mut messages, &mut budget, &events)
        .await;
    drop(events);

    let mut stages = Vec::new();
    while let Some(event) = rx.recv().await {
        if let AgentEvent::Stage { name, scope } = event {
            stages.push((name, scope));
        }
    }
    assert!(
        stages.is_empty(),
        "the engine must emit no stage boundary — the run owner owns that \
         vocabulary (#3416), and a turn that emits one cannot be wrapped by a \
         run that is in any stage but Execute: {stages:?}"
    );
}
