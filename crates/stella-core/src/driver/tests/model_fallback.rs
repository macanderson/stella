// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Mid-turn provider fallback on an exhausted retry ladder (#2679).
//!
//! Before `driver::model_fallback`, a provider whose retry ladder exhausted
//! ended the turn as `Aborted { Failure }` even when a healthy fallback was
//! configured and resolvable. These tests drive real turns through scripted
//! providers and a scripted [`FallbackResolver`] and assert the contract
//! from every side: the witness (a terminally failing primary is swapped for
//! the resolver's replacement and the turn completes), the bound (one swap
//! per engine — two sick providers never ping-pong), the repair (the
//! transcript the replacement receives is well-paired), the accounting
//! (every failed attempt is billed through `UsageIncomplete`), and the
//! control (no resolver → the pre-#2679 terminal surfacing, byte-identical).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;
use stella_protocol::{CompletionResult, ToolCall, ToolSchema, UsageIncompleteReason};

use super::super::*;
use crate::ports::{FallbackResolver, ResolvedFallback};
use crate::retry::RetryPolicy;

/// Fails every call with the error `build` produces, counting the calls.
struct AlwaysFailing {
    id: &'static str,
    build: fn() -> ProviderError,
    calls: AtomicU32,
}

impl AlwaysFailing {
    fn terminal(id: &'static str) -> Self {
        Self {
            id,
            build: || ProviderError::Terminal("scripted terminal failure".to_string()),
            calls: AtomicU32::new(0),
        }
    }

    fn transport(id: &'static str) -> Self {
        Self {
            id,
            build: || ProviderError::transport("scripted transport failure"),
            calls: AtomicU32::new(0),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Provider for AlwaysFailing {
    fn id(&self) -> &str {
        self.id
    }

    async fn complete_ref(
        &self,
        _req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err((self.build)())
    }
}

/// Completes every call with a fixed answer, recording the exact transcript
/// each call was handed — the repair assertions read what the *replacement*
/// really saw, not what the engine claims it sent.
struct HealthyRecording {
    id: &'static str,
    calls: AtomicU32,
    seen: Mutex<Vec<Vec<CompletionMessage>>>,
}

impl HealthyRecording {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            calls: AtomicU32::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    fn transcripts(&self) -> Vec<Vec<CompletionMessage>> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Provider for HealthyRecording {
    fn id(&self) -> &str {
        self.id
    }

    async fn complete_ref(
        &self,
        req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(req.messages.to_vec());
        Ok(CompletionResult {
            text: "rescued".to_string(),
            tool_calls: Vec::new(),
            usage: stella_protocol::CompletionUsage::default(),
            model: "scripted-fallback".to_string(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// Hands out its provider on every ask, counting resolutions — the count is
/// how the bound test proves a second exhaustion never even *asks* again.
struct ScriptedResolver<'p> {
    to: &'p dyn Provider,
    resolutions: AtomicU32,
}

impl<'p> ScriptedResolver<'p> {
    fn new(to: &'p dyn Provider) -> Self {
        Self {
            to,
            resolutions: AtomicU32::new(0),
        }
    }

    fn resolutions(&self) -> u32 {
        self.resolutions.load(Ordering::SeqCst)
    }
}

impl FallbackResolver for ScriptedResolver<'_> {
    fn resolve_fallback(&self, failed_provider_id: &str) -> Option<ResolvedFallback<'_>> {
        self.resolutions.fetch_add(1, Ordering::SeqCst);
        Some(ResolvedFallback {
            provider: self.to,
            reason: format!("scripted re-resolution away from `{failed_provider_id}`"),
        })
    }
}

struct NoTools;

#[async_trait::async_trait]
impl ToolExecutor for NoTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: String::new(),
        }
    }
}

struct NoSleep;

#[async_trait::async_trait]
impl crate::retry::Sleeper for NoSleep {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// One real turn against `provider` with `resolver` attached (when given),
/// returning the outcome and every event.
async fn run_turn_collecting(
    provider: &dyn Provider,
    resolver: Option<&dyn FallbackResolver>,
    mut messages: Vec<CompletionMessage>,
    config: EngineConfig,
) -> (TurnOutcome, Vec<AgentEvent>) {
    let tools = NoTools;
    let sleeper = NoSleep;
    let mut engine = Engine::with_sleeper(provider, &tools, config, &sleeper);
    if let Some(resolver) = resolver {
        engine = engine.with_fallback_resolver(resolver);
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    (outcome, events)
}

fn goal() -> Vec<CompletionMessage> {
    vec![CompletionMessage::user("do the thing")]
}

/// The #2679 witness: on `main` a terminally failing provider aborts the
/// turn (`model call failed: …`); with the fallback seam the engine
/// re-resolves through the port, announces the swap, and the turn completes
/// on the replacement — with the terminal channels silent.
#[tokio::test]
async fn exhausted_retries_swap_to_the_resolved_fallback_and_the_turn_completes() {
    let primary = AlwaysFailing::terminal("primary");
    let fallback = HealthyRecording::new("fallback");
    let resolver = ScriptedResolver::new(&fallback);
    let (outcome, events) =
        run_turn_collecting(&primary, Some(&resolver), goal(), EngineConfig::default()).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { ref text, .. } if text == "rescued"),
        "the turn must complete on the fallback, got {outcome:?}"
    );
    assert_eq!(primary.calls(), 1, "Terminal bails the ladder on attempt 1");
    assert_eq!(fallback.calls(), 1, "one rescued re-issue");
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ProviderFallback { from, to, .. } if from == "primary" && to == "fallback"
        )),
        "the swap must be announced (L-M7): {events:?}"
    );
    // Withheld from the terminal channels: no observer should tear down a
    // session that recovered.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::RetriesExhausted { .. })),
        "a recovered exhaustion must not report exhausted retries: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::Error {
                retryable: false,
                ..
            }
        )),
        "a recovered exhaustion must not surface a terminal error: {events:?}"
    );
    // …and announced on the same retryable-notice channel overflow recovery
    // uses.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: true } if message.contains("continuing the turn")
        )),
        "the swap must narrate itself: {events:?}"
    );
}

