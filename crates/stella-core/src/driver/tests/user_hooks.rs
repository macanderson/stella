//! End-to-end tests for the settings-declared user-hook surface on the
//! turn path (#2684 and its predecessors): the decision-aware `PreToolUse`
//! gate, `PostToolUse` observation, the `Stop` completion gate, and the
//! `PreCompact` summarization gate — all driven through `run_turn` against
//! scripted providers and a fake `HookRunner`. Moved out of `tests.rs`
//! (closed to growth) when the #2684 witnesses joined.

use super::*;

// The #3380 witnesses (structured denials, host-chosen hold allowance) live in
// a sibling file: this one is close enough to the 1500-line ratchet that the
// rule is to split rather than grow up to it.
mod verdicts;

/// A no-I/O [`HookRunner`] test double: returns a fixed exit code +
/// stdout/stderr for every command and records the JSON payload of each
/// call, so a test can assert which lifecycle event fired and what it
/// carried — the same fake-runner discipline as `hooks.rs`'s own tests,
/// but here driven end-to-end through `run_turn`.
struct RecordingHookRunner {
    exit_code: i32,
    stdout: String,
    stderr: String,
    payloads: Arc<TokioMutex<Vec<String>>>,
}
#[async_trait]
impl HookRunner for RecordingHookRunner {
    async fn run(
        &self,
        _action: &HookAction,
        payload_json: &str,
        _cwd: &str,
    ) -> Result<HookExecResult, HookExecError> {
        self.payloads.lock().await.push(payload_json.to_string());
        Ok(HookExecResult {
            exit_code: self.exit_code,
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
        })
    }
}

#[tokio::test]
async fn pre_tool_use_hook_nonzero_exit_blocks_the_tool_and_model_sees_it() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tool_calls = Arc::new(AtomicU32::new(0));
    let tools = CountingTools {
        calls: tool_calls.clone(),
    };
    let sleeper = NoopSleeper;
    let payloads = Arc::new(TokioMutex::new(Vec::new()));
    let runner = RecordingHookRunner {
        exit_code: 1,
        stdout: String::new(),
        stderr: "blocked by policy".into(),
        payloads: payloads.clone(),
    };
    let hooks = Hooks {
        pre_tool_use: Some(vec![HookMatcher {
            matcher: Some("*".into()),
            hooks: vec![HookAction::new("exit 1")],
        }]),
        ..Hooks::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    // A blocking PreToolUse hook (non-zero exit) must keep the tool from
    // ever reaching the executor.
    assert_eq!(
        tool_calls.load(Ordering::SeqCst),
        0,
        "a PreToolUse hook that exits non-zero must block the tool from executing"
    );
    // ...and the model must see the block as a tool-result error, so it
    // can react — never an engine error.
    let tool_message = messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .expect("a tool message was appended");
    match &tool_message.tool_results[0].output {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("blocked by a PreToolUse hook"));
            assert!(
                message.contains("blocked by policy"),
                "the hook's own reason must be surfaced to the model: {message}"
            );
        }
        other => panic!("expected a hook-blocked error, got {other:?}"),
    }
    // Only PreToolUse fired — a blocked tool never runs, so no
    // PostToolUse observation follows.
    let payloads = payloads.lock().await.clone();
    assert_eq!(payloads.len(), 1);
    assert!(payloads[0].contains("\"event\":\"PreToolUse\""));
}

#[tokio::test]
async fn post_tool_use_hook_runs_after_the_tool_and_never_blocks() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tool_calls = Arc::new(AtomicU32::new(0));
    let tools = CountingTools {
        calls: tool_calls.clone(),
    };
    let sleeper = NoopSleeper;
    let payloads = Arc::new(TokioMutex::new(Vec::new()));
    // Exit 3 (non-zero) proves a *failing* PostToolUse hook is still a
    // pure observation — it can neither block nor abort the turn.
    let runner = RecordingHookRunner {
        exit_code: 3,
        stdout: String::new(),
        stderr: String::new(),
        payloads: payloads.clone(),
    };
    let hooks = Hooks {
        post_tool_use: Some(vec![HookMatcher {
            matcher: Some("*".into()),
            hooks: vec![HookAction::new("exit 3")],
        }]),
        ..Hooks::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: 0.0002
        }
    );
    // The tool ran — PostToolUse never gates execution.
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    // Exactly one PostToolUse hook fired, and it ran AFTER the tool: it
    // carries the tool's own result ("ok" from CountingTools).
    let payloads = payloads.lock().await.clone();
    assert_eq!(payloads.len(), 1);
    assert!(payloads[0].contains("\"event\":\"PostToolUse\""));
    assert!(
        payloads[0].contains("\"toolResult\":\"ok\""),
        "PostToolUse must fire after the tool and carry its result: {}",
        payloads[0]
    );
}

