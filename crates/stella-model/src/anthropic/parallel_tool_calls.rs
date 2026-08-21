// SPDX-License-Identifier: AGPL-3.0-only
//! The parallel-tool-call fan-in witness for the Messages dialect (#4163).
//!
//! Anthropic delivers each tool call as its own `tool_use` content block,
//! announced by a `content_block_start` and filled by `input_json_delta`
//! fragments keyed on the block `index`. Several of them ride one assistant
//! message, and this proves the adapter turns all of them into `ToolCall`s
//! rather than the first.
//!
//! `anthropic/tests.rs` already streams two `tool_use` blocks, but it asserts
//! on the *truncation error* that follows — an adapter that kept only the
//! first block would pass it unchanged. See `provider_parity::parallel` for
//! why fan-in is proven separately from admission on this axis, and why
//! anthropic's row is `Undetermined` on the admission half: the request-side
//! control here is an opt-*out* (`tool_choice.disable_parallel_tool_use`)
//! that this adapter never sends.
//!
//! Declared from `anthropic.rs` rather than from `anthropic/tests.rs`, which
//! is a grandfathered god file closed to growth.

use super::*;
use stella_protocol::CompletionRequest;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Three `tool_use` blocks in one message, their argument fragments
/// interleaved across events so nothing can pass by keying on arrival order
/// instead of the block `index`.
const THREE_INTERLEAVED_BLOCKS: &str = concat!(
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_a\",\"name\":\"read_file\"}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_b\",\"name\":\"search\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_c\",\"name\":\"bash\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\\\"fn main\\\"}\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.rs\\\"}\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\\\"ls\\\"}\"}}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":42}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

/// The fan-in witness named by the `anthropic` row of
/// `PARALLEL_TOOL_CALL_POSTURE`.
#[tokio::test]
async fn several_tool_use_blocks_fan_in_as_several_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(THREE_INTERLEAVED_BLOCKS, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(ApiKey::new("sk-test"), "claude-fable-5")
        .with_base_url(server.uri());
    let result = provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("read, search, and list")],
            max_output_tokens: None,
            temperature: None,
            effort: None,
            tools: vec![],
            reasoning: None,
            params: None,
        })
        .await
        .expect("three interleaved tool_use blocks parse");

    let names: Vec<&str> = result
        .tool_calls
        .iter()
        .map(|call| call.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["read_file", "search", "bash"],
        "every tool_use block must survive, in block-index order"
    );

    let ids: Vec<&str> = result
        .tool_calls
        .iter()
        .map(|call| call.call_id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["toolu_a", "toolu_b", "toolu_c"],
        "each block keeps its own tool_use id — the id is what the next turn's \
         tool_result correlates against, so a shared one answers the wrong call"
    );

    assert_eq!(result.tool_calls[0].input["path"], "a.rs");
    assert_eq!(result.tool_calls[1].input["query"], "fn main");
    assert_eq!(
        result.tool_calls[2].input["command"], "ls",
        "interleaved input_json_delta fragments reassemble against their own \
         block index, never the most recently started one"
    );
}
