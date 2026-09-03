// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The spend gate: what a turn buys, pinned scenario by scenario.
//!
//! A change to the step loop can leave every answer the same and still buy
//! an extra model call. This file is what fails when it does. Each test
//! below drives one turn over scripted ports and pins three numbers: model
//! calls, tool runs, and the cost the turn reports.
//!
//! Read a red run one of two ways. Either the change is a bug, and the fix
//! is in the code. Or the new numbers are the ones you meant, and you edit
//! the pin in this file in the same pull request. A reviewer then reads the
//! new price as a diff line. Never widen a pin to whatever the loop does
//! now: the worth of this file is that intent is written down.
//!
//! The ports are scripted, so the engine does no I/O (architecture rule 2).
//! An answered model call costs a flat quarter dollar here. That price is a
//! round binary number, so a sum of them is exact and a pin can be too.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use stella_core::budget::BudgetGuard;
use stella_core::ports::ToolExecutor;
use stella_core::retry::Sleeper;
use stella_core::{Engine, EngineConfig, TurnOutcome};
use stella_protocol::{
    BudgetMode, CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage,
    Provider, ProviderError, ToolCall, ToolOutput, ToolSchema,
};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;

/// What one answered model call costs the scripted provider.
const CALL_COST_USD: f64 = 0.25;

/// What a turn bought.
///
/// `model_calls` counts attempts, not bills: a refused call reaches the
/// provider and takes wall clock, so it belongs in the number a reviewer
/// reads. `cost_usd` is what the turn reported, which only answered calls
/// add to.
#[derive(Debug, PartialEq)]
struct Spend {
    model_calls: u32,
    tool_calls: u32,
    cost_usd: f64,
}

/// A `Sleeper` that never waits, so a retry ladder costs no test time.
struct NoopSleeper;
#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// A scripted `Provider`: one entry per call, repeating the last entry once
/// the script runs out. Counts every attempt it is handed.
struct ScriptedProvider {
    script: TokioMutex<Vec<Result<CompletionResult, ProviderError>>>,
    calls: Arc<AtomicU32>,
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
        let mut script = self.script.lock().await;
        if script.len() > 1 {
            script.remove(0)
        } else {
            clone_step(&script[0])
        }
    }
}

fn clone_step(
    step: &Result<CompletionResult, ProviderError>,
) -> Result<CompletionResult, ProviderError> {
    match step {
        Ok(result) => Ok(result.clone()),
        Err(error) => Err(error.clone()),
    }
}

/// A tool that always succeeds and counts the times it really ran.
struct CountingTools {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl ToolExecutor for CountingTools {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolOutput::Ok {
            content: "ok".into(),
            data: None,
        }
    }
}

fn answer(text: &str) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: text.into(),
        tool_calls: vec![],
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: CALL_COST_USD,
        finish_reason: None,
    }
}

/// A step that asks for one run of the scripted tool per command given.
/// Each call carries its own id and command, so the two calls of a two-tool
/// step are two units of work rather than one call sent twice.
fn tool_step(commands: &[(&str, &str)]) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: String::new(),
        tool_calls: commands
            .iter()
            .map(|(call_id, command)| ToolCall {
                call_id: (*call_id).into(),
                name: "bash".into(),
                input: serde_json::json!({ "cmd": command }),
            })
            .collect(),
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: CALL_COST_USD,
        finish_reason: None,
    }
}

/// Drive one turn over the scripted ports and report what it bought.
async fn run_turn(script: Vec<Result<CompletionResult, ProviderError>>) -> (TurnOutcome, Spend) {
    let model_calls = Arc::new(AtomicU32::new(0));
    let tool_calls = Arc::new(AtomicU32::new(0));
    let provider = ScriptedProvider {
        script: TokioMutex::new(script),
        calls: model_calls.clone(),
    };
    let tools = CountingTools {
        calls: tool_calls.clone(),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("do the thing"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    let cost_usd = match &outcome {
        TurnOutcome::Completed { cost_usd, .. } | TurnOutcome::Aborted { cost_usd, .. } => {
            *cost_usd
        }
    };
    let spend = Spend {
        model_calls: model_calls.load(Ordering::SeqCst),
        tool_calls: tool_calls.load(Ordering::SeqCst),
        cost_usd,
    };
    (outcome, spend)
}

/// Fail with the scenario named, the pin, and what the loop just bought.
fn assert_spend(scenario: &str, got: &Spend, want: &Spend) {
    assert_eq!(
        got, want,
        "spend gate: the scenario `{scenario}` changed price.\n  \
         pinned: {want:?}\n  bought: {got:?}\n  \
         Fix the loop, or edit this pin in the same pull request."
    );
}

#[tokio::test]
async fn a_one_step_answer_buys_one_model_call() {
    let (outcome, spend) = run_turn(vec![Ok(answer("done"))]).await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: CALL_COST_USD,
        }
    );
    assert_spend(
        "one-step answer",
        &spend,
        &Spend {
            model_calls: 1,
            tool_calls: 0,
            cost_usd: CALL_COST_USD,
        },
    );
}

#[tokio::test]
async fn two_tools_in_one_step_buy_one_model_call_between_them() {
    let (outcome, spend) = run_turn(vec![
        Ok(tool_step(&[("call_1", "ls"), ("call_2", "pwd")])),
        Ok(answer("done")),
    ])
    .await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: 2.0 * CALL_COST_USD,
        }
    );
    assert_spend(
        "two-tool step",
        &spend,
        &Spend {
            model_calls: 2,
            tool_calls: 2,
            cost_usd: 2.0 * CALL_COST_USD,
        },
    );
}

#[tokio::test]
async fn a_rate_limited_call_is_retried_once_and_billed_once() {
    let refused = ProviderError::RateLimited {
        message: "429".into(),
        retry_after_ms: Some(10),
    };
    let (outcome, spend) = run_turn(vec![Err(refused), Ok(answer("done"))]).await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: CALL_COST_USD,
        }
    );
    assert_spend(
        "retry after a 429",
        &spend,
        &Spend {
            model_calls: 2,
            tool_calls: 0,
            cost_usd: CALL_COST_USD,
        },
    );
}

#[tokio::test]
async fn a_stuck_loop_is_steered_once_and_then_stopped() {
    // The script never runs out, so the only thing that can end this turn
    // is loop detection. Three no-progress calls earn one warning and a
    // fourth ends the turn, which is where the pinned four comes from.
    let (outcome, spend) = run_turn(vec![Ok(tool_step(&[("call_1", "ls")]))]).await;

    match &outcome {
        TurnOutcome::Aborted { reason, .. } => {
            assert!(reason.contains("stuck-loop"), "unexpected reason: {reason}");
        }
        other => panic!("expected a stuck-loop abort, got {other:?}"),
    }
    assert_spend(
        "loop-detector trip",
        &spend,
        &Spend {
            model_calls: 4,
            tool_calls: 4,
            cost_usd: 4.0 * CALL_COST_USD,
        },
    );
}
