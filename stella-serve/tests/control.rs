// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Mid-turn controls over the wire (#932): steer, pause, resume.
//!
//! The engine has modeled all three since the CLI grew them; these tests pin
//! that the HTTP surface actually reaches them — and, most importantly, that
//! the combinations that used to be able to wedge an OS thread (paused +
//! cancelled) unwind instead. Every await that would hang on a regression is
//! bounded by an explicit timeout so a failure reads as an assertion, not as
//! a stuck suite.

mod common;

use std::time::Duration;

use common::{
    TOKEN, create_turn, model_result, model_wants_echo, next_event, open_sse, post_json,
    start_observed_server_with, start_server,
};
use serde_json::json;
use stella_serve::observe::ServeEvent;

/// Answer the parked provider request `request_id` with `result`.
async fn answer_provider(
    addr: std::net::SocketAddr,
    turn_id: &str,
    request_id: &str,
    result: serde_json::Value,
) {
    let body = json!({ "request_id": request_id, "status": "ok", "result": result }).to_string();
    let (status, body) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/provider-result"),
        Some(TOKEN),
        &body,
    )
    .await;
    assert!(status.contains("200"), "provider-result: {status} {body}");
}

/// Answer the parked tool request `request_id` with a canned success.
async fn answer_tool(addr: std::net::SocketAddr, turn_id: &str, request_id: &str) {
    let body = json!({
        "request_id": request_id,
        "output": { "ok": { "content": "echoed" } },
    })
    .to_string();
    let (status, body) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/tool-result"),
        Some(TOKEN),
        &body,
    )
    .await;
    assert!(status.contains("200"), "tool-result: {status} {body}");
}

/// Read frames until the next `provider_request`, returning it.
async fn next_provider_request(
    reader: &mut tokio::io::BufReader<tokio::net::TcpStream>,
) -> serde_json::Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(reader))
            .await
            .expect("a provider_request must arrive")
            .expect("stream must not end while a provider_request is owed");
        if frame["type"] == "provider_request" {
            return frame;
        }
    }
}

/// A turn with a tool, so it runs at least two steps and therefore crosses at
/// least one step boundary — the place steer and pause act.
async fn create_tool_turn(addr: std::net::SocketAddr) -> String {
    let create = json!({
        "provider_id": "mock",
        "tools": [common::echo_tool()],
        "messages": [{ "role": "user", "content": "hi" }],
        "reverse_request_timeout_ms": 20_000,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "create: {status} {body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["turn_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The acceptance line of #932's steering half: a message POSTed mid-turn is
/// injected at the next step boundary — it reaches the model's next request
/// and is echoed on the event stream as `steered` — without the turn being
/// cancelled or the transcript lost.
#[tokio::test]
async fn a_steer_lands_in_the_next_model_request_and_echoes_on_the_stream() {
    let addr = start_server().await;
    let turn_id = create_tool_turn(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;

    // Step 1 is parked on its model call. Steer now: the queue drains at the
    // *next* boundary, i.e. before step 2's model call.
    let first = next_provider_request(&mut sse).await;
    let (status, body) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/steer"),
        Some(TOKEN),
        &json!({ "message": "actually, skip the migration" }).to_string(),
    )
    .await;
    assert!(status.contains("200"), "steer: {status} {body}");

    // Finish step 1 with a tool call so a step 2 exists.
    answer_provider(
        addr,
        &turn_id,
        first["request_id"].as_str().unwrap(),
        model_wants_echo(),
    )
    .await;
    let tool = loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a tool_request must arrive")
            .expect("stream must not end before the tool_request");
        if frame["type"] == "tool_request" {
            break frame;
        }
    };
    answer_tool(addr, &turn_id, tool["request_id"].as_str().unwrap()).await;

    // Step 2's model call must see the steered message in its transcript.
    let second = next_provider_request(&mut sse).await;
    let messages = second["request"]["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|m| { m["role"] == "user" && m["content"] == "actually, skip the migration" }),
        "the steered message must reach the next model request: {messages:?}"
    );

    // Let the turn finish, collecting the remaining frames: the `steered`
    // echo must be among the events the stream delivered.
    answer_provider(
        addr,
        &turn_id,
        second["request_id"].as_str().unwrap(),
        model_result("done"),
    )
    .await;
    let mut saw_steered = false;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("the stream must reach its terminal frame");
        let Some(frame) = frame else { break };
        if frame["type"] == "event" && frame["event"]["type"] == "steered" {
            assert_eq!(frame["event"]["text"], "actually, skip the migration");
            saw_steered = true;
        }
        if frame["type"] == "turn_complete" {
            assert_eq!(frame["outcome"]["status"], "completed");
            break;
        }
    }
    assert!(
        saw_steered,
        "the host must see its steer echoed as an event"
    );
}

/// Pause holds the turn at its next step boundary — no further model call is
/// dispatched — and resume releases exactly the same turn.
#[tokio::test]
async fn a_paused_turn_holds_at_the_boundary_until_resumed() {
    let addr = start_server().await;
    let turn_id = create_tool_turn(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;

    let first = next_provider_request(&mut sse).await;
    let (status, _) = post_json(addr, &format!("/v1/turns/{turn_id}/pause"), Some(TOKEN), "").await;
    assert!(status.contains("200"), "pause: {status}");

    // Finish step 1; step 2 must then park on the gate instead of asking for
    // its model call.
    answer_provider(
        addr,
        &turn_id,
        first["request_id"].as_str().unwrap(),
        model_wants_echo(),
    )
    .await;
    let tool = loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a tool_request must arrive")
            .expect("stream must not end before the tool_request");
        if frame["type"] == "tool_request" {
            break frame;
        }
    };
    answer_tool(addr, &turn_id, tool["request_id"].as_str().unwrap()).await;

    // While paused, whatever else the stream delivers (step events from the
    // settled step), no provider_request may appear.
    let held_until = tokio::time::Instant::now() + Duration::from_millis(300);
    loop {
        let remaining = held_until.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, next_event(&mut sse)).await {
            Err(_) => break, // quiet — exactly what paused should look like
            Ok(Some(frame)) => assert_ne!(
                frame["type"], "provider_request",
                "a paused turn must not dispatch a model call"
            ),
            Ok(None) => panic!("the stream must not end while the turn is paused"),
        }
    }

    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/resume"),
        Some(TOKEN),
        "",
    )
    .await;
    assert!(status.contains("200"), "resume: {status}");

    // The released turn continues: step 2's model call arrives, and the turn
    // can complete normally.
    let second = next_provider_request(&mut sse).await;
    answer_provider(
        addr,
        &turn_id,
        second["request_id"].as_str().unwrap(),
        model_result("done"),
    )
    .await;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("the stream must reach its terminal frame")
            .expect("the terminal frame must arrive");
        if frame["type"] == "turn_complete" {
            assert_eq!(frame["outcome"]["status"], "completed");
            break;
        }
    }
}

