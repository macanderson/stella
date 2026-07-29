//! Stream frame-shape regressions: what the adapter does with a frame it
//! cannot fully parse. Split out of the parent `tests.rs` (file-size gate)
//! since they share one fixture shape — a mock SSE stream whose frames are
//! deliberately malformed.
//!
//! The through-line: a dropped frame is indistinguishable from a turn that
//! had nothing to say. The driver ends a turn on `tool_calls.is_empty()`, so
//! silently discarding a frame that carried a tool call makes the run report
//! a clean, successful completion having executed nothing.

use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a minimal single-tool request for the frame-shape tests below.
fn tool_req(prompt: &str) -> CompletionRequest {
    CompletionRequest {
        messages: vec![CompletionMessage::user(prompt)],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![ToolSchema {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type":"object"}),
            read_only: false,
        }],
        reasoning: None,
        params: None,
    }
}

async fn provider_streaming(body: &'static str) -> (MockServer, ZaiProvider) {
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

/// A tool-call fragment with no `index` must still produce the call.
///
/// `index` was the one non-defaulted field in the stream tree, so a frame
/// omitting it failed to deserialize and was dropped whole — taking the tool
/// call with it. The driver ends a turn on `tool_calls.is_empty()`, so the run
/// would have reported a clean completion having executed nothing.
#[tokio::test]
async fn a_tool_call_fragment_without_an_index_is_not_dropped() {
    let (_server, provider) = provider_streaming(concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"src/lib.rs\\\"}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    ))
    .await;

    let result = provider
        .complete(tool_req("read src/lib.rs"))
        .await
        .expect("completion should succeed");

    assert_eq!(
        result.tool_calls.len(),
        1,
        "a fragment without `index` must fall back to its position, not vanish"
    );
    assert_eq!(result.tool_calls[0].name, "read_file");
    assert_eq!(
        result.tool_calls[0].input,
        serde_json::json!({"path": "src/lib.rs"})
    );
}

/// An explicit `"tool_calls": null` must not fail the frame. `#[serde(default)]`
/// covers a MISSING field only; a null landed in the error arm and dropped the
/// whole chunk, including any text it carried.
#[tokio::test]
async fn an_explicit_null_tool_calls_does_not_drop_the_frame() {
    let (_server, provider) = provider_streaming(concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hello\",\"tool_calls\":null}}]}\n\n",
        "data: [DONE]\n\n",
    ))
    .await;

    let result = provider
        .complete(tool_req("say hello"))
        .await
        .expect("a null tool_calls is tolerated");
    assert_eq!(result.text, "hello");
    assert!(result.tool_calls.is_empty());
}

/// Non-JSON frames stay ignorable — the constraint on the fix. Keep-alives and
/// gateway comments must not become hard errors.
#[tokio::test]
async fn non_json_keep_alive_frames_are_still_ignored() {
    let (_server, provider) = provider_streaming(concat!(
        "data: OPENROUTER PROCESSING\n\n",
        "data: ping\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: [DONE]\n\n",
    ))
    .await;

    let result = provider
        .complete(tool_req("hi"))
        .await
        .expect("keep-alive frames are not errors");
    assert_eq!(result.text, "hi");
}

/// An empty or unknown JSON object still parses (every field defaults), so it
/// must not trip the new loud arm either.
#[tokio::test]
async fn unknown_json_frames_still_parse_as_empty_chunks() {
    let (_server, provider) = provider_streaming(concat!(
        "data: {}\n\n",
        "data: {\"ping\":true}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        "data: [DONE]\n\n",
    ))
    .await;

    let result = provider
        .complete(tool_req("ok"))
        .await
        .expect("unknown-but-valid JSON frames are not errors");
    assert_eq!(result.text, "ok");
}

/// A valid-JSON frame whose shape genuinely does not fit must FAIL, not be
/// swallowed. Silently dropping it yields a turn with no tool calls, which the
/// driver reads as a successful completion — the run reports done having done
/// nothing. A typed error is strictly better than a confident empty answer.
#[tokio::test]
async fn a_type_mismatched_frame_errors_instead_of_reporting_an_empty_turn() {
    let (_server, provider) = provider_streaming(concat!(
        // `choices` must be a list; a string is a real dialect deviation.
        "data: {\"choices\":\"not-a-list\"}\n\n",
        "data: [DONE]\n\n",
    ))
    .await;

    let err = provider
        .complete(tool_req("read src/lib.rs"))
        .await
        .expect_err("a malformed frame must not pass as an empty success");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("unparseable stream frame"),
        "the error must name the dropped frame: {rendered}"
    );
}

/// The observer twin of
/// `super::complete_falls_back_to_null_when_streamed_tool_arguments_never_parse`:
/// a call whose accumulated arguments never parse must reach the
/// end-of-stream repair path (the `Value::Null` sentinel) WITHOUT ever being
/// announced for speculative execution — the same "never speculate on
/// unparseable input" contract
/// `anthropic::tests::complete_observed_never_announces_a_block_whose_json_is_broken`
/// asserts for the sibling dialect. Nothing previously pinned this for the
/// zai/GLM stream shape, even though `announce_completed_below` has the same
/// `.ok()` short-circuit anthropic.rs guards with a dedicated witness.
#[tokio::test]
async fn complete_observed_never_announces_a_call_whose_json_never_parses() {
    // The stream ends with no later index and no `finish_reason:
    // "tool_calls"` chunk, so the end-of-stream fallback is what owns this
    // call — the same boundary the anthropic sibling test exercises via
    // `content_block_stop`.
    let (_server, provider) = provider_streaming(concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{not valid json\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    ))
    .await;

    let observer = super::RecordingObserver::new();
    let result = provider
        .complete_observed(tool_req("read src/lib.rs"), &observer)
        .await
        .expect("broken JSON on an un-truncated call is the repair sentinel, not an error");
    assert!(
        observer.calls.lock().unwrap().is_empty(),
        "unparseable input must never be announced to the observer"
    );
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].input, Value::Null);
}
