//! End-to-end over a real socket: a mock host binds the server, POSTs a turn,
//! opens the SSE stream, and answers the engine's reverse-RPC requests with
//! `provider-result` / `tool-result` POSTs — exactly the protocol Oxagen's
//! client will speak. Proves the transport on top of the (separately proven)
//! `!Send` bridge.
//!
//! The harness itself lives in `common/mod.rs`; resumable-stream coverage lives
//! in `resume.rs`.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use stella_protocol::CompletionMessage;
use stella_serve::observe::{Capture, MisrouteFault, ReverseKind, ServeEvent, SettledOutcome};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn healthz_needs_no_auth_and_missing_token_is_rejected() {
    let addr = start_server().await;

    let (status, body) = get_json(addr, "/healthz").await;
    assert!(status.contains("200"), "health status: {status}");
    assert!(body.contains("\"status\":\"ok\""), "health body: {body}");

    // A turn without the bearer token is refused.
    let (status, _) = post_json(addr, "/v1/turns", None, "{}").await;
    assert!(status.contains("401"), "unauthenticated status: {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_max_steps_is_rejected_instead_of_starting_a_doomed_turn() {
    let addr = start_server().await;

    let create = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
        "max_steps": 0,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(
        status.contains("400"),
        "create status: {status}, body: {body}"
    );
    assert!(body.contains("max_steps"), "error names the field: {body}");

    // The ordinary path is untouched.
    let create = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
        "max_steps": 1,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(
        status.contains("200"),
        "create status: {status}, body: {body}"
    );
}

/// The reverse-request deadline is reachable from the wire, and an unanswered
/// reverse request fails the turn on it instead of wedging the connection. The
/// override keeps this test fast; the served default is five minutes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unanswered_reverse_request_fails_on_the_wire_deadline() {
    let addr = start_server().await;

    let create = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
        "reverse_request_timeout_ms": 50,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(
        status.contains("200"),
        "create status: {status}, body: {body}"
    );
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let turn_id = created["turn_id"].as_str().unwrap().to_string();

    // Stream the turn but never POST a provider-result.
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let mut outcome = None;
    while let Some(event) = next_event(&mut sse).await {
        if event["type"].as_str() == Some("turn_complete") {
            outcome = Some(event["outcome"].clone());
        }
    }

    let outcome = outcome.expect("the stream ended with a terminal frame, not a hang");
    assert_eq!(outcome["status"].as_str(), Some("aborted"));
    let reason = outcome["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("deadline"), "abort reason: {reason}");
}

/// A zero deadline would expire every reverse request before the host could
/// answer, so it is refused at the door rather than starting a doomed turn —
/// the same treatment `max_steps: 0` gets.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_reverse_request_timeout_is_rejected() {
    let addr = start_server().await;

    let create = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
        "reverse_request_timeout_ms": 0,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(
        status.contains("400"),
        "create status: {status}, body: {body}"
    );
    assert!(
        body.contains("reverse_request_timeout_ms"),
        "error names the field: {body}"
    );
}

/// An in-flight turn can be cancelled by its id: the parked reverse request wakes
/// at once, the stream still delivers a terminal `aborted` frame, and the id is
/// gone from the registry afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_flight_turn_can_be_cancelled_by_id() {
    let addr = start_server().await;

    let create = json!({
        "provider_id": "mock",
        "tools": [echo_tool()],
        "messages": [serde_json::to_value(CompletionMessage::user("cancel me")).unwrap()],
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(
        status.contains("200"),
        "create status: {status}, body: {body}"
    );
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let turn_id = created["turn_id"].as_str().unwrap().to_string();
    let cancel_path = format!("/v1/turns/{turn_id}/cancel");

    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;

    // Wait until the turn is genuinely in flight — parked on a reverse request
    // the host is expected to answer — then cancel instead of answering.
    let mut cancelled = false;
    let mut outcome = None;
    while let Some(event) = next_event(&mut sse).await {
        match event["type"].as_str().unwrap_or_default() {
            "provider_request" if !cancelled => {
                let (status, resp) = post_json(addr, &cancel_path, Some(TOKEN), "").await;
                assert!(status.contains("200"), "cancel: {status} {resp}");
                assert!(resp.contains("cancelled"), "cancel body: {resp}");
                cancelled = true;
            }
            "turn_complete" => outcome = Some(event["outcome"].clone()),
            _ => {}
        }
    }

    assert!(cancelled, "the turn was in flight before being cancelled");
    let outcome = outcome.expect("a cancelled turn still reports a terminal frame");
    assert_eq!(
        outcome["status"].as_str(),
        Some("aborted"),
        "outcome: {outcome}"
    );

    // The turn is gone: cancelling again, or answering it late, is a 404.
    let (status, _) = post_json(addr, &cancel_path, Some(TOKEN), "").await;
    assert!(status.contains("404"), "second cancel status: {status}");
    let late = json!({ "request_id": "prov-0", "status": "ok", "result": model_result("late") })
        .to_string();
    let (status, _) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/provider-result"),
        Some(TOKEN),
        &late,
    )
    .await;
    assert!(status.contains("404"), "late result status: {status}");
}