#[tokio::test]
async fn non_blocking_hook_failure_surfaces_as_one_retryable_error_event() {
    // A PostToolUse hook exiting non-zero stays non-blocking (pinned by the
    // test above) but must no longer vanish: the dispatch path forwards the
    // hook diagnostics as exactly one non-fatal Error event per tool call
    // (issue #373, item 6).
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let payloads = Arc::new(TokioMutex::new(Vec::new()));
    let runner = RecordingHookRunner {
        exit_code: 3,
        stdout: String::new(),
        stderr: "post hook broke".into(),
        payloads: payloads.clone(),
    };
    let hooks = Hooks {
        post_tool_use: Some(vec![HookMatcher {
            matcher: Some("*".into()),
            hooks: vec![HookAction::new("exit 3")],
        }]),
        ..Hooks::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "a failing post-hook must never abort the turn: {outcome:?}"
    );
    let events = drain_events(&mut rx);
    let hook_errors: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::Error { message, retryable: true } if message.contains("hook problem")
            )
        })
        .collect();
    assert_eq!(
        hook_errors.len(),
        1,
        "exactly one non-fatal diagnostic event per affected call: {events:?}"
    );
    assert!(
        matches!(
            hook_errors[0],
            AgentEvent::Error { message, .. }
                if message.contains("bash") && message.contains("exited 3")
        ),
        "the event must name the tool and the failure: {hook_errors:?}"
    );
}

#[tokio::test]
async fn no_hooks_configured_leaves_the_turn_path_unchanged() {
    // With no hooks attached the tool executes normally and the turn
    // completes exactly as it did before the hooks seam existed — the
    // `None` branch is `self.tools.execute(...)` verbatim.
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tool_calls = Arc::new(AtomicU32::new(0));
    let tools = CountingTools {
        calls: tool_calls.clone(),
    };
    let sleeper = NoopSleeper;
    // Built WITHOUT `with_hooks` — `hooks` stays `None`.
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: 0.0002
        }
    );
    assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    // The recorded result is the tool's own output, never a hook block.
    let tool_message = messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .expect("a tool message was appended");
    assert_eq!(
        tool_message.tool_results[0].output,
        ToolOutput::Ok {
            content: "ok".into(),
            data: None
        }
    );
}

#[tokio::test]
async fn run_turn_never_fires_session_start_hooks() {
    // SessionStart is a session-level event and a host obligation (#2674):
    // the host fires it once, via `hooks::run_hooks`, while assembling the
    // system prompt. run_turn is per-turn — a REPL calls it many times per
    // session — so it must never fire SessionStart, or every turn would
    // re-run session setup.
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("hi there"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let payloads = Arc::new(TokioMutex::new(Vec::new()));
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: "on-call: alice".into(),
        stderr: String::new(),
        payloads: payloads.clone(),
    };
    let hooks = Hooks {
        session_start: Some(vec![HookMatcher {
            matcher: None,
            hooks: vec![HookAction::new("echo on-call: alice")],
        }]),
        ..Hooks::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);

    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    let payloads = payloads.lock().await.clone();
    assert!(
        !payloads
            .iter()
            .any(|p| p.contains("\"event\":\"SessionStart\"")),
        "run_turn fired SessionStart — session setup would re-run on every turn"
    );
}

