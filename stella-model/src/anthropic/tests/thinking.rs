//! Extended-thinking deltas reach the observer on the reasoning channel.
//! Split from the parent `tests.rs` (file-size gate).

use super::*;

/// `thinking_delta` blocks used to deserialize as `Other` and vanish, so a
/// thinking model's deliberation was invisible in the transcript. They now
/// announce on the reasoning channel — separate from the answer, which is
/// what lets the deck render them collapsed and dimmed rather than as a
/// reply.
#[tokio::test]
async fn thinking_deltas_reach_the_reasoning_channel_not_the_answer() {
    #[derive(Default)]
    struct SplitObserver {
        text: std::sync::Mutex<Vec<String>>,
        thinking: std::sync::Mutex<Vec<String>>,
    }
    impl ToolCallObserver for SplitObserver {
        fn tool_call_streamed(&self, _call: &stella_protocol::ToolCall) {}
        fn text_delta(&self, delta: &str) {
            self.text.lock().unwrap().push(delta.to_string());
        }
        fn reasoning_delta(&self, delta: &str) {
            self.thinking.lock().unwrap().push(delta.to_string());
        }
    }

    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"weighing \"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"the options\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"The answer.\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new(ApiKey::new("sk-test"), "claude-fable-5")
        .with_base_url(server.uri());
    let req = CompletionRequest {
        messages: vec![CompletionMessage::user("hi")],
        max_output_tokens: None,
        temperature: None,
        effort: None,
        tools: vec![],
        reasoning: None,
        params: None,
    };
    let observer = SplitObserver::default();
    let result = provider
        .complete_observed(req, &observer)
        .await
        .expect("should succeed");

    assert_eq!(
        *observer.thinking.lock().unwrap(),
        vec!["weighing ", "the options"],
        "thinking streams in order on its own channel"
    );
    assert_eq!(
        *observer.text.lock().unwrap(),
        vec!["The answer."],
        "the answer channel never carries thinking"
    );
    assert_eq!(
        result.text, "The answer.",
        "thinking is never folded into the reply"
    );
}