/// The bound: the set-once latch means a fallback that is itself sick is
/// never swapped again — each provider gets exactly one ladder, the resolver
/// is asked exactly once, and the second exhaustion surfaces terminally.
#[tokio::test]
async fn a_sick_fallback_is_never_swapped_again() {
    let primary = AlwaysFailing::terminal("primary");
    let also_sick = AlwaysFailing::terminal("fallback");
    let resolver = ScriptedResolver::new(&also_sick);
    let (outcome, events) =
        run_turn_collecting(&primary, Some(&resolver), goal(), EngineConfig::default()).await;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Aborted { ref reason, kind: AbortKind::Failure, .. }
                if reason.contains("model call failed")
        ),
        "the second exhaustion is terminal, got {outcome:?}"
    );
    assert_eq!(primary.calls(), 1);
    assert_eq!(also_sick.calls(), 1);
    assert_eq!(
        resolver.resolutions(),
        1,
        "a spent latch must refuse before ever asking the resolver again"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ProviderFallback { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::RetriesExhausted { .. }))
            .count(),
        1,
        "exactly one terminal surfacing: {events:?}"
    );
}

/// The control: with no resolver attached the terminal surfacing is the
/// pre-#2679 shape — one `RetriesExhausted`, one non-retryable `Error`, the
/// same abort reason — so relocating the emission out of the attempt ladder
/// changed nothing an observer can see.
#[tokio::test]
async fn without_a_resolver_the_terminal_surfacing_is_unchanged() {
    let primary = AlwaysFailing::terminal("primary");
    let (outcome, events) =
        run_turn_collecting(&primary, None, goal(), EngineConfig::default()).await;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Aborted { ref reason, kind: AbortKind::Failure, .. }
                if reason.contains("model call failed: terminal provider error")
        ),
        "got {outcome:?}"
    );
    assert_eq!(primary.calls(), 1);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::RetriesExhausted {
                    retryable: false,
                    ..
                }
            ))
            .count(),
        1,
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: false }
                if message.contains("terminal provider error")
        )),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ProviderFallback { .. })),
        "no swap may be announced when none is possible: {events:?}"
    );
}

