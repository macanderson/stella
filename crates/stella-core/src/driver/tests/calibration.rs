//! Token-drift calibration witnesses: the feedback loop's write side (every
//! committed step records its `(estimated, actual)` pair), its read side (the
//! compaction decision consumes the calibrated estimate), and the latch
//! regression (#2133) — a fresh session's `calibration_factor` must leave 1.0
//! within the very turn that warms it, not stay frozen at the identity the
//! latch captured before the sample floor was met.

use super::*;

use crate::estimator::CalibrationMap;

/// A conversation with one old, evictable tool output — the shape
/// compaction acts on (the LAST tool message is protected).
fn compactable_history() -> Vec<CompletionMessage> {
    let tool_msg = |call_id: &str, content: String| CompletionMessage {
        role: MessageRole::Tool,
        content: String::new(),
        tool_calls: vec![],
        tool_results: vec![ToolResult {
            call_id: call_id.into(),
            output: ToolOutput::Ok {
                content,
                data: None,
            },
        }],
        attachments: Vec::new(),
    };
    let assistant_with_call = |call_id: &str| CompletionMessage {
        role: MessageRole::Assistant,
        content: String::new(),
        tool_calls: vec![ToolCall {
            call_id: call_id.into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": call_id}),
        }],
        tool_results: vec![],
        attachments: Vec::new(),
    };
    vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("do things"),
        assistant_with_call("c1"),
        tool_msg("c1", "old ".repeat(1000)),
        assistant_with_call("c2"),
        tool_msg("c2", "new ".repeat(1000)),
    ]
}

/// Witness for the feedback loop's read side: the SAME conversation
/// under the SAME configured budget compacts only when calibration says
/// the raw estimate runs low against this model's tokenizer — the
/// compaction decision demonstrably consumes the calibrated estimate,
/// not the raw one.
#[tokio::test]
async fn calibrated_estimate_changes_the_compaction_decision() {
    let run = |calibrate: bool| async move {
        let provider = ScriptedProvider {
            id: "scripted".into(),
            script: TokioMutex::new(vec![Ok(text_result("done"))]),
            calls: Arc::new(AtomicU32::new(0)),
        };
        let tools = CountingTools {
            calls: Arc::new(AtomicU32::new(0)),
        };
        let sleeper = NoopSleeper;
        let mut messages = compactable_history();
        // A budget the RAW estimate just fits under: uncalibrated, no
        // compaction can fire.
        let raw = crate::estimator::estimate_conversation_tokens(&messages);
        let config = EngineConfig {
            compaction_budget_tokens: raw + 10,
            ..EngineConfig::default()
        };
        // Observed drift: this model's tokenizer reports 2× the char
        // heuristic (three samples — the minimum for the factor to
        // apply). Calibrated, the same conversation reads as ~2×(budget)
        // and must compact.
        let calibration = CalibrationMap::new();
        if calibrate {
            calibration.seed("scripted", &[(1000, 2000), (1000, 2000), (1000, 2000)]);
        }
        let engine = Engine::with_sleeper(&provider, &tools, config, &sleeper)
            .with_calibration(&calibration);
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
        assert!(matches!(outcome, TurnOutcome::Completed { .. }));
        drain_events(&mut rx)
            .iter()
            .any(|e| matches!(e, AgentEvent::Compaction { .. }))
    };

    assert!(
        !run(false).await,
        "uncalibrated, the conversation fits the budget — no compaction"
    );
    assert!(
        run(true).await,
        "with observed 2× drift the calibrated estimate exceeds the budget — \
             compaction must fire before the model call"
    );
}

