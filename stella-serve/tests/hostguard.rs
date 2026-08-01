// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The DNS-rebinding `Host` guard over the wire (#1130).
//!
//! `hostguard.rs`'s unit tests pin the policy decision in isolation; these pin
//! that the decision is actually *reached* — before route dispatch, on every
//! route including the unauthenticated `/healthz`, and ahead of the bearer
//! check so a rebound request cannot even provoke a 401 that tells it a token
//! is what it is missing.

mod common;

use common::{
    TOKEN, get_json_authed, get_with_host, get_without_host, start_observed_server, start_server,
};
use stella_serve::observe::ServeEvent;

/// A request announcing an attacker-controlled name is refused, and the
/// refusal is the *only* thing it learns — it never reaches a handler.
#[tokio::test]
async fn a_rebound_host_is_refused_on_an_authenticated_route() {
    let addr = start_server().await;
    let (status, body) = get_with_host(addr, "/v1/metrics", "evil.example", Some(TOKEN)).await;
    assert!(status.starts_with("HTTP/1.1 403"), "{status}");
    assert!(body.contains("Host header"), "{body}");
}

/// The guard runs ahead of the bearer check. A rebound request with a *valid*
/// token is still refused, and one with no token gets the 403 rather than the
/// 401 — the order matters because it is the difference between "you are not
/// allowed to ask" and "you are allowed to ask, just not authenticated".
#[tokio::test]
async fn the_host_guard_runs_before_authentication() {
    let addr = start_server().await;
    let (with_token, _) = get_with_host(addr, "/v1/metrics", "evil.example", Some(TOKEN)).await;
    let (without_token, _) = get_with_host(addr, "/v1/metrics", "evil.example", None).await;
    assert!(with_token.starts_with("HTTP/1.1 403"), "{with_token}");
    assert!(without_token.starts_with("HTTP/1.1 403"), "{without_token}");
}

/// `/healthz` is the one route bearer auth does not cover, so it is the one
/// route where the `Host` guard is the only control — exempting it would carve
/// the hole in exactly the wrong place.
#[tokio::test]
async fn the_unauthenticated_health_route_is_guarded_too() {
    let addr = start_server().await;
    let (rebound, _) = get_with_host(addr, "/healthz", "evil.example", None).await;
    assert!(rebound.starts_with("HTTP/1.1 403"), "{rebound}");

    let (legitimate, _) = get_with_host(addr, "/healthz", "localhost", None).await;
    assert!(legitimate.starts_with("HTTP/1.1 200"), "{legitimate}");
}

/// The loopback identities a real local client actually sends — by name, by
/// address, and with the port it dialed — all pass.
#[tokio::test]
async fn every_loopback_identity_a_real_client_sends_is_permitted() {
    let addr = start_server().await;
    for host in [
        "localhost".to_string(),
        format!("localhost:{}", addr.port()),
        "127.0.0.1".to_string(),
        addr.to_string(),
        "[::1]".to_string(),
    ] {
        let (status, _) = get_with_host(addr, "/healthz", &host, None).await;
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "Host: {host} must be permitted, got {status}"
        );
    }
}

/// A missing `Host` is permitted: a browser `fetch` always sends one, so its
/// absence means the request is not the attack this guards against. Same
/// posture as `stella-observatory::host_is_local`.
#[tokio::test]
async fn a_request_with_no_host_header_is_permitted() {
    let addr = start_server().await;
    let status = get_without_host(addr, "/healthz").await;
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
}

/// The refusal is reported as its own typed event, so a rebinding attempt and
/// an operator's missing allow-list entry are one grep rather than a scan of
/// every 403 — and the offending name is recorded, because that is what tells
/// the two apart.
#[tokio::test]
async fn a_refusal_is_reported_with_the_offending_host() {
    let (addr, capture) = start_observed_server().await;
    let _ = get_with_host(addr, "/v1/metrics", "evil.example", Some(TOKEN)).await;

    let found = capture
        .find(|e| matches!(e, ServeEvent::HostRejected { .. }))
        .expect("a refused Host must be reported");
    let ServeEvent::HostRejected { host, .. } = found else {
        unreachable!("matched above")
    };
    assert_eq!(host, "evil.example");
}

/// A legitimate request emits no refusal — the guard is not firing on the
/// traffic it is supposed to pass, which is the half of a security control
/// that silently breaks a deployment when it is wrong.
#[tokio::test]
async fn a_permitted_request_reports_no_refusal() {
    let (addr, capture) = start_observed_server().await;
    let (status, _) = get_json_authed(addr, "/v1/metrics", TOKEN).await;
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
    assert_eq!(
        capture.count(|e| matches!(e, ServeEvent::HostRejected { .. })),
        0,
    );
}

/// The startup record names which arm the guard compiled to, so an operator
/// can tell an enforcing deployment from an inert one without reading the
/// source.
#[tokio::test]
async fn the_listening_record_names_the_guard_mode() {
    let (_addr, capture) = start_observed_server().await;
    let found = capture
        .find(|e| matches!(e, ServeEvent::Listening { .. }))
        .expect("the server reports that it is listening");
    let ServeEvent::Listening { host_guard, .. } = found else {
        unreachable!("matched above")
    };
    assert_eq!(host_guard, stella_serve::HostMode::Loopback);
}
