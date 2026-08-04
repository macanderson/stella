// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The operator hook plane on a served turn (#1298), proved against a live
//! [`Session`] with a mock host — the same harness shape as `bridge.rs`, and
//! for the same reason: the hook seam is inside the session thread, so testing
//! it through HTTP would be testing the transport twice and the seam once.
//!
//! What these pin is the pair of claims `stella-parity` now makes for
//! `hooks.lifecycle` on the API surface: that the boundaries a turn crosses
//! reach a registered extension, and that a policy extension can refuse a tool
//! **before** the host is asked to run it.

use std::sync::{Arc, Mutex};

use serde_json::json;
use stella_core::bus::{HookBus, HookDecision, HookEvent, names};
use stella_core::{BudgetGuard, EngineConfig};
use stella_protocol::{
    BudgetMode, CompletionMessage, CompletionResult, CompletionUsage, ToolCall, ToolOutput,
    ToolSchema,
};
use stella_serve::observe::TurnRef;
use stella_serve::{ServeExtension, ServerFrame, Session, SessionSpec};

fn final_answer(text: &str) -> CompletionResult {
    CompletionResult {
        text: text.to_string(),
        tool_calls: vec![],
        usage: CompletionUsage::reported_zero(),
        model: "mock".to_string(),
        cost_usd: 0.0,
        finish_reason: None,
    }
}

fn wants_tool(call_id: &str, name: &str, input: serde_json::Value) -> CompletionResult {
    CompletionResult {
        text: String::new(),
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            input,
        }],
        usage: CompletionUsage::reported_zero(),
        model: "mock".to_string(),
        cost_usd: 0.0,
        finish_reason: None,
    }
}

fn echo_tool() -> ToolSchema {
    ToolSchema {
        name: "echo".to_string(),
        description: "echo its input".to_string(),
        input_schema: json!({ "type": "object" }),
        read_only: false,
        speculation_safe: false,
    }
}

fn spec_with(extensions: Vec<Arc<dyn ServeExtension>>) -> SessionSpec {
    SessionSpec {
        provider_id: "mock".to_string(),
        tools: vec![echo_tool()],
        messages: vec![CompletionMessage::user("use the echo tool then answer")],
        config: EngineConfig::default(),
        budget: BudgetGuard::new(BudgetMode::Off, None, None),
        reverse_request_timeout: SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT,
        turn: TurnRef::new("turn-hooktest00"),
        observer: stella_serve::observe::null_observer(),
        on_settled: None,
        checkpoint: None,
        extensions,
        calibration: None,
        // Neither is the subject here: these scenarios drive a single turn and
        // assert on what the hook plane saw, so the goal loop and sub-agents
        // stay off — their own acceptance lives in `goal_and_subagents.rs`.
        goal: None,
        pipeline: None,
        sub_agents: None,
    }
}

/// An observer extension that records the name of every event it sees.
struct Recorder(Arc<Mutex<Vec<String>>>);

impl ServeExtension for Recorder {
    fn name(&self) -> &str {
        "recorder"
    }

    fn install(&self, bus: &HookBus) {
        let seen = Arc::clone(&self.0);
        bus.on("*", move |event: &HookEvent| {
            seen.lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(event.name.clone());
            Ok(())
        })
        .detach();
    }
}

/// A policy extension that refuses one named tool.
struct RefuseTool(&'static str);

impl ServeExtension for RefuseTool {
    fn name(&self) -> &str {
        "refuse-tool"
    }

    fn install(&self, bus: &HookBus) {
        let refused = self.0;
        bus.on_blocking(names::TOOL_CALL_REQUESTED, move |event| {
            if event.payload["tool"].as_str() == Some(refused) {
                return HookDecision::Deny {
                    reason: format!("`{refused}` is not permitted on this deployment"),
                };
            }
            HookDecision::Allow
        })
        .detach();
    }
}

/// A policy extension that rewrites every tool input.
struct RewriteInput;

impl ServeExtension for RewriteInput {
    fn name(&self) -> &str {
        "rewrite-input"
    }

    fn install(&self, bus: &HookBus) {
        bus.on_blocking(names::TOOL_CALL_REQUESTED, |event| HookDecision::Modify {
            payload: json!({
                "tool": event.payload["tool"],
                "input": { "text": "rewritten by policy" },
            }),
        })
        .detach();
    }
}

/// Drive a turn to completion against a mock host, answering the first
/// provider request with a tool call and the second with a final answer.
/// Returns every tool request the host was asked to run.
async fn run_turn(session: &mut Session) -> Vec<(String, serde_json::Value)> {
    let mut provider_calls = 0usize;
    let mut tool_requests = Vec::new();
    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::ProviderRequest { request_id, .. } => {
                provider_calls += 1;
                let result = if provider_calls == 1 {
                    wants_tool("call-1", "echo", json!({ "text": "hi" }))
                } else {
                    final_answer("done")
                };
                session.resolve_provider(&request_id, result).unwrap();
            }
            ServerFrame::ToolRequest {
                request_id,
                name,
                input,
            } => {
                tool_requests.push((name, input));
                session
                    .resolve_tool(
                        &request_id,
                        ToolOutput::Ok {
                            content: "echoed".to_string(),
                        },
                    )
                    .unwrap();
            }
            ServerFrame::TurnComplete { .. } => break,
            // Nothing here pauses or runs the pipeline, so the control frames
            // and the scope-review request are inert; matched rather than
            // wildcarded so a new frame kind lands as a compile error in a
            // test that reads the stream.
            ServerFrame::Event { .. }
            | ServerFrame::TurnHeld { .. }
            | ServerFrame::TurnReleased
            | ServerFrame::ScopeReviewRequest { .. } => {}
        }
    }
    tool_requests
}

