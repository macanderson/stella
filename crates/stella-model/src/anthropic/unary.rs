// SPDX-License-Identifier: AGPL-3.0-only
//! The non-streaming delivery path of the Messages adapter — the fallback a
//! completion takes once the session's streaming path has proven broken
//! (hung before its first byte, or a 200 with an empty stream; #2686,
//! #2746, `crate::stream_recovery`).
//!
//! Same endpoint, same payload, `stream: false`: the response is one JSON
//! `message` object instead of an SSE body, dispatched through the
//! [`crate::http::unary_client`] read bound (the whole generation must fit
//! inside a single read — #547) with #547's classification to match: a unary
//! read-timeout consumed the whole bound and re-issuing the identical
//! request just waits it out again, so it surfaces as non-retryable
//! `Terminal`, never as the retryable `Transport` that turned one wedged
//! Bedrock call into four full 600s attempts. Assembly reuses
//! [`super::stream`]'s shared rules verbatim, so the two paths cannot
//! disagree on how a usage frame folds or what a token-limit-truncated tool
//! call costs. The price of this path is the loss of mid-stream observation:
//! no text/reasoning previews and no speculative tool execution — which is
//! why the latch only confirms on evidence and never arms on an ordinary
//! transport fault.

use serde::Deserialize;
use serde_json::Value;
use stella_protocol::{
    CompletionRequestRef, CompletionResult, CompletionUsage, ProviderError, ToolCall,
};

use super::{
    AnthropicProvider, AnthropicStreamError, AnthropicUsage, classify_anthropic_stream_error,
    map_stop_reason, stream,
};

/// One non-streamed Messages response. Every field defaults for the same
/// reason the stream-event tree's do: an unknown or partial object must
/// degrade to an explicit error below, never fail deserialization into a
/// silently empty turn.
#[derive(Deserialize, Debug, Default)]
struct AnthropicUnaryResponse {
    #[serde(default)]
    content: Vec<AnthropicUnaryBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
    /// Defensive: the Messages API reports an error inside a 200 body as
    /// `{"type":"error","error":{...}}` — the unary sibling of the in-band
    /// SSE error event. Classified by the same shared classifier so the two
    /// paths agree on retryability.
    #[serde(default)]
    error: Option<AnthropicStreamError>,
}

/// One assembled content block. Only the three kinds this adapter reads are
/// modeled; anything else (a future block type, a `thinking` block — which
/// this path cannot announce because there is no observer channel open on a
/// unary call) falls into `Other` and is ignored rather than failing the
/// turn.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicUnaryBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        /// Absent on a call the token limit cut before any arguments were
        /// emitted — the unary spelling of a `tool_use` block with no
        /// `input_json_delta` behind it.
        #[serde(default)]
        input: Option<Value>,
    },
    #[serde(other)]
    Other,
}

impl AnthropicProvider {
    /// One unary request/parse cycle — [`super::AnthropicProvider::complete_inner`]'s
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
                .map_err(|e| crate::http::classify_unary_dispatch_error("Anthropic", &e))?;
            self.assemble_unary(&payload)
        }
        .await;
        self.recovery.note_unary_outcome(outcome.is_ok());
        outcome
    }

    /// Parse one Messages body into the adapter's result, under exactly the
    /// shared assembly rules the streaming path uses.
    fn assemble_unary(&self, payload: &str) -> Result<CompletionResult, ProviderError> {
        // Same fail-loudly contract as the stream-event parse: every field
        // defaults, so an error here is a real type mismatch on a field that
        // matters — swallowing it would report a turn that did nothing as a
        // clean completion.
        let parsed: AnthropicUnaryResponse = serde_json::from_str(payload).map_err(|e| {
            ProviderError::Malformed(format!(
                "Anthropic: unparseable non-streaming message body ({e}); refusing \
                 to treat a dropped response as an empty turn"
            ))
        })?;
        if let Some(err) = &parsed.error {
            return Err(classify_anthropic_stream_error(err));
        }
        if parsed.content.is_empty() && parsed.stop_reason.is_none() {
            // A 200 carrying neither content nor a reason to have stopped is
            // the unary spelling of the empty stream. There is no third
            // transport to fall back to, so it is an ordinary retryable fault.
            return Err(ProviderError::transport(
                "Anthropic returned a message with no content and no stop_reason",
            ));
        }

        // The one content block the token limit could have cut: the last one,
        // and only when the message actually stopped at `max_tokens` — the
        // same blame-pinning rule as the stream's highest-index block.
        let truncated_index = (parsed.stop_reason.as_deref() == Some("max_tokens"))
            .then(|| parsed.content.len().checked_sub(1))
            .flatten();

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        for (index, block) in parsed.content.into_iter().enumerate() {
            match block {
                AnthropicUnaryBlock::Text { text: chunk } => text.push_str(&chunk),
                AnthropicUnaryBlock::ToolUse { id, name, input } => {
                    let truncated = Some(index) == truncated_index;
                    // A tool call arrives with its arguments already parsed,
                    // so the streaming path's repair sentinel has no unary
                    // counterpart — the one shape that survives is the call
                    // the limit cut before any arguments existed, which
                    // executing with `{}` would fail on missing parameters
                    // and re-enter the same unwinnable retry-retruncate loop.
                    let input = match input {
                        Some(Value::Null) | None if truncated => {
                            return Err(stream::truncated_tool_input(&name, ""));
                        }
                        // A no-argument tool call reports no input at all:
                        // that is an empty object, never null.
                        Some(Value::Null) | None => Value::Object(serde_json::Map::new()),
                        Some(value) => value,
                    };
                    tool_calls.push(ToolCall {
                        call_id: id,
                        name,
                        input,
                    });
                }
                AnthropicUnaryBlock::Other => {}
            }
        }

        let mut usage = CompletionUsage::default();
        if let Some(frame) = &parsed.usage {
            stream::fold_usage(frame, &mut usage);
            usage.output_tokens = frame.output_tokens;
            usage.reported = true;
        }
        let cost_usd = self.pricing.map(|p| p.cost_usd(&usage)).unwrap_or(0.0);
        Ok(CompletionResult {
            text,
            tool_calls,
            usage,
            model: self.model.clone(),
            cost_usd,
            finish_reason: map_stop_reason(parsed.stop_reason.as_deref()),
            // A direct endpoint: the provider id is already the whole answer
            // to "who served this?", so there is no upstream to name.
            upstream_provider: None,
        })
    }
}