/// Cancellation is behind the bearer token like every other `/v1` route, and an
/// unknown id is a 404 rather than a silent success.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_requires_auth_and_404s_an_unknown_turn() {
    let addr = start_server().await;

    let (status, _) = post_json(addr, "/v1/turns/turn-99/cancel", None, "").await;
    assert!(status.contains("401"), "unauthenticated cancel: {status}");

    let (status, body) = post_json(addr, "/v1/turns/turn-99/cancel", Some(TOKEN), "").await;
    assert!(status.contains("404"), "unknown turn cancel: {status}");
    assert!(body.contains("unknown turn"), "body: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_turn_round_trips_over_http() {
    let addr = start_server().await;

    let create = json!({
        "provider_id": "mock",
        "tools": [echo_tool()],
        "messages": [serde_json::to_value(CompletionMessage::user("use echo then answer")).unwrap()],
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

    let mut provider_calls = 0;
    let mut tool_calls = 0;
    let mut outcome = None;

    while let Some(event) = next_event(&mut sse).await {
        match event["type"].as_str().unwrap_or_default() {
            "provider_request" => {
                provider_calls += 1;
                let request_id = event["request_id"].as_str().unwrap();
                let result = if provider_calls == 1 {
                    model_wants_echo()
                } else {
                    model_result("done")
                };
                let body = json!({
                    "request_id": request_id,
                    "status": "ok",
                    "result": result,
                })
                .to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-result"),
                    Some(TOKEN),
                    &body,
                )
                .await;
                assert!(status.contains("200"), "provider-result: {status} {resp}");
            }
            "tool_request" => {
                tool_calls += 1;
                assert_eq!(event["name"].as_str(), Some("echo"));
                let request_id = event["request_id"].as_str().unwrap();
                let body = json!({
                    "request_id": request_id,
                    "output": { "ok": { "content": "echoed" } },
                })
                .to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/tool-result"),
                    Some(TOKEN),
                    &body,
                )
                .await;
                assert!(status.contains("200"), "tool-result: {status} {resp}");
            }
            "turn_complete" => outcome = Some(event["outcome"].clone()),
            _ => {}
        }
    }

    assert_eq!(provider_calls, 2, "model called before and after the tool");
    assert_eq!(tool_calls, 1, "one tool call round-tripped over HTTP");
    let outcome = outcome.expect("turn produced a terminal outcome");
    assert_eq!(outcome["status"].as_str(), Some("completed"));
    assert_eq!(outcome["text"].as_str(), Some("done"));
}

/// The per-turn engine surface of #1167, end to end: the overrides land on
/// the completion request the host receives, a value past the operator
/// ceiling is clamped **and the clamp is reported in the create response**,
/// and one server answers turns at different postures without redeploying.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn engine_overrides_reach_the_provider_request_and_clamps_are_reported() {
    let addr = start_server().await;

    let create = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("cold and short")).unwrap()],
        "engine": {
            "temperature": 0.0,
            "max_output_tokens": u32::MAX,
            "effort": "xhigh",
        },
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "create: {status} {body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let turn_id = created["turn_id"].as_str().unwrap().to_string();
    let clamped = created["clamped"]
        .as_array()
        .expect("the ceiling-clamped knob must be reported");
    assert_eq!(clamped.len(), 1, "exactly one knob was lowered: {body}");
    assert_eq!(clamped[0]["knob"].as_str(), Some("max_output_tokens"));
    assert!(
        clamped[0]["effective"].as_f64().unwrap() < clamped[0]["requested"].as_f64().unwrap(),
        "the report shows the lowering: {body}"
    );

    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    while let Some(event) = next_event(&mut sse).await {
        match event["type"].as_str().unwrap_or_default() {
            "provider_request" => {
                let request = &event["request"];
                assert_eq!(request["temperature"].as_f64(), Some(0.0));
                assert_eq!(request["effort"].as_str(), Some("xhigh"));
                let cap = request["max_output_tokens"].as_u64().unwrap();
                assert!(
                    cap < u64::from(u32::MAX),
                    "the clamped cap is what actually reaches the model: {cap}"
                );
                let body = json!({
                    "request_id": event["request_id"].as_str().unwrap(),
                    "status": "ok",
                    "result": model_result("done"),
                })
                .to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-result"),
                    Some(TOKEN),
                    &body,
                )
                .await;
                assert!(status.contains("200"), "provider-result: {status} {resp}");
            }
            "turn_complete" => break,
            _ => {}
        }
    }

    // The same server, a different posture — no `clamped` key when nothing
    // was lowered, so pre-#1167 clients keep parsing the response they know.
    let create = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("hot")).unwrap()],
        "engine": { "temperature": 1.0, "max_output_tokens": 32000 },
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "second create: {status} {body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        created.get("clamped").is_none(),
        "an unclamped create must not grow the key: {body}"
    );
}

