// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Sibling sub-agent spawns in one step run concurrently
//! (`ToolExecutor::parallel_safe_names`).
//!
//! The dispatch scheduler groups consecutive *read-only* calls and makes
//! every other call its own barrier — which silently serialized sibling
//! `task` calls, because the spawn tool is honestly not `read_only` (the
//! flag is load-bearing for the child nesting fence). The CLI dispatcher
//! was built for concurrent siblings (thread per child, snapshot-carve,
//! delta settle), so the concurrency it protected was unreachable from the
//! model's side: three independent research questions paid the sum of
//! their latencies instead of the max.
//!
//! Pinned from outside the crate through the public API, in the same
//! barrier shape as the driver's own
//! `read_only_calls_in_one_step_execute_concurrently`: the step completes
//! ONLY if both spawn calls are in flight at once, so a scheduler that
//! serializes them deadlocks and the timeout names the failure.

use std::time::Duration;

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
use tokio::sync::mpsc;

struct NoopSleeper;
#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// First call: one step carrying two sibling `task` calls. Second call: done.
struct TwoSpawnsThenDone {
    calls: std::sync::atomic::AtomicU32,
}

#[async_trait]
impl Provider for TwoSpawnsThenDone {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (text, tool_calls) = if call == 0 {
            (
                String::new(),
                vec![
                    ToolCall {
                        call_id: "c1".into(),
                        name: "task".into(),
                        input: serde_json::json!({"prompt": "question one"}),
                    },
                    ToolCall {
                        call_id: "c2".into(),
                        name: "task".into(),
                        input: serde_json::json!({"prompt": "question two"}),
                    },
                ],
            )
        } else {
            ("done".to_string(), Vec::new())
        };
        Ok(CompletionResult {
            text,
            tool_calls,
            usage: CompletionUsage::reported_zero(),
            model: "scripted".into(),
            cost_usd: 0.0001,
            finish_reason: None,
        })
    }
}

/// A `task`-shaped executor whose calls rendezvous on a barrier: the step
/// completes only when both spawns are in flight at the same time. The
/// schema is honestly `read_only: false` (the real spawn tool's posture);
/// the concurrency claim rides `parallel_safe_names`, exactly as the real
/// `ToolRegistry` aggregates it from `Tool::parallel_safe`.
struct BarrierSpawns {
    barrier: tokio::sync::Barrier,
}

#[async_trait]
impl ToolExecutor for BarrierSpawns {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "task".into(),
            description: "delegate research".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        self.barrier.wait().await;
        ToolOutput::Ok {
            content: "finding".into(),
        }
    }
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        std::iter::once("task".to_string()).collect()
    }
}

#[tokio::test]
async fn sibling_task_calls_in_one_step_execute_concurrently() {
    let provider = TwoSpawnsThenDone {
        calls: std::sync::atomic::AtomicU32::new(0),
    };
    let tools = BarrierSpawns {
        barrier: tokio::sync::Barrier::new(2),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("research two questions"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        engine.run_turn(&mut messages, &mut budget, &tx),
    )
    .await
    .expect(
        "two sibling task calls in one step must run concurrently — a scheduler \
         that serializes non-read-only calls deadlocks on the barrier",
    );
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
}

/// The claim must not leak into evidence or speculation semantics: a
/// mutating call between two spawns keeps its barrier. `[task, edit, task]`
/// deadlocks if the scheduler ever groups across the edit, because the two
/// spawns could then rendezvous — so completion within the timeout would be
/// the FAILURE here. Instead the serialized schedule parks the first spawn
/// on the barrier forever; the test asserts the turn does NOT complete.
struct SpawnEditSpawn;

#[async_trait]
impl Provider for SpawnEditSpawn {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Ok(CompletionResult {
            text: String::new(),
            tool_calls: vec![
                ToolCall {
                    call_id: "c1".into(),
                    name: "task".into(),
                    input: serde_json::json!({"prompt": "one"}),
                },
                ToolCall {
                    call_id: "c2".into(),
                    name: "edit_file".into(),
                    input: serde_json::json!({"path": "x"}),
                },
                ToolCall {
                    call_id: "c3".into(),
                    name: "task".into(),
                    input: serde_json::json!({"prompt": "two"}),
                },
            ],
            usage: CompletionUsage::reported_zero(),
            model: "scripted".into(),
            cost_usd: 0.0001,
            finish_reason: None,
        })
    }
}

struct BarrierSpawnsAndEdit {
    barrier: tokio::sync::Barrier,
}

#[async_trait]
impl ToolExecutor for BarrierSpawnsAndEdit {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: "task".into(),
                description: "delegate research".into(),
                input_schema: serde_json::json!({"type": "object"}),
                read_only: false,
                speculation_safe: false,
            },
            ToolSchema {
                name: "edit_file".into(),
                description: "edit".into(),
                input_schema: serde_json::json!({"type": "object"}),
                read_only: false,
                speculation_safe: false,
            },
        ]
    }
    async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
        if name == "task" {
            self.barrier.wait().await;
        }
        ToolOutput::Ok {
            content: "ok".into(),
        }
    }
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        std::iter::once("task".to_string()).collect()
    }
}

#[tokio::test]
async fn a_mutating_call_between_spawns_keeps_its_barrier() {
    let provider = SpawnEditSpawn;
    let tools = BarrierSpawnsAndEdit {
        barrier: tokio::sync::Barrier::new(2),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("research around an edit"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let result = tokio::time::timeout(
        Duration::from_millis(500),
        engine.run_turn(&mut messages, &mut budget, &tx),
    )
    .await;
    assert!(
        result.is_err(),
        "task, edit_file, task must stay three groups — the edit is a barrier, \
         so the two spawns can never rendezvous and the turn must still be \
         parked when the timeout fires"
    );
}
