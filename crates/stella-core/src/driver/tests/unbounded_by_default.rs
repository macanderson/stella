//! A turn carries no step cap unless the host sets one (ADR 0030).
//!
//! Before this, every turn stopped at 200 steps. A count cannot tell real
//! work from wandering. It ended both. The runs it ended were the long ones
//! the engine exists to run. Evidence is what ends a wandering turn: a loop,
//! a stall, the budget, the deadline, the goal. All of those are still armed
//! on the turn below.
//!
//! Two tests. The first is the witness. A turn of a thousand new steps
//! completes under the default config. A cap of 200 refused that. The second
//! pins the mechanism. A host that asks for a cap still gets one.

use super::*;

/// Each call gets a new answer. `loop_detect` compares outputs, so this
/// never reads as a loop. Real work answers each step with something new.
struct DistinctTools;

#[async_trait]
impl ToolExecutor for DistinctTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, input: &Value) -> ToolOutput {
        let cmd = input.get("cmd").and_then(Value::as_str).unwrap_or("?");
        ToolOutput::Ok {
            content: format!("ran {cmd}"),
            data: None,
        }
    }
}

/// `steps` tool calls, each new, then one text answer.
fn productive_script(steps: u32) -> Vec<Result<CompletionResultAlias, ProviderError>> {
    let mut script: Vec<_> = (0..steps)
        .map(|i| {
            Ok(CompletionResultAlias {
                upstream_provider: None,
                text: String::new(),
                tool_calls: vec![ToolCall {
                    call_id: format!("call_{i}"),
                    name: "bash".into(),
                    input: serde_json::json!({"cmd": format!("step {i}")}),
                }],
                usage: CompletionUsage::reported_zero(),
                model: "scripted".into(),
                cost_usd: 0.00001,
                finish_reason: None,
            })
        })
        .collect();
    script.push(Ok(text_result("done")));
    script
}

async fn run_productive_turn(steps: u32, config: EngineConfig) -> (TurnOutcome, u32) {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(productive_script(steps)),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &DistinctTools, config, &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("do the long task"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    (outcome, provider.calls.load(Ordering::SeqCst))
}

/// The witness. A thousand steps is five times the old cap. A cap that came
/// back at any round number under it would fail this.
#[tokio::test]
async fn a_long_productive_turn_runs_to_completion_under_the_default_config() {
    const STEPS: u32 = 1_000;
    assert_eq!(
        EngineConfig::default().max_steps,
        None,
        "the default carries no step cap"
    );

    let (outcome, model_calls) = run_productive_turn(STEPS, EngineConfig::default()).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "a productive turn ends when the model is done, not on a count: {outcome:?}"
    );
    assert_eq!(
        model_calls,
        STEPS + 1,
        "every scripted step ran, plus the answer"
    );
}

/// A host that sets a cap still gets it. The stop is a `DeliberateStop` and
/// it names the number the host set.
#[tokio::test]
async fn a_host_set_cap_still_ends_the_turn_where_it_says() {
    const CAP: usize = 25;
    let config = EngineConfig {
        max_steps: Some(CAP),
        ..EngineConfig::default()
    };

    let (outcome, model_calls) = run_productive_turn(1_000, config).await;

    match outcome {
        TurnOutcome::Aborted { reason, kind, .. } => {
            assert_eq!(
                kind,
                AbortKind::DeliberateStop,
                "a cap is policy, not a crash"
            );
            assert_eq!(reason, step_cap_reason(CAP));
        }
        other => panic!("expected the host's cap to end the turn, got {other:?}"),
    }
    assert_eq!(model_calls, CAP as u32, "no model call past the cap");
}
