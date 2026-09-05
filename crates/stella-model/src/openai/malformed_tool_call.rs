// SPDX-License-Identifier: AGPL-3.0-only
//! What an `output_item.added` frame does to a turn when its id is missing.
//!
//! Serde reads the `type` tag first. It then fails the whole event when the
//! matched case is missing a field. So an item that names `function_call`
//! and has no `call_id` does not parse, unless the field defaults. The frame
//! lands in the arm that skips an event type this adapter does not model.
//! The call goes with it. Each later argument delta then opens a fresh
//! accumulator keyed `""`. The turn ends `Ok` holding a call nothing can
//! match to its result.
//!
//! Nothing fails and the turn goes on. That is why the proof runs at the
//! provider edge, not on the type. The claim is about what a caller gets.
//!
//! Declared from `openai.rs`, not from `openai/tests.rs`. That file is close
//! to the size ratchet.

use super::*;
use crate::provider::Provider;
use stella_protocol::CompletionRequest;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// One `function_call` item carrying `name` and no `call_id`, its arguments
/// streamed and closed, ending on `response.completed`. Every frame after the
/// item is well formed — the whole loss comes from the one absent field.
const A_FUNCTION_CALL_WITH_NO_ID: &str = concat!(
    "event: response.output_item.added\n",
    "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"name\":\"bash\"}}\n\n",
    "event: response.function_call_arguments.delta\n",
    "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"command\\\":\\\"ls\\\"}\"}\n\n",
    "event: response.function_call_arguments.done\n",
    "data: {\"type\":\"response.function_call_arguments.done\",\"output_index\":0}\n\n",
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":9}}}\n\n",
);

/// Three frames this adapter does not model, around ordinary text. One is an
/// item type it has no case for. One is an event type it has no case for.
/// One is a data line with no `type`. All three must still be skipped.
const AN_UNMODELLED_OUTPUT_ITEM: &str = concat!(
    "event: response.output_item.added\n",
    "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"summary\":[]}}\n\n",
    "event: something_new\n",
    "data: {\"type\":\"response.brand_new_thing\"}\n\n",
    "event: message\n",
    "data: {\"unrecognised\":true}\n\n",
    "event: response.output_text.delta\n",
    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
);

async fn streaming_provider(body: &'static str) -> (MockServer, OpenAiProvider) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test-openai"), "gpt-5.5").with_base_url(server.uri());
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

/// The witness. Without the fix this body completes `Ok`. The item frame is
/// dropped. The argument delta opens an accumulator with an empty id. The
/// turn hands back a call no tool result can answer.
#[tokio::test]
async fn a_function_call_with_no_id_ends_the_turn_instead_of_vanishing() {
    let (_server, provider) = streaming_provider(A_FUNCTION_CALL_WITH_NO_ID).await;

    let error = provider
        .complete(request())
        .await
        .expect_err("a tool call that cannot be dispatched must not read as a tool call");

    let message = error.to_string();
    assert!(
        message.contains("index 0"),
        "the error names the item that was lost: {message}"
    );
    assert!(
        message.contains("`call_id`"),
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

/// The same item on the non-streaming path, which the session takes once its
/// streaming path has proven broken. Here the fields already defaulted, so
/// the item parsed and the turn committed a call keyed `""`. That call can
/// never be matched to its result. The two delivery paths have to refuse it
/// alike.
#[tokio::test]
async fn a_unary_function_call_with_no_id_ends_the_turn_as_well() {
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
            r#"{"id":"resp_1","status":"completed","output":[{"type":"function_call","name":"bash","arguments":"{\"command\":\"ls\"}"}],"usage":{"input_tokens":12,"output_tokens":9}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    let provider =
        OpenAiProvider::new(ApiKey::new("sk-test-openai"), "gpt-5.5").with_base_url(server.uri());

    // An empty stream arms the latch, so the retry goes out non-streaming.
    let armed = provider
        .complete(request())
        .await
        .expect_err("an empty stream is a fault, never an empty Ok");
    assert!(armed.is_retryable(), "{armed:?}");

    let error = provider
        .complete(request())
        .await
        .expect_err("a unary item with no id must not be committed either");
    let message = error.to_string();
    assert!(message.contains("`call_id`"), "{message}");
    assert!(message.contains("bash"), "{message}");
    assert!(!error.is_retryable(), "{error:?}");
}

/// The constraint on the fix: a frame whose `type` this adapter does not
/// model stays skippable. Only an item that declares itself `function_call`
/// and then cannot be dispatched is fatal. Widening the refusal to every
/// unparsed frame would break on the next item type OpenAI ships.
#[tokio::test]
async fn an_output_item_type_this_adapter_does_not_model_is_still_skipped() {
    let (_server, provider) = streaming_provider(AN_UNMODELLED_OUTPUT_ITEM).await;

    let result = provider
        .complete(request())
        .await
        .expect("an unmodelled item type is not a fault");
    assert_eq!(result.text, "done");
    assert!(result.tool_calls.is_empty());
}
