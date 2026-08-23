// SPDX-License-Identifier: AGPL-3.0-only
//! The streaming→non-streaming fallback (#2686): a stream that hangs before
//! its first byte or comes back empty fails retryably, arms the per-session
//! latch, and the retried attempt re-issues the byte-identical payload with
//! `stream: false`. Split out of the parent `tests.rs` (file-size gate),
//! mirroring the other delivery-path suites.

use super::*;
use crate::stream_fallback_support::{
    empty_streams_stall_the_unary_body, hang_streams_answer_unary, stream_flag_in_body,
};

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

/// The #2686 witness: a stream that hangs before its first byte fails the
/// attempt at the first-byte deadline — retryably, so the ordinary retry
/// machinery re-drives it — and that retry completes as a non-streaming
/// request for the same payload. On main the first call instead waited the
/// full 120s idle bound and every retry re-issued the identical streaming
/// request into the same buffering proxy.
#[tokio::test]
async fn a_stream_hung_before_its_first_byte_falls_back_to_a_non_streaming_request() {
    let base_url = hang_streams_answer_unary(
        stream_flag_in_body,
        r#"{"id":"cmpl-1","choices":[{"message":{"content":"recovered without streaming"},"finish_reason":"stop"}],"usage":{"prompt_tokens":8,"completion_tokens":5}}"#,
    );
    let provider = ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2")
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
    assert_eq!(result.usage.output_tokens, 5);
    assert!(result.usage.reported);
    assert_eq!(result.finish_reason, Some(FinishReason::Stop));
}

/// The other broken-stream shape: a gateway answering 200 with an empty
/// stream (EOF before any data). Same latch, and the unary response here
/// carries a tool call, proving the fallback path parses the whole dialect
/// — not just text.
#[tokio::test]
async fn an_empty_stream_falls_back_to_a_non_streaming_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("\"stream\":false"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"src/lib.rs\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":12,"completion_tokens":9}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());

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
    assert_eq!(result.tool_calls[0].name, "read_file");
    assert_eq!(
        result.tool_calls[0].input,
        serde_json::json!({"path": "src/lib.rs"})
    );
    assert_eq!(result.finish_reason, Some(FinishReason::ToolCalls));
    assert!(result.usage.reported);
}

/// Control: a healthy stream never arms the latch — every request of the
/// session keeps `stream: true` on the wire, so the fallback costs nothing
/// when nothing is broken.
#[tokio::test]
async fn a_healthy_stream_never_arms_the_fallback() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hello!\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":3}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());
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
        let body = String::from_utf8_lossy(&request.body);
        assert!(
            body.contains("\"stream\":true"),
            "no request may lose streaming on a healthy session: {body}"
        );
    }
}

/// A stream that died AFTER delivering content is the existing mid-stream
/// death, not fallback material: there is salvage to bill and the retry
/// stays a stream. Guards the eligibility boundary against widening into
/// "any stream error goes unary".
#[tokio::test]
async fn a_stream_that_died_after_content_is_retried_as_a_stream_not_unary() {
    let server = MockServer::start().await;
    // A real delta arrives, then the connection closes without `[DONE]`.
    let sse_body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n";
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());
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
        let body = String::from_utf8_lossy(&request.body);
        assert!(body.contains("\"stream\":true"), "{body}");
    }
}

/// #547's other half, applied to the fallback path: on the unary client the
/// read bound covers the ENTIRE generation, so its expiry means the request
/// was too long to serve — re-issuing it identically just waits out the full
/// bound again once per retry (the exact storm #547 documented on Bedrock).
/// The unary dispatch must classify a read-timeout as non-retryable
/// `Terminal`, while the SAME expiry on the streaming dispatch stays
/// retryable (there it is only a header stall the next attempt may clear).
/// Exercised in milliseconds by handing `dispatch` a client with a tiny read
/// bound against a server that accepts and then stalls.
#[tokio::test]
async fn a_unary_read_timeout_is_terminal_never_a_retry_storm() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        // Accept, then hold the response — the stalled-generation shape,
        // compressed so the test costs milliseconds instead of 600s.
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;

    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());
    let stalled_client = reqwest::Client::builder()
        .read_timeout(Duration::from_millis(50))
        .build()
        .expect("client builds");
    let request = plain_request();
    let body = provider.build_body(request.as_borrowed(), false, false);

    let error = provider
        .dispatch(&stalled_client, &body, true)
        .await
        .expect_err("the stalled unary dispatch must time out");
    assert!(
        matches!(error, ProviderError::Terminal(_)),
        "a unary read timeout must be Terminal, got {error:?}"
    );
    assert!(
        !error.is_retryable(),
        "a retryable unary read timeout re-issues the identical too-long \
         request until the budget dies (#547): {error:?}"
    );

    // The converse, so the fix cannot over-reach: the same expiry on the
    // STREAMING dispatch is a header stall and stays retryable.
    let error = provider
        .dispatch(&stalled_client, &body, false)
        .await
        .expect_err("the stalled streaming dispatch must time out");
    assert!(
        error.is_retryable(),
        "a streaming header stall generated nothing and must stay retryable: {error:?}"
    );
}

/// The same classification, one read later. The test above pins `send()`,
/// where a whole-response delay lands; the unary read bound also covers the
/// **body**, and `complete_unary_attempt` routes `text()`'s failure through
/// the same classifier for the same reason — a timeout there consumed the
/// bound and the identical re-issue would consume it again. Nothing pinned
/// that hunk, so a refactor could put the body read back on retryable
/// `Transport` and re-create #547's storm with every test green (#2756).
///
/// Driven end-to-end through the fallback: an empty stream arms the latch,
/// the retry goes unary, and its body stalls after the head.
#[tokio::test]
async fn a_unary_body_read_timeout_is_terminal_never_a_retry_storm() {
    let base_url = empty_streams_stall_the_unary_body(stream_flag_in_body);
    let provider = ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2")
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

/// A probe that fails reverts the latch: when the unary retry ALSO fails,
/// the fault evidently wasn't the streaming path's, and the session must
/// not stay pinned to a transport it has no evidence for. The wire
/// sequence proves the full cycle: stream → unary probe → stream again.
#[tokio::test]
async fn a_failed_unary_probe_reverts_the_session_to_streaming() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("\"stream\":false"))
        .respond_with(ResponseTemplate::new(502).set_body_string("upstream down"))
        .mount(&server)
        .await;

    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());
    for _ in 0..3 {
        let _ = provider
            .complete(plain_request())
            .await
            .expect_err("every arm of this server fails");
    }

    let sent = server.received_requests().await.expect("recorded requests");
    let flags: Vec<bool> = sent
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).contains("\"stream\":true"))
        .collect();
    assert_eq!(
        flags,
        vec![true, false, true],
        "stream fault → unary probe → probe failed → back to streaming"
    );
}
