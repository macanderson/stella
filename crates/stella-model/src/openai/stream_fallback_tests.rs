// SPDX-License-Identifier: AGPL-3.0-only
//! The streaming→non-streaming fallback on the Responses dialect (#2746,
//! extending #2686): a stream that hangs before its first byte or comes back
//! empty fails retryably, arms the per-session latch, and the retried attempt
//! re-issues the byte-identical payload with `stream: false`.
//!
//! Beside `openai.rs` rather than inside its `mod tests`: that file is a
//! grandfathered god file closed to growth (AGENTS.md § God files), the same
//! reason `openai/provider.rs` hosts the parallel-tool-call witness.

use std::time::Duration;

use stella_protocol::{CompletionMessage, CompletionRequest, FinishReason, ProviderError};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::OpenAiProvider;
use crate::credential::ApiKey;
use crate::provider::Provider;
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

/// The #2746 witness for this dialect: a stream that hangs before its first
/// byte fails the attempt at the first-byte deadline — retryably, so the
/// ordinary retry machinery re-drives it — and that retry completes as a
/// non-streaming Responses request for the same payload. Before this change
/// the Responses adapter had no unary parse path at all: the first call
/// waited the full 120s idle bound and every retry re-issued the identical
/// streaming request into the same buffering proxy.
#[tokio::test]
async fn an_openai_stream_hung_before_its_first_byte_falls_back_to_a_unary_request() {
    let base_url = hang_streams_answer_unary(
        stream_flag_in_body,
        r#"{"id":"resp_1","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"recovered without streaming"}]}],"usage":{"input_tokens":8,"output_tokens":5,"input_tokens_details":{"cached_tokens":3}}}"#,
    );
    let provider = OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5")
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
    assert_eq!(result.usage.cached_input_tokens, 3);
    assert_eq!(result.usage.output_tokens, 5);
    assert!(result.usage.reported);
    assert_eq!(result.finish_reason, Some(FinishReason::Stop));
}

/// The other broken-stream shape: a 200 with an empty stream (EOF before any
/// event). Same latch, and the unary response here carries a `function_call`
/// item, proving the fallback path parses the whole dialect — not just text.
#[tokio::test]
async fn an_empty_openai_stream_falls_back_to_a_unary_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_string_contains("\"stream\":false"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"id":"resp_2","status":"completed","output":[{"type":"reasoning","summary":[]},{"type":"function_call","call_id":"call_1","name":"read_file","arguments":"{\"path\":\"src/lib.rs\"}"}],"usage":{"input_tokens":12,"output_tokens":9,"output_tokens_details":{"reasoning_tokens":4}}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());

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
    assert_eq!(result.tool_calls[0].call_id, "call_1");
    assert_eq!(result.tool_calls[0].name, "read_file");
    assert_eq!(
        result.tool_calls[0].input,
        serde_json::json!({"path": "src/lib.rs"})
    );
    // A `reasoning` item is not the answer, and its token count still bills.
    assert_eq!(result.text, "");
    assert_eq!(result.usage.reasoning_tokens, Some(4));
    assert_eq!(result.finish_reason, Some(FinishReason::ToolCalls));
    assert!(result.usage.reported);
}

/// The unary path must classify the three terminal statuses exactly as the
/// stream classifies its three terminal events: a cap-hit response is
/// `FinishReason::Length` (the driver continues it), and every other
/// incompletion is terminal. Reached through the latch, so both arms are the
/// real fallback path.
#[tokio::test]
async fn a_unary_openai_response_maps_the_output_cap_to_length_not_a_terminal_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_string_contains("\"stream\":false"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"id":"resp_3","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"half an ans"}]}],"usage":{"input_tokens":9,"output_tokens":64}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
    let _ = provider.complete(plain_request()).await.expect_err("arms");

    let result = provider
        .complete(plain_request())
        .await
        .expect("a cap-hit response is a completion the driver continues");
    assert_eq!(result.text, "half an ans");
    assert_eq!(result.finish_reason, Some(FinishReason::Length));
    // The cap-hit turn is the most expensive kind: its usage must survive.
    assert_eq!(result.usage.output_tokens, 64);
}

/// Control: a healthy stream never arms the latch — every request of the
/// session keeps `stream: true` on the wire, so the fallback costs nothing
/// when nothing is broken.
#[tokio::test]
async fn a_healthy_openai_stream_never_arms_the_fallback() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello!\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":8,\"output_tokens\":3}}}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
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
/// death, not fallback material: there is salvage to bill and the retry stays
/// a stream. Guards the eligibility boundary against widening into "any
/// stream error goes unary".
#[tokio::test]
async fn an_openai_stream_that_died_after_content_is_retried_as_a_stream() {
    let server = MockServer::start().await;
    // A real delta arrives, then the connection closes without
    // `response.completed`.
    let sse_body = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n";
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
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

/// #547's other half, applied to this dialect's fallback path: on the unary
/// client the read bound covers the ENTIRE generation, so its expiry means
/// the request was too long to serve — re-issuing it identically just waits
/// out the full bound again once per retry. Driven end-to-end: an empty
/// stream arms the latch, the retry goes unary, and its body stalls after
/// the head.
#[tokio::test]
async fn an_openai_unary_body_read_timeout_is_terminal_never_a_retry_storm() {
    let base_url = empty_streams_stall_the_unary_body(stream_flag_in_body);
    let provider = OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5")
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
async fn a_failed_openai_unary_probe_reverts_the_session_to_streaming() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_string_contains("\"stream\":false"))
        .respond_with(ResponseTemplate::new(502).set_body_string("upstream down"))
        .mount(&server)
        .await;

    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.5").with_base_url(server.uri());
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
