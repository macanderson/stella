// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Server-owned conversations over the wire (#931).
//!
//! The acceptance line of the issue, verbatim: a host can run a multi-turn
//! conversation without re-sending history, gets the prompt-cache discount the
//! CLI gets (pinned here as a byte-identical system prefix across turns), and
//! sees one aggregated cost for the session. Plus the composition that makes
//! #932 worth having: a turn inside a session answers to steer like any other
//! turn, and what the steer injected is *retained* in the session history.

mod common;

use std::time::Duration;

use common::{TOKEN, model_result, model_wants_echo, next_event, open_sse, post_json};
use serde_json::json;

async fn create_session(addr: std::net::SocketAddr, system_prompt: &str) -> String {
    let (status, body) = post_json(
        addr,
        "/v1/sessions",
        Some(TOKEN),
        &json!({ "system_prompt": system_prompt }).to_string(),
    )
    .await;
    assert!(status.contains("200"), "create session: {status} {body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Start the next turn of a session with one user message, returning its
/// turn id.
async fn create_session_turn(
    addr: std::net::SocketAddr,
    session_id: &str,
    prompt: &str,
    tools: bool,
) -> String {
    let tools = if tools {
        json!([common::echo_tool()])
    } else {
        json!([])
    };
    let create = json!({
        "provider_id": "mock",
        "tools": tools,
        "input": [{ "role": "user", "content": prompt }],
        "reverse_request_timeout_ms": 20_000,
    })
    .to_string();
    let (status, body) = post_json(
        addr,
        &format!("/v1/sessions/{session_id}/turns"),
        Some(TOKEN),
        &create,
    )
    .await;
    assert!(status.contains("200"), "create turn: {status} {body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(created["session_id"], session_id);
    created["turn_id"].as_str().unwrap().to_string()
}

async fn get_session(addr: std::net::SocketAddr, session_id: &str) -> serde_json::Value {
    let (status, body) =
        common::get_json_authed(addr, &format!("/v1/sessions/{session_id}"), TOKEN).await;
    assert!(status.contains("200"), "get session: {status} {body}");
    serde_json::from_str(&body).unwrap()
}

/// Stream a session turn to completion, answering its single model call with
/// `result`. Returns the messages array of the model request — the transcript
/// the engine actually sent — and asserts the turn completed.
async fn run_turn_to_completion(
    addr: std::net::SocketAddr,
    turn_id: &str,
    result: serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let request = loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a provider_request must arrive")
            .expect("stream must not end before the provider_request");
        if frame["type"] == "provider_request" {
            break frame;
        }
    };
    let messages = request["request"]["messages"].as_array().unwrap().clone();
    let body = json!({
        "request_id": request["request_id"],
        "status": "ok",
        "result": result,
    })
    .to_string();
    let (status, body) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/provider-result"),
        Some(TOKEN),
        &body,
    )
    .await;
    assert!(status.contains("200"), "provider-result: {status} {body}");
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("the terminal frame must arrive")
            .expect("the stream must reach turn_complete");
        if frame["type"] == "turn_complete" {
            assert_eq!(frame["outcome"]["status"], "completed");
            break;
        }
    }
    messages
}

