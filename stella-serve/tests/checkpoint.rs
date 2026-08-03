// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Durable served turns, end to end (#1198).
//!
//! `drive_turn` has always reached both checkpoint seams, but nothing set
//! `checkpoint_sink` and there was nowhere to read a resume point back from,
//! so a served turn died with the process while a CLI turn did not. These
//! tests pin the three claims that closes:
//!
//! 1. a served turn writes a resume point at each step boundary and clears it
//!    when it ends — the same lifecycle `stella-cli::durability` proves for a
//!    local turn;
//! 2. that resume point is readable through a route that consults the
//!    **store**, not the session registry, which is the only version of the
//!    feature a crashed server can benefit from;
//! 3. a host can reclaim one, because nothing else ever will.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{TOKEN, model_result, model_wants_echo, next_event, open_sse, post_json};
use serde_json::json;
use stella_serve::{CheckpointKey, CheckpointStore, MemoryCheckpointStore};

/// A server whose checkpoints land in a store the test can also read directly.
///
/// The store is `MemoryCheckpointStore` rather than the file-backed one on
/// purpose: what these tests are about is the *wiring* — that a served turn
/// reaches a store at all, and that the route reads the same one — and a
/// filesystem would add nothing but a temp directory. `FileCheckpointStore`'s
/// own durability (an atomic write that a reopened store still finds) is
/// proved in `checkpoint.rs`'s unit tests, where it can be asserted without a
/// socket.
async fn start_durable_server() -> (std::net::SocketAddr, Arc<MemoryCheckpointStore>) {
    let store = Arc::new(MemoryCheckpointStore::new());
    let erased: Arc<dyn CheckpointStore> = store.clone();
    let (addr, _capture, shutdown) = common::start_drainable_server_with_checkpoints(
        common::TEST_RESUME_GRACE,
        stella_serve::DEFAULT_SHUTDOWN_GRACE,
        Some(erased),
    )
    .await;
    // Same reasoning as `start_observed_server_with`: `pending_shutdown`
    // swallows the drop, so releasing the handle is not a drain signal.
    drop(shutdown);
    (addr, store)
}

