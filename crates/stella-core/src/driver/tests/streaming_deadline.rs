// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The model deadline counts idle time. It does not count total time.
//!
//! A model that keeps sending parts is still at work. It may take a long
//! time. That is fine. A hard task can run for many minutes.
//!
//! Each test here starts such a call. Each one shows the deadline lets it
//! run. One sends text. The other sends the parts of a tool call.

use std::time::Duration;

use super::*;

/// Sends one part every `interval`, and never stops. It is at work the
/// whole time. A model can do this on a hard task. It may last much longer
/// than any fixed clock bound.
struct SlowStreamingProvider {
    interval: Duration,
    calls: Arc<AtomicU32>,
}
#[async_trait]
impl Provider for SlowStreamingProvider {
    fn id(&self) -> &str {
        "slow-streaming"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResultAlias, ProviderError> {
        unreachable!("the engine always takes the observed path")
    }
    async fn complete_observed_ref(
        &self,
        _req: CompletionRequestRef<'_>,
        observer: &dyn stella_protocol::provider::ToolCallObserver,
    ) -> Result<CompletionResultAlias, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        loop {
            tokio::time::sleep(self.interval).await;
            observer.text_delta("thinking…");
        }
    }
}

/// The line the deadline must draw. This model sends a part every 20ms. The
/// deadline is 50ms. A clock bound would cut the call at 50ms, though a part
/// came in 20ms ago. An idle bound lets it run. That is right. It is at work.
///
/// The test shows the turn does not stop in a span many times the deadline.
/// It is still going when the test times out, so the call is dropped in
/// flight.
#[tokio::test]
async fn a_streaming_generation_outlives_the_deadline_because_it_is_not_stalled() {
    let calls = Arc::new(AtomicU32::new(0));
    let provider = SlowStreamingProvider {
        interval: Duration::from_millis(20),
        calls: calls.clone(),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let config = EngineConfig {
        model_timeout: Some(Duration::from_millis(50)),
        ..EngineConfig::default()
    };
    let seams = TurnCapabilities::none();
    let engine = Engine::assemble(&provider, &tools, config, &sleeper, seams);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = tokio::time::timeout(
        Duration::from_millis(600),
        engine.run_turn(&mut messages, &mut budget, &tx),
    )
    .await;

    assert!(
        outcome.is_err(),
        "a provider that keeps streaming must not trip the deadline: {outcome:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the streaming call is never re-issued"
    );
    drain_events(&mut rx);
}

/// Sends only tool-call content: parts of the input, and whole calls it
/// names. It sends no text at all. A model writing one big file looks like
/// this. A tap that watches text alone sees silence.
struct CallOnlyStreamingProvider {
    interval: Duration,
    calls: Arc<AtomicU32>,
}
#[async_trait]
impl Provider for CallOnlyStreamingProvider {
    fn id(&self) -> &str {
        "call-only-streaming"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResultAlias, ProviderError> {
        unreachable!("the engine always takes the observed path")
    }
    async fn complete_observed_ref(
        &self,
        _req: CompletionRequestRef<'_>,
        observer: &dyn stella_protocol::provider::ToolCallObserver,
    ) -> Result<CompletionResultAlias, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut announced = false;
        loop {
            tokio::time::sleep(self.interval).await;
            // Take turns with the two tool-side signs of life: a part of
            // the input, then a whole named call that changes state (the
            // gate holds that one, and it still means the model is at work).
            if announced {
                observer.tool_input_delta();
            } else {
                observer.tool_call_streamed(&ToolCall {
                    call_id: "call_w".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"cmd": "true"}),
                });
                announced = true;
            }
        }
    }
}

/// The blind spot in an idle bound. A call that sends only tool content is
/// still at work. It must not be cut off as stuck.
///
/// Put the tick on the gate's event sender and only text goes through it. A
/// call that writes one big file then looks silent. It dies at the deadline,
/// and the whole call is paid for.
#[tokio::test]
async fn a_call_only_stream_outlives_the_deadline_because_it_is_not_stalled() {
    let calls = Arc::new(AtomicU32::new(0));
    let provider = CallOnlyStreamingProvider {
        interval: Duration::from_millis(20),
        calls: calls.clone(),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let config = EngineConfig {
        model_timeout: Some(Duration::from_millis(50)),
        ..EngineConfig::default()
    };
    let seams = TurnCapabilities::none();
    let engine = Engine::assemble(&provider, &tools, config, &sleeper, seams);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = tokio::time::timeout(
        Duration::from_millis(600),
        engine.run_turn(&mut messages, &mut budget, &tx),
    )
    .await;

    assert!(
        outcome.is_err(),
        "a call-only stream is still an answering provider — the deadline \
         must not fire: {outcome:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the streaming call is never re-issued"
    );
    drain_events(&mut rx);
}