#[tokio::test]
async fn a_hooked_read_fires_its_hook_once_never_for_a_dropped_speculative_attempt() {
    // A read-only tool with a configured PreToolUse hook. The first stream
    // attempt announces it and then fails; the retry commits it. The hook
    // must fire exactly ONCE — on the committed dispatch path — never for
    // the dropped attempt. Regression: before the fix the read was
    // speculated on the doomed attempt too, firing the hook a second time
    // for a call that never reached the transcript (#370).
    let executed = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicU32::new(0));
    let input = serde_json::json!({"path": "a.rs"});
    let provider = FlakySpeculatingProvider {
        announce: read_call(input.clone()),
        commit: read_call(input),
        executed: executed.clone(),
        // Pre-fix the dropped attempt speculates the read and fires the hook
        // before this returns; post-fix the hooked read is never speculated,
        // so nothing notifies and this bounded wait just times out.
        first_attempt_wait_ms: 300,
        invocation: AtomicU32::new(0),
    };
    let tools = NotifyingReadTools {
        calls: calls.clone(),
        executed,
    };
    let sleeper = NoopSleeper;
    let payloads = Arc::new(TokioMutex::new(Vec::new()));
    // exit 0: non-blocking, so the tool runs and the hook is a pure
    // observation — what matters is how many times it is invoked.
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
        payloads: payloads.clone(),
    };
    let hooks = Hooks {
        pre_tool_use: Some(vec![HookMatcher {
            matcher: Some("read_file".into()),
            hooks: vec![HookAction::new("audit-log")],
        }]),
        ..Hooks::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![CompletionMessage::user("read a.rs")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        engine.run_turn(&mut messages, &mut budget, &tx),
    )
    .await
    .expect("a hung turn means the pump/provider join deadlocked");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );

    let pre_fires = payloads
        .lock()
        .await
        .iter()
        .filter(|p| p.contains("\"event\":\"PreToolUse\""))
        .count();
    assert_eq!(
        pre_fires, 1,
        "PreToolUse must fire once per committed call, never for a dropped speculative attempt"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the hooked read must execute once, on the committed dispatch path"
    );
}

// ---- #2684: the decision plane on the dispatch path -------------------

/// A `ToolExecutor` that records every input it was handed — the witness
/// for "the tool sees the rewritten input", which a call counter cannot
/// distinguish from "the tool ran with the original".
struct InputRecordingTools {
    schemas: Vec<ToolSchema>,
    inputs: Arc<TokioMutex<Vec<(String, Value)>>>,
}
#[async_trait]
impl ToolExecutor for InputRecordingTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.schemas.clone()
    }
    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        self.inputs
            .lock()
            .await
            .push((name.to_string(), input.clone()));
        ToolOutput::Ok {
            content: "ok".into(),
            data: None,
        }
    }
}

fn bash_schema(read_only: bool) -> ToolSchema {
    ToolSchema {
        name: "bash".into(),
        description: "run a command".into(),
        input_schema: serde_json::json!({"type": "object"}),
        read_only,
        speculation_safe: false,
    }
}

/// **Witness (a), #2684.** A `PreToolUse` hook printing a `modify`
/// decision rewrites the input the tool actually receives — and the
/// `PostToolUse` payload reports the input the tool saw. On the base
/// commit stdout JSON is ignored and the tool runs with the model's
/// original input, so this fails there.
#[tokio::test]
async fn pre_tool_use_modify_decision_rewrites_the_input_the_tool_receives() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let inputs = Arc::new(TokioMutex::new(Vec::new()));
    let tools = InputRecordingTools {
        schemas: vec![bash_schema(false)],
        inputs: inputs.clone(),
    };
    let sleeper = NoopSleeper;
    let payloads = Arc::new(TokioMutex::new(Vec::new()));
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: r#"{"action":"modify","payload":{"input":{"cmd":"echo safe"}}}"#.into(),
        stderr: String::new(),
        payloads: payloads.clone(),
    };
    // Both hook events configured, one command each: the pre hook rewrites,
    // the post hook (same scripted stdout, but `PostToolUse` parses no
    // decisions) observes what actually ran.
    let hooks: Hooks = serde_json::from_str(
        r#"{
            "PreToolUse":  [ { "matcher": "*", "hooks": [{ "command": "rewrite" }] } ],
            "PostToolUse": [ { "matcher": "*", "hooks": [{ "command": "observe" }] } ]
        }"#,
    )
    .unwrap();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );

    let inputs = inputs.lock().await.clone();
    assert_eq!(inputs.len(), 1, "the tool ran exactly once");
    assert_eq!(
        inputs[0].1,
        serde_json::json!({"cmd": "echo safe"}),
        "the tool must receive the hook's rewritten input, not the model's original"
    );
    let payloads = payloads.lock().await.clone();
    let post = payloads
        .iter()
        .find(|p| p.contains("\"event\":\"PostToolUse\""))
        .expect("the post hook fired");
    assert!(
        post.contains("echo safe"),
        "PostToolUse must report the input the tool saw: {post}"
    );
}

