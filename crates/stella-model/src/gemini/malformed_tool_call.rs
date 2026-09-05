// SPDX-License-Identifier: AGPL-3.0-only
//! What a `functionCall` part does to a turn when its name is missing.
//!
//! This wire carries no call id, so the adapter mints one from the call's
//! ordinal. The name is the one dispatch field a server can drop. A part that
//! leaves it out fails the whole chunk unless the field defaults, and the arm
//! that tolerates a keep-alive frame skips it. The call goes with the chunk,
//! and so does any `finishReason` beside it.
//!
//! The field now defaults, so the part parses and assembly refuses it by
//! name. The fold is shared with the unary path and with `vertex.rs`, so one
//! refusal covers all three.
//!
//! Declared from `gemini.rs`, so the suite in `gemini/tests.rs` keeps its own
//! subject.

use super::*;
use stella_protocol::CompletionRequest;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// One `functionCall` part with `args` and no `name`, then a clean stop.
const A_CALL_WITH_NO_NAME: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"args\":{\"path\":\"a.rs\"}}}]}}]}\n\n",
    "data: {\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"parts\":[]}}]}\n\n",
);

/// The same shape with the name present, which must still complete.
const A_WHOLE_CALL: &str = concat!(
    "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"read_file\",\"args\":{\"path\":\"a.rs\"}}}]}}]}\n\n",
    "data: {\"candidates\":[{\"finishReason\":\"STOP\",\"content\":{\"parts\":[]}}]}\n\n",
);

async fn streaming_provider(body: &'static str) -> (MockServer, GeminiProvider) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let provider =
        GeminiProvider::new(ApiKey::new("k"), "gemini-3-pro").with_base_url(server.uri());
    (server, provider)
}

fn request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![CompletionMessage::user("read a.rs")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
}

/// The witness. Without the fix the first chunk is dropped, the second sets a
/// stop reason, and the turn ends `Ok` with no text and no tool calls. A
/// caller reading that learns the model asked for nothing.
#[tokio::test]
async fn a_function_call_with_no_name_ends_the_turn_instead_of_vanishing() {
    let (_server, provider) = streaming_provider(A_CALL_WITH_NO_NAME).await;

    let error = provider
        .complete(request())
        .await
        .expect_err("a call that cannot be dispatched must not read as no call");

    let message = error.to_string();
    assert!(
        message.contains("index 0"),
        "the error names the call that was lost: {message}"
    );
    assert!(
        message.contains("`name`"),
        "the error names the field that never arrived: {message}"
    );
    assert!(
        !error.is_retryable(),
        "the same request re-streams the same absent field: {error:?}"
    );
}

/// The constraint on the fix: a whole call still rides through.
#[tokio::test]
async fn a_function_call_with_a_name_still_completes() {
    let (_server, provider) = streaming_provider(A_WHOLE_CALL).await;

    let result = provider.complete(request()).await.expect("a whole call");
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].name, "read_file");
    assert_eq!(result.tool_calls[0].call_id, "call_0");
    assert_eq!(result.tool_calls[0].input["path"], "a.rs");
}
