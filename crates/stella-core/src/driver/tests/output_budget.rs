// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Reactive recovery from a refused output ceiling at the model-call boundary.
//!
//! A gateway prices a request against the ceiling the caller *asks for*, so a
//! session configured with a large `max_output_tokens` is refused the moment
//! the balance falls below the price of the ask — while still holding credit
//! enough for dozens of real calls. That rejection used to arrive as
//! `ProviderError::Terminal` and end the turn: three benchmark runs were lost
//! or maimed by it, every trial dead against a balance the provider itself
//! said could fund a smaller ask.
//!
//! These tests drive real turns through scripted providers that reject with
//! [`ProviderError::OutputBudgetExceeded`] and assert both sides of the
//! contract: a refused ceiling is re-asked smaller (the witness), and the
//! ladder is latched so endless refusal aborts after a bounded number of
//! paid attempts instead of looping (the death-spiral guard).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;
use stella_protocol::{CompletionResult, ToolSchema};

use super::super::*;

/// Rejects the first `refusals` calls as an unaffordable ceiling, naming
/// `affordable` each time, then completes. Records the ceiling every attempt
/// actually asked for — the whole question this file exists to answer.
struct RefuseCeilingThenComplete {
    refusals: u32,
    affordable: Option<u32>,
    calls: AtomicU32,
    asked: Mutex<Vec<Option<u32>>>,
}

impl RefuseCeilingThenComplete {
    fn new(refusals: u32, affordable: Option<u32>) -> Self {
        Self {
            refusals,
            affordable,
            calls: AtomicU32::new(0),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<Option<u32>> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Provider for RefuseCeilingThenComplete {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn complete_ref(
        &self,
        req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.asked.lock().unwrap().push(req.max_output_tokens);
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.refusals {
            return Err(ProviderError::OutputBudgetExceeded {
                message: "Scripted cannot afford the requested output ceiling (HTTP 402)"
                    .to_string(),
                affordable_output_tokens: self.affordable,
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

/// One real turn against `provider` with a configured output ceiling.
async fn run_turn_with_ceiling(provider: &dyn Provider, ceiling: u32) -> TurnOutcome {
    let tools = NoTools;
    let sleeper = NoSleep;
    let config = EngineConfig {
        max_output_tokens: Some(ceiling),
        ..EngineConfig::default()
    };
    let engine = Engine::with_sleeper(provider, &tools, config, &sleeper);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut messages = vec![CompletionMessage::user("do the thing")];
    let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
    engine.run_turn(&mut messages, &mut budget, &tx).await
}

/// The witness. On `main` the 402 is `ProviderError::Terminal` and the turn
/// aborts on the first call; with recovery the engine re-asks under the
/// figure the provider named and the turn completes.
#[tokio::test]
async fn a_refused_ceiling_is_re_asked_smaller_and_the_turn_completes() {
    let provider = RefuseCeilingThenComplete::new(1, Some(117_676));
    let outcome = run_turn_with_ceiling(&provider, 128_000).await;

    let asked = provider.asked();
    assert_eq!(asked.len(), 2, "expected a retry, got {asked:?}");
    assert_eq!(
        asked[0],
        Some(128_000),
        "the first ask is the configured one"
    );
    let retried = asked[1].expect("the retry must still name a ceiling");
    assert!(
        retried < 117_676,
        "the retry must sit under the stated affordable figure, got {retried}"
    );
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "the turn must survive a refused ceiling: {outcome:?}"
    );
}

/// A provider that names no figure still gets a smaller ask — the engine
/// halves its own rather than inventing one the provider never stated.
#[tokio::test]
async fn an_unnamed_ceiling_still_produces_a_smaller_ask() {
    let provider = RefuseCeilingThenComplete::new(1, None);
    let outcome = run_turn_with_ceiling(&provider, 64_000).await;

    let asked = provider.asked();
    assert_eq!(asked, vec![Some(64_000), Some(32_000)], "{asked:?}");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
}

/// The death-spiral guard: a provider that refuses every ceiling must abort
/// after a bounded number of paid attempts, not clamp forever. The abort is
/// the ordinary terminal shape, indistinguishable from a failure that never
/// had a recovery to spend.
#[tokio::test]
async fn endless_refusal_aborts_after_a_bounded_number_of_attempts() {
    let provider = RefuseCeilingThenComplete::new(u32::MAX, Some(100_000));
    let outcome = run_turn_with_ceiling(&provider, 128_000).await;

    let asked = provider.asked();
    let max_rungs = usize::from(crate::driver::output_budget_recovery::MAX_RECOVERY_RUNGS);
    assert_eq!(
        asked.len(),
        max_rungs + 1,
        "one initial ask plus one per rung, got {asked:?}"
    );
    // Monotonically tighter, so the ladder cannot oscillate.
    for pair in asked.windows(2) {
        assert!(pair[1] < pair[0], "ceilings must only fall: {asked:?}");
    }
    let TurnOutcome::Aborted { reason, .. } = &outcome else {
        panic!("a spent ladder must abort: {outcome:?}");
    };
    assert!(reason.contains("model call failed"), "{reason}");
}
