// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Graceful shutdown and readiness (#1131).
//!
//! The behaviour under test is what a container orchestrator relies on during
//! a redeploy: `/readyz` flips to not-ready so traffic stops arriving, new
//! turns are refused, in-flight turns reach a step boundary and emit their
//! terminal frame, and only then does the process leave.
//!
//! A `oneshot` stands in for `SIGTERM` — see `common::start_drainable_server`
//! for why a real signal handler has no place in a test binary. The path it
//! drives is identical: `serve` is `serve_until` with the process's signals
//! supplied.

mod common;

use std::time::Duration;

use common::{
    TOKEN, create_turn, get_json, model_result, next_event, open_sse, post_json,
    start_drainable_server, start_server,
};
use serde_json::json;
use stella_serve::observe::ServeEvent;

const TEST_RESUME_GRACE: Duration = Duration::from_millis(250);

/// Read frames until the next `provider_request`, returning its request id.
/// A turn parked here is exactly the "in flight" state a drain has to handle.
async fn next_provider_request(reader: &mut tokio::io::BufReader<tokio::net::TcpStream>) -> String {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), next_event(reader))
            .await
            .expect("a provider_request must arrive")
            .expect("the stream must not end before it");
        if frame["type"] == "provider_request" {
            return frame["request_id"].as_str().unwrap().to_string();
        }
    }
}