/// A scripted [`crate::hooks::decision::ApprovalRoute`]: one fixed
/// answer, invocation-counted.
struct ScriptedRoute {
    resolution: crate::hooks::decision::ApprovalRouteResolution,
    calls: Arc<AtomicU32>,
    seen: Arc<TokioMutex<Vec<crate::hooks::decision::ApprovalRouteRequest>>>,
}
#[async_trait]
impl crate::hooks::decision::ApprovalRoute for ScriptedRoute {
    async fn resolve(
        &self,
        request: &crate::hooks::decision::ApprovalRouteRequest,
    ) -> crate::hooks::decision::ApprovalRouteResolution {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().await.push(request.clone());
        self.resolution.clone()
    }
}

/// One require-approval scenario's parts, named so the tuple does not trip
/// clippy's type-complexity lint.
struct ApprovalFixture {
    provider: ScriptedProvider,
    inputs: Arc<TokioMutex<Vec<(String, Value)>>>,
    tools: InputRecordingTools,
    runner: RecordingHookRunner,
    hooks: Hooks,
}

fn require_approval_fixture(stdout: &str) -> ApprovalFixture {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let inputs = Arc::new(TokioMutex::new(Vec::new()));
    let tools = InputRecordingTools {
        schemas: vec![bash_schema(false)],
        inputs: inputs.clone(),
    };
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: stdout.into(),
        stderr: String::new(),
        payloads: Arc::new(TokioMutex::new(Vec::new())),
    };
    let hooks: Hooks = serde_json::from_str(
        r#"{ "PreToolUse": [ { "matcher": "*", "hooks": [{ "command": "gate" }] } ] }"#,
    )
    .unwrap();
    ApprovalFixture {
        provider,
        inputs,
        tools,
        runner,
        hooks,
    }
}

/// **Witness (b), #2684 — engine half.** A hook's `require_approval`
/// parks the dispatch on the attached route: an approving route lets the
/// tool run, a denying route blocks it with the human's reason, and the
/// route sees the tool's name and `read_only` bit.
#[tokio::test]
async fn a_hook_require_approval_parks_on_the_route_and_the_answer_decides() {
    let ask = r#"{"action":"require_approval","reason":"mutating call"}"#;
    // Approving route: the tool runs.
    let ApprovalFixture {
        provider,
        inputs,
        tools,
        runner,
        hooks,
    } = require_approval_fixture(ask);
    let sleeper = NoopSleeper;
    let route = ScriptedRoute {
        resolution: crate::hooks::decision::ApprovalRouteResolution::Approved,
        calls: Arc::new(AtomicU32::new(0)),
        seen: Arc::new(TokioMutex::new(Vec::new())),
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner)
        .with_hook_approval_route(&route);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(route.calls.load(Ordering::SeqCst), 1, "the route was asked");
    assert_eq!(inputs.lock().await.len(), 1, "the approved tool ran");
    let seen = route.seen.lock().await.clone();
    assert_eq!(
        seen[0].subject,
        crate::hooks::decision::ApprovalSubject::Tool {
            name: "bash".into(),
            read_only: false,
        },
        "a parked tool call names the tool and carries its advertised bit"
    );
    assert_eq!(seen[0].reason, "mutating call");

    // Denying route: the tool never runs and the model reads the reason.
    let ApprovalFixture {
        provider,
        inputs,
        tools,
        runner,
        hooks,
    } = require_approval_fixture(ask);
    let route = ScriptedRoute {
        resolution: crate::hooks::decision::ApprovalRouteResolution::Denied {
            reason: "not on my watch".into(),
        },
        calls: Arc::new(AtomicU32::new(0)),
        seen: Arc::new(TokioMutex::new(Vec::new())),
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner)
        .with_hook_approval_route(&route);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(inputs.lock().await.len(), 0, "a denied tool must not run");
    let tool_message = messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .expect("a tool message was appended");
    match &tool_message.tool_results[0].output {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("requires approval"), "{message}");
            assert!(
                message.contains("not on my watch"),
                "the human's words reach the model: {message}"
            );
        }
        other => panic!("expected an approval denial, got {other:?}"),
    }
}

/// With no route attached, a `require_approval` decision refuses the call
/// and names the missing surface — the same headless posture as #2676's
/// broker, never a silent allow or a hang.
#[tokio::test]
async fn require_approval_without_a_route_refuses_with_the_grant_path() {
    let ask = r#"{"action":"require_approval","reason":"mutating call"}"#;
    let ApprovalFixture {
        provider,
        inputs,
        tools,
        runner,
        hooks,
    } = require_approval_fixture(ask);
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        inputs.lock().await.len(),
        0,
        "the unanswerable call must not run"
    );
    let tool_message = messages
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .expect("a tool message was appended");
    match &tool_message.tool_results[0].output {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("no interactive surface"), "{message}");
        }
        other => panic!("expected the headless refusal, got {other:?}"),
    }
}

