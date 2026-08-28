//! OpenRouter streaming regressions: the gateway's own frame shapes —
//! `content: ""` beside `reasoning` on a tool turn, and in-band error frames
//! whose only status lives in `code`. Split out of the parent `tests.rs`
//! (file-size gate) since they share a distinct fixture shape: a mock SSE
//! stream driven through the `openrouter` identity.

use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The regression behind the "stella is talking to itself" reports: an
/// Anthropic model routed through OpenRouter streams `content: ""` alongside
/// `reasoning` and the tool call on EVERY tool-calling turn. The
/// reasoning-only fallback existed for turns that would otherwise render
/// blank, but without a `calls.is_empty()` guard it fired here too and
/// published the model's private deliberation as its user-visible answer.
/// A turn that called a tool is not blank and must keep `text` empty.
#[tokio::test]
async fn openrouter_tool_call_turn_never_leaks_reasoning_as_the_answer() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"\",\"reasoning\":\"The user is asking \"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"\",\"reasoning\":\"something I should check first.\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"project_overview\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "anthropic/claude-fable-5")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("scaffold a nextjs app")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };
    let result = provider.complete(req).await.expect("should succeed");
    assert_eq!(
        result.text, "",
        "chain-of-thought must never become the answer on a tool turn"
    );
    assert_eq!(result.tool_calls.len(), 1, "the tool call still lands");
    assert_eq!(result.tool_calls[0].name, "project_overview");
}

/// OpenRouter reports the upstream HTTP status ONLY in the in-band frame's
/// `code` field. Dropping it meant the classifier's own `contains("502")`
/// arm could never fire, so a transient gateway failure fell through to
/// non-retryable `Terminal` and burned the turn with every configured retry
/// unused.
#[tokio::test]
async fn openrouter_in_band_error_code_502_is_retryable() {
    let server = MockServer::start().await;
    // The exact frame a live OpenRouter stream emits when its upstream
    // provider returns nothing — note the status lives in `code`, and the
    // message text contains no digits at all.
    let sse_body = concat!(
        "data: {\"error\":{\"message\":\"Provider returned an empty response\",\"code\":502}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "anthropic/claude-fable-5")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };
    let err = provider.complete(req).await.expect_err("must be an error");
    assert!(
        err.is_retryable(),
        "a transient upstream 502 must be retried, got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("Provider returned an empty response"),
        "surfaces the gateway's own text: {msg}"
    );
}

/// A `code: 429` frame whose prose never says "rate limit" must still
/// classify as throttling rather than a permanent rejection.
#[tokio::test]
async fn openrouter_in_band_error_code_429_is_rate_limited() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"error\":{\"message\":\"Too many requests upstream\",\"code\":429}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "anthropic/claude-fable-5")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };
    let err = provider.complete(req).await.expect_err("must be an error");
    assert!(
        matches!(err, ProviderError::RateLimited { .. }),
        "got {err:?}"
    );
    assert!(err.is_retryable());
}

/// The negative control for the two tests above: a genuinely blank turn —
/// reasoning streamed, no content, and NO tool call — must still fall back
/// to the reasoning so the turn is never silently empty. This is the
/// behavior the `calls.is_empty()` guard has to preserve.
#[tokio::test]
async fn openrouter_reasoning_only_turn_still_falls_back() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"\",\"reasoning\":\"only thinking\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "anthropic/claude-fable-5")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };
    let result = provider.complete(req).await.expect("should succeed");
    assert_eq!(result.text, "only thinking");
    assert!(result.tool_calls.is_empty());
}

/// Thinking must reach the observer on its OWN channel. The transcript folds
/// `reasoning_delta` into a collapsible, dimmed entry, so wiring it here is
/// what makes a reasoning model's deliberation visible live without it ever
/// being mistaken for the answer — the whole point of keeping the two
/// channels separate rather than reviving the old text fallback.
#[tokio::test]
async fn reasoning_streams_to_the_observer_separately_from_the_answer() {
    #[derive(Default)]
    struct SplitObserver {
        text: std::sync::Mutex<Vec<String>>,
        thinking: std::sync::Mutex<Vec<String>>,
    }
    impl ToolCallObserver for SplitObserver {
        fn tool_call_streamed(&self, _call: &ToolCall) {}
        fn text_delta(&self, delta: &str) {
            self.text.lock().unwrap().push(delta.to_string());
        }
        fn reasoning_delta(&self, delta: &str) {
            self.thinking.lock().unwrap().push(delta.to_string());
        }
    }

    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"\",\"reasoning\":\"weighing \"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"\",\"reasoning\":\"the options\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"The answer.\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = ZaiProvider::new(ApiKey::new("sk-or-test"), "anthropic/claude-fable-5")
        .with_base_url(server.uri())
        .with_identity("openrouter", "OpenRouter");
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };
    let observer = SplitObserver::default();
    let result = provider
        .complete_observed(req, &observer)
        .await
        .expect("should succeed");

    assert_eq!(
        *observer.thinking.lock().unwrap(),
        vec!["weighing ", "the options"],
        "thinking streams in order on its own channel"
    );
    assert_eq!(
        *observer.text.lock().unwrap(),
        vec!["", "", "The answer."],
        "the answer channel carries only content, never thinking"
    );
    assert_eq!(result.text, "The answer.");
}