/// Unusable engine values — and typoed knobs — are refused with a 400 naming
/// the problem, never silently dropped or silently honored.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unusable_engine_overrides_are_rejected_by_name() {
    let addr = start_server().await;

    for (engine, named) in [
        (json!({ "max_output_tokens": 0 }), "max_output_tokens"),
        (json!({ "temperature": -0.5 }), "temperature"),
        (json!({ "temprature": 0.5 }), "temprature"),
    ] {
        let create = json!({
            "provider_id": "mock",
            "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
            "engine": engine,
        })
        .to_string();
        let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
        assert!(status.contains("400"), "expected a 400: {status} {body}");
        assert!(
            body.contains(named),
            "the refusal must name `{named}`: {body}"
        );
    }
}

/// The provider-delta route of #1165 over a real socket: fragments POSTed for
/// an in-flight provider request surface as `text_delta` frames on the SSE
/// stream before the completion, an empty batch is refused, and fragments
/// arriving after the result get an honest 409.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_deltas_stream_onto_the_event_stream_over_http() {
    let addr = start_server().await;

    let create = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("stream it")).unwrap()],
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "create: {status} {body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let turn_id = created["turn_id"].as_str().unwrap().to_string();

    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let mut text_deltas: Vec<String> = Vec::new();
    let mut completed_at: Option<usize> = None;
    let mut frames = 0usize;
    let mut request_id = String::new();

    while let Some(event) = next_event(&mut sse).await {
        frames += 1;
        match event["type"].as_str().unwrap_or_default() {
            "provider_request" => {
                request_id = event["request_id"].as_str().unwrap().to_string();

                // An empty batch is refused: it would not reset the idle
                // deadline, so accepting it would fake liveness.
                let empty = json!({ "request_id": request_id, "deltas": [] }).to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-delta"),
                    Some(TOKEN),
                    &empty,
                )
                .await;
                assert!(status.contains("400"), "empty batch: {status} {resp}");

                let deltas = json!({
                    "request_id": request_id,
                    "deltas": [
                        { "kind": "text", "text": "Hel" },
                        { "kind": "text", "text": "lo" },
                    ],
                })
                .to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-delta"),
                    Some(TOKEN),
                    &deltas,
                )
                .await;
                assert!(status.contains("200"), "delta batch: {status} {resp}");

                let result = json!({
                    "request_id": request_id,
                    "status": "ok",
                    "result": model_result("Hello"),
                })
                .to_string();
                let (status, resp) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-result"),
                    Some(TOKEN),
                    &result,
                )
                .await;
                assert!(status.contains("200"), "provider-result: {status} {resp}");
            }
            // Agent events ride nested under the `event` frame type.
            "event" if event["event"]["type"].as_str() == Some("text_delta") => {
                text_deltas.push(
                    event["event"]["delta"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                );
            }
            "turn_complete" => {
                completed_at = Some(frames);
                break;
            }
            _ => {}
        }
    }

    assert_eq!(
        text_deltas,
        vec!["Hel".to_string(), "lo".to_string()],
        "the fragments must surface as text_delta frames, in order"
    );
    assert!(completed_at.is_some(), "the turn still completes");

    // The request is resolved; late fragments must 409, not vanish.
    let late = json!({
        "request_id": request_id,
        "deltas": [{ "kind": "text", "text": "too late" }],
    })
    .to_string();
    let (status, resp) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/provider-delta"),
        Some(TOKEN),
        &late,
    )
    .await;
    assert!(
        status.contains("409") || status.contains("404"),
        "late fragments must be refused (409 while the turn lives, 404 once \
         it is gone): {status} {resp}"
    );
}

/// The whole response head, not just the status line — for assertions about
/// headers a rejection must carry (`Retry-After` on a 429).
async fn post_json_head(
    addr: SocketAddr,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (String, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    (head.to_string(), body.to_string())
}

fn create_body() -> String {
    json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
    })
    .to_string()
}

/// A body one byte over the cap used to get *no response at all* — the connection
/// simply closed, so a host could not tell "too large" from a crashed peer, and a
/// `tool-result` that tripped it left its engine step parked until teardown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oversized_body_is_refused_with_413_rather_than_silence() {
    let addr = start_server().await;

    // Declared, not sent: the cap is enforced on the header, so the server never
    // buffers the body it is about to refuse.
    let declared = 8 * 1024 * 1024 + 1;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST /v1/turns HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();

    assert!(
        response.contains("413"),
        "an over-cap body must be answered, not silently dropped; got: {response:?}"
    );
}

