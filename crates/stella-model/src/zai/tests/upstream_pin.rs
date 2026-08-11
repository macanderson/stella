//! Gateway upstream pinning, and recording who actually served the call.
//!
//! A gateway is the one endpoint shape where "which provider served this?"
//! has an answer other than the provider id, and OpenRouter chooses that
//! answer per app identity — so the same slug can be served by a different
//! vendor between two runs, or between two trials of one run, while every
//! trace records the same id. These cover both halves of closing that:
//! constraining the routing (`provider.order`, fallbacks refused) and
//! recording what came back. Split out of the parent `tests.rs` for the
//! file-size gate, per its sibling modules.

use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OK_SSE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";

async fn mock_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(OK_SSE, "text/event-stream"))
        .mount(server)
        .await;
}

async fn first_request_body(server: &MockServer) -> String {
    let requests = server.received_requests().await.expect("recorded requests");
    String::from_utf8_lossy(&requests[0].body).into_owned()
}

fn hello() -> CompletionRequest {
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

/// A pinned gateway must say so on the wire, with fallbacks refused.
///
/// Without `provider.order` the gateway routes wherever it likes, which makes
/// a head-to-head vary the model provider it claims to hold fixed.
/// `allow_fallbacks: false` rides along because a pin that silently falls back
/// under load is a preference, not a pin — and it would fail exactly when the
/// data matters most, on some trials and not others.
#[tokio::test]
async fn a_pinned_gateway_sends_its_upstream_order_with_fallbacks_refused() {
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "z-ai/glm-5.2")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter")
        .with_upstream_pin(vec!["z-ai".to_string()]);
    provider.complete(hello()).await.expect("should succeed");

    let body = first_request_body(&server).await;
    assert!(
        body.contains("\"provider\":{\"order\":[\"z-ai\"],\"allow_fallbacks\":false}"),
        "{body}"
    );
}

/// An unpinned provider keeps its pre-field bytes exactly.
///
/// Asserted rather than assumed for two reasons: no other Chat Completions
/// server speaks `provider`, so an unconditional field risks a hard 400 on
/// endpoints the user never opted into experimenting with; and the request
/// body is the prompt-cache key, so a field appearing for everyone would be a
/// silent cost regression (invariant 7).
#[tokio::test]
async fn an_unpinned_gateway_sends_no_provider_field() {
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "z-ai/glm-5.2")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");
    provider.complete(hello()).await.expect("should succeed");

    let body = first_request_body(&server).await;
    assert!(!body.contains("\"provider\""), "{body}");
}

/// A pin offered to a NON-gateway endpoint stays off the wire. Same gate as
/// the cache opt-in: "does this actually address OpenRouter", not "was a pin
/// configured" — Z.ai direct would reject the unknown key.
#[tokio::test]
async fn a_pin_never_reaches_an_endpoint_that_is_not_the_gateway() {
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider = ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2")
        .with_base_url(server.uri())
        .with_upstream_pin(vec!["z-ai".to_string()]);
    provider.complete(hello()).await.expect("should succeed");

    let body = first_request_body(&server).await;
    assert!(!body.contains("\"provider\""), "{body}");
}

/// The streamed answer records WHICH upstream served it.
///
/// The pin constrains routing; this proves it, and is the half that makes a
/// trace auditable rather than merely intended. A probe carrying Stella's own
/// attribution once asked OpenRouter for `anthropic/claude-sonnet-5` and was
/// served by Amazon Bedrock — unprovable from any trace, because the adapter
/// recorded the gateway and discarded the upstream.
#[tokio::test]
async fn a_streamed_gateway_answer_records_the_upstream_that_served_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"provider\":\"Amazon Bedrock\",\
             \"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "anthropic/claude-sonnet-5")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");
    let result = provider.complete(hello()).await.expect("should succeed");

    assert_eq!(result.upstream_provider.as_deref(), Some("Amazon Bedrock"));
}

/// A direct endpoint names no upstream: `None` is the honest answer, and is a
/// different fact from "a gateway that declined to say".
#[tokio::test]
async fn a_direct_endpoint_records_no_upstream() {
    let server = MockServer::start().await;
    mock_ok(&server).await;

    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());
    let result = provider.complete(hello()).await.expect("should succeed");

    assert_eq!(result.upstream_provider, None);
}
