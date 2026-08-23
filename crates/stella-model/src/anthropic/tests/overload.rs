//! A brownout must reach the engine as the park-eligible `Overloaded` class,
//! not as an undifferentiated `Transport` — off the status line (#2742) and
//! from inside an open stream (#3859). Split from the parent `tests.rs`
//! (file-size gate).

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
    assert!(
        err.partial_usage().is_none(),
        "a status-line 529 never opened a stream, so it has nothing to report"
    );
}

/// The in-band half (#3859): Anthropic's own `overloaded_error` frame,
/// arriving after `message_start` already reported the prompt's input tokens.
///
/// Classified as `Transport` the brownout could not park, so a sustained one
/// burned the ~16s ladder and aborted a turn with wall-clock budget left.
/// Classified as an `Overloaded` that carries no accounting, the input tokens
/// the provider had already billed for died with the adapter's stack frame.
/// Which side of the response boundary the provider shed load on is invisible
/// to the user and must decide neither, so the test pins both.
#[tokio::test]
async fn a_mid_stream_overloaded_error_parks_and_keeps_the_usage_the_stream_reported() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":3000,\"output_tokens\":0,\"cache_read_input_tokens\":58000}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n",
        "event: error\n",
        "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
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
    assert!(
        err.is_park_eligible(),
        "an in-band brownout is the same condition waiting fixes: {err:?}"
    );

    let partial = err
        .partial_usage()
        .expect("the input tokens message_start reported must survive the error");
    assert_eq!(partial.usage.input_tokens, 61_000);
    assert_eq!(partial.usage.cached_input_tokens, 58_000);
    assert!(partial.input_reported);
}
