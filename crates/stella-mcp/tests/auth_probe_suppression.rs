// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The witness for auth-probe suppression (#2687), kept in its own file so it
//! compiles against the PRE-change crate surface: it uses only
//! `McpToolSet::connect_with_auth`, `OAuthManager`, and wiremock hit counts.
//! On the base commit the second "session" re-dials the 401 server and the
//! final hit-count assertion fails; with the suppression cache it opens no
//! connection at all. (The richer assertions — the synthetic tool, health,
//! login/TTL re-arm — need the new API and live in
//! `auth_probe_integration.rs`.)

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use stella_mcp::{McpServerConfig, McpToolSet, McpTransport, OAuthManager};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// One "session startup": a fresh `OAuthManager` over the same on-disk state
/// (exactly what a new CLI process constructs) driving one connect.
async fn startup(server_uri: &str, store_dir: &std::path::Path) -> McpToolSet {
    let manager = Arc::new(OAuthManager::new(store_dir.join("mcp_oauth.json")));
    let configs = vec![McpServerConfig {
        name: "guarded".into(),
        transport: McpTransport::Http {
            url: server_uri.into(),
            headers: BTreeMap::new(),
        },
        candidate_safe: false,
    }];
    McpToolSet::connect_with_auth(&configs, Duration::from_secs(5), Some(manager)).await
}

/// After a 401, the next startup within the TTL opens NO connection to the
/// server: its wiremock receives zero new requests.
#[tokio::test]
async fn a_401_server_is_not_probed_again_within_the_ttl() {
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&mcp)
        .await;

    let dir = std::env::temp_dir().join(format!("stella-authprobe-witness-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // Session 1 pays the probe and gets the 401.
    let first = startup(&mcp.uri(), &dir).await;
    assert_eq!(
        first.connected_count(),
        0,
        "the 401 server must not connect"
    );
    let probed = mcp.received_requests().await.unwrap().len();
    assert!(probed >= 1, "the first session probes the server");

    // Session 2, well within the 15-minute TTL.
    let second = startup(&mcp.uri(), &dir).await;
    assert_eq!(second.connected_count(), 0);
    let after = mcp.received_requests().await.unwrap().len();
    assert_eq!(
        after,
        probed,
        "a suppressed server's wiremock must receive ZERO requests on the \
         second startup within the TTL (got {} new)",
        after - probed
    );

    let _ = std::fs::remove_dir_all(&dir);
}
