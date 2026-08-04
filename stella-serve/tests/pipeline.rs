//! The witness for `pipeline.verified_run` on the API surface (#1288): a
//! caller can request a verified run over `POST /v1/turns`, watch it plan and
//! reach a scope-review gate, answer that gate over `POST
//! /v1/turns/{id}/approve`, and see the verification ladder's own diagnostic
//! run cross the wire as an ordinary tool call — exactly the reverse-RPC
//! boundary every other tool call already goes through.
//!
//! The scripted sequence mirrors `stella-pipeline`'s own
//! `interactive_scope_review_approve_proceeds_past_the_gate` (a `"multi"`
//! triage classification, a six-step plan, which is what reliably crosses
//! `ScopeThresholds::default()`), so this test is not re-proving the
//! pipeline's internal planning/scope logic — that is `stella-pipeline`'s
//! job, and it is exhaustively tested there. What this test proves is
//! narrower and is what `stella-parity`'s row is about: that the same
//! pipeline is *reachable at all* from an HTTP client, that its approval
//! gate is a real round trip rather than an unreachable route, and that its
//! verification commands cross the wire rather than running on this server.

mod common;

use common::*;
use serde_json::json;

/// An API caller can request a verified run, answer its scope-review gate
/// over the wire, see the verification ladder's diagnostic call cross the
/// same reverse-RPC boundary every tool call uses, and watch the turn settle
/// — the three "Done when" bullets on #1288, exercised end to end over a
/// real socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pipeline_run_is_requestable_over_the_wire_and_its_approval_gate_round_trips() {
    let addr = start_server().await;

    let create = json!({
        "provider_id": "scripted",
        "messages": [],
        "pipeline": {
            "goal": "Refactor across the codebase and then update all callers"
        }
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(
        status.contains("200"),
        "create status: {status}, body: {body}"
    );
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let turn_id = created["turn_id"].as_str().unwrap().to_string();

    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;

    let mut provider_calls = 0u32;
    let mut saw_scope_review_event = false;
    let mut saw_scope_review_request = false;
    let mut saw_diagnostic_tool_request = false;
    let mut saw_stage_execute = false;
    let mut outcome = None;

    while let Some(event) = next_event(&mut sse).await {
        match event["type"].as_str().unwrap_or_default() {
            "event" => match event["event"]["type"].as_str().unwrap_or_default() {
                "scope_review" => saw_scope_review_event = true,
                "stage" if event["event"]["name"].as_str() == Some("execute") => {
                    saw_stage_execute = true;
                }
                _ => {}
            },
            "provider_request" => {
                provider_calls += 1;
                let request_id = event["request_id"].as_str().unwrap();
                // 1: triage → a plan-worthy classification. 2: the plan
                // itself — six steps, the same count
                // `interactive_scope_review_approve_proceeds_past_the_gate`
                // uses to reliably cross the default scope thresholds.
                // 3+: whatever the execute/witness/judge stages ask for; a
                // plain "done" is not a meaningful answer to every one of
                // them, but this test is not pinning the pipeline's internal
                // verdict logic — only that the turn can be driven to a
                // terminal frame at all.
                let text = match provider_calls {
                    1 => "multi",
                    2 => r#"["s1","s2","s3","s4","s5","s6"]"#,
                    _ => "done",
                };
                let result = model_result(text);
                let resolve = json!({
                    "request_id": request_id,
                    "status": "ok",
                    "result": result,
                })
                .to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-result"),
                    Some(TOKEN),
                    &resolve,
                )
                .await;
                assert!(status.contains("200"), "provider-result: {status} {resp}");
            }
            "tool_request" => {
                // The only tool calls a served pipeline run raises that this
                // test never advertised a schema for are its own
                // verification ports — `RemoteVerificationRunner`'s reserved
                // names. Answered with an empty-but-well-formed
                // `CmdOutcomeWire`, matching an empty `git diff` (nothing to
                // verify) so the run does not need a witness author scripted
                // too.
                let name = event["name"].as_str().unwrap_or_default();
                if name == "stella.verify.run_diagnostic" {
                    saw_diagnostic_tool_request = true;
                }
                assert!(
                    name == "stella.verify.run_diagnostic" || name == "stella.verify.run_test",
                    "a served pipeline run must never ask the host to run anything but its own \
                     reserved verification calls: got `{name}`"
                );
                let request_id = event["request_id"].as_str().unwrap();
                let cmd_outcome = json!({
                    "kind": "completed",
                    "exit_code": 0,
                    "stdout_tail": "",
                    "stderr_tail": "",
                })
                .to_string();
                let resolve = json!({
                    "request_id": request_id,
                    "output": { "ok": { "content": cmd_outcome } },
                })
                .to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/tool-result"),
                    Some(TOKEN),
                    &resolve,
                )
                .await;
                assert!(status.contains("200"), "tool-result: {status} {resp}");
            }
            "scope_review_request" => {
                saw_scope_review_request = true;
                assert!(
                    event["proposal"]["steps"].as_array().map(Vec::len) == Some(6),
                    "the card the host approves must be the plan the pipeline actually built: {event}"
                );
                let request_id = event["request_id"].as_str().unwrap();
                let approve = json!({
                    "request_id": request_id,
                    "decision": { "decision": "approve" },
                })
                .to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/approve"),
                    Some(TOKEN),
                    &approve,
                )
                .await;
                assert!(status.contains("200"), "approve: {status} {resp}");
            }
            "turn_complete" => {
                outcome = Some(event["outcome"].clone());
                break;
            }
            other => panic!("unexpected frame on a served pipeline run: {other} ({event})"),
        }
    }

    assert!(
        saw_scope_review_event,
        "the plan's card must be visible on the ordinary event stream, not only in the \
         reverse-RPC request"
    );
    assert!(
        saw_scope_review_request,
        "the approval gate must be reachable over the wire, not merely emitted as an event"
    );
    assert!(
        saw_diagnostic_tool_request,
        "the verification ladder's diagnostic call must cross the same reverse-RPC boundary \
         every tool call does"
    );
    assert!(
        saw_stage_execute,
        "an approved plan must proceed into execution"
    );
    assert!(
        outcome.is_some(),
        "a served pipeline run must reach a terminal frame"
    );
}

/// `pipeline` and `goal` are two different ways of driving a turn — the
/// pipeline already loops internally and drives its own judge — so a request
/// naming both is refused at the route rather than silently picking one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipeline_combined_with_goal_is_refused_at_the_route() {
    let addr = start_server().await;

    let create = json!({
        "provider_id": "scripted",
        "messages": [],
        "pipeline": { "goal": "do the thing" },
        "goal": { "goal": "also judge it" },
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(
        status.contains("400"),
        "create status: {status}, body: {body}"
    );
    assert!(
        body.contains("pipeline"),
        "the refusal must name the field that conflicted: {body}"
    );
}
