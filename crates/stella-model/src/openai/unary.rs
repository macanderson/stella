// SPDX-License-Identifier: AGPL-3.0-only
//! The non-streaming delivery path of the Responses adapter — the fallback a
//! completion takes once the session's streaming path has proven broken
//! (hung before its first byte, or a 200 with an empty stream; #2686, #2746,
//! `crate::stream_recovery`).
//!
//! Same endpoint, same payload, `stream: false`: the response is one JSON
//! `response` object instead of an SSE body, dispatched through the
//! [`crate::http::unary_client`] read bound (the whole generation must fit
//! inside a single read — #547) with #547's classification to match: a unary
//! read-timeout consumed the whole bound and re-issuing the identical request
//! just waits it out again, so it surfaces as non-retryable `Terminal`, never
//! as the retryable `Transport` that turned one wedged Bedrock call into four
//! full 600s attempts. Assembly reuses [`super::stream`]'s shared rules
//! verbatim, so the two paths cannot disagree on usage folding, argument-JSON
//! repair, or the finish reason. The price of this path is the loss of
//! mid-stream observation: no text previews and no speculative tool
//! execution — which is why the latch only confirms on evidence and never
//! arms on an ordinary transport fault.

use serde::Deserialize;
use stella_protocol::{
    CompletionRequestRef, CompletionResult, CompletionUsage, ProviderError, ToolCall,
};

use super::{
    OpenAiIncompleteDetails, OpenAiProvider, OpenAiResponseError, OpenAiUsage,
    classify_openai_stream_error, stream,
};

/// One non-streamed `response` object. Every field defaults for the same
/// reason the stream-event tree's do: an unknown or partial object must
/// degrade to an explicit error below, never fail deserialization into a
/// silently empty turn.
#[derive(Deserialize, Debug, Default)]
struct OpenAiUnaryResponse {
    /// `completed` / `incomplete` / `failed` — the unary spelling of the
    /// three terminal stream events. Absent is treated as the empty-stream
    /// case below rather than assumed complete.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output: Vec<OpenAiUnaryItem>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
    /// Present on `status: "failed"`.
    #[serde(default)]
    error: Option<OpenAiResponseError>,
    /// Present on `status: "incomplete"`.
    #[serde(default)]
    incomplete_details: Option<OpenAiIncompleteDetails>,
}

/// One assembled output item. Only the two kinds this adapter reads are
/// modeled; anything else (`reasoning` summaries, and whatever the API adds
/// later) falls into `Other` and is ignored rather than failing the turn —
/// the same posture the stream's `#[serde(other)]` events take.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiUnaryItem {
    Message {
        #[serde(default)]
        content: Vec<OpenAiUnaryContent>,
    },
    FunctionCall {
        #[serde(default)]
        call_id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        arguments: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiUnaryContent {
    OutputText {
        #[serde(default)]
        text: String,
    },
    #[serde(other)]
    Other,
}

impl OpenAiProvider {
    /// One unary request/parse cycle — [`OpenAiProvider::complete_inner`]'s
    /// other arm. Reports its outcome to the recovery latch: success while
    /// probing confirms the fallback for the session, failure reverts to
    /// streaming (see `crate::stream_recovery` for why neither is skipped).
    pub(super) async fn complete_unary_attempt(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let body = self.build_body(req, false);
        let outcome = async {
            let response = self.dispatch(&self.unary_client, &body, true).await?;
            // The body arrives inside the same 600s read bound as the head,
            // so a timeout here is #547 too — classify it terminal rather than
            // retryable, matching the dispatch send path.
            let payload = response
                .text()
                .await
                .map_err(|e| crate::http::classify_unary_dispatch_error("OpenAI", &e))?;
            self.assemble_unary(&payload)
        }
        .await;
        self.recovery.note_unary_outcome(outcome.is_ok());
        outcome
    }

    /// Parse one `response` body into the adapter's result, under exactly the
    /// shared assembly rules the streaming path uses.
    fn assemble_unary(&self, payload: &str) -> Result<CompletionResult, ProviderError> {
        // Same fail-loudly contract as the stream-event parse: every field
        // defaults, so an error here is a real type mismatch on a field that
        // matters — swallowing it would report a turn that did nothing as a
        // clean completion.
        let parsed: OpenAiUnaryResponse = serde_json::from_str(payload).map_err(|e| {
            ProviderError::Malformed(format!(
                "OpenAI: unparseable non-streaming response body ({e}); refusing to \
                 treat a dropped response as an empty turn"
            ))
        })?;

        let mut usage = CompletionUsage::default();
        if let Some(frame) = parsed.usage {
            usage.reported = true;
            stream::fold_usage(frame, &mut usage);
        }

        // The three terminal statuses answer to the three terminal stream
        // events, and must classify identically — a cap-hit turn is
        // `FinishReason::Length`, every other incompletion is terminal, and a
        // failure carries the provider's own code through the shared
        // classifier.
        let mut truncated_at_limit = false;
        match parsed.status.as_deref() {
            Some("failed") => {
                // Bill what the failed attempt still cost: the usage envelope
                // is already folded above, so `attach_partial` carries it out
                // on the error rather than reporting the attempt as free.
                let (code, message) = parsed
                    .error
                    .map(|e| (e.code, e.message.unwrap_or_default()))
                    .unwrap_or((None, String::new()));
                let error = classify_openai_stream_error(code.as_deref(), &message);
                return Err(crate::http::attach_partial(
                    error,
                    &usage,
                    "",
                    self.pricing.as_ref(),
                ));
            }
            Some("incomplete") => {
                let reason = parsed
                    .incomplete_details
                    .and_then(|d| d.reason)
                    .unwrap_or_else(|| "unspecified".to_string());
                if reason == "max_output_tokens" {
                    truncated_at_limit = true;
                } else {
                    return Err(ProviderError::Terminal(format!(
                        "OpenAI response incomplete: {reason}"
                    )));
                }
            }
            Some("completed") => {}
            // A 200 with no terminal status is the unary spelling of the
            // empty stream. There is no third transport to fall back to, so
            // it is an ordinary retryable fault.
            _ => {
                return Err(ProviderError::transport(
                    "OpenAI returned a response with no terminal status",
                ));
            }
        }

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for item in parsed.output {
            match item {
                OpenAiUnaryItem::Message { content } => {
                    for part in content {
                        if let OpenAiUnaryContent::OutputText { text: chunk } = part {
                            text.push_str(&chunk);
                        }
                    }
                }
                OpenAiUnaryItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                } => tool_calls.push(ToolCall {
                    call_id,
                    name,
                    input: stream::tool_call_input(&arguments),
                }),
                OpenAiUnaryItem::Other => {}
            }
        }

        let cost_usd = self.pricing.map(|p| p.cost_usd(&usage)).unwrap_or(0.0);
        let finish_reason = stream::final_finish_reason(truncated_at_limit, !tool_calls.is_empty());
        Ok(CompletionResult {
            text,
            tool_calls,
            usage,
            model: self.model.clone(),
            cost_usd,
            finish_reason: Some(finish_reason),
            upstream_provider: None,
        })
    }
}