/// **Deliverable 4, #2684.** The `PreToolUse` payload spells the tool's
/// advertised `read_only` bit from its schema, so a "deny anything
/// non-read-only" hook needs no tool-name allowlist.
#[tokio::test]
async fn pre_tool_use_payload_carries_the_schemas_read_only_bit() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = InputRecordingTools {
        schemas: vec![bash_schema(true)],
        inputs: Arc::new(TokioMutex::new(Vec::new())),
    };
    let sleeper = NoopSleeper;
    let payloads = Arc::new(TokioMutex::new(Vec::new()));
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
        payloads: payloads.clone(),
    };
    let hooks: Hooks = serde_json::from_str(
        r#"{ "PreToolUse": [ { "matcher": "*", "hooks": [{ "command": "audit" }] } ] }"#,
    )
    .unwrap();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let payloads = payloads.lock().await.clone();
    let pre = payloads
        .iter()
        .find(|p| p.contains("\"event\":\"PreToolUse\""))
        .expect("the pre hook fired");
    assert!(
        pre.contains("\"read_only\":true"),
        "the schema's read_only bit must reach the hook: {pre}"
    );
}

// ---- #2684: the Stop gate ---------------------------------------------

/// Like [`RecordingHookRunner`], but each run pops the next scripted
/// stdout — the shape a verification hook actually has: deny the first
/// completion, watch the revision, decide again. The last entry repeats if
/// the script runs dry, so an over-consulted gate fails an assertion
/// rather than panicking.
struct ScriptedHookRunner {
    stdouts: TokioMutex<Vec<String>>,
    payloads: Arc<TokioMutex<Vec<String>>>,
}
#[async_trait]
impl HookRunner for ScriptedHookRunner {
    async fn run(
        &self,
        _action: &HookAction,
        payload_json: &str,
        _cwd: &str,
    ) -> Result<HookExecResult, HookExecError> {
        self.payloads.lock().await.push(payload_json.to_string());
        let mut stdouts = self.stdouts.lock().await;
        let stdout = if stdouts.len() > 1 {
            stdouts.remove(0)
        } else {
            stdouts[0].clone()
        };
        Ok(HookExecResult {
            exit_code: 0,
            stdout,
            stderr: String::new(),
        })
    }
}

/// **Witness (d), #2684, re-pinned for #3246's bounded loop.** A `Stop`
/// hook that always denies holds the completing turn open
/// [`DEFAULT_STOP_HOLDS`](crate::driver::DEFAULT_STOP_HOLDS)
/// times — each reason a marked tail user message, the LAST one announcing
/// itself as final — and then the next completion stands: the death-spiral
/// guard, now a counter. On the once-per-turn boolean this fails: the turn
/// completed on the second answer with a single Stop fire.
#[tokio::test]
async fn an_always_denying_stop_hook_is_consulted_to_the_bound_then_the_turn_stands() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(text_result("first answer")),
            Ok(text_result("second answer")),
            Ok(text_result("third answer")),
            Ok(text_result("fourth answer")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let model_calls = provider.calls.clone();
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let payloads = Arc::new(TokioMutex::new(Vec::new()));
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: r#"{"action":"deny","reason":"the checklist is not done"}"#.into(),
        stderr: String::new(),
        payloads: payloads.clone(),
    };
    let hooks: Hooks =
        serde_json::from_str(r#"{ "Stop": [ { "hooks": [{ "command": "checklist" }] } ] }"#)
            .unwrap();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    // Three held-open rounds, then the fourth completion stands.
    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "fourth answer".into(),
            cost_usd: 0.0004
        },
        "the turn must complete on the FOURTH answer — held open to the bound, then stands"
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 4);
    // Each deny is a marked tail user message; only the last is final-marked.
    let feedback: Vec<&CompletionMessage> = messages
        .iter()
        .filter(|m| {
            m.role == MessageRole::User
                && m.content
                    .starts_with(crate::driver::user_hooks::STOP_HOOK_MARKER_PREFIX)
        })
        .collect();
    assert_eq!(
        feedback.len(),
        3,
        "one feedback message per held-open round"
    );
    assert!(
        feedback
            .iter()
            .all(|m| m.content.contains("the checklist is not done")),
        "every round carries the hook's reason"
    );
    assert!(
        !feedback[0].content.contains("final verification round")
            && !feedback[1].content.contains("final verification round"),
        "rounds with budget left do not claim to be last"
    );
    assert!(
        feedback[2]
            .content
            .contains(crate::driver::user_hooks::STOP_FINAL_ROUND_NOTE.trim_start()),
        "the last permitted round announces itself (#2810): {}",
        feedback[2].content
    );
    // The bound: three Stop fires, each carrying that round's final text.
    let payloads = payloads.lock().await.clone();
    let stop_fires: Vec<&String> = payloads
        .iter()
        .filter(|p| p.contains("\"event\":\"Stop\""))
        .collect();
    assert_eq!(
        stop_fires.len(),
        3,
        "the counter must stop a fourth fire: {payloads:?}"
    );
    for (fire, expected) in stop_fires
        .iter()
        .zip(["first answer", "second answer", "third answer"])
    {
        assert!(
            fire.contains(&format!("\"finalText\":\"{expected}\"")),
            "each Stop payload carries the text that round would have completed with: {fire}"
        );
    }
}

