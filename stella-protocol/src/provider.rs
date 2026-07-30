//! The `Provider` port. Lives in `stella-protocol`,
//! not `stella-model`, so `stella-core` can drive every model call through
//! `&dyn Provider` without depending on any concrete adapter — `stella-model`
//! depends on this crate to implement the trait, never the reverse.

use async_trait::async_trait;

use crate::completion::{CompletionRequest, CompletionRequestRef, CompletionResult};
use crate::error::ProviderError;
use crate::tool::ToolCall;

/// Observes tool calls as their blocks finish streaming, while the rest of
/// the completion is still in flight. This is the seam speculative tool
/// execution hangs on: `stella-core` hands an observer to
/// [`Provider::complete_observed_ref`] and begins executing *read-only* calls
/// the moment they are announced, instead of waiting for the full response.
///
/// Strictly advisory: the definitive tool-call list is the returned
/// [`CompletionResult`] — an adapter may announce all, some, or none of the
/// calls it will return, but every announced call MUST be byte-identical
/// (same `call_id`, `name`, and parsed `input`) to the one in the final
/// result, because consumers match announced work back by exact equality.
/// An adapter must never announce a call whose input failed to parse.
///
/// Both methods are synchronous and are invoked **inline on the task polling
/// the provider's stream**, so an implementation must return promptly: hand
/// the work to a runtime task or a channel, never block, sleep, do file or
/// network I/O, or take a lock the completion path also wants. Stalling here
/// stalls the model call itself, which is the opposite of what speculation is
/// for.
pub trait ToolCallObserver: Send + Sync {
    /// One tool call's block has fully streamed: id, name, and complete,
    /// well-formed input are known.
    fn tool_call_streamed(&self, call: &ToolCall);

    /// One fragment of user-visible answer text arrived on the stream, in
    /// order. Only answer text — never thinking/reasoning content — and
    /// strictly best-effort: the definitive text is `CompletionResult::text`
    /// (a retried attempt re-streams from the start, and an adapter without
    /// mid-stream visibility calls this not at all). Default no-op so
    /// existing observers compile unchanged.
    fn text_delta(&self, delta: &str) {
        let _ = delta;
    }

    /// One fragment of *thinking* arrived on the stream, in order — the
    /// counterpart to [`Self::text_delta`] for models that stream
    /// chain-of-thought on a separate channel (OpenRouter's `reasoning`,
    /// GLM's `reasoning_content`, Anthropic's `thinking_delta`).
    ///
    /// Kept a distinct method rather than folded into `text_delta` because
    /// the two must never be confused downstream: thinking is displayed as
    /// collapsible, visibly-secondary content, while answer text is the
    /// reply. Conflating them is exactly the defect that made the adapter
    /// publish private deliberation as the model's answer.
    ///
    /// Same best-effort contract as `text_delta`: lossy, re-streamed on a
    /// retried attempt, and not called at all by adapters without mid-stream
    /// visibility. Default no-op so existing observers compile unchanged.
    fn reasoning_delta(&self, delta: &str) {
        let _ = delta;
    }
}

/// One model provider adapter. `stella-core` drives every call through
/// `&dyn Provider` — no adapter-specific code ever lives outside
/// `stella-model`.
///
/// # Which method to implement (#921)
///
/// The port's currency is [`CompletionRequestRef`], a borrowed view: adapters
/// implement [`Provider::complete_ref`] (and, if they have mid-stream
/// visibility, [`Provider::complete_observed_ref`]) and serialize straight off
/// the caller's slices. That is what keeps the engine's hot path free of a
/// per-attempt deep copy of the whole transcript — see [`CompletionRequestRef`]
/// for why the retry loop made an owning request so expensive.
///
/// [`Provider::complete`] and [`Provider::complete_observed`] are provided
/// shims for the many callers that genuinely *own* their request — a one-shot
/// reflection, ingest, or judge call that built its messages and will never
/// reuse them. They are not override points; overriding one would leave the
/// borrowed path pointing at a different implementation.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable id for this provider instance, e.g. `"zai"` or `"anthropic"`.
    fn id(&self) -> &str;

    /// Run one completion end-to-end (streams internally, aggregates the
    /// result). Returns a typed, retry-classified error on failure — never
    /// panics on a malformed/erroring HTTP response.
    ///
    /// This is the method adapters implement. The request is borrowed for the
    /// duration of the call and must be serialized, not stored; an adapter that
    /// needs it to outlive the call takes the copy explicitly with
    /// [`CompletionRequestRef::into_owned`].
    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError>;

    /// [`Provider::complete_ref`], additionally announcing each tool call to
    /// `observer` as its block finishes streaming (see [`ToolCallObserver`]).
    /// The default ignores the observer and delegates to `complete_ref`, so an
    /// adapter without mid-stream visibility keeps exactly its old behavior
    /// — the engine simply gets no speculation from it.
    async fn complete_observed_ref(
        &self,
        req: CompletionRequestRef<'_>,
        observer: &dyn ToolCallObserver,
    ) -> Result<CompletionResult, ProviderError> {
        let _ = observer;
        self.complete_ref(req).await
    }

    /// [`Provider::complete_ref`] for a caller that already owns its request.
    /// Borrows and delegates — no copy is taken, so an owning caller pays
    /// nothing for the convenience.
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResult, ProviderError> {
        self.complete_ref(req.as_borrowed()).await
    }

    /// [`Provider::complete_observed_ref`] for a caller that already owns its
    /// request.
    async fn complete_observed(
        &self,
        req: CompletionRequest,
        observer: &dyn ToolCallObserver,
    ) -> Result<CompletionResult, ProviderError> {
        self.complete_observed_ref(req.as_borrowed(), observer)
            .await
    }
}
