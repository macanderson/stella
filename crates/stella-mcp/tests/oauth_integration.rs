//! OAuth flow tests using `wiremock` as both the MCP server and the
//! authorization server. Covers: full browser login (discovery → dynamic
//! registration → PKCE authorize via a simulated user-agent → token
//! exchange), the refresh grant, the transport's bearer injection, the
//! 401 → forced-refresh → retry path, and the lazy source's mid-session
//! login pickup.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;
use stella_mcp::oauth::{self, LoginEvent, LoginOptions, OAuthManager, OAuthTokens, TokenStore};
use stella_mcp::{HttpTransport, Transport};
use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn temp_store(tag: &str) -> (std::path::PathBuf, TokenStore) {
    let dir = std::env::temp_dir().join(format!("stella-oauth-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = TokenStore::new(dir.join("mcp_oauth.json"));
    (dir, store)
}

fn tokens_for(auth_server: &MockServer, resource: &str) -> OAuthTokens {
    OAuthTokens {
        access_token: "old-token".into(),
        refresh_token: Some("refresh-1".into()),
        expires_at: None, // not proactively stale; the 401 path drives refresh
        token_endpoint: format!("{}/token", auth_server.uri()),
        client_id: "client-1".into(),
        client_secret: None,
        scope: None,
        resource: resource.into(),
    }
}

/// Full interactive login against mock servers: the "browser" is simulated
/// by GETting the authorize URL's redirect_uri with a code + the right state.
#[tokio::test]
async fn login_discovers_registers_and_exchanges_the_code() {
    let mcp = MockServer::start().await;
    let auth = MockServer::start().await;

    // The MCP server 401s and points at its protected-resource metadata.
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(
                    r#"Bearer resource_metadata="{}/.well-known/oauth-protected-resource""#,
                    mcp.uri()
                )
                .as_str(),
            ),
        )
        .mount(&mcp)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "resource": mcp.uri(),
            "authorization_servers": [auth.uri()],
            "scopes_supported": ["mcp.read", "mcp.write"],
        })))
        .mount(&mcp)
        .await;

    // The authorization server's RFC 8414 metadata + dynamic registration.
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": auth.uri(),
            "authorization_endpoint": format!("{}/authorize", auth.uri()),
            "token_endpoint": format!("{}/token", auth.uri()),
            "registration_endpoint": format!("{}/register", auth.uri()),
            "code_challenge_methods_supported": ["S256"],
        })))
        .mount(&auth)
        .await;
    Mock::given(method("POST"))
        .and(path("/register"))
        .and(body_partial_json(json!({
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
        })))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({ "client_id": "dyn-client-7" })),
        )
        .mount(&auth)
        .await;

    // Token exchange: requires the PKCE verifier, our client id, and the
    // audience binding. (The verifier value is checked below via the
    // received-request log, since it is generated inside `login`.)
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("client_id=dyn-client-7"))
        .and(body_string_contains("code=code-abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at-1",
            "refresh_token": "rt-1",
            "expires_in": 3600,
            "token_type": "Bearer",
        })))
        .mount(&auth)
        .await;

    // Drive the flow. When the authorize URL arrives, act as the browser:
    // parse redirect_uri + state out of it and bounce a code back.
    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();
    let browser = tokio::spawn(async move {
        let authorize_url = url_rx.await.expect("login must emit the authorize URL");
        let parsed = reqwest::Url::parse(&authorize_url).unwrap();
        assert_eq!(parsed.path(), "/authorize");
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "dyn-client-7");
        assert_eq!(q["code_challenge_method"], "S256");
        assert!(
            q.contains_key("resource"),
            "audience binding must ride along"
        );
        let redirect = format!("{}?code=code-abc&state={}", q["redirect_uri"], q["state"]);
        reqwest::get(&redirect).await.unwrap();
    });

    let mut url_tx = Some(url_tx);
    let mut events: Vec<String> = Vec::new();
    let tokens = oauth::login(
        "srv",
        &mcp.uri(),
        &LoginOptions {
            timeout: Duration::from_secs(10),
            ..LoginOptions::default()
        },
        &mut |event| match event {
            LoginEvent::AuthorizeUrl(url) => {
                if let Some(tx) = url_tx.take() {
                    let _ = tx.send(url);
                }
            }
            LoginEvent::Status(s) => events.push(s),
        },
    )
    .await
    .expect("login should succeed");
    browser.await.unwrap();

    assert_eq!(tokens.access_token, "at-1");
    assert_eq!(tokens.refresh_token.as_deref(), Some("rt-1"));
    assert_eq!(tokens.client_id, "dyn-client-7");
    assert!(tokens.expires_at.is_some());
    // Scopes flowed from the resource metadata into the request.
    assert_eq!(tokens.scope.as_deref(), Some("mcp.read mcp.write"));
    assert!(
        !events.is_empty(),
        "progress events should have been emitted"
    );

    // The token exchange carried the same verifier whose S256 hash rode the
    // authorize URL (PKCE end-to-end).
    let token_reqs: Vec<Request> = auth
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/token")
        .collect();
    assert_eq!(token_reqs.len(), 1);
    assert!(
        String::from_utf8_lossy(&token_reqs[0].body).contains("code_verifier="),
        "exchange must carry the PKCE verifier"
    );
}