/// The liveness invariant from `stella-serve/src/controls.rs`: a cancel must
/// release the pause gate, or a paused-then-cancelled turn parks its OS
/// thread forever. This test hangs (and fails on its timeouts) if that
/// release is ever lost.
#[tokio::test]
async fn a_paused_turn_can_still_be_cancelled() {
    let addr = start_server().await;
    let turn_id = create_tool_turn(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;

    let first = next_provider_request(&mut sse).await;
    let (status, _) = post_json(addr, &format!("/v1/turns/{turn_id}/pause"), Some(TOKEN), "").await;
    assert!(status.contains("200"), "pause: {status}");

    // Drive the turn to the boundary so it is genuinely parked on the gate,
    // not on a reverse request.
    answer_provider(
        addr,
        &turn_id,
        first["request_id"].as_str().unwrap(),
        model_wants_echo(),
    )
    .await;
    let tool = loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a tool_request must arrive")
            .expect("stream must not end before the tool_request");
        if frame["type"] == "tool_request" {
            break frame;
        }
    };
    answer_tool(addr, &turn_id, tool["request_id"].as_str().unwrap()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/cancel"),
        Some(TOKEN),
        "",
    )
    .await;
    assert!(status.contains("200"), "cancel: {status}");

    // The cancelled turn must unwind to its terminal frame despite the pause.
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a paused turn must still unwind after cancel — the gate was not released")
            .expect("the terminal frame must arrive");
        if frame["type"] == "turn_complete" {
            assert_eq!(frame["outcome"]["status"], "aborted");
            break;
        }
    }
}

/// The control endpoints answer honestly at the edges: unknown turns are 404,
/// an empty steer is 400, and pause/resume are idempotent.
#[tokio::test]
async fn control_endpoints_answer_honestly_at_the_edges() {
    let addr = start_server().await;

    for path in ["steer", "pause", "resume"] {
        let (status, _) = post_json(
            addr,
            &format!("/v1/turns/turn-doesnotexist/{path}"),
            Some(TOKEN),
            &json!({ "message": "x" }).to_string(),
        )
        .await;
        assert!(status.contains("404"), "{path} on unknown turn: {status}");
    }

    let turn_id = create_turn(addr).await;
    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/steer"),
        Some(TOKEN),
        &json!({ "message": "" }).to_string(),
    )
    .await;
    assert!(status.contains("400"), "empty steer: {status}");

    for _ in 0..2 {
        let (status, _) =
            post_json(addr, &format!("/v1/turns/{turn_id}/pause"), Some(TOKEN), "").await;
        assert!(status.contains("200"), "pause is idempotent: {status}");
    }
    for _ in 0..2 {
        let (status, _) = post_json(
            addr,
            &format!("/v1/turns/{turn_id}/resume"),
            Some(TOKEN),
            "",
        )
        .await;
        assert!(status.contains("200"), "resume is idempotent: {status}");
    }

    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/cancel"),
        Some(TOKEN),
        "",
    )
    .await;
    assert!(status.contains("200"), "cleanup cancel: {status}");
}

