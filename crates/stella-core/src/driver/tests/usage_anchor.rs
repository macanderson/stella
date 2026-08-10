//! Usage-anchor witnesses (#2681): context-size accounting re-bases on the
//! provider's reported usage, so the byte estimator prices only the tail
//! appended since the last report instead of the whole transcript — the
//! 1.8×-drift class #2671 measured. Deliberately run WITHOUT a calibration
//! map: the anchor is a per-step measurement, not a learned correction, and
//! these witnesses must not be satisfiable by calibration alone (its factor
//! clamps at 2×, so the fixtures put the truth beyond its reach).

use super::*;

/// A conversation with one old, evictable tool output — the shape compaction
/// acts on (the LAST tool message is protected).
fn compactable_history() -> Vec<CompletionMessage> {
    let tool_msg = |call_id: &str, content: String| CompletionMessage {
        role: MessageRole::Tool,
        content: String::new(),
        tool_calls: vec![],
        tool_results: vec![ToolResult {
            call_id: call_id.into(),
            output: ToolOutput::Ok { content },
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

/// Run a two-step turn whose first step reports `reported_input_tokens`
/// against a budget of `3 × raw estimate`, and say whether any compaction
/// fired. The raw estimate — even doubled by a maximally-warmed calibration —
/// can never reach that budget, so a compaction can only mean the decision
/// consumed the provider's report.
async fn compaction_fired_with_report(usage: CompletionUsage) -> bool {
    let mut first = tool_call_result("call_1", "bash");
    first.usage = usage;
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(first), Ok(text_result("done"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let mut messages = compactable_history();
    let raw = crate::estimator::estimate_conversation_tokens(&messages);
    let config = EngineConfig {
        compaction_budget_tokens: raw * 3,
        // Keep the fixture pure-pass: an overflow summary would spend a
        // scripted completion this two-entry script does not have.
        summarize_overflow: false,
        ..EngineConfig::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, config, &sleeper);
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    drain_events(&mut rx)
        .iter()
        .any(|e| matches!(e, AgentEvent::Compaction { .. }))
}

/// **Witness (#2681).** The compaction decision tracks the provider's
/// reported usage, not the local estimate alone.
///
/// The transcript's raw estimate sits at a third of the budget, so on an
/// estimate-only engine no compaction can ever fire here — while the
/// provider attests that this exact prefix actually cost more than the whole
/// budget (#2671 measured the estimator ~1.8× low; this fixture makes the
/// gap decisive rather than marginal). Anchored, the next step's decision is
/// `reported + estimate(tail) > budget` and must compact.
#[tokio::test]
async fn provider_reported_usage_rebases_the_compaction_decision() {
    let raw = crate::estimator::estimate_conversation_tokens(&compactable_history());
    assert!(
        compaction_fired_with_report(CompletionUsage {
            reported: true,
            input_tokens: raw * 6, // 2× the whole budget
            output_tokens: 50,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: None,
        })
        .await,
        "the provider attested the prefix already exceeds the budget — \
         compaction must fire before the next model call"
    );
}

/// Control for the witness above: an envelope with no usage signal (the
/// scripted default — reported, all counters zero) anchors nothing, and the
/// same conversation under the same budget stays estimate-governed: no
/// compaction, exactly the pre-anchor behavior.
#[tokio::test]
async fn an_absent_usage_report_leaves_the_estimator_in_charge() {
    assert!(
        !compaction_fired_with_report(CompletionUsage::reported_zero()).await,
        "no report, and the raw estimate fits the budget three times over"
    );
}

/// The anchor is a measurement of one exact prefix: cache-write tokens count
/// toward it (they are prompt tokens the provider read — the same rule as
/// the calibration feed), so a cache-writing first call anchors just as
/// decisively as a plain one.
#[tokio::test]
async fn cache_write_tokens_count_toward_the_anchor() {
    let raw = crate::estimator::estimate_conversation_tokens(&compactable_history());
    assert!(
        compaction_fired_with_report(CompletionUsage {
            reported: true,
            // Split so neither side alone exceeds the 3×raw budget.
            input_tokens: raw * 2,
            output_tokens: 50,
            cached_input_tokens: 0,
            cache_write_tokens: raw * 4,
            reasoning_tokens: None,
        })
        .await,
        "input + cache writes together exceed the budget — the anchor must \
         total them"
    );
}
