// SPDX-License-Identifier: AGPL-3.0-only
//! The parallel-tool-call fan-in witness for the shared chat-completions
//! dialect (#4163) — the half of `ParallelToolCallPosture` that is a fact
//! about code in this tree rather than about a vendor's live API.
//!
//! Five provider ids ride this one adapter (`zai`, `openrouter`, `xai`,
//! `deepseek`, `local`), so this file is the fan-in proof for all five and
//! all five name the test below. Two of them — `zai` and `openrouter` — are
//! also the only ids on the axis whose *admission* is settled by observation;
//! see `provider_parity::parallel`'s module doc for that census.
//!
//! What is proven here is specifically what a `Vec<ToolCall>` of length one
//! would silently break: an assistant message carrying several tool calls
//! must arrive as several calls, each with its own id, name, and arguments,
//! in the order the stream announced them. Existing suites already stream two
//! calls, but they assert on a *truncation error* rather than on the calls
//! surviving — a fan-in that kept only the first call would pass every one of
//! them.
//!
//! Declared from `zai.rs` rather than from `zai/tests.rs`, which is a
//! grandfathered god file sitting exactly on its ceiling — the module
//! declaration is one line and would have failed the file-size ratchet, so
//! this rides beside `tests.rs` instead of inside it. `super` is the adapter
//! module either way.

use super::*;
use stella_protocol::CompletionRequest;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Two tool calls interleaved across SSE frames, exactly as the OpenAI-shaped
/// wire delivers them: `index` is the only thing tying a fragment to its
/// call, fragments for the two calls arrive out of order, and each call's
/// arguments are split across frames.
///
/// Interleaving is the point. Sequential-by-index is the easy case; a fan-in
/// that keyed on arrival order rather than `index` would pass a tidy stream
/// and corrupt this one, which is the shape a real provider sends.
const TWO_INTERLEAVED_CALLS: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"function\":{\"name\":\"search\",\"arguments\":\"{\\\"query\\\":\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.rs\\\"}\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"\\\"fn main\\\"}\"}}]}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// The fan-in witness named by every chat-completions row of
/// `PARALLEL_TOOL_CALL_POSTURE`.
#[tokio::test]
async fn several_tool_calls_in_one_message_fan_in_as_several_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(TWO_INTERLEAVED_CALLS, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let provider =
        ZaiProvider::new(ApiKey::new("sk-test-zai"), "glm-5.2").with_base_url(server.uri());
    let response = provider
        .complete(CompletionRequest {
            messages: vec![CompletionMessage::user("read a.rs and search for fn main")],
            max_output_tokens: None,
            temperature: None,
            effort: None,
            tools: vec![],
            reasoning: None,
            params: None,
        })
        .await
        .expect("two interleaved tool calls parse");

    let names: Vec<&str> = response
        .tool_calls
        .iter()
        .map(|call| call.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["read_file", "search"],
        "both calls must survive, in index order — a fan-in that kept only the \
         first would strand every parallel batch the prompt asks the model for"
    );

    let ids: Vec<&str> = response
        .tool_calls
        .iter()
        .map(|call| call.call_id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["call_a", "call_b"],
        "each call keeps its own id: the ids are what correlate results back, \
         so two calls sharing one would silently answer the wrong question"
    );

    assert_eq!(
        response.tool_calls[0].input["path"], "a.rs",
        "call 0's split arguments reassemble against its own index"
    );
    assert_eq!(
        response.tool_calls[1].input["query"], "fn main",
        "call 1's arguments are not cross-contaminated by the interleaved frames"
    );
    assert!(
        matches!(response.finish_reason, Some(FinishReason::ToolCalls)),
        "a multi-call message still finishes as ToolCalls: {:?}",
        response.finish_reason
    );
}
