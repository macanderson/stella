// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! End-to-end proof that the live plane is actually live.
//!
//! These drive a real listener over a real socket against a real store,
//! because the property under test is not "the query returns rows" — the unit
//! tests cover that — but "a tool call made *now* reaches a connected client
//! *now*, without the turn having ended". Nothing short of the whole path
//! demonstrates that: the projection, the cursor, the change detection, the
//! SSE framing and the socket all have to work together for the number on the
//! dashboard to move.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use stella_protocol::{AgentEvent, ToolCall, ToolOutput};
use stella_store::Store;

/// Start the observatory on an ephemeral port, returning its address.
fn serve(root: std::path::PathBuf) -> std::net::SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            stella_observatory::serve(root, 0, |addr| {
                let _ = tx.send(addr);
            })
            .await
        })
        .expect("serve");
    });
    rx.recv_timeout(Duration::from_secs(10)).expect("bound")
}

fn start(call_id: &str, name: &str) -> AgentEvent {
    AgentEvent::ToolStart {
        call: ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            input: serde_json::json!({ "path": "a.rs" }),
        },
    }
}

/// Read SSE frames until `want` returns `Some`, or the deadline passes.
///
/// Frames are accumulated line by line and dispatched on the blank line, the
/// way any SSE client does — parsing this the same way the browser will is
/// part of what the test is for.
fn read_until<T>(
    reader: &mut BufReader<TcpStream>,
    within: Duration,
    mut want: impl FnMut(&serde_json::Value) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + within;
    let mut pending: Option<String> = None;
    while Instant::now() < deadline {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if let Some(data) = line.strip_prefix("data: ") {
            pending = Some(data.trim_end().to_string());
            continue;
        }
        // A blank line dispatches the frame it terminates.
        if line.trim().is_empty()
            && let Some(data) = pending.take()
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&data)
            && let Some(found) = want(&value)
        {
            return Some(found);
        }
    }
    None
}

fn open_stream(addr: std::net::SocketAddr) -> BufReader<TcpStream> {
    let mut socket = TcpStream::connect(addr).expect("connect");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    socket
        .write_all(
            format!(
                "GET /api/v1/live HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
                addr.port()
            )
            .as_bytes(),
        )
        .expect("request");
    let mut reader = BufReader::new(socket);
    // Drain the response head; the body is the frame sequence.
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("head");
        if line.trim().is_empty() {
            break;
        }
        if line.starts_with("HTTP/") {
            assert!(line.contains("200 OK"), "stream refused: {line}");
        }
    }
    reader
}

/// **The headline property.** A tool call made while a client is connected
/// reaches it, mid-turn, without the execution ever finishing.
#[test]
fn a_tool_call_made_now_reaches_a_connected_client_now() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "long running turn", "anthropic", "opus")
        .expect("begin");
    let addr = serve(dir.path().to_path_buf());
    let mut reader = open_stream(addr);

    // The opening frame lands before anything happens, so a fresh tab renders
    // immediately rather than waiting for the first change.
    let opening = read_until(&mut reader, Duration::from_secs(5), |v| {
        v.get("cursor").map(|_| v.clone())
    })
    .expect("an opening frame");
    assert_eq!(
        opening["live"]["executions"][0]["tool_calls"], 0,
        "nothing has happened yet: {opening}"
    );

    // Now make two calls, mid-turn. No finish_execution anywhere.
    store
        .record_event(id, 0, &start("c1", "read_file"))
        .expect("start");
    store
        .record_event(
            id,
            1,
            &AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "x".into(),
                },
                duration_ms: 4,
                speculated: false,
            },
        )
        .expect("result");
    store
        .record_event(id, 2, &start("c2", "bash"))
        .expect("second");

    let seen = read_until(&mut reader, Duration::from_secs(5), |v| {
        (v["live"]["executions"][0]["tool_calls"] == 2).then(|| v.clone())
    })
    .expect("the stream reported both calls without the turn ending");

    assert_eq!(
        seen["changed"], true,
        "it arrived as a change, not a heartbeat"
    );
    assert_eq!(
        seen["live"]["executions"][0]["tool_calls_running"], 1,
        "the outstanding call is visibly still running: {seen}"
    );
    let running = &seen["live"]["running_calls"];
    assert_eq!(running[0]["name"], "bash", "and it is named: {running}");
}

/// The cursor is a change *detector*, not a clock: an idle workspace must not
/// produce a storm of frames claiming something happened.
#[test]
fn an_idle_workspace_reports_no_changes() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    store
        .begin_execution("run", "idle", "anthropic", "opus")
        .expect("begin");
    let addr = serve(dir.path().to_path_buf());
    let mut reader = open_stream(addr);

    let mut changes = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let Some(was_change) = read_until(&mut reader, Duration::from_millis(600), |v| {
            v.get("changed").and_then(serde_json::Value::as_bool)
        }) else {
            break;
        };
        if was_change {
            changes += 1;
        }
    }
    assert!(
        changes <= 1,
        "an idle workspace sent {changes} change frames; only the opening one is legitimate"
    );
}

/// A completed tool call must move the cursor. It is an in-place UPDATE that
/// changes no maximum, so a cursor built only from `max(rowid)` would miss
/// every completion and leave calls rendered as permanently running.
#[test]
fn completing_a_call_moves_the_cursor() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "completion", "anthropic", "opus")
        .expect("begin");
    store
        .record_event(id, 0, &start("c1", "bash"))
        .expect("start");

    let before: serde_json::Value =
        serde_json::from_slice(&stella_observatory::respond(dir.path(), "/api/v1/cursor").body)
            .expect("json");
    assert_eq!(before["tool_calls_running"], 1);

    store
        .record_event(
            id,
            1,
            &AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "done".into(),
                },
                duration_ms: 2,
                speculated: false,
            },
        )
        .expect("result");

    let after: serde_json::Value =
        serde_json::from_slice(&stella_observatory::respond(dir.path(), "/api/v1/cursor").body)
            .expect("json");
    assert_ne!(before, after, "the completion must be detectable");
    assert_eq!(after["tool_calls_running"], 0);
}

/// The stream is gated by the same DNS-rebinding defense as every one-shot
/// route. A long-lived subscription to the workspace's telemetry is a
/// strictly larger prize than a single read of it.
#[test]
fn a_rebound_host_cannot_open_a_stream() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    Store::open(dir.path()).expect("store");
    let addr = serve(dir.path().to_path_buf());

    let mut socket = TcpStream::connect(addr).expect("connect");
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    socket
        .write_all(b"GET /api/v1/live HTTP/1.1\r\nHost: attacker.example\r\n\r\n")
        .expect("request");
    let mut response = String::new();
    socket.read_to_string(&mut response).expect("read");

    assert!(
        response.contains("403 Forbidden"),
        "a rebound Host must be refused: {response}"
    );
    assert!(
        !response.contains("text/event-stream"),
        "and must not receive a stream: {response}"
    );
}