/// The witness for `hooks.lifecycle` on the API surface: an operator
/// extension installed on the server sees the turn, step, model-call and
/// tool-call boundaries of a turn the server ran.
///
/// The turn boundaries matter most here. `stella-serve` drives `run_step`
/// itself rather than calling `run_turn`, so `agent.turn.started` /
/// `agent.turn.completed` are framing this crate owns — they were absent for a
/// served turn even once a bus was attached, and an observer would have seen
/// steps with no turn around them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_hooks_fire_across_a_served_turn() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut session = Session::start(spec_with(vec![Arc::new(Recorder(Arc::clone(&seen)))]));
    let tool_requests = run_turn(&mut session).await;
    assert_eq!(tool_requests.len(), 1, "the host ran the tool");

    let seen = seen.lock().unwrap().clone();
    for expected in [
        names::SESSION_STARTED,
        names::AGENT_TURN_STARTED,
        names::AGENT_STEP_STARTED,
        names::MODEL_REQUEST_STARTED,
        names::MODEL_REQUEST_COMPLETED,
        names::TOOL_CALL_STARTED,
        names::TOOL_CALL_COMPLETED,
        names::AGENT_STEP_COMPLETED,
        names::AGENT_TURN_COMPLETED,
    ] {
        assert!(
            seen.iter().any(|name| name == expected),
            "a served turn never emitted `{expected}`; it emitted {seen:?}"
        );
    }
    // The decision record for the allowed tool call. `tool.call.requested`
    // itself is delivered only to blocking handlers — that is the bus's
    // payload-hygiene boundary — so an observer's proof that a chain ran is
    // the `policy.*` pair, not the raw event.
    assert!(
        seen.iter().any(|name| name == names::POLICY_EVALUATED)
            && seen.iter().any(|name| name == names::POLICY_ALLOWED),
        "the tool call ran without a recorded policy decision: {seen:?}"
    );
    assert!(
        !seen.iter().any(|name| name == names::TOOL_CALL_REQUESTED),
        "the raw request payload must never reach a plain observer: {seen:?}"
    );
    // Pairing, not just presence: an unpaired boundary is the failure mode
    // that makes a lifecycle stream useless for accounting.
    let count = |needle: &str| seen.iter().filter(|name| *name == needle).count();
    assert_eq!(count(names::AGENT_TURN_STARTED), 1);
    assert_eq!(count(names::AGENT_TURN_COMPLETED), 1);
    assert_eq!(
        count(names::AGENT_STEP_STARTED),
        count(names::AGENT_STEP_COMPLETED),
        "every step that started must have completed: {seen:?}"
    );
}

/// A policy extension refuses the tool, and the refusal lands *before* the
/// reverse request — the host is never asked to run it.
///
/// That ordering is the whole point of gating in the port rather than on the
/// answer: a `Deny` evaluated after dispatch would be a veto of the result,
/// by which time the host has already done the thing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocking_extension_refuses_a_tool_before_the_host_is_asked() {
    let mut session = Session::start(spec_with(vec![Arc::new(RefuseTool("echo"))]));
    let tool_requests = run_turn(&mut session).await;
    assert!(
        tool_requests.is_empty(),
        "a denied tool must never reach the host, but it was asked to run {tool_requests:?}"
    );
}

/// A `Modify` decision rewrites what the host is asked to run — the "adding
/// context" half of the hook contract, as opposed to the refusing half.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocking_extension_rewrites_the_input_the_host_receives() {
    let mut session = Session::start(spec_with(vec![Arc::new(RewriteInput)]));
    let tool_requests = run_turn(&mut session).await;
    assert_eq!(tool_requests.len(), 1);
    assert_eq!(
        tool_requests[0].1,
        json!({ "text": "rewritten by policy" }),
        "the host must receive the policy's input, not the model's"
    );
}

/// A deployment that installed no extensions must behave exactly as it did
/// before hooks existed: the turn runs, and nothing pays for a bus.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_with_no_extensions_runs_unchanged() {
    let mut session = Session::start(spec_with(Vec::new()));
    let tool_requests = run_turn(&mut session).await;
    assert_eq!(tool_requests.len(), 1);
}

/// The registration rule, structurally: **no route registers a hook.**
///
/// Written as a sweep over the real route table rather than as prose, because
/// prose is what let the rule be quietly reversed. A future route that
/// accepted an extension from a remote caller would hand a host the raw input
/// of every tool call and a `Deny` it can steer the engine with — so adding
/// one has to fail here first, and be argued rather than merged.
#[test]
fn no_route_lets_a_remote_caller_register_a_hook() {
    for route in stella_serve::observe::Route::ALL {
        let template = route.template();
        assert!(
            !template.contains("hook") && !template.contains("extension"),
            "{template} looks like a hook-registration route — extensions are \
             operator-installed only (see stella_serve::extensions)"
        );
    }
}