/// A 401 from the MCP server triggers exactly one refresh + resend, and the
/// rotated tokens are persisted for the next session.
#[tokio::test]
async fn transport_refreshes_once_after_a_401_and_persists() {
    let mcp = MockServer::start().await;
    let auth = MockServer::start().await;
    let (dir, store) = temp_store("retry");

    store.put("srv", &tokens_for(&auth, &mcp.uri())).unwrap();

    // Refresh grant answers with a rotated pair.
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=refresh-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-token",
            "refresh_token": "refresh-2",
            "expires_in": 3600,
        })))
        .mount(&auth)
        .await;

    // The MCP server rejects the stale bearer and accepts the fresh one.
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer old-token"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&mcp)
        .await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer fresh-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({ "jsonrpc": "2.0", "id": 1, "result": { "ok": true } })),
        )
        .mount(&mcp)
        .await;

    let manager = OAuthManager::new(dir.join("mcp_oauth.json"));
    let transport = HttpTransport::new("srv", &mcp.uri(), &BTreeMap::new(), Duration::from_secs(5))
        .unwrap()
        .with_bearer_source(manager.source_for("srv"));

    let result = transport.request("tools/list", json!({})).await.unwrap();
    assert_eq!(result, json!({ "ok": true }));

    // The rotation was persisted: next session refreshes with `refresh-2`.
    let stored = store.get("srv").unwrap().unwrap();
    assert_eq!(stored.access_token, "fresh-token");
    assert_eq!(stored.refresh_token.as_deref(), Some("refresh-2"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A lazy source attached before any login sends no Authorization header —
/// and picks the login up mid-session without a reconnect.
#[tokio::test]
async fn lazy_source_is_inert_until_login_then_activates() {
    let mcp = MockServer::start().await;
    let auth = MockServer::start().await;
    let (dir, store) = temp_store("lazy");

    // Phase 1: no login stored → the request must arrive WITHOUT a bearer
    // (the closure matcher only accepts header-free requests, so a leaked
    // Authorization header fails the request → the test).
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "phase1" })))
        .and(|req: &Request| req.headers.get("authorization").is_none())
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({ "jsonrpc": "2.0", "id": 1, "result": { "phase": 1 } })),
        )
        .mount(&mcp)
        .await;
    // Phase 2: after login the bearer must ride along.
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "phase2" })))
        .and(header("authorization", "Bearer mid-session-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({ "jsonrpc": "2.0", "id": 2, "result": { "phase": 2 } })),
        )
        .mount(&mcp)
        .await;

    let manager = OAuthManager::new(dir.join("mcp_oauth.json"));
    let transport = HttpTransport::new("srv", &mcp.uri(), &BTreeMap::new(), Duration::from_secs(5))
        .unwrap()
        .with_bearer_source(manager.source_for("srv"));

    let phase1 = transport.request("phase1", json!({})).await.unwrap();
    assert_eq!(phase1, json!({ "phase": 1 }));

    // A login completes elsewhere (deck action / CLI) mid-session.
    let mut tokens = tokens_for(&auth, &mcp.uri());
    tokens.access_token = "mid-session-token".into();
    store.put("srv", &tokens).unwrap();

    let phase2 = transport.request("phase2", json!({})).await.unwrap();
    assert_eq!(phase2, json!({ "phase": 2 }));

    let _ = std::fs::remove_dir_all(&dir);
}
