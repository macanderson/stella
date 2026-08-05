// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The Mode-A embedding contract, enforced by what this file is allowed to
//! link (#1494).
//!
//! `docs/spec/engine-embedding.md` promises a host can implement the engine's
//! ports against `stella-engine` alone. That promise was untested and untrue:
//! `Provider` was re-exported while `CompletionRequestRef`, `CompletionResult`
//! and `CompletionUsage` — the types its own signature names — were not, so
//! implementing the crate's primary port required linking `stella-protocol`
//! too. `EngineConfig` was re-exported while `CheckpointSink`, the trait its
//! `checkpoint_sink` field holds, was not, so a durable host could not name the
//! trait it must implement. `stella-serve` had already reached around the
//! facade with `use stella_core::step::CheckpointSink;`.
//!
//! # Why this is an integration test and not a unit test
//!
//! This is the whole point of the file. `stella-engine`'s `[dev-dependencies]`
//! are `tokio` and `async-trait` — **not** `stella-protocol` or `stella-core`.
//! An integration test links the crate under test plus its dev-dependencies,
//! and nothing else, so `use stella_protocol::…` here is a compile error rather
//! than a style violation. A unit test in `src/` could always reach past the
//! facade, which is exactly how the gap survived: `src/tests.rs` did.
//!
//! So this file compiles only while the facade is complete. It needs no
//! assertions to be a witness — building it *is* the assertion — but it drives
//! a real turn anyway, so the re-exported types are exercised rather than
//! merely named.

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::mpsc;

use stella_engine::{
    AgentEvent, BudgetGuard, BudgetMode, CheckpointSink, CompletionMessage, CompletionRequestRef,
    CompletionResult, CompletionUsage, Engine, EngineConfig, Provider, ProviderError, Sleeper,
    ToolExecutor, ToolOutput, ToolSchema, TurnOutcome,
};

/// A host's model transport. Every type in this impl is named through the
/// facade; on the pre-fix crate the three completion types were unreachable
/// and this block did not compile.
struct HostProvider;

#[async_trait]
impl Provider for HostProvider {
    fn id(&self) -> &str {
        "host"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Ok(CompletionResult {
            text: "done".to_string(),
            tool_calls: Vec::new(),
            usage: CompletionUsage {
                reported: true,
                input_tokens: 1,
                output_tokens: 1,
                ..CompletionUsage::default()
            },
            model: "host-model".to_string(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// A durable host's resume-point store — the audience `stella-engine`'s first
/// paragraph names, and the one that could not name `CheckpointSink`.
#[derive(Debug, Default)]
struct HostCheckpoints {
    persisted: Mutex<Vec<String>>,
    cleared: Mutex<bool>,
}

impl CheckpointSink for HostCheckpoints {
    fn persist(&self, json: &str) {
        self.persisted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(json.to_string());
    }

    fn discard(&self) {
        *self.cleared.lock().unwrap_or_else(|p| p.into_inner()) = true;
    }
}

struct NoTools;

#[async_trait]
impl ToolExecutor for NoTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }

    async fn execute(&self, _name: &str, _input: &serde_json::Value) -> ToolOutput {
        ToolOutput::Error {
            message: "no tools".to_string(),
        }
    }
}

struct NoopSleeper;

#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// A host assembles the engine, supplies both ports, and runs a turn — using
/// only what `stella-engine` exports.
#[tokio::test]
async fn a_host_can_drive_a_turn_through_the_facade_alone() {
    let sink: Arc<HostCheckpoints> = Arc::new(HostCheckpoints::default());
    // The field is `Option<Arc<dyn CheckpointSink>>`; naming the trait to make
    // this coercion is what a durable host could not do before #1494.
    let config = EngineConfig {
        checkpoint_sink: Some(Arc::clone(&sink) as Arc<dyn CheckpointSink>),
        ..EngineConfig::default()
    };

    let provider = HostProvider;
    let tools = NoTools;
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, config, &sleeper);

    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut messages = vec![CompletionMessage::user("say done")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    match outcome {
        TurnOutcome::Completed { text, .. } => assert!(
            text.contains("done"),
            "the host's own provider produced the answer: {text}"
        ),
        TurnOutcome::Aborted { reason, .. } => {
            panic!("a turn assembled entirely through the facade should run: {reason}")
        }
    }
}