/// The repair: an unanswered `tool_use` in caller-supplied history is closed
/// with a synthetic result before the replacement sees the transcript, so
/// the fallback call cannot be rejected for a pairing violation. Asserted on
/// what the replacement provider actually received.
#[tokio::test]
async fn the_transcript_handed_to_the_fallback_is_well_paired() {
    let primary = AlwaysFailing::terminal("primary");
    let fallback = HealthyRecording::new("fallback");
    let resolver = ScriptedResolver::new(&fallback);
    let mut history = goal();
    history.push(CompletionMessage {
        role: MessageRole::Assistant,
        content: String::new(),
        tool_calls: vec![ToolCall {
            call_id: "call_orphan".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({}),
        }],
        tool_results: Vec::new(),
        attachments: Vec::new(),
    });
    let (outcome, _) =
        run_turn_collecting(&primary, Some(&resolver), history, EngineConfig::default()).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );

    let transcripts = fallback.transcripts();
    let seen = transcripts.first().expect("the fallback ran");
    // Same pairing scan as `close_open_tool_calls`: every tool_use answered.
    let mut unanswered: Vec<&ToolCall> = Vec::new();
    for entry in seen {
        match entry.role {
            MessageRole::Assistant => unanswered.extend(entry.tool_calls.iter()),
            MessageRole::Tool => {
                for result in &entry.tool_results {
                    if let Some(position) = unanswered
                        .iter()
                        .rposition(|call| call.call_id == result.call_id)
                    {
                        unanswered.remove(position);
                    }
                }
            }
            MessageRole::System | MessageRole::User => {}
        }
    }
    assert!(
        unanswered.is_empty(),
        "the replacement must never see an unanswered tool_use: {seen:#?}"
    );
    // …and the stub names why it exists.
    let stubbed = seen.iter().any(|entry| {
        entry.tool_results.iter().any(|result| {
            result.call_id == "call_orphan"
                && matches!(
                    &result.output,
                    ToolOutput::Error { message }
                        if message.contains("switched to a fallback provider")
                )
        })
    });
    assert!(stubbed, "the orphan must be closed by the fallback repair");
}

/// The accounting: every attempt the primary burned before the swap was a
/// paid dispatch and leaves its content-free `UsageIncomplete` envelope —
/// the swap changes what happens next, never whether a failed attempt is
/// billed.
#[tokio::test]
async fn every_attempt_before_the_swap_is_billed_through_usage_incomplete() {
    let primary = AlwaysFailing::transport("primary");
    let fallback = HealthyRecording::new("fallback");
    let resolver = ScriptedResolver::new(&fallback);
    let config = EngineConfig {
        retry_policy: RetryPolicy {
            max_retries: 2,
            ..RetryPolicy::standard()
        },
        ..EngineConfig::default()
    };
    let (outcome, events) = run_turn_collecting(&primary, Some(&resolver), goal(), config).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        primary.calls(),
        3,
        "1 attempt + 2 retries, all on the primary"
    );
    let envelopes = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::UsageIncomplete {
                    reason: UsageIncompleteReason::ProviderError,
                    ..
                }
            )
        })
        .count();
    assert_eq!(envelopes, 3, "one envelope per burned attempt: {events:?}");
}

/// A resolution landing back on the failed provider is a refusal, not a
/// swap: nothing is announced and the failure surfaces terminally.
#[tokio::test]
async fn a_resolution_back_onto_the_failed_provider_is_refused() {
    let primary = AlwaysFailing::terminal("primary");
    // The resolver hands back a provider wearing the SAME id — e.g. a router
    // whose only healthy profile is the one that just failed.
    let same_id = AlwaysFailing::terminal("primary");
    let resolver = ScriptedResolver::new(&same_id);
    let (outcome, events) =
        run_turn_collecting(&primary, Some(&resolver), goal(), EngineConfig::default()).await;

    assert!(
        matches!(
            outcome,
            TurnOutcome::Aborted {
                kind: AbortKind::Failure,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(same_id.calls(), 0, "the refused provider is never called");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ProviderFallback { .. })),
        "a refused swap must not be announced: {events:?}"
    );
}
