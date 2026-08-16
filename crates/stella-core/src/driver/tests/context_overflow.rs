// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Reactive context-overflow recovery at the model-call boundary (#2680).
//!
//! Before `driver::overflow_recovery`, a provider rejecting a request as too
//! large surfaced as an unretryable error and aborted the turn — the
//! pre-emptive compaction pass never got a second opinion on its estimate.
//! These tests drive real turns through scripted providers that reject with
//! [`ProviderError::ContextOverflow`] and assert the recovery contract from
//! both sides: a rejected call is retried after a forced compaction clamp
//! (the witness), and the ladder is latched so repeated rejection aborts
//! after a bounded number of paid attempts instead of looping (the
//! death-spiral guard).

use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;
use stella_protocol::{CompletionResult, ToolSchema, UsageIncompleteReason};

use super::super::*;

/// Rejects the first `overflows` calls as context overflow, then completes.
struct OverflowThenComplete {
    overflows: u32,
    calls: AtomicU32,
}

impl OverflowThenComplete {
    fn new(overflows: u32) -> Self {
        Self {
            overflows,
            calls: AtomicU32::new(0),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Provider for OverflowThenComplete {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn complete_ref(
        &self,
        _req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.overflows {
            return Err(ProviderError::ContextOverflow {
                message: "Scripted rejected the request as exceeding the model's context \
                          window (HTTP 400): prompt is too long"
                    .to_string(),
            });
        }
        Ok(CompletionResult {
            upstream_provider: None,
            text: "recovered".to_string(),
            tool_calls: Vec::new(),
            usage: stella_protocol::CompletionUsage::default(),
            model: "scripted-strong".to_string(),
            cost_usd: 0.0,
            finish_reason: None,
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
            data: None,
        }
    }
}

struct NoSleep;

#[async_trait::async_trait]
impl crate::retry::Sleeper for NoSleep {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// One real turn against `provider`, returning the outcome and every event.
async fn run_turn_collecting(provider: &dyn Provider) -> (TurnOutcome, Vec<AgentEvent>) {
    let tools = NoTools;
    let sleeper = NoSleep;
    let engine = Engine::with_sleeper(provider, &tools, EngineConfig::default(), &sleeper);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut messages = vec![CompletionMessage::user("do the thing")];
    let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    (outcome, events)
}

/// The #2680 witness: on `main` a context-overflow rejection aborts the turn
/// (`model call failed: …`); with reactive recovery the engine compacts under
/// a forced clamp, re-issues the call, and the turn completes.
#[tokio::test]
async fn a_context_overflow_is_recovered_by_forced_compaction_and_a_reissued_call() {
    let provider = OverflowThenComplete::new(1);
    let (outcome, events) = run_turn_collecting(&provider).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { ref text, .. } if text == "recovered"),
        "the turn must complete after recovery, got {outcome:?}"
    );
    assert_eq!(provider.calls(), 2, "one rejection, one recovered re-issue");

    // The overflow was announced on the retryable-notice channel…
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: true } if message.contains("overflow recovery")
        )),
        "recovery must be announced: {events:?}"
    );
    // …and withheld from the terminal channels: no observer should tear down
    // a session that recovered.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::RetriesExhausted { .. })),
        "a recovered overflow must not report exhausted retries: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::Error {
                retryable: false,
                ..
            }
        )),
        "a recovered overflow must not surface a terminal error: {events:?}"
    );
}

/// The bound / death-spiral guard: a provider that rejects every call gets
/// exactly `1 + MAX_RECOVERY_RUNGS` attempts, then the turn aborts with the
/// overflow surfaced terminally — never an unbounded error → recover → error
/// loop. The latch never re-arms within the turn.
#[tokio::test]
async fn repeated_overflow_is_latched_and_aborts_after_the_ladder_is_spent() {
    let provider = OverflowThenComplete::new(u32::MAX);
    let (outcome, events) = run_turn_collecting(&provider).await;

    let expected_calls = 1 + u32::from(overflow_recovery::MAX_RECOVERY_RUNGS);
    assert_eq!(
        provider.calls(),
        expected_calls,
        "the ladder is the bound on paid attempts"
    );
    assert!(
        matches!(
            outcome,
            TurnOutcome::Aborted { ref reason, kind: AbortKind::Failure, .. }
                if reason.contains("context overflow")
        ),
        "a spent ladder surfaces the overflow terminally, got {outcome:?}"
    );
    // The terminal surfacing matches the unrecovered shape: one
    // RetriesExhausted plus one non-retryable Error.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::RetriesExhausted { .. }))
            .count(),
        1,
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: false } if message.contains("context overflow")
        )),
        "{events:?}"
    );
}

/// Every rejected attempt was a paid dispatch and must leave its
/// content-free accounting envelope behind — recovery changes what happens
/// *next*, never whether a failed attempt is billed (the `UsageIncomplete`
/// discard-accounting pattern).
#[tokio::test]
async fn every_rejected_overflow_attempt_is_billed_through_usage_incomplete() {
    let provider = OverflowThenComplete::new(u32::MAX);
    let (_, events) = run_turn_collecting(&provider).await;

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
    assert_eq!(
        envelopes,
        1 + usize::from(overflow_recovery::MAX_RECOVERY_RUNGS),
        "one envelope per rejected attempt: {events:?}"
    );
}