/// Issue #931's acceptance, end to end: two turns, no history re-sent, the
/// system prefix byte-identical across both model calls, and the session
/// aggregating cost across turns.
#[tokio::test]
async fn a_session_threads_history_across_turns_on_a_stable_prefix() {
    let addr = common::start_server().await;
    let session_id = create_session(addr, "You are stella.").await;

    let turn1 = create_session_turn(addr, &session_id, "hi", false).await;
    let result = json!({
        "text": "hello!",
        "usage": { "input_tokens": 0, "output_tokens": 0 },
        "model": "mock",
        "cost_usd": 0.25,
    });
    let messages1 = run_turn_to_completion(addr, &turn1, result.clone()).await;
    assert_eq!(
        messages1
            .first()
            .map(|m| (m["role"].clone(), m["content"].clone())),
        Some((json!("system"), json!("You are stella."))),
        "the session's system prompt heads the first turn"
    );

    // The second turn sends only its new input; the server supplies the rest.
    let turn2 = create_session_turn(addr, &session_id, "and again", false).await;
    let messages2 = run_turn_to_completion(addr, &turn2, result).await;

    assert_eq!(
        serde_json::to_string(&messages1[0]).unwrap(),
        serde_json::to_string(&messages2[0]).unwrap(),
        "the system prefix must be byte-identical across turns — that is the \
         prompt-cache contract the session exists to keep"
    );
    let flat: Vec<(String, String)> = messages2
        .iter()
        .map(|m| {
            (
                m["role"].as_str().unwrap_or_default().to_string(),
                m["content"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        flat,
        vec![
            ("system".into(), "You are stella.".into()),
            ("user".into(), "hi".into()),
            ("assistant".into(), "hello!".into()),
            ("user".into(), "and again".into()),
        ],
        "turn 2 must start from turn 1's full settled transcript plus its own input"
    );

    let session = get_session(addr, &session_id).await;
    assert_eq!(session["turns_completed"], 2);
    assert_eq!(session["turns_aborted"], 0);
    assert!(
        (session["cost_usd"].as_f64().unwrap() - 0.5).abs() < 1e-9,
        "two 0.25 turns must aggregate to one 0.50 session: {}",
        session["cost_usd"]
    );
    assert!(session["live_turn"].is_null());
    assert_eq!(
        session["messages"].as_array().unwrap().len(),
        5,
        "system + (user, assistant) x2"
    );
}

/// An aborted turn must leave the session history exactly as it was — a
/// cancelled turn can hold a dangling `tool_use` that would poison the next
/// model call — while its spend still joins the session total.
#[tokio::test]
async fn an_aborted_turn_leaves_the_history_untouched() {
    let addr = common::start_server().await;
    let session_id = create_session(addr, "sys").await;

    let turn1 = create_session_turn(addr, &session_id, "hi", false).await;
    run_turn_to_completion(addr, &turn1, model_result("ok")).await;
    let settled = get_session(addr, &session_id).await;
    let settled_len = settled["messages"].as_array().unwrap().len();

    // Turn 2: park on its model call, then cancel it mid-flight.
    let turn2 = create_session_turn(addr, &session_id, "risky", false).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn2}/events"), TOKEN).await;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a provider_request must arrive")
            .expect("stream must not end early");
        if frame["type"] == "provider_request" {
            break;
        }
    }
    let (status, _) = post_json(addr, &format!("/v1/turns/{turn2}/cancel"), Some(TOKEN), "").await;
    assert!(status.contains("200"), "cancel: {status}");
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("the aborted terminal frame must arrive")
            .expect("the stream must reach turn_complete");
        if frame["type"] == "turn_complete" {
            assert_eq!(frame["outcome"]["status"], "aborted");
            break;
        }
    }

    let after = get_session(addr, &session_id).await;
    assert_eq!(after["turns_completed"], 1);
    assert_eq!(after["turns_aborted"], 1);
    assert_eq!(
        after["messages"].as_array().unwrap().len(),
        settled_len,
        "an aborted turn must not write back a transcript"
    );
    assert!(
        after["live_turn"].is_null(),
        "the aborted turn must release the session's live slot"
    );
}

/// One live turn per session: a concurrent second turn is `409`, and the slot
/// frees once the first settles.
#[tokio::test]
async fn a_session_admits_one_turn_at_a_time_over_the_wire() {
    let addr = common::start_server().await;
    let session_id = create_session(addr, "sys").await;

    let turn1 = create_session_turn(addr, &session_id, "hi", false).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn1}/events"), TOKEN).await;
    let request = loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a provider_request must arrive")
            .expect("stream must not end early");
        if frame["type"] == "provider_request" {
            break frame;
        }
    };

    let (status, body) = post_json(
        addr,
        &format!("/v1/sessions/{session_id}/turns"),
        Some(TOKEN),
        &json!({
            "provider_id": "mock",
            "input": [{ "role": "user", "content": "impatient" }],
        })
        .to_string(),
    )
    .await;
    assert!(status.contains("409"), "concurrent turn: {status} {body}");

    // The session reports its live turn — the handle a reconnecting host
    // needs to rejoin the stream.
    let session = get_session(addr, &session_id).await;
    assert_eq!(session["live_turn"], turn1.as_str());

    // Settle turn 1; the slot must free.
    let answer = json!({
        "request_id": request["request_id"],
        "status": "ok",
        "result": model_result("done"),
    })
    .to_string();
    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn1}/provider-result"),
        Some(TOKEN),
        &answer,
    )
    .await;
    assert!(status.contains("200"));
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("the terminal frame must arrive")
            .expect("the stream must reach turn_complete");
        if frame["type"] == "turn_complete" {
            break;
        }
    }
    let turn2 = create_session_turn(addr, &session_id, "next", false).await;
    assert_ne!(turn1, turn2);
    let (status, _) = post_json(addr, &format!("/v1/turns/{turn2}/cancel"), Some(TOKEN), "").await;
    assert!(status.contains("200"), "cleanup cancel: {status}");
}

