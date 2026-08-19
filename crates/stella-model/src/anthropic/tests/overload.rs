//! A 529 brownout must reach the engine as the park-eligible `Overloaded`
//! class, not as an undifferentiated `Transport` (#2742). Split from the
//! parent `tests.rs` (file-size gate).

use super::*;

/// 529 is Anthropic's own load-shedding status, and the adapter must hand it
/// to the engine as the park-eligible `Overloaded` class rather than the
/// undifferentiated `Transport` — otherwise a sustained brownout burns the
/// ~16s inline ladder and aborts a turn that still has budget (#2742).
#[tokio::test]
async fn complete_maps_529_to_park_eligible_overloaded_with_the_stated_wait() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(529)
                .insert_header("retry-after", "45")
                .set_body_string("overloaded"),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(ApiKey::new("sk-test"), "claude-fable-5")
        .with_base_url(server.uri());
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };

    let err = provider.complete(req).await.unwrap_err();
    assert!(matches!(err, ProviderError::Overloaded { .. }), "{err:?}");
    assert!(err.is_retryable(), "529 must be retryable");
    assert!(err.is_park_eligible(), "529 must be able to park");
    assert_eq!(
        err.retry_after_hint_ms(),
        Some(45_000),
        "the server's stated wait must survive classification"
    );
}