/// **The verification loop's own witness (#3246).** A `Stop` hook that
/// denies the first completion and allows the second is genuinely
/// consulted BOTH times — the fail→pass observation a verification hook
/// exists to make. On the once-per-turn boolean this fails: the second
/// completion was never offered to the hook (one Stop fire, not two).
#[tokio::test]
async fn a_deny_then_allow_is_reconsulted_and_the_allowed_completion_stands() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(text_result("first answer")),
            Ok(text_result("second answer")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let model_calls = provider.calls.clone();
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let payloads = Arc::new(TokioMutex::new(Vec::new()));
    let runner = ScriptedHookRunner {
        stdouts: TokioMutex::new(vec![
            r#"{"action":"deny","reason":"the witness still fails"}"#.into(),
            r#"{"action":"allow"}"#.into(),
        ]),
        payloads: payloads.clone(),
    };
    let hooks: Hooks =
        serde_json::from_str(r#"{ "Stop": [ { "hooks": [{ "command": "verify" }] } ] }"#).unwrap();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "second answer".into(),
            cost_usd: 0.0002
        },
        "the allowed second completion stands"
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
    // Two consultations: the deny observed the failure, the allow observed
    // the fix. The boolean latch never made the second observation.
    let payloads = payloads.lock().await.clone();
    let stop_fires: Vec<&String> = payloads
        .iter()
        .filter(|p| p.contains("\"event\":\"Stop\""))
        .collect();
    assert_eq!(
        stop_fires.len(),
        2,
        "the revised completion must be re-offered to the hook: {payloads:?}"
    );
    assert!(
        stop_fires[1].contains("\"finalText\":\"second answer\""),
        "the second consultation judges the revision: {}",
        stop_fires[1]
    );
    // With budget left, no round claimed to be the last.
    assert!(
        messages
            .iter()
            .all(|m| !m.content.contains("final verification round")),
        "an under-budget loop never announces a final round"
    );
}

/// A failing Stop hook (non-zero exit) never blocks completion — failing
/// closed at a turn boundary IS the death spiral, so the failure surfaces
/// as a diagnostic and the turn ends normally.
#[tokio::test]
async fn a_failing_stop_hook_never_holds_the_turn_open() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("done"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let model_calls = provider.calls.clone();
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let runner = RecordingHookRunner {
        exit_code: 3,
        stdout: String::new(),
        stderr: "checklist script crashed".into(),
        payloads: Arc::new(TokioMutex::new(Vec::new())),
    };
    let hooks: Hooks =
        serde_json::from_str(r#"{ "Stop": [ { "hooks": [{ "command": "checklist" }] } ] }"#)
            .unwrap();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: 0.0001
        },
        "a broken Stop hook must not wedge the turn"
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: true }
                if message.contains("Stop hook problem") && message.contains("exited 3")
        )),
        "the failure surfaces as a diagnostic: {events:?}"
    );
}

