//! The non-streaming delivery path of the shared OpenAI-compatible adapter —
//! the fallback a completion takes once the session's streaming path has
//! proven broken (hung before its first byte, or a 200 with an empty
//! stream; #2686, `crate::stream_recovery`).
//!
//! Same endpoint, same payload, `stream: false`: the response is one JSON
//! `chat.completion` object instead of an SSE body, dispatched through the
//! [`crate::http::unary_client`] read bound (the whole generation must fit
//! inside a single read — #547) with #547's classification to match: a
//! unary read-timeout consumed the whole bound and re-issuing the identical
//! request just waits it out again, so it surfaces as non-retryable
//! `Terminal`, never as the retryable `Transport` that turned one wedged
//! Bedrock call into four full 600s attempts. Assembly reuses [`super::stream`]'s shared
//! rules verbatim, so the two paths cannot disagree on argument-JSON repair,
//! the reasoning-only promotion, usage folding, or the finish reason. The
//! price of this path is the loss of mid-stream observation: no text/
//! reasoning previews and no speculative tool execution — which is why the
//! latch only confirms on evidence and never arms on an ordinary transport
//! fault.

use serde::Deserialize;
use serde_json::Value;
use stella_protocol::{
    CompletionRequestRef, CompletionResult, CompletionUsage, ProviderError, ToolCall,
};

use super::{ZaiProvider, ZaiUsage, classify_zai_stream_error, stream};

/// One non-streamed `chat.completion` response. Every field defaults for
/// the same reason the stream-chunk tree's do: an unknown or partial object
/// must degrade to an explicit error below, never fail deserialization into
/// a silently empty turn.
#[derive(Deserialize, Debug, Default)]
struct ZaiUnaryResponse {
    #[serde(default)]
    choices: Vec<ZaiUnaryChoice>,
    #[serde(default)]
    usage: Option<ZaiUsage>,
    /// The upstream OpenRouter routed to — see the sibling field on
    /// `ZaiStreamChunk`. The unary path carries it too, and must record it
    /// too: the stream→unary fallback (#2686) means a measured run can serve
    /// some of its calls through this path without anyone choosing that.
    #[serde(default)]
    provider: Option<String>,
    /// Defensive: a gateway that reports an error inside a 200 body (the
    /// unary sibling of the in-band SSE error frame). Classified by the same
    /// shared classifier so the two paths agree on retryability.
    #[serde(default)]
    error: Option<super::ZaiStreamError>,
}

#[derive(Deserialize, Debug, Default)]
struct ZaiUnaryChoice {
    #[serde(default)]
    message: ZaiUnaryMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// The assembled assistant message: the unary spelling of everything the
/// stream delivers as deltas, including both chain-of-thought field names
/// (GLM's `reasoning_content`, OpenRouter's normalized `reasoning`).
#[derive(Deserialize, Debug, Default)]
struct ZaiUnaryMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ZaiUnaryToolCall>>,
}

#[derive(Deserialize, Debug, Default)]
struct ZaiUnaryToolCall {
    #[serde(default)]
    id: String,
    #[serde(default)]
    function: ZaiUnaryFunction,
}

#[derive(Deserialize, Debug, Default)]
struct ZaiUnaryFunction {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: String,
}

impl ZaiProvider {
    /// One unary request/parse cycle — [`super::ZaiProvider::complete_attempt`]'s
    /// other arm. Reports its outcome to the recovery latch: success while
    /// probing confirms the fallback for the session, failure reverts to
    /// streaming (see `crate::stream_recovery` for why neither is skipped).
    pub(super) async fn complete_unary_attempt(
        &self,
        req: CompletionRequestRef<'_>,
        force_default_reasoning: bool,
    ) -> Result<CompletionResult, ProviderError> {
        let body = self.build_body(req, force_default_reasoning, false);
        let outcome = async {
            let response = self.dispatch(&self.unary_client, &body, true).await?;
            // The body arrives inside the same 600s read bound as the head,
            // so a timeout here is #547 too — classify it terminal rather than
            // retryable, matching the dispatch send path.
            let payload = response
                .text()
                .await
                .map_err(|e| crate::http::classify_unary_dispatch_error(&self.label, &e))?;
            self.assemble_unary(&payload)
        }
        .await;
        self.recovery.note_unary_outcome(outcome.is_ok());
        outcome
    }