/// Every live turn holds an OS thread until its stream ends, and nothing reclaims
/// one the host abandons — so without a cap an authenticated caller could
/// register turns until the process ran out of threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_live_turn_cap_refuses_further_turns_with_retry_after() {
    let addr = start_server().await;

    // 32 is the cap; none of these is streamed, so all stay live.
    for i in 0..32 {
        let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create_body()).await;
        assert!(
            status.contains("200"),
            "turn {i} should be admitted: {status} {body}"
        );
    }

    let (head, body) = post_json_head(addr, "/v1/turns", Some(TOKEN), &create_body()).await;
    assert!(
        head.contains("429"),
        "the 33rd live turn must be refused: {head} {body}"
    );
    assert!(
        head.to_ascii_lowercase().contains("retry-after"),
        "a 429 must tell the caller when to come back: {head}"
    );

    // The cap admits again once a turn is reclaimed, so it is a queue and not a
    // one-way latch — a latch would take the server down permanently. That the
    // registry *can* drain is proven over the wire by
    // `abandoned_finished_turns_do_not_wedge_the_cap` below.
    let (status, _) = post_json(
        addr,
        "/v1/turns/turn-does-not-exist/cancel",
        Some(TOKEN),
        "",
    )
    .await;
    assert!(status.contains("404"), "sanity: unknown turn is a 404");
}

/// The cap counts registry entries, and a turn that *finished* without ever
/// being streamed still holds one. A stream ending and an explicit cancel are
/// the only other reclaimers, and an abandoned turn reaches neither — so
/// without `reclaim_finished_unstreamed` a host that creates turns and walks
/// away drives the server into a permanent `429`, no matter how long anyone
/// waits. The test above states that this cannot happen; this one proves it,
/// end to end, over a socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandoned_finished_turns_do_not_wedge_the_cap() {
    let addr = start_server().await;

    // Nothing answers the reverse request these turns park on, so each aborts
    // on its own 50 ms deadline — settled, and never streamed by anyone.
    let abandoned = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
        "reverse_request_timeout_ms": 50,
    })
    .to_string();

    for i in 0..32 {
        let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &abandoned).await;
        assert!(
            status.contains("200"),
            "turn {i} should be admitted: {status} {body}"
        );
    }

    // Poll rather than sleep a tuned interval: the turns settle on their own
    // schedule, and the claim under test is "eventually admitted", not
    // "admitted within N ms". The deadline is what makes a server that never
    // reclaims fail here instead of hanging.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &abandoned).await;
        if status.contains("200") {
            break;
        }
        assert!(
            status.contains("429"),
            "the only legitimate refusal here is the cap: {status} {body}"
        );
        assert!(
            Instant::now() < deadline,
            "a registry full of finished, unstreamed turns never admitted a new \
             one — the live-turn cap is a one-way latch"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The bearer check now runs on the request *head*, before a byte of body is
/// buffered — an unauthenticated peer must not be able to make the server hold
/// megabytes on its behalf, once per connection, on a server that deliberately
/// caps turns rather than connections.
///
/// What a socket can assert is the other half of that change: the body is still
/// drained before the 401 is written, so the response actually arrives. Answer
/// and close with a megabyte still in flight and the kernel sends an RST — the
/// client is mid-`write_all`, never reads the status, and an operator sees a
/// connection reset instead of "your token is wrong".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_unauthenticated_body_is_answered_401_rather_than_reset() {
    let addr = start_server().await;

    let body = "x".repeat(1024 * 1024);
    let (status, response) = post_json(addr, "/v1/turns", None, &body).await;
    assert!(
        status.contains("401"),
        "an unauthenticated megabyte must still get its 401: {status} {response}"
    );
    assert!(
        response.contains("bearer token"),
        "and the whole body must arrive, not a truncated write: {response}"
    );
}

/// A known path reached with the wrong verb is a 405 naming what it accepts.
/// It used to be a 404, which sends an integrator hunting for a typo in a path
/// that is perfectly correct.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_known_path_with_the_wrong_method_is_405_with_an_allow_header() {
    let addr = start_server().await;

    // POST to the SSE stream (a GET route).
    let (head, _) = post_json_head(addr, "/v1/turns/turn-abc/events", Some(TOKEN), "").await;
    assert!(head.contains("405"), "wrong method on /events: {head}");
    assert!(
        head.to_ascii_lowercase().contains("allow: get"),
        "a 405 must name the methods it accepts (RFC 9110 §15.5.6): {head}"
    );

    // GET the create route (a POST route).
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET /v1/turns HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(
        response.contains("405"),
        "wrong method on create: {response}"
    );
    assert!(
        response.to_ascii_lowercase().contains("allow: post"),
        "{response}"
    );

    // A genuinely unknown path stays a 404 — the two must not blur together.
    let (status, _) = post_json(addr, "/v1/nope", Some(TOKEN), "{}").await;
    assert!(status.contains("404"), "unknown path: {status}");
}

/// Chunked framing is not decoded, and used to surface as a 400 blaming the
/// host's JSON for a body the server never assembled. Name the real cause.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chunked_request_is_refused_by_name() {
    let addr = start_server().await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST /v1/turns HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\n{{}}\r\n0\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.contains("501"), "{response}");
    assert!(response.contains("Content-Length"), "{response}");
}