/// A turn that already finished refuses controls with `409` rather than
/// accepting a steer that can never be drained — silence is the failure mode
/// this crate keeps being fixed for.
#[tokio::test]
async fn steering_a_finished_turn_is_a_conflict_not_a_silent_no_op() {
    // A wide resume window keeps the finished session parked in its entry
    // (rather than reaped) while the assertions below run.
    let (addr, capture) = start_observed_server_with(Duration::from_secs(30)).await;
    let turn_id = create_turn(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let first = next_provider_request(&mut sse).await;

    // Disconnect, then finish the turn while nobody is streaming: the session
    // settles and is parked back into its entry, finished.
    drop(sse);
    common::await_parked_after_disconnect(&capture).await;
    answer_provider(
        addr,
        &turn_id,
        first["request_id"].as_str().unwrap(),
        model_result("done"),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while capture
        .find(|e| matches!(e, ServeEvent::TurnSettled { .. }))
        .is_none()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the answered turn must settle"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    for (path, body) in [
        ("steer", json!({ "message": "too late" }).to_string()),
        ("pause", String::new()),
        ("resume", String::new()),
    ] {
        let (status, body) = post_json(
            addr,
            &format!("/v1/turns/{turn_id}/{path}"),
            Some(TOKEN),
            &body,
        )
        .await;
        assert!(
            status.contains("409"),
            "{path} on a finished turn: {status} {body}"
        );
    }
}

/// **#932's acceptance sentence, end to end:** "A host can pause a running
/// turn, inject a message, and resume it, without cancelling and losing the
/// transcript."
///
/// The three halves are each covered above in isolation; this is the
/// composition, and the composition is what the issue actually promises. It is
/// worth its own test because the expensive thing an agent turn holds is its
/// transcript — the whole argument for pause-and-steer over cancel-and-restart
/// is that the prompt prefix survives, and a test that only checked "the turn
/// did not die" would pass even if the history had been rebuilt from scratch.
///
/// So the assertion is on *identity*, not liveness: after a pause → steer →
/// resume cycle, the model's next request must still carry the original user
/// message, the assistant turn that followed it, and its tool result — with
/// the steered message appended rather than replacing them.
#[tokio::test]
async fn a_turn_survives_pause_steer_resume_with_its_transcript_intact() {
    let addr = start_server().await;
    let turn_id = create_tool_turn(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;

    // Step 1: let the model ask for a tool, so the turn has real history to
    // lose before anything is paused.
    let first = next_provider_request(&mut sse).await;
    let opening = first["request"]["messages"]
        .as_array()
        .expect("the first request carries the host's messages")
        .clone();
    assert_eq!(opening.len(), 1, "the turn starts from one user message");

    // Pause *before* answering, so the hold lands at the boundary between
    // step 1 and step 2 rather than racing the turn's end.
    let (status, body) =
        post_json(addr, &format!("/v1/turns/{turn_id}/pause"), Some(TOKEN), "").await;
    assert!(status.contains("200"), "pause: {status} {body}");

    answer_provider(
        addr,
        &turn_id,
        first["request_id"].as_str().unwrap(),
        model_wants_echo(),
    )
    .await;
    let tool = loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("a tool_request must arrive")
            .expect("stream must not end before the tool_request");
        if frame["type"] == "tool_request" {
            break frame;
        }
    };
    answer_tool(addr, &turn_id, tool["request_id"].as_str().unwrap()).await;

    // The turn is now held at the step boundary. Inject the correction a human
    // would type — the "actually, don't" the issue is written around.
    let (status, body) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/steer"),
        Some(TOKEN),
        &json!({ "message": "actually, stop after this" }).to_string(),
    )
    .await;
    assert!(status.contains("200"), "steer: {status} {body}");

    let (status, body) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/resume"),
        Some(TOKEN),
        "",
    )
    .await;
    assert!(status.contains("200"), "resume: {status} {body}");

    // The released turn's next model call is the proof. This is the SAME turn
    // — not a new one seeded with a copied history — so everything step 1
    // produced is still there, with the steer appended.
    let second = next_provider_request(&mut sse).await;
    let messages = second["request"]["messages"]
        .as_array()
        .expect("the resumed request carries a transcript");

    assert!(
        messages.len() > opening.len(),
        "the transcript grew across the pause rather than restarting: {messages:?}"
    );
    assert_eq!(
        messages[0], opening[0],
        "the original user message must survive the pause verbatim — a rebuilt \
         transcript would re-pay the prompt prefix this whole mechanism exists to keep",
    );
    assert!(
        messages.iter().any(|m| m["role"] == "assistant"),
        "the assistant turn from step 1 must survive: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m["role"] == "user" && m["content"] == "actually, stop after this"),
        "the steered message must be appended: {messages:?}"
    );

    // And the turn really does finish — held, not broken.
    answer_provider(
        addr,
        &turn_id,
        second["request_id"].as_str().unwrap(),
        model_result("stopped"),
    )
    .await;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
            .await
            .expect("the stream must reach its terminal frame");
        let Some(frame) = frame else { break };
        if frame["type"] == "turn_complete" {
            assert_eq!(
                frame["outcome"]["status"], "completed",
                "a paused-and-resumed turn completes; it is not cancelled",
            );
            break;
        }
    }
}
