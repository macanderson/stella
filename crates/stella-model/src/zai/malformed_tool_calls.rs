// SPDX-License-Identifier: AGPL-3.0-only
//! Two ways a tool call is lost on this dialect while the turn still ends
//! `Ok`.
//!
//! Both come from a server that leaves out a field the wire normally
//! carries. So both reach us through the same gateways as the `tool_use`
//! block the sibling `anthropic` module covers.
//!
//! Declared from `zai.rs`, not from `zai/tests.rs`. That file is a god file
//! sitting on its ceiling.

use super::*;
use stella_protocol::CompletionRequest;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Two whole tool calls, one per frame, neither with an `index`. This is what
/// a server that never sends `index` puts on the wire.
const TWO_UNINDEXED_CALLS_ONE_PER_FRAME: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_b\",\"function\":{\"name\":\"search\",\"arguments\":\"{\\\"query\\\":\\\"fn main\\\"}\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// One tool call whose `id` never arrives, streamed and finished cleanly.
const A_CALL_WITH_NO_ID: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n",
);

async fn streaming_provider(body: &'static str) -> (MockServer, ZaiProvider) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());
    (server, provider)
}

fn request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![CompletionMessage::user("read a.rs and search for fn main")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    }
}

/// The witness for the index fallback. A place inside one chunk starts again
/// at 0 on every frame. That files both calls under 0 and merges them. The
/// turn then holds one call named `read_filesearch`, with the two argument
/// objects glued into JSON that will not parse. That falls back to the `Null`
/// repair value, not to an error. Both real calls are gone, and nothing says
/// so.
#[tokio::test]
async fn two_tool_calls_with_no_index_stay_two_calls() {
    let (_server, provider) = streaming_provider(TWO_UNINDEXED_CALLS_ONE_PER_FRAME).await;

    let result = provider
        .complete(request())
        .await
        .expect("two index-less calls parse");

    let names: Vec<&str> = result
        .tool_calls
        .iter()
        .map(|call| call.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["read_file", "search"],
        "each frame opens its own call: a shared accumulator concatenates the names"
    );

    let ids: Vec<&str> = result
        .tool_calls
        .iter()
        .map(|call| call.call_id.as_str())
        .collect();
    assert_eq!(ids, ["call_a", "call_b"]);

    assert_eq!(result.tool_calls[0].input["path"], "a.rs");
    assert_eq!(
        result.tool_calls[1].input["query"], "fn main",
        "the second call's arguments are its own, not appended to the first's"
    );
}

/// The witness for the empty `call_id`. `announce_completed_below` already
/// skips such a call. Final assembly has to skip it too. If it does not, a
/// turn can hold two calls both keyed `""`, and they pair up with the wrong
/// results in `stella-core`'s loop evidence.
#[tokio::test]
async fn a_tool_call_whose_id_never_arrives_ends_the_turn() {
    let (_server, provider) = streaming_provider(A_CALL_WITH_NO_ID).await;

    let error = provider
        .complete(request())
        .await
        .expect_err("a call with no id cannot be correlated and must not be committed");

    let message = error.to_string();
    assert!(
        message.contains("`id`"),
        "the error names the field that never arrived: {message}"
    );
    assert!(
        message.contains("bash"),
        "the error names the tool the model asked for: {message}"
    );
    assert!(!error.is_retryable(), "{error:?}");
}