/// Witness for the feedback loop's write side: every committed step
/// records its (estimated, actual) pair into the attached calibration —
/// keyed by the model that served it — and emits the raw estimate on
/// `StepUsage` for persistence.
#[tokio::test]
async fn each_committed_step_feeds_the_calibration_and_reports_its_estimate() {
    let with_real_usage = |result: CompletionResultAlias| {
        let mut result = result;
        result.usage = CompletionUsage {
            reported: true,
            input_tokens: 4_000,
            output_tokens: 50,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: None,
        };
        // Vary each input: three byte-identical bash calls are exactly what
        // `loop_detect` exists to abort — this test is about the
        // calibration feed, not the loop breaker.
        if let Some(call) = result.tool_calls.first_mut() {
            call.input = serde_json::json!({ "cmd": format!("echo {}", call.call_id) });
        }
        result
    };
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(with_real_usage(tool_call_result("call_1", "bash"))),
            Ok(with_real_usage(tool_call_result("call_2", "bash"))),
            Ok(with_real_usage(tool_call_result("call_3", "bash"))),
            Ok(with_real_usage(text_result("done"))),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let calibration = CalibrationMap::new();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_calibration(&calibration);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    // Every StepUsage carries the raw pre-call estimate (> 0: the
    // conversation is never empty).
    let estimates: Vec<u64> = drain_events(&mut rx)
        .iter()
        .filter_map(|e| match e {
            AgentEvent::StepUsage {
                estimated_input_tokens,
                ..
            } => Some(*estimated_input_tokens),
            _ => None,
        })
        .collect();
    assert_eq!(estimates.len(), 4);
    assert!(estimates.iter().all(|&e| e > 0), "{estimates:?}");

    // Four samples of a model reporting far more tokens than the tiny
    // history estimates: the correction engaged (past min-samples),
    // pushed up, and stayed inside its clamp — a noisy run can shift
    // budgeting by at most 2× in either direction.
    let factor = calibration.factor(Some("scripted"));
    assert!(
        factor > 1.0 && factor <= 2.0,
        "factor must be engaged and bounded, got {factor}"
    );
    assert_eq!(
        calibration.factor(Some("some-other-model")),
        1.0,
        "drift is keyed by the model that served the call"
    );
}

/// Witness for the cache-write undercount: cache-WRITE tokens are real
/// prompt tokens the provider read (adapters split them out of
/// `input_tokens` for pricing only), so the actual side of every drift
/// sample must be the SUM. Fed the bare `input_tokens`, a cache-enabled
/// session's first call — nearly its whole prompt a cache write — recorded
/// a near-zero ratio that dragged the factor toward the floor and inflated
/// the effective compaction budget past the provider's context window.
#[tokio::test]
async fn cache_write_tokens_count_toward_the_calibration_actual() {
    let with_cache_write_usage = |result: CompletionResultAlias| {
        let mut result = result;
        result.usage = CompletionUsage {
            reported: true,
            // Almost the entire prompt was a cache write: the bare input
            // side alone would read as a wild UNDER-run of the estimate.
            input_tokens: 10,
            output_tokens: 50,
            cached_input_tokens: 0,
            cache_write_tokens: 100_000,
            reasoning_tokens: None,
        };
        if let Some(call) = result.tool_calls.first_mut() {
            call.input = serde_json::json!({ "cmd": format!("echo {}", call.call_id) });
        }
        result
    };
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(with_cache_write_usage(tool_call_result("call_1", "bash"))),
            Ok(with_cache_write_usage(tool_call_result("call_2", "bash"))),
            Ok(with_cache_write_usage(tool_call_result("call_3", "bash"))),
            Ok(with_cache_write_usage(text_result("done"))),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let calibration = CalibrationMap::new();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_calibration(&calibration);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    // The true total (10 + 100_000) dwarfs the tiny history's estimate, so
    // every sample clamps HIGH and the factor lands pinned at its 2.0
    // ceiling. Under the old bare-`input_tokens` feed, actual (10) read as
    // a wild under-run and the factor sank toward the 0.5 floor instead —
    // the two feeds are on opposite sides of 1.0, so this equality is a
    // strict witness.
    assert_eq!(
        calibration.factor(Some("scripted")),
        2.0,
        "cache-write-dominated usage must push the factor up, not down"
    );
}

