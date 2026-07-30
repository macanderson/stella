//! The mock-host harness shared by the transport tests.
//!
//! A real socket, a real server, and the reverse-RPC POSTs a host answers
//! with — the protocol Oxagen's client speaks. Extracted from `http.rs` so
//! `resume.rs` drives the same harness rather than a second copy of it that
//! could drift; the extraction also puts `http.rs` back under the file-size
//! ratchet it had grown past.
//!
//! Everything here is `pub` because an integration test is its own crate:
//! `mod common;` compiles this file into each test binary separately, and
//! anything private would be unreachable from the tests that need it.

// Each test binary compiles this whole module, so any helper only *one* of them
// uses is genuinely dead code in the other — `http.rs` never resumes a stream,
// `resume.rs` never drives a tool call. That is inherent to `mod common;`, not a
// sign of an unused helper, so it is allowed here rather than worked around by
// splitting the harness along the same seam twice.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use stella_protocol::{CompletionMessage, ToolSchema};
use stella_serve::observe::{
    Capture, Fanout, Metrics, ServeEvent, SharedObserver, StreamEndReason,
};
use stella_serve::{ServeConfig, serve};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

pub const TOKEN: &str = "test-secret";

pub fn echo_tool() -> serde_json::Value {
    serde_json::to_value(ToolSchema {
        name: "echo".to_string(),
        description: "echo".to_string(),
        input_schema: json!({ "type": "object" }),
        read_only: false,
    })
    .unwrap()
}

/// Start the server on an ephemeral loopback port; returns its address.
pub async fn start_server() -> SocketAddr {
    start_observed_server().await.0
}

/// The resume window these tests run with.
///
/// Short on purpose. A disconnected turn is parked for this long before the
/// server gives up on a reconnect, so the production default (30s) would make
/// every disconnect assertion wait half a minute. Short is not *zero*: the
/// window still has to be real, or the resume tests would be asserting against
/// a feature that was switched off for the suite.
pub const TEST_RESUME_GRACE: Duration = Duration::from_millis(250);

/// Start the server with a [`Capture`] wired in place of the JSONL sink, so a
/// test can assert on the **typed** events the server emitted rather than
/// scraping stderr. That indirection is the point: an assertion on
/// `ServeEvent::ReverseTimedOut { .. }` survives any change to how records are
/// rendered, and a phrase-matching one does not.
pub async fn start_observed_server() -> (SocketAddr, Arc<Capture>) {
    start_observed_server_with(TEST_RESUME_GRACE).await
}

/// [`start_observed_server`] with an explicit resume window, for the tests that
/// are *about* that window.
pub async fn start_observed_server_with(resume_grace: Duration) -> (SocketAddr, Arc<Capture>) {
    let capture = Arc::new(Capture::new());
    let observer = capture.clone();
    let metrics = Arc::new(Metrics::new());
    let (tx, rx) = oneshot::channel();
    let fanout: SharedObserver = Arc::new(Fanout::new(vec![observer, metrics.clone()]));
    tokio::spawn(async move {
        let config = ServeConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: TOKEN.to_string(),
            observer: fanout,
            metrics,
            resume_grace,
        };
        let _ = serve(config, move |addr| {
            let _ = tx.send(addr);
        })
        .await;
    });
    let addr = rx.await.expect("server reported its bound address");
    (addr, capture)
}

/// POST a JSON body and read the whole response (server sends `Connection:
/// close`). Returns `(status_line, body)`.
pub async fn post_json(
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
pub async fn get_json(addr: SocketAddr, path: &str) -> (String, String) {
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
pub async fn get_json_authed(addr: SocketAddr, path: &str, token: &str) -> (String, String) {
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
pub async fn open_sse(addr: SocketAddr, path: &str, token: &str) -> BufReader<TcpStream> {
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
pub async fn next_event(reader: &mut BufReader<TcpStream>) -> Option<serde_json::Value> {
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

pub fn model_result(text: &str) -> serde_json::Value {
    json!({
        "text": text,
        "usage": { "input_tokens": 0, "output_tokens": 0 },
        "model": "mock",
        "cost_usd": 0.0,
    })
}

pub fn model_wants_echo() -> serde_json::Value {
    json!({
        "text": "",
        "tool_calls": [{ "call_id": "c1", "name": "echo", "input": { "text": "hi" } }],
        "usage": { "input_tokens": 0, "output_tokens": 0 },
        "model": "mock",
        "cost_usd": 0.0,
    })
}

// ── resumable streams (#971 phase 2) ──────────────────────────────────────

/// Block until the server has noticed the subscriber went away and parked the
/// turn, returning the reason it recorded.
///
/// Dropping a socket tells the *client* the stream is over; the server learns
/// it whenever its own read or write next touches the dead connection. Until it
/// does, the session is still checked out by the previous connection, and a
/// reconnect is answered from the retained tail alone — a replay with no live
/// source behind it, which is correct for a genuinely concurrent second
/// subscriber and useless to a resuming one. Waiting for the server's own
/// `StreamEnded` removes that race from every resume test: the emit is ordered
/// after the turn is parked (see `routes::handle_events`), so observing it is a
/// guarantee the session is back in its entry, not a hint that it soon will be.
///
/// The reason is asserted, not merely returned: a disconnect that ended the
/// turn instead of parking it is exactly the regression these tests exist to
/// catch, and it would otherwise surface as a puzzling timeout here.
pub async fn await_parked_after_disconnect(capture: &Capture) -> StreamEndReason {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(ServeEvent::StreamEnded { reason, .. }) =
            capture.find(|e| matches!(e, ServeEvent::StreamEnded { .. }))
        {
            assert!(
                reason.leaves_the_turn_resumable(),
                "a subscriber hanging up must leave the turn resumable, but the \
                 server ended it with {reason:?} — the turn is gone and no \
                 reconnect can recover it"
            );
            return reason;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the server never recorded the subscriber going away"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Read one SSE event as `(id, payload)`, so a test can assert on the `id:`
/// line a browser `EventSource` would use to resume.
pub async fn next_event_with_id(
    reader: &mut BufReader<TcpStream>,
) -> Option<(Option<u64>, serde_json::Value)> {
    let mut data: Option<String> = None;
    let mut id: Option<u64> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.ok()?;
        if n == 0 {
            return data.map(|d| (id, serde_json::from_str(&d).unwrap()));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if let Some(d) = &data {
                return Some((id, serde_json::from_str(d).unwrap()));
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("id: ") {
            id = rest.parse().ok();
        }
        if let Some(rest) = trimmed.strip_prefix("data: ") {
            data = Some(rest.to_string());
        }
    }
}

/// Open an SSE stream carrying a `Last-Event-ID` header, the way a browser
/// `EventSource` reconnects.
pub async fn open_sse_resuming(
    addr: SocketAddr,
    path: &str,
    token: &str,
    last_event_id: u64,
) -> BufReader<TcpStream> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: engine\r\nAuthorization: Bearer {token}\r\n\
         Last-Event-ID: {last_event_id}\r\nConnection: close\r\n\r\n"
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

pub async fn create_turn(addr: SocketAddr) -> String {
    let create = json!({
        "provider_id": "mock",
        "messages": [serde_json::to_value(CompletionMessage::user("hi")).unwrap()],
        // Bounded so a resume that fails to answer surfaces in seconds rather
        // than parking for the five-minute served default.
        "reverse_request_timeout_ms": 20_000,
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("200"), "create: {status} {body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    created["turn_id"].as_str().unwrap().to_string()
}
