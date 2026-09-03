// SPDX-License-Identifier: AGPL-3.0-only
//! What a `tool_use` start frame does to a turn when a field is missing.
//!
//! Serde reads the `type` tag first. It then fails the whole event when the
//! matched case is missing a field. So a `content_block_start` that names
//! `tool_use` and has no `id` does not parse at all, unless the fields
//! default. The frame lands in the arm that skips an event type this adapter
//! does not know. The block goes with it. Its `input_json_delta` parts miss
//! the map. The turn ends `Ok` with no tool calls, next to a finish reason
//! of `ToolCalls`.
//!
//! Nothing fails and the turn goes on. That is why the proof runs at the
//! provider edge, not on the type. The claim is about what a caller gets.
//!
//! Declared from `anthropic.rs`, not from `anthropic/tests.rs`. That file is
//! a god file closed to growth.

use super::*;
use stella_protocol::CompletionRequest;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A `tool_use` block carrying `name` and no `id`, its arguments streamed and
/// closed, ending on a `stop_reason` of `tool_use`. Every frame after the
/// start is well formed — the whole loss comes from the one absent field.
const TOOL_USE_WITHOUT_AN_ID: &str = concat!(
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"name\":\"bash\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\\\"ls\\\"}\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":10}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

/// Three frames this adapter does not model — a block type it has no case
/// for, an event type it has no case for, and a data line with no `type` at
/// all — around an ordinary text block. All three must still be skipped.
const AN_UNMODELLED_BLOCK_TYPE: &str = concat!(
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"server_tool_use\",\"id\":\"srvtoolu_1\",\"name\":\"web_search\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: ping\n",
    "data: {\"type\":\"ping\"}\n\n",
    "event: something_new\n",
    "data: {\"unrecognised\":true}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

async fn streaming_provider(body: &'static str) -> (MockServer, AnthropicProvider) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let provider = AnthropicProvider::new(ApiKey::new("sk-ant-test"), "claude-fable-5")
        .with_base_url(server.uri());
    (server, provider)
}

fn request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![CompletionMessage::user("list the files")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
}

/// The witness. Without the fix this body completes `Ok`, so the `expect_err`
/// is what fails: `tool_calls` comes back empty beside a `finish_reason` of
/// `ToolCalls`, and a caller reading that result learns the model asked for
/// nothing.
#[tokio::test]
async fn a_tool_use_block_with_no_id_ends_the_turn_instead_of_vanishing() {
    let (_server, provider) = streaming_provider(TOOL_USE_WITHOUT_AN_ID).await;

    let error = provider
        .complete(request())
        .await
        .expect_err("a tool call that cannot be dispatched must not read as no tool call");

    let message = error.to_string();
    assert!(
        message.contains("index 0"),
        "the error names the block that was lost: {message}"
    );
    assert!(
        message.contains("`id`"),
        "the error names the field that never arrived: {message}"
    );
    assert!(
        message.contains("bash"),
        "the error names the tool the model asked for: {message}"
    );
    assert!(
        !error.is_retryable(),
        "the same request re-streams the same absent field: {error:?}"
    );
}

/// The same block on the non-streaming path, which the session takes once its
/// streaming path has proven broken. Here the fields already defaulted, so the
/// block parsed and the turn committed a call keyed `""`. That call can never
/// be matched to its result. The two delivery paths have to refuse it alike.
#[tokio::test]
async fn a_unary_tool_use_block_with_no_id_ends_the_turn_as_well() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_string_contains("\"stream\":true"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_string_contains("\"stream\":false"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"id":"msg_2","type":"message","role":"assistant","content":[{"type":"tool_use","name":"bash","input":{"command":"ls"}}],"stop_reason":"tool_use","usage":{"input_tokens":12,"output_tokens":9}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    let provider = AnthropicProvider::new(ApiKey::new("sk-ant-test"), "claude-fable-5")
        .with_base_url(server.uri());

    // An empty stream arms the latch, so the retry goes out non-streaming.
    let armed = provider
        .complete(request())
        .await
        .expect_err("an empty stream is a fault, never an empty Ok");
    assert!(armed.is_retryable(), "{armed:?}");

    let error = provider
        .complete(request())
        .await
        .expect_err("a unary call with no id must not be committed either");
    let message = error.to_string();
    assert!(message.contains("`id`"), "{message}");
    assert!(message.contains("bash"), "{message}");
    assert!(!error.is_retryable(), "{error:?}");
}

/// The constraint on the fix: a frame whose `type` this adapter does not model
/// stays skippable. Only a block that declares itself `tool_use` and then
/// cannot be dispatched is fatal — widening the refusal to every unparsed frame
/// would break on the next block type Anthropic ships.
#[tokio::test]
async fn a_content_block_type_this_adapter_does_not_model_is_still_skipped() {
    let (_server, provider) = streaming_provider(AN_UNMODELLED_BLOCK_TYPE).await;

    let result = provider
        .complete(request())
        .await
        .expect("an unmodelled block type is not a fault");
    assert_eq!(result.text, "done");
    assert!(result.tool_calls.is_empty());
}
