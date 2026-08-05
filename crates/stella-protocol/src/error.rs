//! Typed provider errors. Provider adapters classify every failure as
//! retryable or terminal at the source — the step-driver never re-derives
//! that classification from a status code or string later.

use thiserror::Error;

use crate::completion::PartialUsage;

/// An error from a model-provider call, always carrying a retry
/// classification so `stella-core` never has to re-derive one downstream.
///
/// Every payload here is free-form prose an adapter writes and the user
/// reads: these strings render on the TUI and ride the wire as
/// [`crate::event::AgentEvent::Error`]'s `message`. Adapters must therefore
/// summarize, never dump — no `Authorization` header, no request URL with a
/// key in its query string, no raw response body that might echo one back.
/// A leak here escapes the process on the very stream `--output-format
/// stream-json` publishes.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// The request never reached a usable response: DNS, TCP, TLS, a dropped
    /// connection, or a client-side timeout — and, by adapter convention, a
    /// provider `5xx` (a response arrived, but the server failed transiently
    /// and the same request may yet succeed). Retryable.
    ///
    /// This is the one variant that carries accounting, because it is the one
    /// failure that can strike *after* the provider has begun reporting it. A
    /// stream cut during generation has already delivered its input-token
    /// frame; `partial` is where that survives instead of dying with the
    /// adapter's stack frame. Build it with [`ProviderError::transport`] and
    /// attach with [`ProviderError::with_partial`] rather than naming the
    /// field, so the ~20 dispatch-time sites that have nothing to report stay
    /// a single call.
    #[error("provider transport error: {message}")]
    Transport {
        message: String,
        /// Accounting observed before this attempt died, when any was.
        /// `None` for a failure that never got far enough to learn anything —
        /// DNS, connect, TLS, or a 5xx whose body never became a stream.
        partial: Option<PartialUsage>,
    },

    /// The provider refused the call for quota/rate reasons (HTTP 429 and its
    /// per-dialect equivalents). Retryable, and the only variant that can
    /// carry the server's own backoff hint.
    // The hint is interpolated by hand, not with `{retry_after_ms:?}`: the
    // Debug form leaks Rust syntax into a message the user reads on the TUI
    // ("retry after Some(500)ms", or the nonsense "retry after Nonems" when
    // the server sent no hint at all).
    #[error("provider rate limited{}: {message}", .retry_after_ms.as_ref().map(|ms| format!(" (retry after {ms}ms)")).unwrap_or_default())]
    RateLimited {
        /// The provider's own explanation, summarized for the user.
        message: String,
        /// The server's stated backoff, when it sent one. Honored verbatim by
        /// `stella-core::retry` — a stated window always beats a guessed one.
        retry_after_ms: Option<u64>,
    },

    /// The credential was missing, malformed, expired, or not entitled to the
    /// model (HTTP 401/403). Terminal — retrying the same key cannot help.
    #[error("provider auth error: {0}")]
    Auth(String),

    /// The slug is not in the resolved catalog, so no request was ever
    /// dispatched. Terminal, and the one variant whose message names the fix.
    #[error(
        "unknown model `{slug}` — run `stella models refresh` or pick from `stella models list`"
    )]
    UnknownModel {
        /// The slug as the caller spelled it, echoed so the user can see the
        /// typo. Provider-native form (`glm-5.2`), never `provider/model`.
        slug: String,
    },

    /// A response arrived but could not be decoded into a
    /// [`crate::completion::CompletionResult`] — truncated JSON, a missing
    /// required field, an unparseable tool-call argument. Terminal: the same
    /// request would produce the same undecodable shape.
    #[error("provider returned a malformed response: {0}")]
    Malformed(String),

    /// The caller dropped the turn while the call was in flight. Never
    /// retried — the work was abandoned on purpose. Paired with
    /// [`crate::event::UsageIncompleteReason::Cancelled`] when the attempt may
    /// still have cost money server-side.
    #[error("request cancelled")]
    Cancelled,

    /// A failure the adapter classified as terminal without it fitting a
    /// narrower variant — a 4xx the dialect does not model, a refusal, a
    /// content-policy stop. The catch-all, so it fails closed to "do not
    /// retry" rather than looping on something that will never succeed.
    #[error("terminal provider error: {0}")]
    Terminal(String),
}

