// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The task deadline reaching the model, not just the journal.
//!
//! `BudgetGuard::task_deadline` has always been consulted at every step
//! boundary and its headroom has always ridden every `BudgetTick`. The model
//! was told none of it, so a turn against a deadline behaved exactly like a
//! turn without one until the moment it was cut off — with a partial answer
//! it would have written down, had anything asked. These tests drive real
//! turns and read what the provider was actually sent.

use std::sync::Mutex;

use serde_json::Value;
use stella_protocol::{CompletionResult, ToolSchema};

use super::super::*;

/// Answers immediately, recording the transcript it was handed.
#[derive(Default)]
struct RecordingProvider {
    seen: Mutex<Vec<String>>,
}

impl RecordingProvider {
    /// Every user-role message text the provider was sent, flattened.
    fn user_texts(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn complete_ref(
        &self,
        req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let mut seen = self.seen.lock().unwrap();
        for message in req.messages {
            if message.role == stella_protocol::MessageRole::User {
                seen.push(message.content.clone());
            }
        }
        Ok(CompletionResult {
            upstream_provider: None,
            text: "done".to_string(),
            tool_calls: Vec::new(),
            usage: stella_protocol::CompletionUsage::default(),
            model: "scripted".to_string(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

struct NoSleep;

#[async_trait::async_trait]
impl crate::retry::Sleeper for NoSleep {
    async fn sleep(&self, _duration_ms: u64) {}
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

/// One real turn with `deadline` of wall clock left, or none at all.
async fn run_turn_with_deadline(
    provider: &RecordingProvider,
    deadline: Option<std::time::Duration>,
) {
    let tools = NoTools;
    let sleeper = NoSleep;
    let engine = Engine::with_sleeper(provider, &tools, EngineConfig::default(), &sleeper);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut messages = vec![CompletionMessage::user("do the thing")];
    let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
    budget.set_task_deadline(deadline.map(|left| std::time::Instant::now() + left));
    engine.run_turn(&mut messages, &mut budget, &tx).await;
}

/// The witness: with a deadline inside the tightest band, the model is told
/// to submit what it has. Without this the transcript carries no mention of
/// the clock at all, which is what "still digging at the wall" looks like
/// from the model's side.
#[tokio::test]
async fn a_near_deadline_reaches_the_model_with_advice() {
    let provider = RecordingProvider::default();
    run_turn_with_deadline(&provider, Some(std::time::Duration::from_secs(90))).await;

    let texts = provider.user_texts();
    let notice = texts
        .iter()
        .find(|text| text.starts_with("[time]"))
        .unwrap_or_else(|| panic!("no deadline notice in the transcript: {texts:?}"));
    assert!(
        notice.contains("submit your best answer"),
        "the notice must change the strategy, not just state a number: {notice}"
    );
}

/// A turn with no deadline armed must be byte-identical to what it always
/// was — nobody was timing this, so nothing is said about time.
#[tokio::test]
async fn no_deadline_puts_no_clock_in_the_prompt() {
    let provider = RecordingProvider::default();
    run_turn_with_deadline(&provider, None).await;

    let texts = provider.user_texts();
    assert!(
        !texts.iter().any(|text| text.starts_with("[time]")),
        "an unarmed deadline must add nothing: {texts:?}"
    );
}

/// Invariant 7: a deadline far out is not a reason to stamp a clock into
/// every prompt. The prefix stays cacheable until a band is actually crossed.
#[tokio::test]
async fn a_distant_deadline_puts_no_clock_in_the_prompt() {
    let provider = RecordingProvider::default();
    run_turn_with_deadline(&provider, Some(std::time::Duration::from_secs(3_600))).await;

    let texts = provider.user_texts();
    assert!(
        !texts.iter().any(|text| text.starts_with("[time]")),
        "a distant deadline must add nothing: {texts:?}"
    );
}
