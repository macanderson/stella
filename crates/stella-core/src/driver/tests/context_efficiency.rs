//! Witnesses for the context-efficiency work (#1285): the retention pass
//! shaping a long turn's standing context far below the compaction budget,
//! and the elision of truncated-partial debris on the paths that end or
//! continue a turn outside `plan_continuation`.

use super::*;

/// A `ToolExecutor` whose every call returns a distinct ~5 KB payload —
/// the shape of a long turn full of file reads and command output.
struct BigOutputTools {
    calls: Arc<AtomicU32>,
}
#[async_trait]
impl ToolExecutor for BigOutputTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        ToolOutput::Ok {
            content: format!(
                "OUT{n}-HEAD\n{}\nOUT{n}-TAIL",
                format!("{n}x").repeat(2_500)
            ),
        }
    }
}

#[tokio::test]
async fn a_long_turn_ages_old_tool_results_far_below_the_compaction_budget() {
    // The #1285 step-loop witness. Thirteen tool-bearing steps at ~5 KB each
    // is ~19k estimated tokens — an eighth of the 150k budget — so before the
    // retention pass the step loop re-sent every one of those outputs
    // verbatim on every call to the end of the turn. With the default
    // horizon (8) the turn must age its older results MID-TURN, report them
    // in a `Compaction` event, and leave the recent horizon verbatim.
    let mut script: Vec<Result<CompletionResultAlias, ProviderError>> = (0..13)
        .map(|i| Ok(tool_call_result(&format!("c{i}"), "bash")))
        .collect();
    script.push(Ok(text_result("done")));
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(script),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = BigOutputTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    let events = drain_events(&mut rx);
    let aged_total: usize = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Compaction { aged, evicted, .. } => {
                assert_eq!(*evicted, 0, "under budget nothing may be dropped whole");
                Some(*aged)
            }
            _ => None,
        })
        .sum();
    assert!(
        aged_total >= 4,
        "old tool results must age mid-turn without budget pressure: {aged_total}"
    );
    // The oldest output kept its framing and lost its middle…
    let first_tool = messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .expect("tool messages survive");
    match &first_tool.tool_results[0].output {
        ToolOutput::Ok { content } => {
            assert!(content.starts_with("OUT0-HEAD"), "head lost: {content:.40}");
            assert!(content.ends_with("OUT0-TAIL"), "tail lost");
            assert!(content.contains("middle elided"), "expected aged content");
        }
        _ => panic!("expected the oldest output aged in place"),
    }
    // …and the newest tool result rode to the end verbatim.
    let last_tool = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Tool)
        .expect("tool messages survive");
    match &last_tool.tool_results[0].output {
        ToolOutput::Ok { content } => assert!(
            content.len() > 5_000,
            "the newest result must stay verbatim"
        ),
        _ => panic!("expected the newest output intact"),
    }
}

/// A ~10 KB partial with distinguishable head, middle, and tail, so "the
/// middle is gone" is testable (homogeneous filler would let tail bytes
/// satisfy a middle assertion).
fn huge_partial() -> String {
    format!(
        "HEAD-MARK {}MIDDLE-MARK{} TAIL-MARK",
        "a".repeat(5_000),
        "b".repeat(5_000)
    )
}

#[tokio::test]
async fn a_spent_allowance_retains_an_elided_partial_not_the_scratchpad() {
    // Post-mortem §3.3 lever 2, the terminal half. The continuation path
    // already elides what it retains; the path that ENDS the turn after the
    // allowance is spent used to push the full cut-off text — up to a whole
    // output budget of it — as protected assistant text a resumed session
    // re-sends forever. History must keep the elided form; the user-facing
    // outcome keeps the whole partial (it was already streamed).
    let provider = ScriptedProvider {
        id: "scripted".into(),
        // Loops the last entry: every step truncates with the same huge text,
        // spending the whole allowance and then landing on the terminal path.
        script: TokioMutex::new(vec![Ok(length_text_result(&huge_partial()))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    match outcome {
        TurnOutcome::Completed { text, .. } => assert!(
            text.contains("MIDDLE-MARK"),
            "the user-facing outcome keeps the whole partial"
        ),
        other => panic!("a truncated turn with text still completes: {other:?}"),
    }
    let last_assistant = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant)
        .expect("the terminal partial is recorded");
    assert!(
        !last_assistant.content.contains("MIDDLE-MARK"),
        "history must not keep the middle of the cut-off scratchpad"
    );
    assert!(
        last_assistant.content.contains("elided"),
        "the model must be told bytes went missing: {}",
        &last_assistant.content[..last_assistant.content.len().min(120)]
    );
    assert!(
        last_assistant.content.len() < 2_000,
        "a 10k partial must shrink: {}",
        last_assistant.content.len()
    );
    drain_events(&mut rx);
}

#[tokio::test]
async fn a_truncated_step_with_tool_calls_retains_elided_narration() {
    // The other residual hole: a step cut at the output limit that still
    // carried a tool call proceeds normally — and used to retain its whole
    // cut-off narration as protected assistant text beside the call.
    let truncated_with_call = CompletionResultAlias {
        text: huge_partial(),
        tool_calls: vec![ToolCall {
            call_id: "c1".into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": "echo hi"}),
        }],
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: 0.001,
        finish_reason: Some(FinishReason::Length),
    };
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(truncated_with_call), Ok(text_result("done"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tool_calls = Arc::new(AtomicU32::new(0));
    let tools = CountingTools {
        calls: tool_calls.clone(),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1, "the tool call ran");
    let with_call = messages
        .iter()
        .find(|m| !m.tool_calls.is_empty())
        .expect("the calling step is recorded");
    assert!(
        !with_call.content.contains("MIDDLE-MARK"),
        "cut-off narration beside a tool call is debris, not an answer"
    );
    assert!(
        with_call.content.starts_with("HEAD-MARK"),
        "orientation survives"
    );
    assert!(
        with_call.content.ends_with("TAIL-MARK"),
        "the resume point survives"
    );
    assert_tool_pairing(&messages);
    drain_events(&mut rx);
}
