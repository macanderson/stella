//! Engine-backed paid-call incompleteness witnesses.

use super::*;

#[tokio::test]
async fn exhausted_worker_call_emits_one_content_free_incompleteness_event() {
    let provider = ScriptedProvider {
        id: "anthropic-fallback".into(),
        script: TokioMutex::new(vec![Err(ProviderError::Terminal(
            "private upstream body".into(),
        ))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("work"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Aborted { .. }));
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let incomplete: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::UsageIncomplete { .. }))
        .collect();
    assert_eq!(incomplete.len(), 1);
    assert!(matches!(
        incomplete[0],
        AgentEvent::UsageIncomplete {
            role: stella_protocol::ModelCallRole::Worker,
            provider,
            model,
            reason: stella_protocol::UsageIncompleteReason::ProviderError,
            retries: Some(0),
            ..
        } if provider == "anthropic-fallback"
            && model == stella_protocol::UNKNOWN_MODEL
            && model != provider
    ));
    let wire = serde_json::to_string(incomplete[0]).unwrap();
    assert!(!wire.contains("private upstream body"));
}

/// An adapter that declares the model it is bound to (`Provider::model`), as
/// every shipping adapter in `stella-model` does. `ScriptedProvider` takes the
/// trait's `None` default, which is what the test above pins — this one is the
/// other side, and lives here rather than in `driver/tests.rs` because that
/// file is closed to growth (AGENTS.md § God files).
struct ModelBoundProvider {
    inner: ScriptedProvider,
    model: String,
}

#[async_trait]
impl Provider for ModelBoundProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn model(&self) -> Option<&str> {
        Some(&self.model)
    }

    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResultAlias, ProviderError> {
        self.inner.complete_ref(req).await
    }
}

/// #2831's witness: the row that accounts for a FAILED call names the model
/// that made it.
///
/// Fails on the code before #2831, which wrote `"unknown"` unconditionally at
/// this emit site — 435 such rows in one 16-trial panel. That is exactly the
/// population mid-turn model fallback (#2769) re-resolves from, so the
/// failures that trigger a swap were the ones unable to say what had failed.
#[tokio::test]
async fn a_failed_call_names_the_model_that_made_it() {
    let provider = ModelBoundProvider {
        inner: ScriptedProvider {
            id: "zai".into(),
            script: TokioMutex::new(vec![Err(ProviderError::Terminal("upstream 500".into()))]),
            calls: Arc::new(AtomicU32::new(0)),
        },
        model: "glm-5.2".into(),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("work"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    engine.run_turn(&mut messages, &mut budget, &tx).await;

    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let models: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::UsageIncomplete { model, .. } => Some(model.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        models,
        vec!["glm-5.2"],
        "the failed attempt must be attributable to the model it called, not \
         to the {:?} placeholder",
        stella_protocol::UNKNOWN_MODEL
    );
    // Still content-free: naming the model adds an identifier, never a body.
    // Scoped to the usage envelopes on purpose — `RetriesExhausted` and
    // `Error` carry the provider's message by contract, and it is only this
    // event that promises never to.
    let envelopes: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::UsageIncomplete { .. }))
        .collect();
    let wire = serde_json::to_string(&envelopes).unwrap();
    assert!(!wire.contains("upstream 500"), "{wire}");
}

/// The salvaged accounting has to reach the *event stream*, not just the
/// error: `retry.rs` returns history only for calls that COMMIT, so the
/// per-attempt observer is the sole path by which a doomed attempt's usage can
/// ever be recorded. If it drops the partial, the fix stops at the adapter
/// boundary and nothing downstream is any wiser.
#[tokio::test]
async fn a_failed_attempts_recovered_usage_reaches_the_event_stream() {
    let recovered = stella_protocol::PartialUsage {
        usage: stella_protocol::CompletionUsage {
            input_tokens: 14_000,
            cached_input_tokens: 12_000,
            output_tokens: 130,
            ..Default::default()
        },
        cost_usd: 0.0213,
        input_reported: true,
    };
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Err(ProviderError::transport(
            "connection closed mid-response",
        )
        .with_partial(recovered))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("work"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let _ = engine.run_turn(&mut messages, &mut budget, &tx).await;
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let incomplete = events
        .iter()
        .find(|event| matches!(event, AgentEvent::UsageIncomplete { .. }))
        .expect("a failed attempt emits an incompleteness envelope");
    let AgentEvent::UsageIncomplete { partial, .. } = incomplete else {
        unreachable!("filtered above")
    };
    let partial = partial
        .as_ref()
        .expect("the attempt's recovered usage rides its UsageIncomplete event");
    assert_eq!(partial.usage.input_tokens, 14_000);
    assert_eq!(partial.usage.cached_input_tokens, 12_000);
    assert_eq!(partial.cost_usd, 0.0213);

    // The event stays content-free while carrying the numbers: token counts
    // cross the wire, the adapter's prose does not. (The turn's separate
    // `Error` event is where the message belongs and is deliberately not
    // covered by this assertion.)
    let wire = serde_json::to_string(incomplete).unwrap();
    assert!(
        !wire.contains("connection closed mid-response"),
        "prose leaked into a content-free event: {wire}"
    );
    assert!(wire.contains("14000"), "the numbers do cross: {wire}");
}

#[tokio::test]
async fn exhausted_retries_emit_typed_reasons_before_the_error() {
    // Receipts spec §6.3 (#364 gap 3): `Retry` events only flush for steps
    // that COMMIT, so a terminally-failed call's doomed attempts were
    // previously lost. RetriesExhausted is their durable record — one
    // reason per dispatched attempt, oldest first, ahead of the prose
    // `Error`.
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Err(ProviderError::transport("first drop")),
            Err(ProviderError::transport("second drop")),
            Err(ProviderError::Terminal("gave up".into())),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("work"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Aborted { .. }));
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let exhausted: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::RetriesExhausted { .. }))
        .collect();
    assert_eq!(exhausted.len(), 1, "{events:?}");
    match exhausted[0] {
        AgentEvent::RetriesExhausted {
            attempts, reasons, ..
        } => {
            assert_eq!(*attempts, 3, "two retried transports plus the terminal");
            assert_eq!(reasons.len(), 3);
            assert!(reasons[0].contains("first drop"), "{reasons:?}");
            assert!(reasons[1].contains("second drop"), "{reasons:?}");
            assert!(reasons[2].contains("gave up"), "{reasons:?}");
        }
        other => panic!("filtered above: {other:?}"),
    }
    // Ordered ahead of the paired Error, so a receipt reading forward has
    // the typed record before the prose.
    let exhausted_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::RetriesExhausted { .. }));
    let error_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Error { .. }));
    assert!(
        exhausted_pos < error_pos,
        "{exhausted_pos:?} vs {error_pos:?}"
    );
}

