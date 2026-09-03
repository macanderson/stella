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

async fn request_bodies(server: &MockServer) -> Vec<String> {
    let requests = server.received_requests().await.expect("recorded requests");
    requests
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect()
}

async fn first_request_body(server: &MockServer) -> String {
    request_bodies(server)
        .await
        .into_iter()
        .next()
        .expect("at least one request")
}

/// Answer every call with a stream naming `upstream` as the vendor that
/// served it — OpenRouter's `provider` field, in the shape the gateway sends.
async fn mock_served_by(server: &MockServer, upstream: &str) {
    let body = format!(
        "data: {{\"provider\":\"{upstream}\",\
         \"choices\":[{{\"delta\":{{\"content\":\"ok\"}}}}]}}\n\ndata: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(server)
        .await;
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
/// silent cost regression (AGENTS.md #7).
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
    mock_served_by(&server, "Amazon Bedrock").await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "anthropic/claude-sonnet-5")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");
    let result = provider.complete(hello()).await.expect("should succeed");

    assert_eq!(result.upstream_provider.as_deref(), Some("Amazon Bedrock"));
}

/// Once an answer names a vendor, every later call asks for that vendor —
/// and lets the gateway fall back if it is down.
///
/// The sticky `session_id` is a hint the gateway drops under load: one
/// 174-call turn on a single slug (execution 327 of this repo's own store)
/// was served by three vendors, the route moved twelve times, and five calls
/// read no cache at all for $0.97 of the turn's $4.65. Three of those five
/// were the first call on a vendor new to the turn, which is what re-asking
/// removes.
///
/// The first request still carries no `provider` field: there is nothing to
/// ask for yet, so a session's opening bytes are unchanged.
#[tokio::test]
async fn a_gateway_is_reasked_for_the_upstream_that_served_the_first_call() {
    let server = MockServer::start().await;
    mock_served_by(&server, "Sail Research").await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "moonshotai/kimi-k3")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");
    provider.complete(hello()).await.expect("first call");
    provider.complete(hello()).await.expect("second call");

    let bodies = request_bodies(&server).await;
    assert!(!bodies[0].contains("\"provider\""), "{}", bodies[0]);
    assert!(
        bodies[1].contains("\"provider\":{\"order\":[\"Sail Research\"],\"allow_fallbacks\":true}"),
        "{}",
        bodies[1]
    );
}

/// An operator's pin outranks the learned one, fallbacks still refused.
///
/// The two want different things. A learned pin chases a warm cache and would
/// rather be served late than not at all; an operator pinning by hand is
/// holding a variable fixed for a measurement, and a quietly re-routed trial
/// reads just like a clean one.
#[tokio::test]
async fn an_operator_pin_outranks_the_upstream_that_served_the_first_call() {
    let server = MockServer::start().await;
    mock_served_by(&server, "Sail Research").await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "moonshotai/kimi-k3")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter")
        .with_upstream_pin(vec!["z-ai".to_string()]);
    provider.complete(hello()).await.expect("first call");
    provider.complete(hello()).await.expect("second call");

    let bodies = request_bodies(&server).await;
    assert!(
        bodies[1].contains("\"provider\":{\"order\":[\"z-ai\"],\"allow_fallbacks\":false}"),
        "{}",
        bodies[1]
    );
    assert!(!bodies[1].contains("Sail Research"), "{}", bodies[1]);
}

/// A non-gateway endpoint that names a vendor is still never sent `provider`.
///
/// Same gate as the cache opt-in: "does this address OpenRouter", not "did
/// something answer with a name we recognise". Z.ai direct, a local server or
/// a proxy that echoes the field would all reject the unknown key.
#[tokio::test]
async fn a_direct_endpoint_never_asks_for_the_upstream_it_was_told_about() {
    let server = MockServer::start().await;
    mock_served_by(&server, "Sail Research").await;

    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());
    provider.complete(hello()).await.expect("first call");
    provider.complete(hello()).await.expect("second call");

    let bodies = request_bodies(&server).await;
    assert!(!bodies[1].contains("\"provider\":{"), "{}", bodies[1]);
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