/// Witness for the attachment-poisoning fix: the drift-sample estimate on
/// `StepUsage` excludes attachment weight. The media estimate is a
/// deliberate ~80× over-estimate of billed tokens — right for context
/// pressure, poison for calibration, where one screenshot-bearing step
/// clamped the ratio to the sample floor and doubled the effective
/// compaction budget for the rest of the session.
#[tokio::test]
async fn attachment_weight_is_excluded_from_the_drift_sample_estimate() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("done"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user_with_attachments(
            "what is in this screenshot?",
            vec![stella_protocol::Attachment::from_path(
                "shot.png",
                "image/png",
                350_000,
                "/tmp/shot.png",
            )],
        ),
    ];
    let full = crate::estimator::estimate_conversation_tokens(&messages);
    let attachment = crate::estimator::estimate_conversation_attachment_tokens(&messages);
    assert!(
        attachment > 100_000,
        "fixture sanity: the attachment must dominate the estimate"
    );
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    let estimates: Vec<u64> = drain_events(&mut rx)
        .iter()
        .filter_map(|e| match e {
            AgentEvent::StepUsage {
                estimated_input_tokens,
                ..
            } => Some(*estimated_input_tokens),
            _ => None,
        })
        .collect();
    assert_eq!(
        estimates,
        vec![full - attachment],
        "the persisted drift-sample estimate must be the text share only"
    );
}

/// **Witness (#2133).** A fresh (unseeded) session's `calibration_factor`
/// leaves 1.0 within the very turn that warms it.
///
/// Every trial of bench match `cc00894779ff` reported `calibration_factor:
/// 1.0` on every step while estimates ran 2.4–9× under the provider's
/// actuals: the effective-budget latch captured the identity at its first
/// model-known read — one sample into a three-sample warm-up — and served it
/// for the rest of the turn, so the 40+ samples the turn itself recorded were
/// never read back into any decision.
#[tokio::test]
async fn a_fresh_sessions_calibration_factor_leaves_identity_within_the_turn() {
    // Actuals that grow with the transcript, all far above the tiny history
    // estimates — the drift shape the bench trace measured.
    let with_usage = |result: CompletionResultAlias, input_tokens: u64| {
        let mut result = result;
        result.usage = CompletionUsage {
            reported: true,
            input_tokens,
            output_tokens: 50,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: None,
        };
        // Vary each input so the loop detector never mistakes the fixture
        // for a stuck loop (same discipline as the write-side witness).
        if let Some(call) = result.tool_calls.first_mut() {
            call.input = serde_json::json!({ "cmd": format!("echo {}", call.call_id) });
        }
        result
    };
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(with_usage(tool_call_result("call_1", "bash"), 4_000)),
            Ok(with_usage(tool_call_result("call_2", "bash"), 8_000)),
            Ok(with_usage(tool_call_result("call_3", "bash"), 12_000)),
            Ok(with_usage(text_result("done"), 16_000)),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    // Unseeded, exactly like a bench container's first run: warm-up happens
    // (or fails to matter) entirely inside this one turn.
    let calibration = CalibrationMap::new();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_calibration(&calibration);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    // The manifest's factor is the one the compaction decision actually used
    // (`ReceiptLedger::set_effective_budget` from the latched value).
    let factors: Vec<f64> = drain_events(&mut rx)
        .iter()
        .filter_map(|e| match e {
            AgentEvent::StepManifest {
                calibration_factor, ..
            } => Some(*calibration_factor),
            _ => None,
        })
        .collect();
    assert_eq!(factors.len(), 4, "one manifest per committed step");
    assert_eq!(
        factors[0], 1.0,
        "step 0 predates any sample — the identity is the honest answer"
    );
    let last = *factors.last().expect("four manifests");
    assert!(
        last > 1.0,
        "by the fourth step three samples of large observed drift are on \
         record; the factor the decision used must have left 1.0: {factors:?}"
    );
}