/// **The #3486 witness.** A Stop hook returning `require_approval` resolves
/// through the route both ways, and the turn ends differently in each.
///
/// Fails on `main` in the denied half: `stop_hook_feedback` matched
/// `RequireApproval` and answered it with a diagnostic saying the verb did not
/// apply at a turn boundary, then let the completion stand — because
/// `ApprovalRouteRequest` was keyed on a tool name and there is no tool here.
/// The route was never asked at all, so the approved half is anti-vacuity: the
/// same hook, the same engine, and the answer is what changes the outcome.
#[tokio::test]
async fn a_stop_hooks_require_approval_resolves_both_ways() {
    async fn run(
        resolution: crate::hooks::decision::ApprovalRouteResolution,
    ) -> (
        TurnOutcome,
        Vec<crate::hooks::decision::ApprovalRouteRequest>,
        Vec<CompletionMessage>,
    ) {
        let provider = ScriptedProvider {
            id: "scripted".into(),
            script: TokioMutex::new(vec![
                Ok(text_result("done")),
                Ok(text_result("done again")),
                Ok(text_result("done once more")),
                Ok(text_result("and again")),
            ]),
            calls: Arc::new(AtomicU32::new(0)),
        };
        let tools = CountingTools {
            calls: Arc::new(AtomicU32::new(0)),
        };
        let sleeper = NoopSleeper;
        let runner = RecordingHookRunner {
            exit_code: 0,
            stdout: r#"{"action":"require_approval","reason":"verification budget exhausted, continue?"}"#
                .into(),
            stderr: String::new(),
            payloads: Arc::new(TokioMutex::new(Vec::new())),
        };
        let hooks: Hooks =
            serde_json::from_str(r#"{ "Stop": [ { "hooks": [{ "command": "vera" }] } ] }"#)
                .unwrap();
        let route = ScriptedRoute {
            resolution,
            calls: Arc::new(AtomicU32::new(0)),
            seen: Arc::new(TokioMutex::new(Vec::new())),
        };
        let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
            .with_hooks(&hooks, &runner)
            .with_hook_approval_route(&route);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("hi"),
        ];
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let (tx, _rx) = mpsc::unbounded_channel();
        let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
        let seen = route.seen.lock().await.clone();
        (outcome, seen, messages)
    }

    // Approved: the completion stands, exactly as it would with no hook.
    let (outcome, seen, _messages) =
        run(crate::hooks::decision::ApprovalRouteResolution::Approved).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "an approved completion stands: {outcome:?}"
    );
    assert_eq!(seen.len(), 1, "the route was asked at the turn boundary");
    match &seen[0].subject {
        crate::hooks::decision::ApprovalSubject::TurnCompletion { final_text_digest } => {
            assert!(
                final_text_digest.starts_with("sha256:"),
                "the subject carries a digest of the answer, never the answer: {final_text_digest}"
            );
            assert!(
                !final_text_digest.contains("done"),
                "and really is a digest: {final_text_digest}"
            );
        }
        other => panic!("a turn boundary has no tool to name, got {other:?}"),
    }
    assert_eq!(seen[0].reason, "verification budget exhausted, continue?");

    // Denied: the turn is held open and the refusal is the model's next
    // observation — the same tail-message path a `deny` takes.
    let (outcome, seen, messages) = run(crate::hooks::decision::ApprovalRouteResolution::Denied {
        reason: "top up the budget first".into(),
    })
    .await;
    assert!(
        !seen.is_empty(),
        "the route was asked, and asked again on each held-open round"
    );
    let held = messages
        .iter()
        .any(|m| m.role == MessageRole::User && m.content.contains("top up the budget first"));
    assert!(
        held,
        "the refusal reaches the model as its next observation: {messages:?}"
    );
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "the turn still ends — the hold is bounded, not a wedge: {outcome:?}"
    );
}

/// With no route attached, a Stop hook's `require_approval` **fails open**:
/// the completion stands and the ask is reported as a diagnostic.
///
/// The opposite of `PreToolUse`'s posture and deliberately so — the module
/// docs' § "The Stop gate" argues it: refusing to complete because nobody was
/// there to answer is the compact→error→stop-hook→retry spiral, not safety.
#[tokio::test]
async fn a_stop_hooks_require_approval_with_no_route_lets_the_turn_complete() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("done"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: r#"{"action":"require_approval","reason":"ask a human"}"#.into(),
        stderr: String::new(),
        payloads: Arc::new(TokioMutex::new(Vec::new())),
    };
    let hooks: Hooks =
        serde_json::from_str(r#"{ "Stop": [ { "hooks": [{ "command": "vera" }] } ] }"#).unwrap();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "a headless run completes: {outcome:?}"
    );
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, .. }
                if message.contains("no interactive surface is attached")
        )),
        "and says why nobody was asked: {events:?}"
    );
}

// ---- #2684: the PreCompact gate ---------------------------------------

