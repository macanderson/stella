// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Sub-agent primitive tests (#922).
//!
//! The load-bearing one is
//! [`the_parent_transcript_does_not_grow_by_the_childs_intermediate_work`] —
//! the witness for the whole feature. Everything else pins one of the five
//! contracts in the module docs.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_protocol::{
    BudgetMode, CompletionRequestRef, CompletionResult, CompletionUsage, ProviderError, ToolCall,
    ToolOutput, ToolSchema,
};
use tokio::sync::mpsc;

use super::*;
use crate::budget::BudgetOutcome;
use crate::ports::TurnGate;
use crate::retry::Sleeper;

// ---- fakes -----------------------------------------------------------

pub(crate) struct NoSleep;
#[async_trait]
impl Sleeper for NoSleep {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// A provider that returns a fixed sequence of results, then errors.
pub(crate) struct ScriptedProvider {
    script: Mutex<Vec<Result<CompletionResult, ProviderError>>>,
    calls: AtomicU32,
}

impl ScriptedProvider {
    pub(crate) fn new(script: Vec<Result<CompletionResult, ProviderError>>) -> Self {
        Self {
            script: Mutex::new(script),
            calls: AtomicU32::new(0),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    // `complete_ref` is the trait's required method; `complete` is a default
    // that delegates to it. Overriding the default left this double missing the
    // real one, so the crate's tests stopped compiling.
    async fn complete_ref(
        &self,
        _request: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut script = self.script.lock().unwrap();
        if script.is_empty() {
            return Err(ProviderError::Terminal("script exhausted".into()));
        }
        script.remove(0)
    }
}

/// One read-only tool and one mutating tool, counting executions of each.
#[derive(Default)]
pub(crate) struct MixedTools {
    pub(crate) reads: AtomicUsize,
    pub(crate) writes: AtomicUsize,
}

#[async_trait]
impl ToolExecutor for MixedTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
                read_only: true,
                speculation_safe: false,
            },
            ToolSchema {
                name: "write_file".into(),
                description: "write a file".into(),
                input_schema: json!({"type": "object"}),
                read_only: false,
                speculation_safe: false,
            },
        ]
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        match name {
            "read_file" => {
                self.reads.fetch_add(1, Ordering::SeqCst);
                ToolOutput::Ok {
                    // Deliberately bulky: this is the content a parent would
                    // otherwise be carrying for the rest of the session. Keyed
                    // on the input so distinct reads produce distinct output —
                    // identical output from identical arguments is a stuck
                    // loop, and `crate::loop_detect` is right to abort it.
                    content: format!("{input}{}", "x".repeat(4_000)),
                    data: None,
                }
            }
            "write_file" => {
                self.writes.fetch_add(1, Ordering::SeqCst);
                ToolOutput::Ok {
                    content: "written".into(),
                    data: None,
                }
            }
            other => ToolOutput::error(format!("no such tool {other}")),
        }
    }
}

pub(crate) fn text_result(text: &str, cost: f64) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: text.into(),
        tool_calls: vec![],
        usage: CompletionUsage {
            reported: true,
            ..CompletionUsage::default()
        },
        model: "scripted".into(),
        cost_usd: cost,
        finish_reason: None,
    }
}

pub(crate) fn tool_call_result(name: &str, call_id: &str, cost: f64) -> CompletionResult {
    // `call_id` doubles as the argument so consecutive calls differ — see
    // `MixedTools::execute` on why identical calls are a loop, not a fixture.
    CompletionResult {
        upstream_provider: None,
        text: String::new(),
        tool_calls: vec![ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            input: json!({ "path": call_id }),
        }],
        usage: CompletionUsage {
            reported: true,
            ..CompletionUsage::default()
        },
        model: "scripted".into(),
        cost_usd: cost,
        finish_reason: None,
    }
}

fn drain(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

/// The `Finished` phase of the (single) child in a stream.
fn finished(events: &[AgentEvent]) -> SubAgentPhase {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::SubAgent {
                phase: phase @ SubAgentPhase::Finished { .. },
            } => Some(phase.clone()),
            _ => None,
        })
        .expect("a spawn always emits exactly one Finished")
}

mod failure_and_events;
mod seams;
mod spend_and_cancellation;
mod tools_and_budget;
mod witness;