#[tokio::test]
async fn auth_failure_on_first_attempt_reports_not_retryable() {
    // #926: a terminal `ProviderError::Auth` on attempt 1 was previously
    // indistinguishable, at the typed level, from a genuine retry-budget
    // exhaustion — both emitted `RetriesExhausted`, and only `attempts == 1`
    // (an implicit contract) hinted that no retry was ever attempted. This
    // is the acceptance case: attempts is 1, and `retryable` says so
    // explicitly.
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Err(ProviderError::Auth("bad api key".into()))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("work"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Aborted { .. }));
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let exhausted: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::RetriesExhausted { .. }))
        .collect();
    assert_eq!(exhausted.len(), 1, "{events:?}");
    match exhausted[0] {
        AgentEvent::RetriesExhausted {
            attempts,
            retryable,
            ..
        } => {
            assert_eq!(*attempts, 1, "no retry was ever attempted");
            assert!(!*retryable, "auth errors are never retryable");
        }
        other => panic!("filtered above: {other:?}"),
    }
    let error = events
        .iter()
        .find(|event| matches!(event, AgentEvent::Error { .. }));
    assert!(
        matches!(
            error,
            Some(AgentEvent::Error {
                retryable: false,
                ..
            })
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn successful_retry_keeps_the_failed_attempt_usage_incomplete() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Err(ProviderError::transport("private failed attempt")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let config = EngineConfig {
        retry_policy: RetryPolicy::new(1, 0, 0),
        ..EngineConfig::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, config, &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("work"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    let events = drain_events(&mut rx);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::UsageIncomplete { .. }))
            .count(),
        1,
        "the first dispatched attempt has unknowable usage even though its retry succeeded"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::StepUsage {
            retries: 1,
            complete: true,
            ..
        }
    )));
    let incomplete: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::UsageIncomplete { .. }))
        .collect();
    let wire = serde_json::to_string(&incomplete).expect("wire");
    assert!(!wire.contains("private failed attempt"));
}

/// #4565's witness: the metering record claims what the request actually
/// asked for — the resolved effort and the effective output ceiling — not
/// blanks the Observatory's profile card has to render as "not recorded for
/// this run". Fails before #4565, when `StepUsage` had neither field.
#[tokio::test]
async fn step_usage_carries_the_requests_effort_and_output_ceiling() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("done"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let config = EngineConfig {
        effort: Some(stella_protocol::completion::ReasoningEffort::High),
        max_output_tokens: Some(32_000),
        ..EngineConfig::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, config, &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("work"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    let events = drain_events(&mut rx);
    let usage = events
        .iter()
        .find(|event| matches!(event, AgentEvent::StepUsage { .. }))
        .expect("a committed step emits its metering record");
    assert!(
        matches!(
            usage,
            AgentEvent::StepUsage {
                effort: Some(stella_protocol::completion::ReasoningEffort::High),
                max_output_tokens: Some(32_000),
                ..
            }
        ),
        "the metering record must carry the dispatched request's resolved \
         effort and effective ceiling: {usage:?}"
    );
}

fn overflow_messages() -> Vec<CompletionMessage> {
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("task"),
    ];
    for index in 0..6 {
        messages.push(big_assistant_text(&format!("t{index}")));
    }
    messages
}

#[tokio::test]
async fn overflow_summarizer_emits_its_own_usage_envelope() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("SUMMARY")), Ok(text_result("done"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, overflow_config(), &sleeper);
    let mut messages = overflow_messages();
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    let events = drain_events(&mut rx);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::StepUsage {
            role: stella_protocol::ModelCallRole::Summarization,
            provider,
            model,
            ..
        } if provider == "scripted" && model == "scripted"
    )));
}

#[tokio::test]
async fn failed_overflow_summarizer_emits_content_free_incompleteness() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Err(ProviderError::Terminal("private upstream body".into())),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, overflow_config(), &sleeper);
    let mut messages = overflow_messages();
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    let events = drain_events(&mut rx);
    let incomplete = events
        .iter()
        .find(|event| {
            matches!(
                event,
                AgentEvent::UsageIncomplete {
                    role: stella_protocol::ModelCallRole::Summarization,
                    reason: stella_protocol::UsageIncompleteReason::ProviderError,
                    ..
                }
            )
        })
        .expect("summarizer incomplete envelope");
    assert!(
        !serde_json::to_string(incomplete)
            .expect("wire")
            .contains("private upstream body")
    );
}
