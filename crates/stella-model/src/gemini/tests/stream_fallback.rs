// SPDX-License-Identifier: AGPL-3.0-only
//! The streaming→non-streaming fallback on the Google dialect (#2746,
//! extending #2686): a stream that hangs before its first byte or comes back
//! empty fails retryably, arms the per-session latch, and the retried attempt
//! re-issues the byte-identical payload against plain `generateContent`.
//!
//! Unlike the OpenAI-shaped dialects, the two delivery paths here differ on
//! the URL rather than in the body — which is why the mock arms match on
//! `path` and the hand-rolled servers take
//! [`crate::stream_fallback_support::stream_method_in_url`].

use super::*;
use crate::stream_fallback_support::{
    empty_streams_stall_the_unary_body, hang_streams_answer_unary, stream_method_in_url,
};

const STREAM_PATH: &str = "/models/gemini-3-pro:streamGenerateContent";
const UNARY_PATH: &str = "/models/gemini-3-pro:generateContent";

fn plain_request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![CompletionMessage::user("say hello")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
}

/// The #2746 witness for this dialect: a stream that hangs before its first
/// byte fails the attempt at the first-byte deadline — retryably, so the
/// ordinary retry machinery re-drives it — and that retry completes as a
/// plain `generateContent` call for the same payload. Before this change
/// `streamGenerateContent` was the only path the adapter knew: the first call
/// waited the full 120s idle bound and every retry re-issued the identical
/// streaming request into the same buffering proxy.
#[tokio::test]
async fn a_gemini_stream_hung_before_its_first_byte_falls_back_to_generate_content() {
    let base_url = hang_streams_answer_unary(
        stream_method_in_url,
        r#"{"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"recovered without streaming"}]}}],"usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":4,"thoughtsTokenCount":1,"cachedContentTokenCount":2}}"#,
    );
    let provider = GeminiProvider::new(ApiKey::new("test-key"), "gemini-3-pro")
        .with_base_url(base_url)
        .with_first_byte_deadline(Duration::from_millis(120));

    let error = provider
        .complete(plain_request())
        .await
        .expect_err("a hung stream must fault at the first-byte deadline, not hang");
    assert!(
        error.is_retryable(),
        "the fault must be retryable so the retry ladder drives the fallback: {error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("first byte"), "{message}");
    assert!(
        message.contains("non-streaming"),
        "the error names the switch it armed: {message}"
    );

    // The engine retries a retryable fault through the same provider
    // instance; the armed latch makes that retry unary.
    let result = provider
        .complete(plain_request())
        .await
        .expect("the fallback attempt completes without streaming");
    assert_eq!(result.text, "recovered without streaming");
    assert_eq!(result.usage.input_tokens, 8);
    // Thinking tokens are billed output on this dialect, and the unary path
    // must normalize them exactly as the aggregator does.
    assert_eq!(result.usage.output_tokens, 5);
    assert_eq!(result.usage.cached_input_tokens, 2);
    assert!(result.usage.reported);
    assert_eq!(result.finish_reason, Some(FinishReason::Stop));
}

/// The other broken-stream shape: a 200 with an empty stream (EOF before any
/// frame). Same latch, and the unary response here carries a `functionCall`
/// part with a thought signature, proving the fallback path parses the whole
/// dialect — call-id minting included.
#[tokio::test]
async fn an_empty_gemini_stream_falls_back_to_generate_content() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(STREAM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(UNARY_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"thinking out loud","thought":true},{"functionCall":{"name":"read_file","args":{"path":"src/lib.rs"}},"thoughtSignature":"sig123"}]}}],"usageMetadata":{"promptTokenCount":12,"candidatesTokenCount":9}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("test-key"), "gemini-3-pro").with_base_url(server.uri());

    let error = provider
        .complete(plain_request())
        .await
        .expect_err("an empty stream is a fault, never an empty Ok");
    assert!(error.is_retryable(), "{error:?}");
    assert!(error.to_string().contains("non-streaming"), "{error}");

    let result = provider
        .complete(plain_request())
        .await
        .expect("the fallback attempt completes");
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].call_id, "call_0#sig123");
    assert_eq!(result.tool_calls[0].name, "read_file");
    assert_eq!(
        result.tool_calls[0].input,
        serde_json::json!({"path": "src/lib.rs"})
    );
    // A thought part is not the answer, on either path.
    assert_eq!(result.text, "");
    assert_eq!(result.finish_reason, Some(FinishReason::ToolCalls));
    assert!(result.usage.reported);
}