    /// Parse one `chat.completion` body into the adapter's result, under
    /// exactly the shared assembly rules the streaming path uses.
    fn assemble_unary(&self, payload: &str) -> Result<CompletionResult, ProviderError> {
        let label = &self.label;
        // Same fail-loudly contract as the stream-frame parse: every field
        // defaults, so an error here is a real type mismatch on a field that
        // matters — swallowing it would report a turn that did nothing as a
        // clean completion.
        let parsed: ZaiUnaryResponse = serde_json::from_str(payload).map_err(|e| {
            ProviderError::Malformed(format!(
                "{label}: unparseable non-streaming completion body ({e}); refusing \
                 to treat a dropped response as an empty turn"
            ))
        })?;
        if let Some(err) = &parsed.error {
            return Err(classify_zai_stream_error(err, label));
        }
        let Some(choice) = parsed.choices.into_iter().next() else {
            // A 200 with no choices is the unary spelling of the empty
            // stream. There is no third transport to fall back to, so it is
            // an ordinary retryable fault.
            return Err(ProviderError::transport(format!(
                "{label} returned a completion with no choices"
            )));
        };

        let truncated_at_token_limit = choice.finish_reason.as_deref() == Some("length");
        let text = choice.message.content.unwrap_or_default();
        // Both chain-of-thought spellings, folded exactly as the stream does.
        let mut reasoning = choice.message.reasoning_content.unwrap_or_default();
        reasoning.push_str(choice.message.reasoning.as_deref().unwrap_or_default());

        let unary_calls = choice.message.tool_calls.unwrap_or_default();
        // Unary tool calls arrive whole, so only the LAST one can be the one
        // a token limit cut — the same blame-pinning rule as the stream's
        // highest-index accumulator.
        let truncated_index = truncated_at_token_limit
            .then(|| unary_calls.len().checked_sub(1))
            .flatten();
        let mut calls = Vec::with_capacity(unary_calls.len());
        for (index, call) in unary_calls.into_iter().enumerate() {
            let truncated = Some(index) == truncated_index;
            let input: Value = stream::tool_call_input(
                label,
                &call.function.name,
                &call.function.arguments,
                truncated,
            )?;
            calls.push(ToolCall {
                call_id: call.id,
                name: call.function.name,
                input,
            });
        }

        let text = if stream::promote_reasoning_as_text(
            &text,
            &calls,
            &reasoning,
            truncated_at_token_limit,
        ) {
            reasoning
        } else {
            text
        };

        let mut usage = CompletionUsage::default();
        let mut reported_cost_usd = None;
        let usage_seen = parsed.usage.is_some();
        if let Some(frame) = parsed.usage {
            stream::fold_usage(frame, &mut usage, &mut reported_cost_usd);
        }
        usage.reported = usage_seen;
        let cost_usd = reported_cost_usd
            .unwrap_or_else(|| self.pricing.map(|p| p.cost_usd(&usage)).unwrap_or(0.0));

        let finish_reason =
            stream::final_finish_reason(truncated_at_token_limit, !calls.is_empty());
        Ok(CompletionResult {
            text,
            tool_calls: calls,
            usage,
            model: self.model.clone(),
            cost_usd,
            finish_reason,
            // Read off the envelope, not the usage frame: a gateway names its
            // upstream on the response itself, so a call whose usage frame
            // never arrived can still say who served it.
            upstream_provider: parsed.provider,
        })
    }
}