/// The resource behaves like one: unknown ids are 404, deletion is terminal,
/// empty input is refused, and deleting a session cancels its live turn.
#[tokio::test]
async fn sessions_are_resources_with_honest_edges() {
    let addr = common::start_server().await;

    let (status, _) = common::get_json_authed(addr, "/v1/sessions/session-nope", TOKEN).await;
    assert!(status.contains("404"), "get unknown: {status}");

    let session_id = create_session(addr, "sys").await;

    let (status, body) = post_json(
        addr,
        &format!("/v1/sessions/{session_id}/turns"),
        Some(TOKEN),
        &json!({ "provider_id": "mock", "input": [] }).to_string(),
    )
    .await;
    assert!(status.contains("400"), "empty input: {status} {body}");

    // A live turn, then DELETE: the session vanishes and the turn is
    // cancelled exactly as `POST /v1/turns/{id}/cancel` would.
    let turn_id = create_session_turn(addr, &session_id, "hi", false).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a provider_request must arrive")
            .expect("stream must not end early");
        if frame["type"] == "provider_request" {
            break;
        }
    }

    let mut delete = tokio::net::TcpStream::connect(addr).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let request = format!(
        "DELETE /v1/sessions/{session_id} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
    );
    delete.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    delete.read_to_string(&mut response).await.unwrap();
    assert!(response.contains("200"), "delete: {response}");

    // The cancelled turn unwinds to its aborted terminal frame.
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("the aborted terminal frame must arrive")
            .expect("the stream must reach turn_complete");
        if frame["type"] == "turn_complete" {
            assert_eq!(frame["outcome"]["status"], "aborted");
            break;
        }
    }

    let (status, _) =
        common::get_json_authed(addr, &format!("/v1/sessions/{session_id}"), TOKEN).await;
    assert!(status.contains("404"), "get after delete: {status}");
}

/// `/v1/sessions/{id}` is the API's first two-verb resource: a wrong method
/// must name *both* verbs in `Allow`, per RFC 9110 §15.5.6.
#[tokio::test]
async fn the_session_member_route_names_both_its_verbs_on_405() {
    let addr = common::start_server().await;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST /v1/sessions/session-abc HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.contains("405"), "wrong method: {response}");
    let head = response.split("\r\n\r\n").next().unwrap_or_default();
    assert!(
        head.to_ascii_lowercase().contains("allow: get, delete"),
        "a two-verb resource must name both (RFC 9110 §15.5.6): {head}"
    );
}

/// #931 and #932 compose: a steer posted against a session's turn is not only
/// injected into the running turn — it is *retained*, because the settled
/// transcript the session keeps is the one the steer landed in.
#[tokio::test]
async fn a_steered_session_turn_retains_the_injected_message() {
    let addr = common::start_server().await;
    let session_id = create_session(addr, "sys").await;
    let turn_id = create_session_turn(addr, &session_id, "start", true).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;

    // Park on step 1's model call, steer, then answer with a tool call so a
    // second step exists for the steer to land in.
    let first = loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a provider_request must arrive")
            .expect("stream must not end early");
        if frame["type"] == "provider_request" {
            break frame;
        }
    };
    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/steer"),
        Some(TOKEN),
        &json!({ "message": "actually, skip the migration" }).to_string(),
    )
    .await;
    assert!(status.contains("200"), "steer: {status}");
    let answer = json!({
        "request_id": first["request_id"],
        "status": "ok",
        "result": model_wants_echo(),
    })
    .to_string();
    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/provider-result"),
        Some(TOKEN),
        &answer,
    )
    .await;
    assert!(status.contains("200"));

    let tool = loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a tool_request must arrive")
            .expect("stream must not end early");
        if frame["type"] == "tool_request" {
            break frame;
        }
    };
    let answer = json!({
        "request_id": tool["request_id"],
        "output": { "ok": { "content": "echoed" } },
    })
    .to_string();
    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/tool-result"),
        Some(TOKEN),
        &answer,
    )
    .await;
    assert!(status.contains("200"));

    let second = loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("step 2's provider_request must arrive")
            .expect("stream must not end early");
        if frame["type"] == "provider_request" {
            break frame;
        }
    };
    let answer = json!({
        "request_id": second["request_id"],
        "status": "ok",
        "result": model_result("done"),
    })
    .to_string();
    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/provider-result"),
        Some(TOKEN),
        &answer,
    )
    .await;
    assert!(status.contains("200"));
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("the terminal frame must arrive")
            .expect("the stream must reach turn_complete");
        if frame["type"] == "turn_complete" {
            assert_eq!(frame["outcome"]["status"], "completed");
            break;
        }
    }

    let session = get_session(addr, &session_id).await;
    assert!(
        session["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| { m["role"] == "user" && m["content"] == "actually, skip the migration" }),
        "the steered message must be retained in the session history: {}",
        session["messages"]
    );
}
