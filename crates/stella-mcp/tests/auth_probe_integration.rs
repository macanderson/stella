// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Auth-probe suppression end-to-end (#2687), wiremock as the MCP server:
//! the synthetic `login_required` tool advertised for a suppressed server, a
//! stored login clearing suppression (and a successful connect retiring the
//! probe record), TTL expiry restoring the probe, and a fresh 401 re-arming
//! it. The core hit-count witness lives in `auth_probe_suppression.rs`,
//! which deliberately compiles against the pre-change crate surface.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use stella_core::ports::ToolExecutor as _;
use stella_mcp::oauth::OAuthTokens;
use stella_mcp::{
    AUTH_PROBE_TTL, AuthProbeCache, HealthState, McpServerConfig, McpToolSet, McpTransport,
    OAuthManager, login_required_tool_name,
};
use wiremock::matchers::{body_partial_json, header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SERVER: &str = "guarded";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stella-authprobe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// One "session startup" over the on-disk state in `store_dir`.
async fn startup(server_uri: &str, store_dir: &Path) -> (McpToolSet, Arc<OAuthManager>) {
    let manager = Arc::new(OAuthManager::new(store_dir.join("mcp_oauth.json")));
    let configs = vec![McpServerConfig {
        name: SERVER.into(),
        transport: McpTransport::Http {
            url: server_uri.into(),
            headers: BTreeMap::new(),
        },
        candidate_safe: false,
    }];
    let set =
        McpToolSet::connect_with_auth(&configs, Duration::from_secs(5), Some(manager.clone()))
            .await;
    (set, manager)
}

/// Mount a full, working handshake (initialize → initialized → tools/list).
/// When `require_bearer` is set, every route matches only with that token —
/// so an unauthenticated dial fails the mock rather than silently passing.
async fn mount_handshake(mcp: &MockServer, require_bearer: Option<&str>) {
    let with_auth = |mock: wiremock::MockBuilder| match require_bearer {
        Some(token) => mock.and(header("authorization", format!("Bearer {token}").as_str())),
        None => mock,
    };
    with_auth(
        Mock::given(method("POST")).and(body_partial_json(json!({ "method": "initialize" }))),
    )
    .respond_with(
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_json(json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "protocolVersion": "2025-06-18" }
            })),
    )
    .mount(mcp)
    .await;
    with_auth(Mock::given(method("POST")).and(body_partial_json(
        json!({ "method": "notifications/initialized" }),
    )))
    .respond_with(ResponseTemplate::new(202))
    .mount(mcp)
    .await;
    with_auth(
        Mock::given(method("POST")).and(body_partial_json(json!({ "method": "tools/list" }))),
    )
    .respond_with(
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_json(json!({
                "jsonrpc": "2.0", "id": 2,
                "result": { "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] }
            })),
    )
    .mount(mcp)
    .await;
}

/// Witness (b): the suppressed server advertises exactly one synthetic tool,
/// invoking it returns the login instruction, health reports `AuthRequired`,
/// and the server is NOT in `failed_servers` (it is actionable, not broken).
#[tokio::test]
async fn a_suppressed_server_advertises_the_login_tool_and_answers_the_instruction() {
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&mcp)
        .await;
    let dir = temp_dir("synthetic");

    // Session 1: the 401 arms suppression AND already surfaces the affordance.
    let (first, _m) = startup(&mcp.uri(), &dir).await;
    assert_eq!(first.connected_count(), 0);
    assert!(
        first.failed_servers().is_empty(),
        "an auth-required server is not rendered 'unavailable': {:?}",
        first.failed_servers()
    );
    assert_eq!(first.auth_required_servers().len(), 1);
    assert_eq!(first.auth_required_servers()[0].0, SERVER);

    // Session 2 (suppressed, zero requests — proven in the sibling witness):
    // the synthetic tool is the server's whole advertised surface.
    let (second, _m) = startup(&mcp.uri(), &dir).await;
    let tool = login_required_tool_name(SERVER);
    let schemas = second.schemas();
    let advertised: Vec<_> = schemas.iter().filter(|s| s.name == tool).collect();
    assert_eq!(advertised.len(), 1, "exactly one placeholder: {schemas:?}");
    assert!(
        advertised[0].read_only,
        "advertisable on read-only surfaces"
    );
    assert!(
        advertised[0]
            .description
            .contains(&format!("stella mcp login {SERVER}")),
        "{}",
        advertised[0].description
    );

    match second.execute(&tool, &serde_json::Value::Null).await {
        stella_protocol::ToolOutput::Ok { content } => {
            assert!(
                content.contains(&format!("stella mcp login {SERVER}")),
                "{content}"
            );
        }
        other => panic!("the instruction is a model-visible result, got {other:?}"),
    }

    let health = second.health().await;
    assert_eq!(health.len(), 1);
    assert_eq!(health[0].state, HealthState::AuthRequired);
    assert_eq!(health[0].name, SERVER);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Witness (c): a completed login (tokens present) clears suppression — the
