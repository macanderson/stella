//! The [`stella_protocol::Provider`] port impl for [`OpenAiProvider`] — the whole of this
//! adapter's public surface, and nothing else.
//!
//! Split out of `openai.rs` rather than grown inside it: that file is a
//! grandfathered god file closed to growth (AGENTS.md § God files), and a
//! trait impl cannot be spread across modules, so the block that must absorb
//! every future port method is precisely the block that had to leave. It is a
//! pure delegation seam — `id`/`model` answer from fields, and both completion
//! methods forward to `OpenAiProvider::complete_inner` — so nothing that
//! reasons about SSE, pricing, or the wire dialect moved with it.

use async_trait::async_trait;
use stella_protocol::{CompletionRequestRef, CompletionResult, ProviderError};

use crate::provider::{Provider, ToolCallObserver};

use super::OpenAiProvider;

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn model(&self) -> Option<&str> {
        Some(&self.model)
    }

    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.complete_inner(req, None).await
    }

    async fn complete_observed_ref(
        &self,
        req: CompletionRequestRef<'_>,
        observer: &dyn ToolCallObserver,
    ) -> Result<CompletionResult, ProviderError> {
        self.complete_inner(req, Some(observer)).await
    }
}

/// The parallel-tool-call fan-in witness for the Responses dialect (#4163).
///
/// It lives here for the same reason this file exists at all: `openai.rs` is
/// a grandfathered god file closed to growth, so its inline `mod tests` can
/// take no more lines. This is the sibling that owns the port the test
/// dispatches through, which makes it the right home rather than merely the
/// available one.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::ApiKey;
    use stella_protocol::{CompletionMessage, CompletionRequest, FinishReason};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The fan-in witness named by the `openai` row of
    /// `PARALLEL_TOOL_CALL_POSTURE`.
    ///
    /// The Responses dialect gives each call its own `function_call` item
    /// keyed by `output_index`, with arguments streamed as fragments against
    /// that index. Three calls arrive with their fragments interleaved, so a
    /// fan-in keyed on arrival order rather than `output_index` corrupts them
    /// rather than merely reordering them.
    #[tokio::test]
    async fn several_function_call_items_fan_in_as_several_calls() {
        let server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"fc_a\",\"name\":\"read_file\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"call_id\":\"fc_b\",\"name\":\"read_file\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"path\\\":\"}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":2,\"item\":{\"type\":\"function_call\",\"call_id\":\"fc_c\",\"name\":\"bash\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{\\\"path\\\":\\\"b.rs\\\"}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"\\\"a.rs\\\"}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":2,\"delta\":\"{\\\"command\\\":\\\"ls\\\"}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":20,\"output_tokens\":12}}}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
            .mount(&server)
            .await;

        let provider =
            OpenAiProvider::new(ApiKey::new("sk-test"), "gpt-5.2").with_base_url(server.uri());
        let result = provider
            .complete(CompletionRequest {
                messages: vec![CompletionMessage::user("read both files and list")],
                max_output_tokens: None,
                temperature: None,
                effort: None,
                tools: vec![],
                reasoning: None,
                params: None,
            })
            .await
            .expect("three interleaved function_call items parse");

        let names: Vec<&str> = result
            .tool_calls
            .iter()
            .map(|call| call.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["read_file", "read_file", "bash"],
            "every function_call item becomes a call, in output_index order — \
             including two calls to one tool, the shape a parallel read batch takes"
        );

        let ids: Vec<&str> = result
            .tool_calls
            .iter()
            .map(|call| call.call_id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["fc_a", "fc_b", "fc_c"],
            "each item keeps its own call_id — the id the next turn's \
             function_call_output correlates against"
        );

        assert_eq!(result.tool_calls[0].input["path"], "a.rs");
        assert_eq!(
            result.tool_calls[1].input["path"], "b.rs",
            "interleaved argument fragments reassemble against their own \
             output_index, never the most recently added item"
        );
        assert_eq!(result.tool_calls[2].input["command"], "ls");
        assert!(
            matches!(result.finish_reason, Some(FinishReason::ToolCalls)),
            "{:?}",
            result.finish_reason
        );
    }
}
