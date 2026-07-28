//! Streamable-HTTP transport tests using `wiremock`. Covers both response
//! shapes a server may return (`application/json` and `text/event-stream`),
//! session-id capture + replay, the post-initialize `MCP-Protocol-Version`
//! header, and non-2xx mapping.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;
use stella_mcp::{HttpTransport, McpClient, McpError, McpServerConfig, McpTransport, Transport};
use stella_protocol::ToolOutput;
use wiremock::matchers::{body_partial_json, header, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn json_handshake_replays_the_session_id() {
    let server = MockServer::start().await;

    // `initialize` assigns a session id (no session header required on it).
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "initialize" })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .insert_header("Mcp-Session-Id", "sess-xyz")
                .set_body_json(json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": {
                        "protocolVersion": "2025-06-18",
                        "serverInfo": { "name": "remote", "version": "1" }
                    }
                })),
        )
        .mount(&server)
        .await;

    // The `notifications/initialized` notification (202, no body). It follows
    // the `initialize` response, so the 2025-06-18 spec's protocol-version
    // header is already required on it.
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({ "method": "notifications/initialized" }),
        ))
        .and(header("mcp-protocol-version", "2025-06-18"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    // `tools/list` matches ONLY if the session id and the negotiated protocol
    // version were replayed — proving capture + replay end-to-end.
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "tools/list" })))
        .and(header("mcp-session-id", "sess-xyz"))
        .and(header("mcp-protocol-version", "2025-06-18"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "jsonrpc": "2.0", "id": 2,
                    "result": { "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] }
                })),
        )
        .mount(&server)
        .await;

    // `tools/call` — likewise requires the session id and protocol version.
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "tools/call" })))
        .and(header("mcp-session-id", "sess-xyz"))
        .and(header("mcp-protocol-version", "2025-06-18"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "jsonrpc": "2.0", "id": 3,
                    "result": { "content": [{ "type": "text", "text": "remote ok" }] }
                })),
        )
        .mount(&server)
        .await;

    let cfg = McpServerConfig {
        name: "remote".into(),
        transport: McpTransport::Http {
            url: server.uri(),
            headers: BTreeMap::new(),
        },
        candidate_safe: false,
    };
    let client = McpClient::connect(&cfg, Duration::from_secs(5))
        .await
        .unwrap();

    assert_eq!(client.negotiated_version(), "2025-06-18");
    assert_eq!(client.tools().len(), 1);

    let out = client.call_tool("echo", json!({})).await.unwrap();
    assert_eq!(
        out,
        ToolOutput::Ok {
            content: "remote ok".into()
        }
    );
}

/// The 2025-06-18 streamable-HTTP spec requires every request after
/// initialization to carry the *negotiated* revision in the
/// `MCP-Protocol-Version` header — the one the server named back, not the one
/// the client offered. The server here counter-offers an older revision and
/// answers `notifications/initialized` / `tools/list` only when that revision
/// comes back in the header, so a wrong (or missing) header fails the
/// handshake outright.
#[tokio::test]
async fn the_negotiated_protocol_version_rides_every_request_after_initialize() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "initialize" })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": { "protocolVersion": "2025-03-26" }
                })),
        )
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({ "method": "notifications/initialized" }),
        ))
        .and(header("mcp-protocol-version", "2025-03-26"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "tools/list" })))
        .and(header("mcp-protocol-version", "2025-03-26"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "jsonrpc": "2.0", "id": 2,
                    "result": { "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] }
                })),
        )
        .mount(&server)
        .await;

    let cfg = McpServerConfig {
        name: "remote".into(),
        transport: McpTransport::Http {
            url: server.uri(),
            headers: BTreeMap::new(),
        },
        candidate_safe: false,
    };
    let client = McpClient::connect(&cfg, Duration::from_secs(5))
        .await
        .expect("handshake succeeds only if the negotiated version was replayed");
    assert_eq!(client.negotiated_version(), "2025-03-26");
    assert_eq!(client.tools().len(), 1);
}

#[tokio::test]
async fn sse_response_is_parsed() {
    let server = MockServer::start().await;

    // A progress notification (no id) interleaved before the real response —
    // the transport must skip it and return the correlated result.
    let sse_body = concat!(
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"p\":0.5}}\n",
        "\n",
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"sse ok\"}]}}\n",
        "\n",
    );
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "tools/call" })))
        .respond_with(
            // `set_body_raw` sets the Content-Type cleanly (no duplicate
            // header from a body helper) so the transport detects SSE.
            ResponseTemplate::new(200).set_body_raw(sse_body.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let transport =
        HttpTransport::new("h", &server.uri(), &BTreeMap::new(), Duration::from_secs(5)).unwrap();
    // First request on a fresh transport is id 1 — matches the SSE payload.
    let result = transport
        .request("tools/call", json!({ "name": "echo", "arguments": {} }))
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "sse ok");
}

#[tokio::test]
async fn non_2xx_is_a_transport_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal boom"))
        .mount(&server)
        .await;

    let transport =
        HttpTransport::new("h", &server.uri(), &BTreeMap::new(), Duration::from_secs(5)).unwrap();
    let err = transport
        .request("tools/call", json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, McpError::Transport(_)));
}

/// Witness for the response-body cap. An MCP server is untrusted input and
/// `reqwest`'s body readers are unbounded, so a server that answers with a
/// gigabyte would be an out-of-memory kill of stella long before the per-call
/// timeout noticed. The cap has to hold on the *success* path too — that is
/// the one a hostile server actually uses.
#[tokio::test]
async fn an_oversized_response_body_is_refused_rather_than_buffered() {
    let server = MockServer::start().await;
    // 9 MiB — past the transport's 8 MiB body cap.
    let flood = "x".repeat(9 * 1024 * 1024);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(flood),
        )
        .mount(&server)
        .await;

    let transport = HttpTransport::new(
        "h",
        &server.uri(),
        &BTreeMap::new(),
        Duration::from_secs(30),
    )
    .unwrap();
    let err = transport
        .request("tools/call", json!({}))
        .await
        .unwrap_err();
    match err {
        McpError::Transport(message) => assert!(
            message.contains("cap"),
            "the refusal names the budget: {message}"
        ),
        other => panic!("expected a transport error naming the cap, got {other:?}"),
    }
}

#[tokio::test]
async fn configured_headers_are_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({ "jsonrpc": "2.0", "id": 1, "result": { "ok": true } })),
        )
        .mount(&server)
        .await;

    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_string(), "Bearer tok".to_string());
    let transport =
        HttpTransport::new("h", &server.uri(), &headers, Duration::from_secs(5)).unwrap();
    // Succeeds only because the configured Authorization header was sent.
    let result = transport.request("ping", json!({})).await.unwrap();
    assert_eq!(result["ok"], true);
}
