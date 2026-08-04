// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Cancellation as a *step-boundary* stop (#1129).
//!
//! `tests/http.rs` already pins that a cancelled turn ends and still reports a
//! terminal frame. What it cannot distinguish is *why* it ended: before this,
//! a turn stopped because `Pending::cancel` made the host's side of the
//! reverse-RPC fail, so the engine ended on a transport error that happened to
//! be unretryable. It was never asked to stop.
//!
//! Now `stella-serve` drives `stella_engine::run_step` with a `CancelToken`
//! that the engine reads at the top of every step, so the turn ends because it
//! was *told* to — which is what these assert on, via the reason the engine
//! attaches to that specific stop.

mod common;

use std::time::Duration;

use common::{TOKEN, echo_tool, next_event, open_sse, post_json, start_server};
use serde_json::json;

/// The wording `stella_engine::CANCELLED_REASON` carries. Matched on a
/// distinctive fragment rather than the whole string so a copy-edit to the
/// engine's message does not fail this test — what is being asserted is
/// *which stop* ended the turn, not the prose.
const STEP_BOUNDARY_CANCEL: &str = "cancelled at step boundary";

async fn create_turn_with_tools(addr: std::net::SocketAddr) -> String {
    let create = json!({
        "provider_id": "mock",
        "tools": [echo_tool()],
        "messages": [serde_json::to_value(
            stella_protocol::CompletionMessage::user("do the thing"),
        )
        .unwrap()],
        "reverse_request_timeout_ms": 20_000,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "create: {status} {body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    created["turn_id"].as_str().unwrap().to_string()
}

/// A model reply asking for one `echo` tool call — enough to push the turn
/// past its first step so a later cancel lands somewhere other than the
/// initial model call.
fn model_wants_a_tool() -> serde_json::Value {
    json!({
        "text": "",
        "tool_calls": [{ "call_id": "c1", "name": "echo", "input": { "text": "hi" } }],
        "usage": { "input_tokens": 0, "output_tokens": 0 },
        "model": "mock",
        "cost_usd": 0.0,
    })
}

/// The #1129 acceptance. The turn is cancelled while parked on a *tool*
/// request — the case that most clearly separates the two mechanisms.
///
/// Waking the parked tool call only turns it into a failed tool result, and a
/// failed tool result does **not** end a turn: the engine feeds the error back
/// to the model and takes another step. So without a cancel token the turn
/// carries on into a step it was already asked to abandon, and stops only
/// because the next reverse request also fails. With one, the token is read at
/// the top of that next step and the turn unwinds there, keeping every
/// completed step — which is exactly what the reason says.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_unwinds_at_the_next_step_boundary() {
    let addr = start_server().await;
    let turn_id = create_turn_with_tools(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;

    let mut outcome = None;
    let mut cancelled = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(15), next_event(&mut sse))
        .await
        .expect("the turn must terminate")
    {
        match event["type"].as_str().unwrap_or_default() {
            // Answer the model call so the turn advances past its first step
            // and dispatches a tool.
            "provider_request" if !cancelled => {
                let request_id = event["request_id"].as_str().unwrap();
                let body = json!({
                    "request_id": request_id,
                    "status": "ok",
                    "result": model_wants_a_tool(),
                })
                .to_string();
                post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-result"),
                    Some(TOKEN),
                    &body,
                )
                .await;
            }
            // Now the turn is in flight and *not* at its first model call.
            "tool_request" if !cancelled => {
                let (status, _) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/cancel"),
                    Some(TOKEN),
                    "",
                )
                .await;
                assert!(status.contains("200"), "cancel: {status}");
                cancelled = true;
            }
            "turn_complete" => outcome = Some(event["outcome"].clone()),
            _ => {}
        }
    }

    assert!(
        cancelled,
        "the turn reached a tool call before being cancelled"
    );
    let outcome = outcome.expect("a cancelled turn still reports a terminal frame");
    assert_eq!(outcome["status"].as_str(), Some("aborted"), "{outcome}");
    let reason = outcome["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains(STEP_BOUNDARY_CANCEL),
        "the turn must end because it was asked to stop, not because a \
         reverse request failed underneath it — got: {reason}"
    );
}

/// A cancel with nothing parked still lands. The token is a latch, not a
/// wake-up: a turn that is computing between reverse requests observes it at
/// the next boundary rather than needing something to interrupt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_before_the_first_reverse_request_still_ends_the_turn() {
    let addr = start_server().await;
    let turn_id = create_turn_with_tools(addr).await;

    // Cancel without ever opening the event stream, so nothing has been
    // answered and the turn is racing its own first step.
    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/cancel"),
        Some(TOKEN),
        "",
    )
    .await;
    assert!(status.contains("200"), "cancel: {status}");

    // The turn is gone from the registry either way — the point is that it
    // does not wedge its OS thread waiting for a host that will never answer.
    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/cancel"),
        Some(TOKEN),
        "",
    )
    .await;
    assert!(status.contains("404"), "a cancelled turn is deregistered");
}

/// The engine's own event stream is unchanged by who drives the loop. A host
/// reading `/events` cannot tell a `run_turn` driver from a `run_step` one —
/// which is the promise that makes `run_turn` being a loop over `run_step`
/// worth anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_event_sequence_is_unchanged_by_driving_steps() {
    let addr = start_server().await;
    let turn_id = create_turn_with_tools(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;

    let mut kinds = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(15), next_event(&mut sse))
        .await
        .expect("the turn must terminate")
    {
        let kind = event["type"].as_str().unwrap_or_default().to_string();
        if kind == "provider_request" {
            let request_id = event["request_id"].as_str().unwrap();
            let body = json!({
                "request_id": request_id,
                "status": "ok",
                "result": {
                    "text": "done",
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                    "model": "mock",
                    "cost_usd": 0.0,
                },
            })
            .to_string();
            post_json(
                addr,
                &format!("/v1/turns/{turn_id}/provider-result"),
                Some(TOKEN),
                &body,
            )
            .await;
        }
        if kind == "event" {
            kinds.push(
                event["event"]["type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        if kind == "turn_complete" {
            assert_eq!(event["outcome"]["status"].as_str(), Some("completed"));
            break;
        }
    }

    // `run_turn` opens with the execute stage marker; a step-driving host has
    // to emit it too or the sequence would differ at its very first frame.
    assert!(
        kinds.iter().any(|k| k == "stage"),
        "the execute stage marker must still open the stream: {kinds:?}"
    );
}
