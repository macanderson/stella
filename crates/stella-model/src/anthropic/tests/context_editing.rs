//! Context editing reaches the wire in the shape the API accepts — and does
//! not reach it at all unless the session asked for it.
//!
//! The off-by-default half is the one worth a test. Context editing trades
//! prompt cache for context room: clearing invalidates the cached prefix from
//! the point of the edit, and a cache re-write is priced an order of magnitude
//! above a cache read. A session that quietly gained this feature would get
//! slower and dearer with no symptom, which is exactly the failure this
//! repository keeps finding in its own benchmarks.

use super::*;

fn sse_ok() -> &'static str {
    concat!(
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\
         \"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    )
}

fn request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
}

/// Nothing about a request changes until a caller opts in — no field, and no
/// beta header. Both halves matter: the header is part of the request the
/// prompt cache is keyed on.
#[tokio::test]
async fn a_session_that_did_not_opt_in_sends_no_context_management() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_ok(), "text/event-stream"))
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(ApiKey::new("sk-test"), "claude-sonnet-5")
        .with_base_url(server.uri());
    provider.complete(request()).await.expect("should succeed");

    let sent = &server.received_requests().await.unwrap()[0];
    let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
    assert!(
        body.get("context_management").is_none(),
        "context_management must be absent by default: {body}"
    );
    assert!(
        sent.headers.get("anthropic-beta").is_none(),
        "no beta header rides on a request carrying no beta field"
    );
}

/// The opted-in shape, asserted field by field against the documented
/// contract — including the ordering the API rejects if reversed.
#[tokio::test]
async fn opting_in_sends_both_strategies_and_the_beta_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_ok(), "text/event-stream"))
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(ApiKey::new("sk-test"), "claude-sonnet-5")
        .with_base_url(server.uri())
        .with_context_editing(30_000, Some(1));
    provider.complete(request()).await.expect("should succeed");

    let sent = &server.received_requests().await.unwrap()[0];
    let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
    let edits = body["context_management"]["edits"].as_array().unwrap();

    // Thinking first — the API rejects the reverse order.
    assert_eq!(edits[0]["type"], "clear_thinking_20251015");
    assert_eq!(edits[0]["keep"]["type"], "thinking_turns");
    assert_eq!(edits[0]["keep"]["value"], 1);

    assert_eq!(edits[1]["type"], "clear_tool_uses_20250919");
    assert_eq!(edits[1]["trigger"]["value"], 30_000);
    // Without a floor on what an edit frees, the API may clear a few hundred
    // tokens and bill a full cache re-write to do it.
    assert_eq!(edits[1]["clear_at_least"]["value"], 5_000);
    // The newest calls survive: clearing them is how an agent loses the file
    // it just read.
    assert_eq!(edits[1]["keep"]["value"], 3);
    // Tool *inputs* survive — they are how the model knows what it already
    // ran, and they are small.
    assert_eq!(edits[1]["clear_tool_inputs"], false);

    let beta = sent
        .headers
        .get("anthropic-beta")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        beta.contains("context-management-2025-06-27"),
        "the gating beta must ride with the field: {beta}"
    );
}

/// Keeping every thinking block is the cache-preserving setting, and it
/// serializes as the bare string `"all"` rather than an object — the shape
/// Claude Code itself sends.
#[tokio::test]
async fn keeping_all_thinking_preserves_the_cache_and_uses_the_string_shape() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_ok(), "text/event-stream"))
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(ApiKey::new("sk-test"), "claude-sonnet-5")
        .with_base_url(server.uri())
        .with_context_editing(100_000, None);
    provider.complete(request()).await.expect("should succeed");

    let sent = &server.received_requests().await.unwrap()[0];
    let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(body["context_management"]["edits"][0]["keep"], "all");
}