async fn create_session(addr: std::net::SocketAddr) -> String {
    let (status, body) = post_json(
        addr,
        "/v1/sessions",
        Some(TOKEN),
        &json!({ "system_prompt": "You are stella." }).to_string(),
    )
    .await;
    assert!(status.contains("200"), "create session: {status} {body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn create_session_turn(addr: std::net::SocketAddr, session_id: &str) -> String {
    let create = json!({
        "provider_id": "mock",
        "tools": [common::echo_tool()],
        "input": [{ "role": "user", "content": "do the thing" }],
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
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["turn_id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn get_checkpoint(addr: std::net::SocketAddr, path: &str) -> (String, String) {
    common::get_json_authed(addr, path, TOKEN).await
}

async fn delete_checkpoint(addr: std::net::SocketAddr, path: &str) -> (String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "DELETE {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {TOKEN}\r\n\
         Connection: close\r\n\r\n"
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

async fn next_frame(sse: &mut tokio::io::BufReader<tokio::net::TcpStream>) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(10), next_event(sse))
        .await
        .expect("a frame must arrive within the test deadline")
        .expect("the stream must not end early")
}

/// The witness for the `turn.checkpoint` API row: a served turn's resume point
/// exists at the step boundary and is gone once the turn ends.
///
/// Two steps, not one, because a single-step turn never reaches
/// `StepOutcome::Continue` and so never checkpoints — the seam under test only
/// exists *between* steps. The model asks for a tool on step one; answering it
/// commits the step, and the step-two provider request is proof the persist
/// already ran (`drive_turn` persists before it loops).
#[tokio::test]
async fn a_served_turn_writes_a_resume_point_at_every_step_boundary() {
    let (addr, store) = start_durable_server().await;
    let session_id = create_session(addr).await;
    let turn_id = create_session_turn(addr, &session_id).await;
    let path = format!("/v1/sessions/{session_id}/checkpoint");

    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;

    // Step one: the model calls a tool.
    let mut answered_provider = 0;
    let second = loop {
        let frame = next_frame(&mut sse).await;
        match frame["type"].as_str() {
            Some("provider_request") if answered_provider == 0 => {
                let (status, body) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/provider-result"),
                    Some(TOKEN),
                    &json!({
                        "request_id": frame["request_id"],
                        "status": "ok",
                        "result": model_wants_echo(),
                    })
                    .to_string(),
                )
                .await;
                assert!(status.contains("200"), "provider-result: {status} {body}");
                answered_provider += 1;
            }
            Some("tool_request") => {
                let (status, body) = post_json(
                    addr,
                    &format!("/v1/turns/{turn_id}/tool-result"),
                    Some(TOKEN),
                    &json!({
                        "request_id": frame["request_id"],
                        "output": { "ok": { "content": "echoed" } },
                    })
                    .to_string(),
                )
                .await;
                assert!(status.contains("200"), "tool-result: {status} {body}");
            }
            // Step two has begun, so step one's boundary is behind us.
            Some("provider_request") => break frame,
            _ => {}
        }
    };

    // The claim, over the wire.
    let (status, body) = get_checkpoint(addr, &path).await;
    assert!(
        status.contains("200"),
        "a turn mid-flight must have a readable resume point: {status} {body}"
    );
    let checkpoint = stella_core::step::Checkpoint::from_json(&body)
        .expect("the stored bytes must decode as the engine's own Checkpoint");
    assert_eq!(
        checkpoint.step, 1,
        "the snapshot names the step that runs NEXT, so one committed step means step 1"
    );
    let roles: Vec<String> = checkpoint
        .messages
        .iter()
        .map(|m| format!("{:?}", m.role))
        .collect();
    assert!(
        roles.iter().any(|r| r == "Assistant") && roles.iter().any(|r| r == "User"),
        "the resume point carries the transcript as the engine left it, got {roles:?}"
    );

    // Finish the turn: the terminal path discards, so the point must be gone.
    let (status, body) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/provider-result"),
        Some(TOKEN),
        &json!({
            "request_id": second["request_id"],
            "status": "ok",
            "result": model_result("done"),
        })
        .to_string(),
    )
    .await;
    assert!(status.contains("200"), "provider-result: {status} {body}");
    loop {
        let frame = next_frame(&mut sse).await;
        if frame["type"] == "turn_complete" {
            assert_eq!(frame["outcome"]["status"], "completed");
            break;
        }
    }

    let (status, _) = get_checkpoint(addr, &path).await;
    assert!(
        status.contains("404"),
        "a turn that ended must leave no resume point: it would offer to \
         replay work the host already saw finish"
    );
    assert_eq!(
        store
            .get(&CheckpointKey::new(&session_id).unwrap())
            .unwrap(),
        None,
        "and the store agrees with the route"
    );
}

/// A resume point is looked up in the store, not the session registry — the
/// only version of this feature a crashed server can benefit from.
///
/// The id here names a session this process never created, which is exactly
/// the shape of "the server restarted": the store still has the bytes, the
/// registry has nothing. A `GET` gated on the registry would answer `404` at
/// precisely the moment the answer matters.
#[tokio::test]
async fn a_resume_point_outlives_the_session_it_belongs_to() {
    let (addr, store) = start_durable_server().await;
    let orphan = "session-00000000000000000000000000000000";
    store
        .put(&CheckpointKey::new(orphan).unwrap(), "{\"version\":1}")
        .unwrap();

    // The registry has never heard of it…
    let (status, _) = common::get_json_authed(addr, &format!("/v1/sessions/{orphan}"), TOKEN).await;
    assert!(status.contains("404"), "the session itself must be gone");

    // …and the resume point is still there.
    let (status, body) = get_checkpoint(addr, &format!("/v1/sessions/{orphan}/checkpoint")).await;
    assert!(status.contains("200"), "{status} {body}");
    assert_eq!(body, "{\"version\":1}", "the stored bytes, verbatim");
}

/// The bytes come back exactly as stored, with no parse-and-re-encode in the
/// middle.
///
/// This is not fussiness. `Checkpoint`'s field order *is* its wire order, and
/// this workspace's `serde_json` has no `preserve_order`, so a handler that
/// round-tripped the text through a `Value` would return it alphabetized —
/// still valid JSON, no longer the bytes `stella-core` pins.
#[tokio::test]
async fn a_checkpoint_is_returned_byte_for_byte() {
    let (addr, store) = start_durable_server().await;
    let id = "session-0123456789abcdef0123456789abcdef";
    // Deliberately NOT alphabetical: `version` then `step` is declaration
    // order, and an accidental re-encode would sort `step` first.
    let stored = "{\"version\":1,\"step\":4,\"zeta\":true,\"alpha\":false}";
    store.put(&CheckpointKey::new(id).unwrap(), stored).unwrap();

    let (status, body) = get_checkpoint(addr, &format!("/v1/sessions/{id}/checkpoint")).await;
    assert!(status.contains("200"), "{status} {body}");
    assert_eq!(body, stored, "the response must not re-encode the snapshot");
}

/// A host that has recovered can say so. Nothing else can: a turn that ended
/// discards its own point, and the ones left are precisely the turns that died
/// without ending — which no timer can distinguish from a host that is coming
/// back.
#[tokio::test]
async fn a_host_can_reclaim_a_resume_point_it_has_recovered() {
    let (addr, store) = start_durable_server().await;
    let id = "turn-0123456789abcdef0123456789abcdef";
    store
        .put(&CheckpointKey::new(id).unwrap(), "{\"version\":1}")
        .unwrap();
    let path = format!("/v1/turns/{id}/checkpoint");

    let (status, body) = delete_checkpoint(addr, &path).await;
    assert!(status.contains("200"), "{status} {body}");
    let (status, _) = get_checkpoint(addr, &path).await;
    assert!(status.contains("404"), "the point is gone after a reclaim");

    // Idempotent, like the `discard` half of the sink contract: a host
    // recovering from a crash cannot know which of its turns got far enough to
    // write one.
    let (status, _) = delete_checkpoint(addr, &path).await;
    assert!(status.contains("200"), "reclaiming nothing is not an error");
}

/// Ending a conversation takes its resume point with it — otherwise `DELETE`
/// would be the one operation that grows the store.
#[tokio::test]
async fn deleting_a_session_takes_its_resume_point_with_it() {
    let (addr, store) = start_durable_server().await;
    let session_id = create_session(addr).await;
    let key = CheckpointKey::new(&session_id).unwrap();
    store.put(&key, "{\"version\":1}").unwrap();

    let (status, body) = delete_checkpoint(addr, &format!("/v1/sessions/{session_id}")).await;
    assert!(status.contains("200"), "delete session: {status} {body}");
    assert_eq!(
        store.get(&key).unwrap(),
        None,
        "a destroyed conversation must not leave its resume point behind"
    );
}

/// With no store configured — the default — the routes answer honestly rather
/// than pretending durability is on.
#[tokio::test]
async fn a_server_without_a_store_answers_404_rather_than_pretending() {
    let addr = common::start_server().await;
    let (status, _) = get_checkpoint(
        addr,
        "/v1/sessions/session-0123456789abcdef0123456789abcdef/checkpoint",
    )
    .await;
    assert!(status.contains("404"), "no store means no checkpoint");
}

/// An id that is not a legal key never reaches the store — the property the
/// `CheckpointKey` newtype exists to make structural rather than remembered.
#[tokio::test]
async fn an_id_that_is_not_a_key_is_refused_at_the_route() {
    let (addr, _store) = start_durable_server().await;
    for hostile in [".hidden", "has%20space", "dots..dots"] {
        let (status, _) = get_checkpoint(addr, &format!("/v1/sessions/{hostile}/checkpoint")).await;
        assert!(
            status.contains("404"),
            "{hostile} must not be looked up: {status}"
        );
    }
}
