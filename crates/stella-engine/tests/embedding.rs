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
//! The same shape recurred at `Engine::with_requery` (#3715): the method was
//! reachable through the facade and callable by nobody, because neither the
//! `SteeringRequery` it takes nor the `TurnSignal` that port's one method names
//! could be spelled from this crate. `a_host_can_requery_its_context_plane`
//! below is that gap's witness.
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
//! So this file compiles only while the facade is complete — building it *is*
//! half the assertion. The other half is behavioral and required: both
//! scenarios drive a turn that takes a real `Continue` step, which is the only
//! way the engine reaches `persist_checkpoint` (`driver/drive.rs`) and the only
//! way a re-query is consulted with tool evidence in its signal. A scenario
//! whose provider answers on its first call ends the turn immediately and
//! exercises neither (#3716).

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::mpsc;

use stella_engine::{
    AbortKind, AgentEvent, BudgetGuard, BudgetMode, CheckpointSink, CompletionMessage,
    CompletionRequestRef, CompletionResult, CompletionUsage, Engine, EngineConfig, Provider,
    ProviderError, RECALL_MARKER, Sleeper, SteeringRequery, ToolCall, ToolExecutor, ToolOutput,
    ToolSchema, TurnOutcome, TurnSignal,
};

/// The one tool the host advertises. Its only job is to make the model's first
/// answer a tool call, so the turn takes a committed step before it ends.
const HOST_TOOL: &str = "host_probe";

/// A host's model transport. Every type in this impl is named through the
/// facade; on the pre-fix crate the three completion types were unreachable
/// and this block did not compile.
///
/// Scripted rather than constant: the first answer is a tool call, every
/// answer after it is the final text. That is what makes the turn take a
/// `Continue` step — the boundary where the engine writes a checkpoint and
/// consults the re-query port with a signal that has tool evidence in it.
#[derive(Default)]
struct HostProvider {
    calls: Mutex<u32>,
}

#[async_trait]
impl Provider for HostProvider {
    fn id(&self) -> &str {
        "host"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let call_index = {
            let mut calls = self.calls.lock().unwrap_or_else(|p| p.into_inner());
            let index = *calls;
            *calls += 1;
            index
        };
        let (text, tool_calls) = if call_index == 0 {
            (
                String::new(),
                vec![ToolCall {
                    call_id: "call_1".to_string(),
                    name: HOST_TOOL.to_string(),
                    input: serde_json::json!({}),
                }],
            )
        } else {
            ("done".to_string(), Vec::new())
        };
        Ok(CompletionResult {
            upstream_provider: None,
            text,
            tool_calls,
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

impl HostCheckpoints {
    fn persisted(&self) -> Vec<String> {
        self.persisted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn cleared(&self) -> bool {
        *self.cleared.lock().unwrap_or_else(|p| p.into_inner())
    }
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

/// A host's tool surface: one advertised tool that always succeeds, so the
/// dispatch the provider asks for commits and the turn continues.
#[derive(Default)]
struct HostTools {
    executed: Mutex<Vec<String>>,
}

#[async_trait]
impl ToolExecutor for HostTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: HOST_TOOL.to_string(),
            description: "the host's own tool".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            read_only: true,
            speculation_safe: false,
        }]
    }

    async fn execute(&self, name: &str, _input: &serde_json::Value) -> ToolOutput {
        self.executed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(name.to_string());
        ToolOutput::ok("probed".to_string())
    }
}

struct NoopSleeper;

#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// What one consult of the re-query port was shown, owned so the port can keep
/// it past the borrowed [`TurnSignal`]'s lifetime.
#[derive(Debug, Clone)]
struct SeenSignal {
    prompt: String,
    tools: Vec<String>,
    step: u32,
    since_last_query: u32,
}

/// A host's context plane behind the step-boundary re-query port (#3243
/// Phase 3). Records every consult and answers exactly once, on the first
/// signal carrying tool evidence — the hysteresis and the dedup are the
/// host's, by the port's contract.
#[derive(Default)]
struct HostRequery {
    seen: Mutex<Vec<SeenSignal>>,
    answered: Mutex<bool>,
}

impl HostRequery {
    /// The block this plane injects. It carries [`RECALL_MARKER`] because the
    /// port's contract requires it — a block without the marker is read as a
    /// user turn by the turn-window rule.
    fn block() -> String {
        format!("{RECALL_MARKER} the host's plane answered")
    }

    fn seen(&self) -> Vec<SeenSignal> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[async_trait]
impl SteeringRequery for HostRequery {
    async fn requery(&self, signal: &TurnSignal<'_>) -> Option<String> {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(SeenSignal {
                prompt: signal.prompt.to_string(),
                tools: signal.recent_tool_calls.to_vec(),
                step: signal.step,
                since_last_query: signal.since_last_query,
            });
        let mut answered = self.answered.lock().unwrap_or_else(|p| p.into_inner());
        if *answered || signal.recent_tool_calls.is_empty() {
            return None;
        }
        *answered = true;
        Some(Self::block())
    }
}

/// The answer text out of a turn that must have completed.
fn completed_text(outcome: TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed { text, .. } => text,
        TurnOutcome::Aborted { reason, kind, .. } => {
            // `AbortKind` is matched, not merely named: a host that cannot
            // tell a deliberate stop from a failure is parsing `reason`'s
            // prose, which is what re-exporting it prevents (#3715).
            let sort = match kind {
                AbortKind::DeliberateStop => "a deliberate stop",
                AbortKind::Failure => "a failure",
            };
            panic!("a turn assembled entirely through the facade should run: {reason} ({sort})")
        }
    }
}

