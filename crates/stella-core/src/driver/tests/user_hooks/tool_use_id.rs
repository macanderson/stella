//! The tool call id the hooks and the executor see.
//!
//! A sibling of `user_hooks.rs` because that file is near the 1500-line
//! limit.

use super::*;

/// A `bash` executor that answers with the id of the call it ran for.
struct IdEchoTools {
    seen: Arc<std::sync::Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl ToolExecutor for IdEchoTools {
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
        let id = crate::tool_use::current();
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(id.clone());
        ToolOutput::Ok {
            content: format!("ran {}", id.unwrap_or_default()),
            data: None,
        }
    }
}

/// One model step that calls `bash` once per id, every call with the same
/// input.
fn same_input_calls(ids: &[&str]) -> CompletionResultAlias {
    CompletionResultAlias {
        upstream_provider: None,
        text: String::new(),
        tool_calls: ids
            .iter()
            .map(|id| ToolCall {
                call_id: (*id).into(),
                name: "bash".into(),
                input: serde_json::json!({"cmd": "gh search code retry"}),
            })
            .collect(),
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: 0.0001,
        finish_reason: None,
    }
}

/// Two identical calls in one step, then the same call again on the next
/// step. Each call's `PreToolUse` and `PostToolUse` carry one id, the
/// executor runs under that id, and no two calls share one.
#[tokio::test(start_paused = true)]
async fn each_call_names_one_id_in_both_hooks_and_the_executor() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(same_input_calls(&["call_a", "call_b"])),
            Ok(same_input_calls(&["call_c"])),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let tools = IdEchoTools { seen: seen.clone() };
    let sleeper = PausedSleeper;
    let payloads = Arc::new(TokioMutex::new(Vec::new()));
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
        payloads: payloads.clone(),
    };
    let every_tool = || {
        Some(vec![HookMatcher {
            matcher: Some("*".into()),
            hooks: vec![HookAction::new("record")],
        }])
    };
    let hooks = Hooks {
        pre_tool_use: every_tool(),
        post_tool_use: every_tool(),
        ..Hooks::default()
    };
    let seams = TurnCapabilities {
        hooks: Some((&hooks, &runner)),
        ..TurnCapabilities::none()
    };
    let engine = Engine::assemble(&provider, &tools, EngineConfig::default(), &sleeper, seams);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("search twice"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    let ran: Vec<Option<String>> = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let expected: Vec<Option<String>> = ["call_a", "call_b", "call_c"]
        .iter()
        .map(|id| Some((*id).to_string()))
        .collect();
    assert_eq!(ran, expected, "the executor runs under each call's own id");

    let payloads = payloads.lock().await.clone();
    let fired: Vec<(String, String)> = payloads
        .iter()
        .map(|json| {
            let value: Value = serde_json::from_str(json).expect("a payload is JSON");
            let event = value["event"].as_str().expect("an event").to_string();
            let id = value["tool"]["tool_use_id"]
                .as_str()
                .unwrap_or_else(|| panic!("`{event}` names no call: {json}"))
                .to_string();
            if event == "PostToolUse" {
                assert_eq!(
                    value["toolResult"],
                    format!("ran {id}"),
                    "the result came from the call the payload names"
                );
            }
            (event, id)
        })
        .collect();
    let expected: Vec<(String, String)> = ["call_a", "call_b", "call_c"]
        .iter()
        .flat_map(|id| {
            [
                ("PreToolUse".to_string(), (*id).to_string()),
                ("PostToolUse".to_string(), (*id).to_string()),
            ]
        })
        .collect();
    assert_eq!(
        fired, expected,
        "each call's two hooks share one id, and no two calls share one"
    );
}