/// A host that walks away mid-turn must not leave the engine running. The SSE
/// stream is the only thing holding the turn, and the engine is parked on a
/// reverse request nobody will ever answer — so until the stream noticed the
/// disconnect, the turn kept an OS thread and a registry slot for the whole
/// `reverse_request_timeout` (five minutes by default, an hour at the ceiling),
/// and any work the engine went on to request was billed to a client that had
/// already gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_the_event_stream_cancels_the_turn() {
    let addr = start_server().await;

    // The default five-minute reverse-request deadline: nothing but the
    // disconnect can end this turn inside the test's own deadline.
    let create = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "create: {status} {body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let turn_id = created["turn_id"].as_str().unwrap().to_string();

    // Stream until the engine is genuinely parked on a reverse request, then
    // hang up without answering it.
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    loop {
        let event = next_event(&mut sse)
            .await
            .expect("the turn reaches its first reverse request");
        if event["type"].as_str() == Some("provider_request") {
            break;
        }
    }
    drop(sse);

    // The turn is torn down: its id leaves the registry. Not *immediately* —
    // a disconnect now parks the turn for `resume_grace` so a reconnect can
    // pick it up (#971) — but the property this test exists for is unchanged
    // and is the one that matters: a client that never comes back does not
    // hold an engine thread forever. The suite's window is short, so the
    // reclaim lands well inside the deadline below.
    //
    // Probed with a deliberately malformed `tool-result` POST rather than a
    // second `/events` GET. The GET used to be the non-destructive probe —
    // 409 while registered, 404 once reclaimed — but parking made it
    // *resumptive*: it now takes the parked session and starts streaming,
    // which is the very thing being measured. The POST reads the registry
    // without taking anything: 400 (bad body) while the turn is registered,
    // 404 once it is gone.
    let probe_path = format!("/v1/turns/{turn_id}/tool-result");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, _) = post_json(addr, &probe_path, Some(TOKEN), "not json").await;
        if status.contains("404") {
            break;
        }
        assert!(
            status.contains("400"),
            "the only other legitimate answer is 'that body is not a tool result': {status}"
        );
        assert!(
            Instant::now() < deadline,
            "a turn whose only subscriber disconnected is still registered long \
             after its resume window — the engine thread outlives the client that \
             asked for it"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Sequential ids (`turn-0`, `turn-1`, …) made every other live turn addressable
/// to anyone who saw one in a log line or a proxy trace. The bearer token is
/// still the auth gate; this removes the accidental second way in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turn_ids_are_not_guessable_from_another_turns_id() {
    let addr = start_server().await;

    let mut ids = Vec::new();
    for _ in 0..3 {
        let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create_body()).await;
        assert!(status.contains("200"), "create: {status} {body}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        ids.push(value["turn_id"].as_str().unwrap().to_string());
    }

    for id in &ids {
        let suffix = id.strip_prefix("turn-").expect("ids keep the turn- prefix");
        assert_eq!(suffix.len(), 32, "128 bits of hex: {id}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "id must be hex, not a counter: {id}"
        );
        assert!(
            suffix.parse::<u128>().is_err() || suffix.len() == 32,
            "id must not be a bare decimal counter: {id}"
        );
    }
    assert_ne!(ids[0], ids[1]);
    assert_ne!(ids[1], ids[2]);
}

// ---------------------------------------------------------------------------
// Observability (#930)
//
// The acceptance bar is behavioural: "a wedged turn, a misrouted `request_id`,
// and a token brute-force are each identifiable from the server's output alone,
// without attaching a debugger." So these drive the real server over a real
// socket and assert on the **typed** events it emitted, never on scraped text —
// an assertion on `ServeEvent::ReverseTimedOut { .. }` survives any change to
// how a record is rendered, and a phrase-matching one does not.
// ---------------------------------------------------------------------------

/// Poll until `capture` holds at least `want` events matching `predicate`.
///
/// A record is emitted as the connection future returns, which is just after
/// the client observes EOF — so asserting immediately races the server by
/// microseconds. Polling closes that window without making the test sleep for a
/// fixed guess.
async fn await_events(
    capture: &Arc<Capture>,
    predicate: impl Fn(&ServeEvent) -> bool + Copy,
    want: usize,
    label: &str,
) -> Vec<ServeEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let found: Vec<ServeEvent> = capture.events().into_iter().filter(predicate).collect();
        if found.len() >= want {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {want} `{label}` event(s); saw {}",
            found.len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// #930, situation 1: a turn wedged on a reverse request that will never be
/// answered. Before this, it and a healthy turn produced identical output —
/// none. The wedge signal was a silent `HashMap::remove` inside
/// `Pending::abandon`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wedged_turn_is_identifiable_from_the_servers_output_alone() {
    let (addr, capture) = start_observed_server().await;

    let create = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
        "reverse_request_timeout_ms": 50,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "create: {status} {body}");
    let turn_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["turn_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Stream the turn but never answer its provider request.
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    while next_event(&mut sse).await.is_some() {}

    let timed_out = await_events(
        &capture,
        |e| matches!(e, ServeEvent::ReverseTimedOut { .. }),
        1,
        "reverse_timed_out",
    )
    .await;
    match &timed_out[0] {
        ServeEvent::ReverseTimedOut {
            kind,
            waited_ms,
            turn,
            ..
        } => {
            assert_eq!(*kind, ReverseKind::Provider);
            assert!(*waited_ms >= 50, "the wait must be reported: {waited_ms}ms");
            assert!(
                turn_id.contains(&turn.to_string()),
                "the record must correlate to the turn it wedged"
            );
        }
        other => panic!("expected a timeout record, got {other:?}"),
    }

    // Wedged, not merely slow: the turn settled without its stages advancing
    // past the first model call. That distinction is the whole reason the
    // tally is folded from the engine's own event stream.
    let settled = await_events(
        &capture,
        |e| matches!(e, ServeEvent::TurnSettled { .. }),
        1,
        "turn_settled",
    )
    .await;
    match &settled[0] {
        ServeEvent::TurnSettled { outcome, tally, .. } => {
            assert_eq!(*outcome, SettledOutcome::Aborted);
            assert_eq!(
                tally.model_calls, 0,
                "no model call ever committed — that is what wedged means"
            );
        }
        other => panic!("expected a settle record, got {other:?}"),
    }
}