/// Control: a healthy stream never arms the latch — every request of the
/// session keeps `streamGenerateContent` on the wire, so the fallback costs
/// nothing when nothing is broken.
#[tokio::test]
async fn a_healthy_gemini_stream_never_arms_the_fallback() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello!\"}]}}]}\n\n",
        "data: {\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"parts\":[]}}],\"usageMetadata\":{\"promptTokenCount\":8,\"candidatesTokenCount\":3}}\n\n",
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("test-key"), "gemini-3-pro").with_base_url(server.uri());
    for _ in 0..2 {
        let result = provider
            .complete(plain_request())
            .await
            .expect("healthy streams complete");
        assert_eq!(result.text, "Hello!");
    }

    let sent = server.received_requests().await.expect("recorded requests");
    assert_eq!(sent.len(), 2);
    for request in &sent {
        assert!(
            request.url.path().ends_with(":streamGenerateContent"),
            "no request may lose streaming on a healthy session: {}",
            request.url
        );
    }
}

/// A stream that died AFTER delivering content is the existing mid-stream
/// death, not fallback material: there is salvage to bill and the retry stays
/// a stream. Guards the eligibility boundary against widening into "any
/// stream error goes unary".
#[tokio::test]
async fn a_gemini_stream_that_died_after_content_is_retried_as_a_stream() {
    let server = MockServer::start().await;
    // A real part arrives, then the connection closes without a finishReason.
    let sse_body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hel\"}]}}]}\n\n";
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("test-key"), "gemini-3-pro").with_base_url(server.uri());
    for _ in 0..2 {
        let error = provider
            .complete(plain_request())
            .await
            .expect_err("a half-answer must never commit as Ok");
        assert!(
            !error.to_string().contains("non-streaming"),
            "salvageable deaths must not arm the fallback: {error}"
        );
    }

    let sent = server.received_requests().await.expect("recorded requests");
    assert_eq!(sent.len(), 2);
    for request in &sent {
        assert!(request.url.path().ends_with(":streamGenerateContent"));
    }
}

/// #547's other half, applied to this dialect's fallback path: on the unary
/// client the read bound covers the ENTIRE generation, so its expiry means
/// the request was too long to serve — re-issuing it identically just waits
/// out the full bound again once per retry. Driven end-to-end: an empty
/// stream arms the latch, the retry goes unary, and its body stalls after
/// the head.
#[tokio::test]
async fn a_gemini_unary_body_read_timeout_is_terminal_never_a_retry_storm() {
    let base_url = empty_streams_stall_the_unary_body(stream_method_in_url);
    let provider = GeminiProvider::new(ApiKey::new("test-key"), "gemini-3-pro")
        .with_base_url(base_url)
        .with_unary_read_timeout(Duration::from_millis(120));

    let armed = provider
        .complete(plain_request())
        .await
        .expect_err("an empty stream is a fault, never an empty Ok");
    assert!(
        armed.to_string().contains("non-streaming"),
        "the latch must arm before the unary path can be reached: {armed}"
    );

    let error = provider
        .complete(plain_request())
        .await
        .expect_err("a body that stops mid-read must fault, not hang");
    assert!(
        matches!(error, ProviderError::Terminal(_)),
        "a unary BODY read timeout must be Terminal, got {error:?}"
    );
    assert!(
        !error.is_retryable(),
        "a retryable body-read timeout re-issues the identical too-long \
         request until the budget dies (#547): {error:?}"
    );
}

/// A probe that fails reverts the latch: when the unary retry ALSO fails, the
/// fault evidently wasn't the streaming path's, and the session must not stay
/// pinned to a transport it has no evidence for. The wire sequence proves the
/// full cycle: stream → unary probe → stream again.
#[tokio::test]
async fn a_failed_gemini_unary_probe_reverts_the_session_to_streaming() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(STREAM_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(UNARY_PATH))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream down"))
        .mount(&server)
        .await;

    let provider =
        GeminiProvider::new(ApiKey::new("test-key"), "gemini-3-pro").with_base_url(server.uri());
    for _ in 0..3 {
        let _ = provider
            .complete(plain_request())
            .await
            .expect_err("every arm of this server fails");
    }

    let sent = server.received_requests().await.expect("recorded requests");
    let streamed: Vec<bool> = sent
        .iter()
        .map(|request| request.url.path().ends_with(":streamGenerateContent"))
        .collect();
    assert_eq!(
        streamed,
        vec![true, false, true],
        "stream fault → unary probe → probe failed → back to streaming"
    );
}
