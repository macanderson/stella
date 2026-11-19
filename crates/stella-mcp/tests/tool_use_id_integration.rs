//! The engine's tool call id, sent in each `tools/call` request's `_meta`,
//! tested over the real `mcp-fixture-server` binary.
//!
//! Two calls to one tool with the same input are the same bytes on the wire
//! without it, so a gateway cannot tell which hook event each belongs to.
//! The fixture's `meta_probe` tool answers `{"_meta": <value>}` with the
//! `_meta` it received, or `{}` when the request had none. That is how these
//! tests read what was sent, and how they tell a missing `_meta` from a null
//! one.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::hooks::{HookAction, HookExecError, HookExecResult};
use stella_core::{
    BudgetGuard, Engine, EngineConfig, HookRunner, Hooks, TurnCapabilities, TurnOutcome,
};
use stella_mcp::{McpClient, McpServerConfig, McpToolSet, McpTransport, TOOL_USE_ID_META_KEY};
use stella_protocol::{
    BudgetMode, CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage,
    Provider, ProviderError, ToolCall, ToolOutput,
};
use stella_time::TokioSleeper;

/// The fixture server built by this crate.
const FIXTURE: &str = env!("CARGO_BIN_EXE_mcp-fixture-server");

/// The probe tool as the engine sees it, namespaced under its server.
const PROBE: &str = "mcp__fx__meta_probe";

fn stdio_config() -> McpServerConfig {
    McpServerConfig {
        name: "fx".to_string(),
        transport: McpTransport::Stdio {
            cmd: FIXTURE.to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
        candidate_safe: false,
        cwd: None,
    }
}

/// What a `meta_probe` call answered: `{"_meta": <value>}` when its request
/// carried `_meta`, and `{}` when it did not.
fn probe_answer(output: &ToolOutput) -> Value {
    let ToolOutput::Ok { content, .. } = output else {
        panic!("meta_probe answers Ok, got {output:?}");
    };
    serde_json::from_str(content).expect("meta_probe answers JSON")
}

/// The id a `meta_probe` answer says its request carried, or `None` when
/// the request had no `_meta`.
fn sent_id(output: &ToolOutput) -> Option<String> {
    let answer = probe_answer(output);
    let meta = &answer["_meta"];
    meta.get(TOOL_USE_ID_META_KEY)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Call `meta_probe` as the engine's tool call `id`.
async fn probe_as(client: &McpClient, id: &str, input: &Value) -> ToolOutput {
    let call = client.call_tool("meta_probe", input.clone());
    stella_core::tool_use::scope(id.to_string(), call)
        .await
        .expect("the probe answers")
}

/// Two calls with the same input, in flight on one client at once, each send
/// the id of the call they run for.
#[tokio::test]
async fn concurrent_identical_calls_send_their_own_ids() {
    let client = McpClient::connect(&stdio_config(), Duration::from_secs(5))
        .await
        .expect("the fixture connects");
    let input = serde_json::json!({ "query": "retry policy" });

    let (first, second) = tokio::join!(
        probe_as(&client, "call_a", &input),
        probe_as(&client, "call_b", &input),
    );

    assert_eq!(sent_id(&first).as_deref(), Some("call_a"));
    assert_eq!(sent_id(&second).as_deref(), Some("call_b"));
    client.close().await.expect("the fixture closes");
}

/// A call made outside the engine names no call, and sends no `_meta` key.
/// A `_meta` of null would fail this too.
#[tokio::test]
async fn a_call_outside_the_engine_sends_no_meta() {
    let client = McpClient::connect(&stdio_config(), Duration::from_secs(5))
        .await
        .expect("the fixture connects");
    let output = client
        .call_tool("meta_probe", serde_json::json!({}))
        .await
        .expect("the call answers");
    assert_eq!(
        probe_answer(&output),
        serde_json::json!({}),
        "the request carried no `_meta` key"
    );
    client.close().await.expect("the fixture closes");
}

/// A model that answers from a fixed list of steps, repeating the last.
struct ScriptedProvider {
    steps: tokio::sync::Mutex<Vec<CompletionResult>>,
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let mut steps = self.steps.lock().await;
        let step = if steps.len() > 1 {
            steps.remove(0)
        } else {
            steps[0].clone()
        };
        Ok(step)
    }
}

/// One model step: a probe call per id, every one with the same input.
fn probe_step(ids: &[&str]) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: String::new(),
        tool_calls: ids
            .iter()
            .map(|id| ToolCall {
                call_id: (*id).to_string(),
                name: PROBE.to_string(),
                input: serde_json::json!({ "query": "retry policy" }),
            })
            .collect(),
        usage: CompletionUsage::reported_zero(),
        model: "scripted".to_string(),
        cost_usd: 0.0,
        finish_reason: None,
    }
}