/// #930, situation 2: a host answering with the wrong `request_id`. The `409`
/// it receives is unchanged; what changes is that the server says so, and
/// distinguishes "no such request" from "right request, wrong kind".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_misrouted_request_id_is_recorded_with_its_fault() {
    let (addr, capture) = start_observed_server().await;

    let create = json!({
        "provider_id": "mock",
        "tools": [echo_tool()],
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
        "reverse_request_timeout_ms": 3000,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "create: {status} {body}");
    let turn_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["turn_id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    while let Some(event) = next_event(&mut sse).await {
        if event["type"].as_str() == Some("provider_request") {
            let real_id = event["request_id"].as_str().unwrap().to_string();

            // (a) An id nobody is waiting on.
            let fabricated = json!({
                "request_id": "prov-999",
                "status": "ok",
                "result": model_result("done"),
            })
            .to_string();
            let (status, _) = post_json(
                addr,
                &format!("/v1/turns/{turn_id}/provider-result"),
                Some(TOKEN),
                &fabricated,
            )
            .await;
            assert!(status.contains("409"), "a stale id is a 409: {status}");

            // (b) A real id, answered with the wrong kind of result.
            let miskinded = json!({
                "request_id": real_id,
                "output": { "ok": { "content": "x" } },
            })
            .to_string();
            let (status, _) = post_json(
                addr,
                &format!("/v1/turns/{turn_id}/tool-result"),
                Some(TOKEN),
                &miskinded,
            )
            .await;
            assert!(
                status.contains("409"),
                "a mis-kinded answer is a 409: {status}"
            );

            // Now answer properly so the turn unwinds instead of running out
            // its deadline.
            let good = json!({
                "request_id": real_id,
                "status": "ok",
                "result": model_result("done"),
            })
            .to_string();
            let _ = post_json(
                addr,
                &format!("/v1/turns/{turn_id}/provider-result"),
                Some(TOKEN),
                &good,
            )
            .await;
        }
    }

    let misroutes = await_events(
        &capture,
        |e| matches!(e, ServeEvent::ReverseMisrouted { .. }),
        2,
        "reverse_misrouted",
    )
    .await;
    let faults: Vec<MisrouteFault> = misroutes
        .iter()
        .filter_map(|e| match e {
            ServeEvent::ReverseMisrouted { fault, .. } => Some(*fault),
            _ => None,
        })
        .collect();
    assert!(
        faults.contains(&MisrouteFault::UnknownRequest),
        "a fabricated id must be named as unknown: {faults:?}"
    );
    assert!(
        faults.contains(&MisrouteFault::KindMismatch),
        "a right-id/wrong-kind answer must be named as a mismatch: {faults:?}"
    );
}

/// #930, situation 4: someone brute-forcing the bearer token.
///
/// Also asserts the throttle stays invisible to the guesser: every response is
/// byte-identical whether or not the bucket was empty, so the only place the
/// asymmetry shows is the server's own record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_brute_force_is_visible_inside_and_invisible_outside() {
    let (addr, capture) = start_observed_server().await;

    // The burst allowance is 8; go past it so the throttle engages.
    const ATTEMPTS: usize = 11;
    let mut responses = Vec::new();
    for attempt in 0..ATTEMPTS {
        let guess = format!("wrong-token-{attempt}");
        responses.push(get_json_authed(addr, "/v1/turns", &guess).await);
    }

    for (status, body) in &responses {
        assert!(status.contains("401"), "every guess is a 401: {status}");
        assert_eq!(
            body, &responses[0].1,
            "the body must not vary with the throttle's state — that would \
             tell a guesser they had been noticed"
        );
    }

    let unauthorized = await_events(
        &capture,
        |e| matches!(e, ServeEvent::Unauthorized { .. }),
        ATTEMPTS,
        "unauthorized",
    )
    .await;
    let held: Vec<u64> = unauthorized
        .iter()
        .filter_map(|e| match e {
            ServeEvent::Unauthorized { held_ms, .. } => Some(*held_ms),
            _ => None,
        })
        .collect();
    assert_eq!(held.len(), ATTEMPTS);
    assert!(
        held.iter().any(|ms| *ms > 0),
        "a sustained guess must be distinguishable from a misconfigured \
         client — some attempts have to show as held: {held:?}"
    );
    assert!(
        held.iter().take(4).all(|ms| *ms == 0),
        "the burst must stay free, so a restarting host is not punished: {held:?}"
    );
}