/// next startup connects for real, and the successful connect retires the
/// probe record.
#[tokio::test]
async fn a_stored_login_clears_suppression_and_the_connect_retires_the_record() {
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&mcp)
        .await;
    let dir = temp_dir("login-clears");

    // Session 1: 401 → suppression armed.
    let (first, manager) = startup(&mcp.uri(), &dir).await;
    assert_eq!(first.auth_required_servers().len(), 1);
    assert!(manager.probe_cache().last_unauthorized(SERVER).is_some());

    // The user logs in (`stella mcp login`): usable tokens land in the store,
    // and the server now answers an authorized handshake.
    manager
        .store()
        .put(
            SERVER,
            &OAuthTokens {
                access_token: "granted".into(),
                refresh_token: None,
                expires_at: None, // no stated expiry — usable until a 401
                token_endpoint: "https://auth.invalid/token".into(),
                client_id: "c".into(),
                client_secret: None,
                scope: None,
                resource: mcp.uri(),
            },
        )
        .unwrap();
    mcp.reset().await;
    mount_handshake(&mcp, Some("granted")).await;

    // Session 2, still well inside the TTL: the login wins over the record.
    let (second, manager2) = startup(&mcp.uri(), &dir).await;
    assert_eq!(second.connected_count(), 1, "{:?}", second.failed_servers());
    assert!(second.auth_required_servers().is_empty());
    assert!(
        !mcp.received_requests().await.unwrap().is_empty(),
        "the probe is restored — the server was actually dialed"
    );
    assert_eq!(
        manager2.probe_cache().last_unauthorized(SERVER),
        None,
        "a successful connect retires the probe record"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Witness (d): TTL expiry restores the probe — a record older than
/// [`AUTH_PROBE_TTL`] no longer suppresses, and the (now willing) server
/// connects normally.
#[tokio::test]
async fn ttl_expiry_restores_the_probe() {
    let mcp = MockServer::start().await;
    mount_handshake(&mcp, None).await;
    let dir = temp_dir("ttl-expiry");

    // An aged 401 record, as if the suppression window has lapsed.
    let cache = AuthProbeCache::sibling_of(&dir.join("mcp_oauth.json"));
    cache
        .record_unauthorized(SERVER, unix_now() - AUTH_PROBE_TTL.as_secs() - 10)
        .unwrap();

    let (set, manager) = startup(&mcp.uri(), &dir).await;
    assert_eq!(set.connected_count(), 1, "{:?}", set.failed_servers());
    assert!(set.auth_required_servers().is_empty());
    assert!(
        !mcp.received_requests().await.unwrap().is_empty(),
        "the lapsed record must not keep the server un-dialed"
    );
    assert_eq!(
        manager.probe_cache().last_unauthorized(SERVER),
        None,
        "the successful probe retires the stale record"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh 401 after the TTL lapsed re-arms suppression with a new timestamp.
#[tokio::test]
async fn a_fresh_401_after_expiry_rearms_the_suppression() {
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&mcp)
        .await;
    let dir = temp_dir("rearm");

    let aged = unix_now() - AUTH_PROBE_TTL.as_secs() - 10;
    let cache = AuthProbeCache::sibling_of(&dir.join("mcp_oauth.json"));
    cache.record_unauthorized(SERVER, aged).unwrap();

    let (set, manager) = startup(&mcp.uri(), &dir).await;
    assert!(
        !mcp.received_requests().await.unwrap().is_empty(),
        "the lapsed record restores the probe"
    );
    assert_eq!(set.auth_required_servers().len(), 1, "…which 401s again");
    let rearmed = manager
        .probe_cache()
        .last_unauthorized(SERVER)
        .expect("the fresh 401 re-records");
    assert!(rearmed > aged, "re-armed from now, not the stale timestamp");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The token store alone can rule the connect out (#2687 spec item 1's
/// second half): a stored login that is expired with no refresh token is
/// known-doomed, so the server is skipped before connect — zero requests —
/// even with no 401 on record.
#[tokio::test]
async fn expired_refreshless_tokens_skip_the_connect_without_a_401_record() {
    let mcp = MockServer::start().await;
    mount_handshake(&mcp, None).await;
    let dir = temp_dir("dead-tokens");

    let manager = Arc::new(OAuthManager::new(dir.join("mcp_oauth.json")));
    manager
        .store()
        .put(
            SERVER,
            &OAuthTokens {
                access_token: "stale".into(),
                refresh_token: None,
                expires_at: Some(unix_now() - 60),
                token_endpoint: "https://auth.invalid/token".into(),
                client_id: "c".into(),
                client_secret: None,
                scope: None,
                resource: mcp.uri(),
            },
        )
        .unwrap();

    let (set, _m) = startup(&mcp.uri(), &dir).await;
    assert_eq!(set.connected_count(), 0);
    assert_eq!(set.auth_required_servers().len(), 1);
    assert!(
        set.auth_required_servers()[0].1.contains("expired"),
        "{}",
        set.auth_required_servers()[0].1
    );
    assert!(
        mcp.received_requests().await.unwrap().is_empty(),
        "the store already knows the connect is doomed — no round trip"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
