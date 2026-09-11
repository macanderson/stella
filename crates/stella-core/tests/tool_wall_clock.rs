// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A call that names more time than the turn has left never starts
//! (`#6460`, ADR 0041).
//!
//! `bash` takes a time limit from the model, up to ten minutes. Nothing
//! weighed it against the deadline. So a ten-minute call could start at
//! t=350s of an 840-second budget and run to t=950s. The harness kills the
//! whole process on the way past its own limit. Every edit the turn made goes
//! with it. That is how 17 of the 32 failures ended on the 89-task
//! Terminal-Bench 2.1 run of 2026-09-07, with the engine's own deadline never
//! firing.
//!
//! Pinned from outside the crate through the public API, like
//! `parallel_dispatch.rs` beside it. The tool set names a bound through
//! [`ToolExecutor::declared_timeout`], and notes whether it was ever asked to
//! run. So this holds the shipped dispatch path, not the small function under
//! it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::budget::BudgetGuard;
use stella_core::ports::ToolExecutor;
use stella_core::{Engine, EngineConfig, TurnCapabilities, TurnOutcome};
use stella_protocol::{
    AgentEvent, BudgetMode, CompletionMessage, CompletionRequestRef, CompletionResult,
    CompletionUsage, Provider, ProviderError, ToolCall, ToolOutput, ToolSchema,
};
use stella_time::test_util::NoopSleeper;
use tokio::sync::mpsc;

/// First call: one long shell command. Second call: an answer. The turn only
/// gets there because a refusal is a result it can go on from.
struct OneShellCallThenDone {
    timeout_secs: u64,
    calls: AtomicU32,
}

#[async_trait]
impl Provider for OneShellCallThenDone {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let (text, tool_calls) = if call == 0 {
            (
                String::new(),
                vec![ToolCall {
                    call_id: "c1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({
                        "command": "make test",
                        "timeout_secs": self.timeout_secs,
                    }),
                }],
            )
        } else {
            ("done".to_string(), Vec::new())
        };
        Ok(CompletionResult {
            upstream_provider: None,
            text,
            tool_calls,
            usage: CompletionUsage::reported_zero(),
            model: "scripted".into(),
            cost_usd: 0.0001,
            finish_reason: None,
        })
    }
}

/// A shell-shaped tool set. It names the bound it would hold the call to, and
/// notes whether the engine ever asked it to run.
struct DeclaredShell {
    ran: Arc<AtomicBool>,
}

#[async_trait]
impl ToolExecutor for DeclaredShell {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a shell command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
            speculation_safe: false,
        }]
    }

    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        self.ran.store(true, Ordering::SeqCst);
        ToolOutput::Ok {
            content: "ok".into(),
            data: None,
        }
    }

    /// The same answer the real registry gives for `bash`: what the model
    /// asked for, which is the bound the spawn would arm.
    fn declared_timeout(&self, _name: &str, input: &Value) -> Option<Duration> {
        input
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .map(Duration::from_secs)
    }
}

/// Run one turn under `turn_budget`, asking for a shell call of
/// `timeout_secs`. Report whether the tool ran, and every result it made.
async fn run(timeout_secs: u64, turn_budget: Option<Duration>) -> (bool, Vec<ToolOutput>) {
    let provider = OneShellCallThenDone {
        timeout_secs,
        calls: AtomicU32::new(0),
    };
    let ran = Arc::new(AtomicBool::new(false));
    let tools = DeclaredShell { ran: ran.clone() };
    let sleeper = NoopSleeper;
    let config = EngineConfig {
        turn_budget,
        ..EngineConfig::default()
    };
    let engine = Engine::assemble(
        &provider,
        &tools,
        config,
        &sleeper,
        TurnCapabilities::none(),
    );
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("run the suite"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        engine.run_turn(&mut messages, &mut budget, &tx),
    )
    .await
    .expect("the turn must settle");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "a refused call is a result the turn goes on from, never an abort: {outcome:?}"
    );

    drop(tx);
    let mut results = Vec::new();
    while let Some(event) = rx.recv().await {
        if let AgentEvent::ToolResult { output, .. } = event {
            results.push(output);
        }
    }
    (ran.load(Ordering::SeqCst), results)
}

/// **The witness for `#6460`.** A ten-minute call on a five-minute turn never
/// reaches the tool. The model is told in seconds what it may ask for. On the
/// old dispatch the call runs, and the turn is killed from outside with
/// nothing sent.
#[tokio::test]
async fn a_call_longer_than_the_turn_has_left_never_starts() {
    let (ran, results) = run(600, Some(Duration::from_secs(300))).await;

    assert!(
        !ran,
        "a 600s call must not start on a turn with 300s of wall clock left"
    );
    let [ToolOutput::Error { message, .. }] = results.as_slice() else {
        panic!("the refused call must still be answered exactly once: {results:?}");
    };
    assert!(
        message.contains("600s"),
        "the refusal names the limit the call declared: {message}"
    );
    let named = message
        .split_once("only ")
        .and_then(|(_, tail)| tail.split_once("s of this turn's"))
        .and_then(|(secs, _)| secs.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("the refusal names the seconds left: {message}"));
    assert!(
        (290..=300).contains(&named),
        "the seconds named are the turn's own clock less what is held back, \
         so the model can shorten the call rather than guess: {message}"
    );
}

/// The control: the same turn, a call that fits, nothing refused. Without it
/// the test above would pass on an engine that had stopped running tools.
#[tokio::test]
async fn a_call_that_fits_the_turn_still_runs() {
    let (ran, results) = run(60, Some(Duration::from_secs(300))).await;

    assert!(ran, "a 60s call fits a turn with 300s left and must run");
    assert_eq!(
        results,
        vec![ToolOutput::Ok {
            content: "ok".into(),
            data: None
        }]
    );
}

/// A turn nobody is timing acts as it did before the clamp. The clamp narrows
/// a deadline somebody set. It makes none up.
#[tokio::test]
async fn an_untimed_turn_runs_the_longest_call_there_is() {
    let (ran, _) = run(600, None).await;

    assert!(
        ran,
        "with no turn budget configured there is no clock to decline against"
    );
}