/// Poll `/readyz` until it reports `expected`, or fail loudly. The flip is
/// driven from another task, so a bare read races it.
async fn await_readyz(addr: std::net::SocketAddr, expected: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = get_json(addr, "/readyz").await;
        if status.contains(expected) {
            return body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "/readyz never reported {expected}; last was {status} {body}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A healthy server is ready, and says so in a shape a human reading the probe
/// by hand can act on.
#[tokio::test]
async fn readyz_reports_ready_while_serving() {
    let addr = start_server().await;
    let (status, body) = get_json(addr, "/readyz").await;
    assert!(status.contains("200"), "{status}");
    assert!(body.contains("\"state\":\"ready\""), "{body}");
}

/// `/readyz` is unauthenticated for the same reason `/healthz` is: a kubelet
/// has no bearer token.
#[tokio::test]
async fn readyz_needs_no_bearer_token() {
    let addr = start_server().await;
    let (status, _) = get_json(addr, "/readyz").await;
    assert!(status.contains("200"), "{status}");
}

/// The two endpoints answer different questions, and a drain is exactly where
/// that difference shows: the process is alive (`/healthz` 200) and must not
/// be sent new work (`/readyz` 503). A single "healthy" boolean cannot say
/// both, which is the whole reason `/readyz` exists.
#[tokio::test]
async fn a_drain_makes_readyz_unready_while_healthz_stays_ok() {
    let (addr, _capture, shutdown) =
        start_drainable_server(TEST_RESUME_GRACE, Duration::from_secs(5)).await;
    // A live turn, so the drain has something to wait on and cannot race past
    // the assertions below.
    let turn_id = create_turn(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let _request_id = next_provider_request(&mut sse).await;

    shutdown.send(()).expect("the server is listening");

    let body = await_readyz(addr, "503").await;
    assert!(body.contains("\"state\":\"draining\""), "{body}");

    let (health, _) = get_json(addr, "/healthz").await;
    assert!(
        health.contains("200"),
        "a draining process is alive, not sick: {health}"
    );
}

/// New work is refused during a drain — that is what "stops accepting" means.
/// The `503` names the reason and carries `Retry-After`, so a client retries
/// against whichever instance the balancer has since chosen.
#[tokio::test]
async fn a_drain_refuses_new_turns_and_sessions() {
    let (addr, _capture, shutdown) =
        start_drainable_server(TEST_RESUME_GRACE, Duration::from_secs(5)).await;
    let turn_id = create_turn(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let _request_id = next_provider_request(&mut sse).await;

    shutdown.send(()).expect("the server is listening");
    await_readyz(addr, "503").await;

    let create = json!({
        "provider_id": "mock",
        "messages": [],
    })
    .to_string();
    let (status, body) = post_json(addr, "/v1/turns", Some(TOKEN), &create).await;
    assert!(status.contains("503"), "{status} {body}");
    assert!(body.contains("shutting down"), "{body}");

    let session = json!({ "system_prompt": "you are a test" }).to_string();
    let (status, _) = post_json(addr, "/v1/sessions", Some(TOKEN), &session).await;
    assert!(status.contains("503"), "{status}");
}

/// The load-bearing half of the drain: an in-flight turn must still be
/// *answerable*. Refusing `provider-result` during a drain would park every
/// turn on a reverse request nobody could answer, and the drain would then
/// time out cancelling turns it had itself wedged.
#[tokio::test]
async fn a_drain_still_accepts_answers_for_in_flight_turns() {
    let (addr, _capture, shutdown) =
        start_drainable_server(TEST_RESUME_GRACE, Duration::from_secs(5)).await;
    let turn_id = create_turn(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let request_id = next_provider_request(&mut sse).await;

    shutdown.send(()).expect("the server is listening");
    await_readyz(addr, "503").await;

    let body = json!({
        "request_id": request_id,
        "status": "ok",
        "result": model_result("done"),
    })
    .to_string();
    let (status, body) = post_json(
        addr,
        &format!("/v1/turns/{turn_id}/provider-result"),
        Some(TOKEN),
        &body,
    )
    .await;
    assert!(
        status.contains("200"),
        "an in-flight turn must stay answerable during a drain: {status} {body}"
    );
}

/// The #1131 acceptance: a signalled server drains its in-flight turn to a
/// step boundary and the subscriber still receives `turn_complete`, rather
/// than having the turn's future dropped and the stream simply stop.
#[tokio::test]
async fn a_drained_turn_still_emits_its_terminal_frame() {
    let (addr, capture, shutdown) =
        start_drainable_server(TEST_RESUME_GRACE, Duration::from_secs(10)).await;
    let turn_id = create_turn(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    let request_id = next_provider_request(&mut sse).await;

    shutdown.send(()).expect("the server is listening");
    await_readyz(addr, "503").await;

    // Answer the parked model call. The soft stop is observed at the *next*
    // step boundary, so this result lands and is kept — the difference
    // between draining and cancelling.
    let body = json!({
        "request_id": request_id,
        "status": "ok",
        "result": model_result("done"),
    })
    .to_string();
    post_json(
        addr,
        &format!("/v1/turns/{turn_id}/provider-result"),
        Some(TOKEN),
        &body,
    )
    .await;

    let mut saw_terminal = false;
    while let Some(frame) = tokio::time::timeout(Duration::from_secs(10), next_event(&mut sse))
        .await
        .expect("the stream must terminate inside the grace period")
    {
        if frame["type"] == "turn_complete" {
            saw_terminal = true;
            break;
        }
    }
    assert!(
        saw_terminal,
        "a drained turn must reach turn_complete, not have its future dropped"
    );

    // And the drain reports what it did, so a redeploy is auditable rather
    // than merely quiet.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(ServeEvent::DrainCompleted {
            drained,
            cancelled,
            timed_out,
        }) = capture.find(|e| matches!(e, ServeEvent::DrainCompleted { .. }))
        {
            assert_eq!(drained, 1, "the turn reached a step boundary");
            assert_eq!(cancelled, 0, "nothing had to be forced");
            assert!(!timed_out, "the grace period was not exhausted");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the drain must report its outcome"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A turn parked on a host that never answers has no step boundary to reach,
/// so the graceful ask alone cannot bound it. The grace period is what does —
/// and the forced half is reported separately, because a persistently
/// non-zero `cancelled` across deploys means the window is too short for this
/// workload rather than that something is broken.
#[tokio::test]
async fn a_turn_that_never_reaches_a_boundary_is_cancelled_when_the_grace_expires() {
    let (addr, capture, shutdown) =
        start_drainable_server(TEST_RESUME_GRACE, Duration::from_millis(300)).await;
    let turn_id = create_turn(addr).await;
    let mut sse = open_sse(addr, &format!("/v1/turns/{turn_id}/events"), TOKEN).await;
    // Parked on a provider request that is deliberately never answered.
    let _request_id = next_provider_request(&mut sse).await;

    shutdown.send(()).expect("the server is listening");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(ServeEvent::DrainCompleted {
            cancelled,
            timed_out,
            ..
        }) = capture.find(|e| matches!(e, ServeEvent::DrainCompleted { .. }))
        {
            assert_eq!(cancelled, 1, "the parked turn had to be forced");
            assert!(timed_out, "the grace period was exhausted");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the drain must end even when nothing reaches a boundary"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Even the forced half emits a terminal frame: cancel unwinds the turn, it
    // does not drop its future.
    let mut saw_terminal = false;
    while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), next_event(&mut sse))
        .await
        .expect("the stream must end")
    {
        if frame["type"] == "turn_complete" {
            saw_terminal = true;
            break;
        }
    }
    assert!(saw_terminal, "a cancelled turn still reports its outcome");
}

/// A drain with nothing in flight ends immediately — it must not sit out the
/// whole grace period waiting for turns that do not exist. A redeploy of an
/// idle instance should be instant.
#[tokio::test]
async fn an_idle_server_drains_immediately() {
    let (addr, capture, shutdown) =
        start_drainable_server(TEST_RESUME_GRACE, Duration::from_secs(60)).await;
    shutdown.send(()).expect("the server is listening");

    let started = tokio::time::Instant::now();
    let deadline = started + Duration::from_secs(5);
    loop {
        if let Some(ServeEvent::DrainCompleted {
            drained,
            cancelled,
            timed_out,
        }) = capture.find(|e| matches!(e, ServeEvent::DrainCompleted { .. }))
        {
            assert_eq!((drained, cancelled), (0, 0));
            assert!(!timed_out);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "an idle drain must not wait out the 60s grace period"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = addr;
}