/// A host assembles the engine, supplies both ports, and runs a turn — using
/// only what `stella-engine` exports — then reads back the resume points its
/// own [`CheckpointSink`] was handed.
///
/// The checkpoint half is the required part (#3716). `persist_checkpoint`
/// is reached only after a `Continue` step (`driver/drive.rs`), so the turn is
/// scripted to take exactly one: tool call, dispatch, final text. That yields
/// exactly one resume point, and `discard_checkpoint` — which runs on every
/// turn ending — clears it. Break either wiring in `stella-core` and one of
/// the two assertions below fails.
#[tokio::test]
async fn a_host_can_drive_a_turn_through_the_facade_alone() {
    let sink: Arc<HostCheckpoints> = Arc::new(HostCheckpoints::default());
    // The field is `Option<Arc<dyn CheckpointSink>>`; naming the trait to make
    // this coercion is what a durable host could not do before #1494.
    let config = EngineConfig {
        checkpoint_sink: Some(Arc::clone(&sink) as Arc<dyn CheckpointSink>),
        ..EngineConfig::default()
    };

    let provider = HostProvider::default();
    let tools = HostTools::default();
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, config, &sleeper);

    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut messages = vec![CompletionMessage::user("say done")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let text = completed_text(engine.run_turn(&mut messages, &mut budget, &tx).await);
    assert!(
        text.contains("done"),
        "the host's own provider produced the answer: {text}"
    );

    let executed = tools
        .executed
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .len();
    assert_eq!(
        executed, 1,
        "the scripted turn dispatches the host's tool once"
    );

    let persisted = sink.persisted();
    assert_eq!(
        persisted.len(),
        1,
        "one `Continue` step is one resume point handed to the host's sink"
    );
    assert!(
        persisted[0].contains("\"version\""),
        "the host is handed an encoded checkpoint, not an opaque token: {}",
        persisted[0]
    );
    assert!(
        sink.cleared(),
        "a turn that is over must not leave a resume point offering to replay it"
    );
}

/// **Witness (#3715).** A host implements the step-boundary re-query port and
/// attaches it with `Engine::with_requery`, naming nothing but
/// `stella_engine::` paths.
///
/// This test does not compile on the pre-fix crate: `SteeringRequery` and the
/// `TurnSignal` its one method takes were both unreachable through the facade,
/// so the `impl` block could not be written at all — which is what made
/// `with_requery` a method callable by nobody who took the facade at its word.
#[tokio::test]
async fn a_host_can_requery_its_context_plane_through_the_facade_alone() {
    let provider = HostProvider::default();
    let tools = HostTools::default();
    let sleeper = NoopSleeper;
    let requery = HostRequery::default();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_requery(&requery);

    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let mut messages = vec![CompletionMessage::user("probe then say done")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let text = completed_text(engine.run_turn(&mut messages, &mut budget, &tx).await);
    assert!(
        text.contains("done"),
        "the turn still reaches its answer: {text}"
    );

    let seen = requery.seen();
    assert_eq!(
        seen.len(),
        2,
        "the port is consulted at every step boundary of a two-step turn: {seen:?}"
    );
    assert_eq!(
        seen[0].prompt, "probe then say done",
        "the engine hands the port the turn's own goal"
    );
    assert!(
        seen[0].tools.is_empty() && seen[0].step == 0 && seen[0].since_last_query == 0,
        "the opening consult has no evidence yet: {:?}",
        seen[0]
    );
    assert_eq!(
        seen[1].tools,
        vec![HOST_TOOL.to_string()],
        "the second consult carries what the turn has actually called"
    );
    assert_eq!(
        seen[1].step, 1,
        "and names the step it is about to buy context for"
    );

    assert!(
        messages
            .iter()
            .any(|message| message.content == HostRequery::block()),
        "the answered block is appended to the transcript verbatim: {messages:?}"
    );
}