/// The counters are behind the same token as everything else, and they agree
/// with what the log saw — because `Metrics` folds the *same* event stream
/// rather than being incremented at its own call sites.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metrics_are_authenticated_and_agree_with_the_records() {
    let (addr, capture) = start_observed_server().await;

    let (status, _) = get_json(addr, "/v1/metrics").await;
    assert!(
        status.contains("401"),
        "metrics describe the host's traffic; an open endpoint is free \
         reconnaissance: {status}"
    );

    // Two turns, so the counters have something to disagree about.
    for _ in 0..2 {
        let create = json!({
            "provider_id": "mock",
            "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
            "reverse_request_timeout_ms": 30,
        })
        .to_string();
        let (status, _) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
        assert!(status.contains("200"));
    }

    let created = await_events(
        &capture,
        |e| matches!(e, ServeEvent::TurnCreated { .. }),
        2,
        "turn_created",
    )
    .await;

    let (status, body) = get_json_authed(addr, "/v1/metrics", TOKEN).await;
    assert!(status.contains("200"), "metrics: {status} {body}");
    let snapshot: serde_json::Value = serde_json::from_str(&body).expect("metrics is JSON");
    assert_eq!(
        snapshot["turns_created_total"].as_u64(),
        Some(created.len() as u64),
        "a counter must never disagree with the log: {body}"
    );
    assert!(
        snapshot["requests_total"].as_u64().unwrap_or(0) >= 3,
        "the metrics request itself and the creates are counted: {body}"
    );
    for (key, value) in snapshot.as_object().expect("a flat object") {
        assert!(
            value.is_number(),
            "`{key}` is not a number — only counts may leave this endpoint"
        );
    }
}

/// Every response carries `X-Request-Id`, and a host-supplied one is honoured
/// so a caller can correlate across the boundary — unless it is unsafe, in
/// which case it is replaced rather than echoed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_id_is_echoed_and_a_hostile_one_is_replaced() {
    let addr = start_server().await;

    let head = raw_request(
        addr,
        "GET /healthz HTTP/1.1\r\nHost: localhost\r\nX-Request-Id: abc-123\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(
        head.contains("X-Request-Id: abc-123"),
        "a usable id must be echoed for correlation: {head}"
    );

    let head = raw_request(
        addr,
        "GET /healthz HTTP/1.1\r\nHost: localhost\r\nX-Request-Id: has space and \"quotes\"\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(
        !head.contains("has space"),
        "an unsafe id must be replaced, not echoed: {head}"
    );
    assert!(
        head.contains("X-Request-Id: "),
        "and a generated one takes its place: {head}"
    );
}

/// Write raw bytes, half-close, and read the whole response.
async fn raw_request(addr: SocketAddr, request: &str) -> String {
    raw_bytes(addr, request.as_bytes()).await
}

/// The byte-level version, for input that is not valid UTF-8 or not a request
/// at all.
async fn raw_bytes(addr: SocketAddr, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let _ = stream.write_all(request).await;
    // Half-close so the server sees EOF rather than waiting out its 30-second
    // read deadline on input that will never be completed.
    let _ = stream.shutdown().await;
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await;
    String::from_utf8_lossy(&buf).into_owned()
}

/// **The** property: for *any* bytes a peer can put on a connection —
/// malformed, truncated, oversized, unauthenticated, well-formed, or nothing at
/// all — the server emits exactly one terminal record.
///
/// Hand-listed cases are how the original gap was missed: the bug is always a
/// path nobody enumerated. Distributing an `emit()` across `route`'s sixteen
/// exits would pass every example test anyone thought to write and still fail
/// here the first time a refactor added a seventeenth.
#[test]
fn exactly_one_record_per_connection_for_any_input() {
    use proptest::prelude::*;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("test runtime");
    let (addr, capture) = runtime.block_on(start_observed_server());

    let count = |capture: &Arc<Capture>| {
        capture.count(|e| matches!(e, ServeEvent::RequestCompleted { .. }))
    };

    let config = ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    proptest!(config, |(raw in proptest::collection::vec(any::<u8>(), 0..320))| {
        let before = count(&capture);
        runtime.block_on(async {
            raw_bytes(addr, &raw).await;
            // The record lands as the connection future returns, just after the
            // client sees EOF. Wait for it rather than guessing a sleep.
            let deadline = Instant::now() + Duration::from_secs(5);
            while count(&capture) == before && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            // Then give a duplicate a chance to appear, so "exactly one" is a
            // real claim and not just "at least one".
            tokio::time::sleep(Duration::from_millis(15)).await;
        });
        let after = count(&capture);
        prop_assert_eq!(
            after - before,
            1,
            "input of {} byte(s) produced {} records, not exactly 1",
            raw.len(),
            after - before
        );
    });
}

