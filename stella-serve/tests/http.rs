//! End-to-end over a real socket: a mock host binds the server, POSTs a turn,
//! opens the SSE stream, and answers the engine's reverse-RPC requests with
//! `provider-result` / `tool-result` POSTs — exactly the protocol Oxagen's
//! client will speak. Proves the transport on top of the (separately proven)
//! `!Send` bridge.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use serde_json::json;
use stella_protocol::{CompletionMessage, ToolSchema};
use stella_serve::{ServeConfig, serve};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

const TOKEN: &str = "test-secret";

fn echo_tool() -> serde_json::Value {
    serde_json::to_value(ToolSchema {
        name: "echo".to_string(),
        description: "echo".to_string(),
        input_schema: json!({ "type": "object" }),
        read_only: false,
    })
    .unwrap()
}

/// Start the server on an ephemeral loopback port; returns its address.
async fn start_server() -> SocketAddr {
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let config = ServeConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: TOKEN.to_string(),
        };
        let _ = serve(config, move |addr| {
            let _ = tx.send(addr);
        })
        .await;
    });
    rx.await.expect("server reported its bound address")
}

/// POST a JSON body and read the whole response (server sends `Connection:
/// close`). Returns `(status_line, body)`.
async fn post_json(
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
        "POST {path} HTTP/1.1\r\nHost: engine\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head.lines().next().unwrap_or_default().to_string();
    (status, body.to_string())
}

/// GET a plain endpoint (used for `/healthz`), returning `(status_line, body)`.
async fn get_json(addr: SocketAddr, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: engine\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    (
        head.lines().next().unwrap_or_default().to_string(),
        body.to_string(),
    )
}

/// GET with the bearer token, reading the whole response. Used to probe a
/// route's status without the side effect a POST would carry.
async fn get_json_authed(addr: SocketAddr, path: &str, token: &str) -> (String, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: engine\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    (
        head.lines().next().unwrap_or_default().to_string(),
        body.to_string(),
    )
}

/// Open the SSE stream and consume the HTTP response head, leaving the reader at
/// the first event.
async fn open_sse(addr: SocketAddr, path: &str, token: &str) -> BufReader<TcpStream> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: engine\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    reader
}

/// Read one SSE `data:` payload; `None` at end of stream.
async fn next_event(reader: &mut BufReader<TcpStream>) -> Option<serde_json::Value> {
    let mut data: Option<String> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.ok()?;
        if n == 0 {
            return data.map(|d| serde_json::from_str(&d).unwrap());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if let Some(d) = &data {
                return Some(serde_json::from_str(d).unwrap());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("data: ") {
            data = Some(rest.to_string());
        }
    }
}

fn model_result(text: &str) -> serde_json::Value {
    json!({
        "text": text,
        "usage": { "input_tokens": 0, "output_tokens": 0 },
        "model": "mock",
        "cost_usd": 0.0,
    })
}

fn model_wants_echo() -> serde_json::Value {
    json!({
        "text": "",
        "tool_calls": [{ "call_id": "c1", "name": "echo", "input": { "text": "hi" } }],
        "usage": { "input_tokens": 0, "output_tokens": 0 },
        "model": "mock",
        "cost_usd": 0.0,
    })
}

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
        "POST {path} HTTP/1.1\r\nHost: engine\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
        "POST /v1/turns HTTP/1.1\r\nHost: engine\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
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
        "GET /v1/turns HTTP/1.1\r\nHost: engine\r\nAuthorization: Bearer {TOKEN}\r\nConnection: close\r\n\r\n"
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
        "POST /v1/turns HTTP/1.1\r\nHost: engine\r\nAuthorization: Bearer {TOKEN}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\n{{}}\r\n0\r\n\r\n"
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

    // The turn is torn down: its id leaves the registry. Probed with a second
    // `/events` GET, which is *non-destructive* — a still-registered turn whose
    // session has been taken answers 409, a reclaimed one answers 404 — where
    // polling `cancel` would have removed the entry itself and passed the test
    // by causing the very thing it claims to observe.
    let events_path = format!("/v1/turns/{turn_id}/events");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, _) = get_json_authed(addr, &events_path, TOKEN).await;
        if status.contains("404") {
            break;
        }
        assert!(
            status.contains("409"),
            "the only other legitimate answer is 'already streaming': {status}"
        );
        assert!(
            Instant::now() < deadline,
            "a turn whose only subscriber disconnected is still registered — the \
             engine thread outlives the client that asked for it"
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