/// **Witness (e), #2684.** A `PreCompact` hook's `deny` vetoes the
/// overflow-summarization round: the hook fires BEFORE the summarizer
/// (proven by the summarizer never running at all) and the transcript is
/// left un-spliced. On the base commit the `PreCompact` key is unknown and
/// the summarizer runs, so this fails there.
#[tokio::test]
async fn pre_compact_hook_veto_skips_the_summarization_round() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("SUMMARY"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let summarizer_calls = provider.calls.clone();
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let hook_payloads = Arc::new(TokioMutex::new(Vec::new()));
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: r#"{"action":"deny","reason":"cheap model day"}"#.into(),
        stderr: String::new(),
        payloads: hook_payloads.clone(),
    };
    let hooks: Hooks =
        serde_json::from_str(r#"{ "PreCompact": [ { "hooks": [{ "command": "gate" }] } ] }"#)
            .unwrap();
    let engine = Engine::with_sleeper(&provider, &tools, overflow_config(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = overflow_messages_for_hooks();
    let before = messages.clone();
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let mut health = SummarizerHealth::default();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = EventSender::new(tx);

    let pass = engine
        .run_compaction_pass(
            &mut messages,
            (500, 1.0),
            &mut budget,
            &mut health,
            0,
            &events,
        )
        .await;

    assert_eq!(
        summarizer_calls.load(Ordering::SeqCst),
        0,
        "the veto must fire BEFORE the summarizer — no model call at all"
    );
    assert_eq!(pass.cost_usd, 0.0, "a vetoed round spends nothing");
    assert_eq!(messages, before, "the transcript is left un-spliced");
    assert!(
        hook_payloads
            .lock()
            .await
            .iter()
            .any(|p| p.contains("\"event\":\"PreCompact\"")),
        "the PreCompact hook fired"
    );
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: true }
                if message.contains("vetoed") && message.contains("cheap model day")
        )),
        "the veto is announced, never silent: {events:?}"
    );
}

/// A `PreCompact` `modify` decision's `instructions` reach the
/// summarizer's request, visibly separated from the span content.
#[tokio::test]
async fn pre_compact_modify_instructions_reach_the_summarizer_request() {
    let requests = Arc::new(TokioMutex::new(Vec::new()));
    let provider = RequestCapturingProvider {
        requests: requests.clone(),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let runner = RecordingHookRunner {
        exit_code: 0,
        stdout: r#"{"action":"modify","payload":{"instructions":"keep every file path verbatim"}}"#
            .into(),
        stderr: String::new(),
        payloads: Arc::new(TokioMutex::new(Vec::new())),
    };
    let hooks: Hooks =
        serde_json::from_str(r#"{ "PreCompact": [ { "hooks": [{ "command": "steer" }] } ] }"#)
            .unwrap();
    let engine = Engine::with_sleeper(&provider, &tools, overflow_config(), &sleeper)
        .with_hooks(&hooks, &runner);
    let mut messages = overflow_messages_for_hooks();
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let mut health = SummarizerHealth::default();
    let (tx, _rx) = mpsc::unbounded_channel();
    let events = EventSender::new(tx);

    let pass = engine
        .run_compaction_pass(
            &mut messages,
            (500, 1.0),
            &mut budget,
            &mut health,
            0,
            &events,
        )
        .await;
    assert!(pass.rewrote, "the steered round still spliced the span");

    let requests = requests.lock().await.clone();
    assert_eq!(requests.len(), 1, "the steered summarizer still ran once");
    assert!(
        requests[0].contains("keep every file path verbatim")
            && requests[0].contains("[PreCompact hook instructions]"),
        "the hook's instructions ride the summarizer request under their own header: {}",
        requests[0]
    );
}

/// The oldest-span-summarizable transcript the PreCompact witnesses need —
/// the same shape as `audit_fixes::overflow_messages`, local because test
/// sibling modules cannot see each other's fixtures.
fn overflow_messages_for_hooks() -> Vec<CompletionMessage> {
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
    ];
    for i in 0..8 {
        messages.push(big_assistant_text(&format!("t{i}")));
    }
    messages
}

/// A provider that records each request's user-message content — what the
/// instructions witness reads.
struct RequestCapturingProvider {
    requests: Arc<TokioMutex<Vec<String>>>,
}
#[async_trait]
impl Provider for RequestCapturingProvider {
    fn id(&self) -> &str {
        "capturing"
    }
    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResultAlias, ProviderError> {
        let user_content: String = req
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        self.requests.lock().await.push(user_content);
        Ok(text_result("SUMMARY"))
    }
}