/// No record carries content, and the sweep is not vacuous.
///
/// The shape `stella-store::content_free` uses for egress, applied here to a
/// surface that does not egress but is routinely shipped: operators collect
/// stderr. A turn is poisoned at every point host data enters — the prompt, the
/// tool's advertised name, the model's output text, the tool result — and every
/// emitted record is swept for those markers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_content_reaches_a_record() {
    const PROMPT_SENTINEL: &str = "STELLA-SENTINEL-CONTENT.prompt";
    const OUTPUT_SENTINEL: &str = "STELLA-SENTINEL-CONTENT.model-output";
    const TOOL_RESULT_SENTINEL: &str = "STELLA-SENTINEL-CONTENT.tool-result";
    const PATH_SENTINEL: &str = "/stella-sentinel-path/must-never-egress";

    let (addr, capture) = start_observed_server().await;

    let create = json!({
        "provider_id": "mock",
        "tools": [echo_tool()],
        "messages": [
            serde_json::to_value(CompletionMessage::user(
                format!("{PROMPT_SENTINEL} at {PATH_SENTINEL}")
            )).unwrap()
        ],
        "reverse_request_timeout_ms": 3000,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "create: {status} {body}");
    let turn_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["turn_id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let mut provider_calls = 0;
    while let Some(event) = next_event(&mut sse).await {
        match event["type"].as_str().unwrap_or_default() {
            "provider_request" => {
                provider_calls += 1;
                let request_id = event["request_id"].as_str().unwrap();
                let result = if provider_calls == 1 {
                    model_wants_echo()
                } else {
                    model_result(OUTPUT_SENTINEL)
                };
                let body = json!({
                    "request_id": request_id,
                    "status": "ok",
                    "result": result,
                })
                .to_string();
                let _ = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-result"),
                    Some(TOKEN),
                    &body,
                )
                .await;
            }
            "tool_request" => {
                let request_id = event["request_id"].as_str().unwrap();
                let body = json!({
                    "request_id": request_id,
                    "output": { "ok": { "content": TOOL_RESULT_SENTINEL } },
                })
                .to_string();
                let _ = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/tool-result"),
                    Some(TOKEN),
                    &body,
                )
                .await;
            }
            _ => {}
        }
    }

    let events = await_events(
        &capture,
        |e| matches!(e, ServeEvent::TurnSettled { .. }),
        1,
        "turn_settled",
    )
    .await;
    assert!(!events.is_empty());

    // --- the vacuity guard -------------------------------------------------
    //
    // Without this, an encoder — or a test — passes every sentinel check by
    // recording nothing at all. `content_free.rs` learned this the same way.
    let all = capture.events();
    assert!(
        all.len() >= 4,
        "the sweep is vacuous: only {} record(s) were emitted",
        all.len()
    );
    for (label, seen) in [
        (
            "request_completed",
            all.iter()
                .any(|e| matches!(e, ServeEvent::RequestCompleted { .. })),
        ),
        (
            "turn_created",
            all.iter()
                .any(|e| matches!(e, ServeEvent::TurnCreated { .. })),
        ),
        (
            "reverse_dispatched",
            all.iter()
                .any(|e| matches!(e, ServeEvent::ReverseDispatched { .. })),
        ),
        (
            "reverse_answered",
            all.iter()
                .any(|e| matches!(e, ServeEvent::ReverseAnswered { .. })),
        ),
        (
            "turn_settled",
            all.iter()
                .any(|e| matches!(e, ServeEvent::TurnSettled { .. })),
        ),
    ] {
        assert!(seen, "no `{label}` record — the sweep would pass vacuously");
    }

    // --- the sweep ---------------------------------------------------------
    let rendered = serde_json::to_string(&all).expect("records serialize");
    for sentinel in [
        PROMPT_SENTINEL,
        OUTPUT_SENTINEL,
        TOOL_RESULT_SENTINEL,
        PATH_SENTINEL,
        "STELLA-SENTINEL-CONTENT",
    ] {
        assert!(
            !rendered.contains(sentinel),
            "`{sentinel}` reached a record. That is a privacy incident, not a \
             test failure: records go to stderr, and operators ship stderr."
        );
    }
    // Check the *hex suffix*, not the `turn-` prefixed id: a record holds the
    // bare hex, so asserting on the prefixed form would let a full-length
    // `TurnRef` through — which is exactly what it did the first time.
    let hex = turn_id.strip_prefix("turn-").expect("ids keep the prefix");
    assert!(
        !rendered.contains(hex),
        "the whole turn id reached a record — it is a second factor (token AND \
         id), and only a truncated `TurnRef` may be written"
    );
    assert!(
        rendered.contains(&hex[..8]),
        "records must still correlate to the turn: no handle at all is the \
         opposite failure"
    );
    assert!(
        !rendered.contains(TOKEN),
        "the bearer token reached a record"
    );
}
