// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Witnesses for the step-boundary context re-query (#3243 Phase 3).
//!
//! Its own file rather than more of `driver/tests.rs`, which is closed to
//! growth under the file-size ratchet — the same reason
//! `memory/tests/steering_selection.rs` exists on the CLI side.

use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::event::BudgetMode;
use stella_protocol::{
    CompletionRequestRef, CompletionResult, CompletionUsage, ProviderError, ToolCall, ToolSchema,
};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;

use super::*;
use crate::budget::BudgetGuard;
use crate::ports::SteeringRequery;
use crate::retry::Sleeper;

struct NoopSleeper;
#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

struct OkTools;
#[async_trait]
impl ToolExecutor for OkTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Error {
            message: "command not found".into(),
            class: Some(stella_protocol::ErrorClass::NotFound),
        }
    }
}

struct ScriptedProvider {
    script: TokioMutex<Vec<CompletionResult>>,
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
        let mut script = self.script.lock().await;
        Ok(if script.len() > 1 {
            script.remove(0)
        } else {
            script[0].clone()
        })
    }
}

fn text_result(text: &str) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
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
        upstream_provider: None,
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

/// A signal, owned, so the port can record what it was shown across steps.
#[derive(Debug, Clone, PartialEq)]
struct SeenSignal {
    prompt: String,
    tools: Vec<String>,
    errors: Vec<String>,
    step: u32,
    since_last_query: u32,
}

/// Records every signal, answers `Some(block)` exactly once — on the first
/// consult that has tool evidence.
struct RecordingRequery {
    seen: std::sync::Mutex<Vec<SeenSignal>>,
    answered: AtomicU32,
    block: String,
}
#[async_trait]
impl SteeringRequery for RecordingRequery {
    async fn requery(&self, signal: &crate::steering::TurnSignal<'_>) -> Option<String> {
        self.seen.lock().unwrap().push(SeenSignal {
            prompt: signal.prompt.to_string(),
            tools: signal.recent_tool_calls.to_vec(),
            errors: signal.errors_seen.iter().map(|e| e.to_string()).collect(),
            step: signal.step,
            since_last_query: signal.since_last_query,
        });
        if !signal.recent_tool_calls.is_empty() && self.answered.load(Ordering::SeqCst) == 0 {
            self.answered.fetch_add(1, Ordering::SeqCst);
            return Some(self.block.clone());
        }
        None
    }
}

/// **Witness (#3243 Phase 3).** The engine consults the re-query port at
/// every step boundary with a signal populated from the turn's own
/// transcript, and appends the answered block to history. Fails on base,
/// where no such port exists: the signal fields it asserts are populated by
/// the engine, not by any host.
#[tokio::test]
async fn the_requery_port_sees_live_turn_evidence_and_its_block_lands_in_history() {
    let provider = ScriptedProvider {
        script: TokioMutex::new(vec![tool_call_result("call_1"), text_result("done")]),
    };
    let tools = OkTools;
    let sleeper = NoopSleeper;
    let requery = RecordingRequery {
        seen: std::sync::Mutex::new(Vec::new()),
        answered: AtomicU32::new(0),
        block: format!(
            "{}\n\nRelevant context:\n- fresh",
            crate::receipts::RECALL_MARKER
        ),
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_requery(&requery);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("fix the flaky test"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "scripted turn completes: {outcome:?}"
    );

    let seen = requery.seen.lock().unwrap().clone();
    assert!(
        seen.len() >= 2,
        "consulted at every step boundary: {seen:?}"
    );
    let first = &seen[0];
    assert_eq!(
        (first.prompt.as_str(), first.tools.as_slice(), first.step),
        ("fix the flaky test", &[][..], 0),
        "step 0 precedes any tool call"
    );
    let second = &seen[1];
    assert_eq!(
        second.tools,
        vec!["bash".to_string()],
        "the turn's calls are the signal"
    );
    assert_eq!(
        second.errors,
        vec!["not_found".to_string()],
        "a failed call's error class rides the signal: {second:?}"
    );
    assert_eq!(
        (second.step, second.since_last_query),
        (1, 1),
        "the hysteresis pair counts steps and steps-since-answer"
    );

    assert!(
        messages.iter().any(|m| m.role == MessageRole::User
            && m.content.starts_with(crate::receipts::RECALL_MARKER)
            && m.content.contains("fresh")),
        "the answered block landed in history as a marker message"
    );
}

/// After a `Some` answer the counter resets: the next consult reports
/// `since_last_query == 0`, so a host's hysteresis can meter re-queries per
/// answer rather than per turn.
#[tokio::test]
async fn since_last_query_resets_when_the_port_answers() {
    let provider = ScriptedProvider {
        script: TokioMutex::new(vec![
            tool_call_result("call_1"),
            tool_call_result("call_2"),
            text_result("done"),
        ]),
    };
    let tools = OkTools;
    let sleeper = NoopSleeper;
    let requery = RecordingRequery {
        seen: std::sync::Mutex::new(Vec::new()),
        answered: AtomicU32::new(0),
        block: format!(
            "{}\n\nRelevant context:\n- fresh",
            crate::receipts::RECALL_MARKER
        ),
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_requery(&requery);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("fix the flaky test"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let _ = engine.run_turn(&mut messages, &mut budget, &tx).await;

    let seen = requery.seen.lock().unwrap().clone();
    // Consult 0: no tools yet, None → counter 1. Consult 1: tools present,
    // answers Some → counter resets. Consult 2 reads the reset.
    assert!(seen.len() >= 3, "three step boundaries: {seen:?}");
    assert_eq!(seen[1].since_last_query, 1);
    assert_eq!(
        seen[2].since_last_query, 0,
        "an answered re-query resets the hysteresis counter"
    );
}
