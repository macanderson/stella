// SPDX-License-Identifier: AGPL-3.0-only
//! The non-streaming delivery path both Google surfaces share — the fallback
//! a completion takes once the session's streaming path has proven broken
//! (hung before its first byte, or a 200 with an empty stream; #2686, #2746,
//! `crate::stream_recovery`).
//!
//! `generateContent` instead of `streamGenerateContent?alt=sse`: same body,
//! and a response that is exactly the shape of one streamed chunk — so
//! assembly is [`super::GeminiAssembly`] fed once, the same fold the
//! aggregator applies to every frame. One implementation covers `gemini` and
//! `vertex` because the two differ only in auth and URL, which is why the
//! caller hands in a [`reqwest::RequestBuilder`] it has already addressed and
//! authorized rather than a base URL and a credential.
//!
//! Dispatch runs through [`crate::http::unary_client`]'s read bound (the
//! whole generation must fit inside a single read — #547) with #547's
//! classification to match: a unary read-timeout consumed the whole bound and
//! re-issuing the identical request just waits it out again, so it surfaces
//! as non-retryable `Terminal`, never as the retryable `Transport` that
//! turned one wedged Bedrock call into four full 600s attempts. The price of
//! this path is the loss of mid-stream observation: no text previews and no
//! speculative tool execution — which is why the latch only confirms on
//! evidence and never arms on an ordinary transport fault.

use stella_protocol::{CompletionResult, ProviderError};

use super::{GeminiAssembly, GeminiRequest, GeminiStreamChunk, classify_google_error};
use crate::catalog::Pricing;
use crate::stream_recovery::StreamRecovery;

/// One unary `generateContent` call, addressed and authorized by its caller.
///
/// A struct rather than a five-argument free function: `label` and `model`
/// are both `&str` and mean entirely different things — one names the surface
/// in every error message, the other the model the catalog priced — and at
/// that width a positional call invites the exact mix-up it cannot catch.
pub(crate) struct GoogleUnaryCall<'a> {
    /// The surface's own name (`Gemini` / `Vertex AI`), so a Vertex failure
    /// never reads as a Gemini one.
    pub(crate) label: &'a str,
    pub(crate) model: &'a str,
    pub(crate) pricing: Option<&'a Pricing>,
    pub(crate) recovery: &'a StreamRecovery,
}

impl GoogleUnaryCall<'_> {
    /// One unary request/parse cycle. Reports its outcome to the recovery
    /// latch: success while probing confirms the fallback for the session,
    /// failure reverts to streaming (see `crate::stream_recovery` for why
    /// neither is skipped).
    pub(crate) async fn complete(
        &self,
        request: reqwest::RequestBuilder,
        body: &GeminiRequest,
    ) -> Result<CompletionResult, ProviderError> {
        let outcome = self.attempt(request, body).await;
        self.recovery.note_unary_outcome(outcome.is_ok());
        outcome
    }

    async fn attempt(
        &self,
        request: reqwest::RequestBuilder,
        body: &GeminiRequest,
    ) -> Result<CompletionResult, ProviderError> {
        let response = request
            .json(body)
            .send()
            .await
            .map_err(|e| crate::http::classify_unary_dispatch_error(self.label, &e))?;
        if !response.status().is_success() {
            return Err(classify_google_error(self.label, response, self.model).await);
        }
        // The body arrives inside the same 600s read bound as the head, so a
        // timeout here is #547 too — classify it terminal rather than
        // retryable, matching the dispatch send path.
        let payload = response
            .text()
            .await
            .map_err(|e| crate::http::classify_unary_dispatch_error(self.label, &e))?;
        self.assemble(&payload)
    }

    /// Parse one `generateContent` body into the adapter's result, under
    /// exactly the shared assembly rules the streaming path uses.
    fn assemble(&self, payload: &str) -> Result<CompletionResult, ProviderError> {
        let label = self.label;
        // Same fail-loudly contract as the stream-frame parse: every field
        // defaults, so an error here is a real type mismatch on a field that
        // matters — swallowing it would report a turn that did nothing as a
        // clean completion.
        let chunk: GeminiStreamChunk = serde_json::from_str(payload).map_err(|e| {
            ProviderError::Malformed(format!(
                "{label}: unparseable non-streaming generateContent body ({e}); \
                 refusing to treat a dropped response as an empty turn"
            ))
        })?;

        let mut assembly = GeminiAssembly::default();
        // No observer: a unary call has no parts to announce as they land,
        // which is the price this path pays and the reason the latch never
        // arms speculatively.
        assembly.absorb(chunk, label, None)?;

        if assembly.finish_raw.is_none() {
            // A 200 whose candidate carries no `finishReason` is the unary
            // spelling of the empty stream — the same "never commit a
            // truncated Ok" rule the aggregator applies at EOF. There is no
            // third transport to fall back to, so it is an ordinary retryable
            // fault.
            return Err(crate::http::attach_partial(
                crate::http::stream_ended_before_terminal(label, "a terminal finishReason"),
                &assembly.usage,
                &assembly.text,
                self.pricing,
            ));
        }

        let model = self.model.to_string();
        let (text, tool_calls, usage, finish_reason) = assembly.finish();
        let cost_usd = self.pricing.map_or(0.0, |p| p.cost_usd(&usage));
        Ok(CompletionResult {
            text,
            tool_calls,
            usage,
            model,
            cost_usd,
            finish_reason,
            // Direct endpoint — no gateway, so no upstream to name.
            upstream_provider: None,
        })
    }
}
