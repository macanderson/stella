//! Typed provider errors. Provider adapters classify every failure as
//! retryable or terminal at the source — the step-driver never re-derives
//! that classification from a status code or string later.

use thiserror::Error;

/// An error from a model-provider call, always carrying a retry
/// classification so `stella-core` never has to re-derive one downstream.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider transport error: {0}")]
    Transport(String),

    // The hint is interpolated by hand, not with `{retry_after_ms:?}`: the
    // Debug form leaks Rust syntax into a message the user reads on the TUI
    // ("retry after Some(500)ms", or the nonsense "retry after Nonems" when
    // the server sent no hint at all).
    #[error("provider rate limited{}: {message}", .retry_after_ms.as_ref().map(|ms| format!(" (retry after {ms}ms)")).unwrap_or_default())]
    RateLimited {
        message: String,
        /// The server's stated backoff, when it sent one. Honored verbatim by
        /// `stella-core::retry` — a stated window always beats a guessed one.
        retry_after_ms: Option<u64>,
    },

    #[error("provider auth error: {0}")]
    Auth(String),

    #[error(
        "unknown model `{slug}` — run `stella models refresh` or pick from `stella models list`"
    )]
    UnknownModel { slug: String },

    #[error("provider returned a malformed response: {0}")]
    Malformed(String),

    #[error("request cancelled")]
    Cancelled,

    #[error("terminal provider error: {0}")]
    Terminal(String),
}

impl ProviderError {
    /// Whether the step-driver should retry this call with backoff.
    /// Terminal on 4xx-class failures (auth, unknown model, malformed
    /// request); retryable on transport/rate-limit/5xx-class failures.
    /// Cancellation is never retried.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::Transport(_) | ProviderError::RateLimited { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_and_rate_limited_are_retryable() {
        assert!(ProviderError::Transport("timeout".into()).is_retryable());
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