impl ProviderError {
    /// A transport failure with no accounting to report — the shape of every
    /// dispatch-time fault, and the default every call site should reach for.
    pub fn transport(message: impl Into<String>) -> Self {
        ProviderError::Transport {
            message: message.into(),
            partial: None,
        }
    }

    /// Attach accounting that survived this failure.
    ///
    /// A no-op on every variant except [`ProviderError::Transport`], so an
    /// adapter can pipe its dying stream's usage through `map_err` without
    /// first proving which error class it is about to decorate. That matters
    /// because the classification is made further down (a mid-stream
    /// `error` frame can be terminal), and a partial hung on a terminal
    /// failure would claim spend for a request the provider rejected outright.
    #[must_use]
    pub fn with_partial(self, partial: PartialUsage) -> Self {
        match self {
            ProviderError::Transport { message, .. } => ProviderError::Transport {
                message,
                partial: Some(partial),
            },
            other => other,
        }
    }

    /// Accounting that survived this failure, if any reached us.
    #[must_use]
    pub fn partial_usage(&self) -> Option<&PartialUsage> {
        match self {
            ProviderError::Transport { partial, .. } => partial.as_ref(),
            _ => None,
        }
    }

    /// Whether the step-driver should retry this call with backoff.
    /// Terminal on 4xx-class failures (auth, unknown model, malformed
    /// request); retryable on transport/rate-limit/5xx-class failures.
    /// Cancellation is never retried.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::Transport { .. } | ProviderError::RateLimited { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_and_rate_limited_are_retryable() {
        assert!(ProviderError::transport("timeout").is_retryable());
        assert!(
            ProviderError::RateLimited {
                message: "429".into(),
                retry_after_ms: Some(500)
            }
            .is_retryable()
        );
    }

    #[test]
    fn auth_unknown_model_and_malformed_are_terminal() {
        assert!(!ProviderError::Auth("bad key".into()).is_retryable());
        assert!(
            !ProviderError::UnknownModel {
                slug: "glm-5.2-turbo".into()
            }
            .is_retryable()
        );
        assert!(!ProviderError::Malformed("bad json".into()).is_retryable());
        assert!(!ProviderError::Cancelled.is_retryable());
    }

    /// Witness for the accounting-loss defect: a stream that dies after the
    /// provider reported its input tokens must carry those tokens out with
    /// the error, not drop them at the `?`.
    #[test]
    fn transport_carries_partial_usage_out_of_a_dead_stream() {
        let partial = PartialUsage {
            usage: crate::completion::CompletionUsage {
                input_tokens: 14_000,
                cached_input_tokens: 12_000,
                output_tokens: 130,
                ..Default::default()
            },
            cost_usd: 0.021,
            input_reported: true,
        };
        let err = ProviderError::transport("connection closed mid-response").with_partial(partial);
        let recovered = err.partial_usage().expect("partial survives the error");
        assert_eq!(recovered.usage.input_tokens, 14_000);
        assert_eq!(recovered.cost_usd, 0.021);
        // Still retryable — attaching accounting must not change the class.
        assert!(err.is_retryable());
    }

    /// A partial hung on a terminal failure would claim spend for a request
    /// the provider refused outright, so `with_partial` declines to decorate
    /// anything but `Transport`.
    #[test]
    fn with_partial_is_inert_on_terminal_variants() {
        let err = ProviderError::Auth("bad key".into()).with_partial(PartialUsage::default());
        assert!(err.partial_usage().is_none());
        assert!(matches!(err, ProviderError::Auth(_)));
    }

    /// A dispatch-time fault learned nothing, and must say so rather than
    /// reporting a zeroed envelope that reads as "this call was free".
    #[test]
    fn a_plain_transport_error_reports_no_accounting() {
        assert!(
            ProviderError::transport("dns failure")
                .partial_usage()
                .is_none()
        );
    }

    #[test]
    fn unknown_model_message_names_the_refresh_command() {
        let err = ProviderError::UnknownModel {
            slug: "glm-5.2-turbo".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("models refresh"), "{msg}");
        assert!(msg.contains("glm-5.2-turbo"), "{msg}");
    }
}