fn answer_step() -> CompletionResult {
    CompletionResult {
        text: "done".to_string(),
        tool_calls: Vec::new(),
        ..probe_step(&[])
    }
}

/// A hook runner that allows every call and keeps each payload it was fed.
#[derive(Default)]
struct RecordingRunner {
    payloads: Mutex<Vec<String>>,
}

#[async_trait]
impl HookRunner for RecordingRunner {
    async fn run(
        &self,
        _action: &HookAction,
        payload_json: &str,
        _cwd: &str,
    ) -> Result<HookExecResult, HookExecError> {
        self.payloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(payload_json.to_string());
        Ok(HookExecResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

/// Two identical MCP calls in one step, then the same call again on the next
/// step. Each call's `PreToolUse`, `PostToolUse` and `tools/call` request all
/// name one id, and no two calls share one. The first call carries a Gemini
/// thought signature, which neither the hooks nor the server see.
#[tokio::test]
async fn the_hooks_and_the_request_name_the_same_call() {
    let config = stdio_config();
    let set = McpToolSet::connect(std::slice::from_ref(&config), Duration::from_secs(5)).await;
    assert_eq!(set.connected_count(), 1, "{:?}", set.failed_servers());
    let provider = ScriptedProvider {
        steps: tokio::sync::Mutex::new(vec![
            probe_step(&["call_a#c2lnbmF0dXJl", "call_b"]),
            probe_step(&["call_c"]),
            answer_step(),
        ]),
    };
    let runner = RecordingRunner::default();
    let hooks: Hooks = serde_json::from_str(
        r#"{
            "PreToolUse": [{ "matcher": "*", "hooks": [{ "command": "record" }] }],
            "PostToolUse": [{ "matcher": "*", "hooks": [{ "command": "record" }] }]
        }"#,
    )
    .expect("the hook settings parse");
    // The engine races every tool call against its tool timeout through the
    // sleeper. One that returned at once would time out each MCP call while
    // the server was still answering, so this test waits in real time.
    let sleeper = TokioSleeper;
    let seams = TurnCapabilities {
        hooks: Some((&hooks, &runner)),
        ..TurnCapabilities::none()
    };
    let engine = Engine::assemble(&provider, &set, EngineConfig::default(), &sleeper, seams);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("search twice"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );

    let payloads = runner
        .payloads
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut fired: Vec<(String, String)> = Vec::new();
    for json in &payloads {
        let value: Value = serde_json::from_str(json).expect("a payload is JSON");
        let event = value["event"].as_str().expect("an event").to_string();
        let id = value["tool"]["tool_use_id"]
            .as_str()
            .unwrap_or_else(|| panic!("`{event}` names no call: {json}"))
            .to_string();
        if event == "PostToolUse" {
            let result = value["toolResult"].as_str().expect("a tool result");
            let sent: Value = serde_json::from_str(result).expect("meta_probe answers JSON");
            assert_eq!(
                sent["_meta"][TOOL_USE_ID_META_KEY], id,
                "the request named the call its hooks name"
            );
        }
        fired.push((event, id));
    }
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
        "each call's hooks share one id, and no two calls share one"
    );
    set.close_all().await;
}
