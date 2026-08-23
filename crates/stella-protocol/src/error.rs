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
// `Clone` because scripted-provider test harnesses replay a scripted error
// more than once, and every one of them was otherwise obliged to keep its own
// hand-written exhaustive mirror of this enum — a copy that goes stale
// silently the moment a variant lands (`stella-core`'s driver tests carried
// exactly that mirror until #2742 added a variant to it). Every payload here
// is owned data with no source chain, so the derive is free.
#[derive(Debug, Clone, Error)]
pub enum ProviderError {
    /// The request never reached a usable response: DNS, TCP, TLS, a dropped
    /// connection, or a client-side timeout — and, by adapter convention, a
    /// provider `5xx` (a response arrived, but the server failed transiently
    /// and the same request may yet succeed). Retryable.
    ///
    /// Carries accounting, because it can strike *after* the provider has
    /// begun reporting. A stream cut during generation has already delivered
    /// its input-token frame; `partial` is where that survives instead of
    /// dying with the adapter's stack frame. Build it with
    /// [`ProviderError::transport`] and attach with
    /// [`ProviderError::with_partial`] rather than naming the field, so the
    /// ~20 dispatch-time sites that have nothing to report stay a single call.
    ///
    /// [`ProviderError::Overloaded`] carries the same field for the same
    /// reason, since #3859: a brownout signalled in band strikes at exactly
    /// this point in a stream's life.
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

    /// The provider is shedding load: it read the request and refused to
    /// serve it *right now* — HTTP 529, the non-standard "overloaded" status
    /// Anthropic and Z.ai return during a brownout. Retryable, and the only
    /// other variant besides [`ProviderError::RateLimited`] that can carry
    /// the server's own backoff hint.
    ///
    /// Split out of [`ProviderError::Transport`] for the reason
    /// [`ProviderError::ContextOverflow`] is split out of
    /// [`ProviderError::Terminal`]: a caller has to *branch* on it, and a
    /// caller reaching that branch by scraping `Transport`'s prose for
    /// "HTTP 529" would be parsing a message written for a human. Every
    /// retryable failure gets the inline ladder, but only a failure whose
    /// recovery is a function of **waiting** earns the parked wait (#2677)
    /// when the ladder runs out — a reset connection retried in five minutes
    /// is no likelier to succeed than one retried in one second, while an
    /// overloaded fleet is. Folded into `Transport`, a sustained 529
    /// brownout burned the ~16s ladder and aborted a turn with wall-clock
    /// budget still unspent (#2742) — the exact loss #2667 measured for 429s.
    ///
    /// Reached two ways, and it carries accounting because of the second.
    /// Off the **status line** (HTTP 529) nothing had become a stream, so
    /// `partial` is `None` and there was never anything to salvage. **In
    /// band**, three frames into an open SSE stream, the provider has already
    /// reported its input tokens — that is the same shape
    /// [`ProviderError::Transport`] carries `partial` for, and dropping it
    /// here would trade a park that never fires for accounting silently lost
    /// on every mid-stream brownout (#3859). Which of the two happened is not
    /// a fact about recoverability, so it must not decide whether the turn
    /// survives.
    // The hint is interpolated by hand for the same reason `RateLimited`
    // does it: `{retry_after_ms:?}` leaks Rust syntax into a user-facing line.
    #[error("provider overloaded{}: {message}", .retry_after_ms.as_ref().map(|ms| format!(" (retry after {ms}ms)")).unwrap_or_default())]
    Overloaded {
        /// The provider's own explanation, summarized for the user.
        message: String,
        /// The server's stated backoff, when it sent one. Honored verbatim by
        /// `stella-core::retry`, exactly as `RateLimited`'s is.
        retry_after_ms: Option<u64>,
        /// Accounting observed before the overload frame arrived, when any
        /// was. `None` for the status-line 529, which never opened a stream.
        /// Build with [`ProviderError::overloaded`] and attach with
        /// [`ProviderError::with_partial`] rather than naming the field.
        partial: Option<PartialUsage>,
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

    /// The provider rejected the request because the prompt exceeds the
    /// model's context window (an HTTP 400 carrying the provider's overflow
    /// signature, or a bare 413). Split from [`ProviderError::Terminal`]
    /// because the two demand opposite reactions: a terminal error ends the
    /// turn, while an overflow is the one rejection the engine can repair —
    /// its reactive recovery compacts the transcript and re-issues the call
    /// (#2680). Not retryable *as-is*: re-sending the identical request
    /// rejects identically, so the retry ladder must never touch it.
    #[error("provider context overflow: {message}")]
    ContextOverflow {
        /// The provider's own explanation, summarized for the user.
        message: String,
    },

    /// The provider refused the request because the account cannot afford
    /// the **requested output ceiling** — not because it has no credit at
    /// all. The mirror image of [`ProviderError::ContextOverflow`] on the
    /// output side, and split from [`ProviderError::Terminal`] for the same
    /// reason: the two demand opposite reactions. Out of credit ends the
    /// turn; "you asked for 128000 tokens and can afford 47365" is a
    /// rejection the engine repairs by asking for fewer.
    ///
    /// Not retryable *as-is* — re-sending the identical ceiling rejects
    /// identically, so the retry ladder must never touch it. The engine's
    /// reactive recovery (`stella-core::driver::output_budget_recovery`)
    /// clamps `max_output_tokens` to what the provider said it could afford
    /// and re-runs the step.
    ///
    /// Born from three bench runs killed or maimed by this exact rejection:
    /// a gateway balance that could still fund dozens of ordinary calls
    /// terminated every trial outright, because a 128K ceiling nobody was
    /// going to spend was priced as if it would be.
    #[error("provider output budget exceeded: {message}")]
    OutputBudgetExceeded {
        /// The provider's own explanation, summarized for the user.
        message: String,
        /// The ceiling the provider said it *could* afford, when it named
        /// one. `None` when the rejection was recognised by shape but
        /// carried no number — the engine then halves its own ask rather
        /// than inventing a figure the provider never stated.
        affordable_output_tokens: Option<u32>,
    },

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

    /// An overload with no accounting to report — the shape of the status-line
    /// 529, which is classified before any response body became a stream. A
    /// mid-stream overload frame builds through here too and picks up its
    /// accounting from [`ProviderError::with_partial`].
    pub fn overloaded(message: impl Into<String>, retry_after_ms: Option<u64>) -> Self {
        ProviderError::Overloaded {
            message: message.into(),
            retry_after_ms,
            partial: None,
        }
    }

    /// Attach accounting that survived this failure.
    ///
    /// Decorates the two retryable variants a *dying stream* can produce —
    /// [`ProviderError::Transport`] and [`ProviderError::Overloaded`] — and is
    /// a no-op on every other, so an adapter can pipe its usage through
    /// `map_err` without first proving which error class it is about to
    /// decorate. That matters because the classification is made further down
    /// (a mid-stream `error` frame can be terminal), and a partial hung on a
    /// terminal failure would claim spend for a request the provider rejected
    /// outright.
    #[must_use]
    pub fn with_partial(self, partial: PartialUsage) -> Self {
        match self {
            ProviderError::Transport { message, .. } => ProviderError::Transport {
                message,
                partial: Some(partial),
            },
            ProviderError::Overloaded {
                message,
                retry_after_ms,
                ..
            } => ProviderError::Overloaded {
                message,
                retry_after_ms,
                partial: Some(partial),
            },
            other => other,
        }
    }

    /// Accounting that survived this failure, if any reached us.
    #[must_use]
    pub fn partial_usage(&self) -> Option<&PartialUsage> {
        match self {
            ProviderError::Transport { partial, .. }
            | ProviderError::Overloaded { partial, .. } => partial.as_ref(),
            _ => None,
        }
    }

    /// Whether the step-driver should retry this call with backoff.
    /// Terminal on 4xx-class failures (auth, unknown model, malformed
    /// request); retryable on transport/rate-limit/overload/5xx-class
    /// failures. Cancellation is never retried.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::Transport { .. }
                | ProviderError::RateLimited { .. }
                | ProviderError::Overloaded { .. }
        )
    }

    /// Whether this failure's recovery is a function of **waiting**, and so
    /// whether it may be converted into a supervised parked wait when the
    /// inline retry ladder runs out (#2677, #2742).
    ///
    /// A strictly narrower question than [`Self::is_retryable`], and the
    /// reason the two are separate methods: every park-eligible error is
    /// retryable, but a dropped connection is retryable without waiting five
    /// minutes being any likelier to fix it than waiting one second. Set
    /// here, at the source, so `stella-core::retry` never re-derives the
    /// classification from a status code or a message (L-M7).
    #[must_use]
    pub fn is_park_eligible(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. } | ProviderError::Overloaded { .. }
        )
    }

    /// The server's own stated backoff in milliseconds, for the two variants
    /// that can carry one. `None` for every other variant, and for a server
    /// that sent no hint — the caller then guesses with its own backoff.
    #[must_use]
    pub fn retry_after_hint_ms(&self) -> Option<u64> {
        match self {
            ProviderError::RateLimited { retry_after_ms, .. }
            | ProviderError::Overloaded { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
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

    /// The variant exists so the engine can react, but the retry ladder must
    /// never re-issue the identical oversized request — that was the original
    /// failure mode #2680 replaces (retry the same request, then abort).
    #[test]
    fn context_overflow_is_never_retryable_as_is() {
        let err = ProviderError::ContextOverflow {
            message: "prompt is too long: 200468 tokens > 200000 maximum".into(),
        };
        assert!(!err.is_retryable());
        assert!(err.to_string().contains("context overflow"), "{err}");
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
    /// the provider refused outright, so `with_partial` decorates only the two
    /// retryable variants a dying stream can produce.
    #[test]
    fn with_partial_is_inert_on_terminal_variants() {
        let err = ProviderError::Auth("bad key".into()).with_partial(PartialUsage::default());
        assert!(err.partial_usage().is_none());
        assert!(matches!(err, ProviderError::Auth(_)));

        for terminal in [
            ProviderError::Malformed("bad json".into()),
            ProviderError::Cancelled,
            ProviderError::Terminal("HTTP 400".into()),
            ProviderError::ContextOverflow {
                message: "too long".into(),
            },
            ProviderError::RateLimited {
                message: "429".into(),
                retry_after_ms: Some(500),
            },
        ] {
            let decorated = terminal.with_partial(PartialUsage::default());
            assert!(decorated.partial_usage().is_none(), "{decorated}");
        }
    }

    /// Witness for #3859's protocol half: an overload signalled *mid-stream*
    /// arrives after the provider has already reported its input tokens, so
    /// the class that lets the turn park must also be able to carry that
    /// accounting out. Before `Overloaded` had a `partial`, an adapter faced
    /// the choice between parking and accounting and could not have both.
    #[test]
    fn a_mid_stream_overload_parks_and_still_reports_what_the_stream_observed() {
        let partial = PartialUsage {
            usage: crate::completion::CompletionUsage {
                input_tokens: 61_000,
                cached_input_tokens: 58_000,
                output_tokens: 240,
                ..Default::default()
            },
            cost_usd: 0.037,
            input_reported: true,
        };
        let err = ProviderError::overloaded("Anthropic stream error: overloaded_error", None)
            .with_partial(partial);

        assert!(err.is_park_eligible(), "{err}");
        let recovered = err.partial_usage().expect("partial survives the overload");
        assert_eq!(recovered.usage.input_tokens, 61_000);
        assert_eq!(recovered.cost_usd, 0.037);
    }

    /// The status-line 529 is classified before any body became a stream, so
    /// it has nothing to report and must say so rather than shipping a zeroed
    /// envelope that reads as "this call was free".
    #[test]
    fn a_status_line_overload_reports_no_accounting() {
        assert!(
            ProviderError::overloaded("anthropic HTTP 529 Overloaded", Some(90_000))
                .partial_usage()
                .is_none()
        );
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

    /// The #2742 witness at the type level: an overloaded provider is
    /// retryable *and* park-eligible, while an ordinary transport fault is
    /// retryable and NOT — that gap is the whole reason the variant exists.
    #[test]
    fn overloaded_is_retryable_and_park_eligible_while_transport_is_only_retryable() {
        let overloaded =
            ProviderError::overloaded("anthropic HTTP 529 Overloaded: overloaded", None);
        assert!(overloaded.is_retryable());
        assert!(overloaded.is_park_eligible());

        let transport = ProviderError::transport("connection reset");
        assert!(transport.is_retryable());
        assert!(
            !transport.is_park_eligible(),
            "waiting five minutes does not un-reset a connection"
        );
    }

    /// Park eligibility is strictly narrower than retryability: nothing
    /// terminal may sneak into a parked wait.
    #[test]
    fn no_terminal_variant_is_park_eligible() {
        for err in [
            ProviderError::Auth("bad key".into()),
            ProviderError::Malformed("bad json".into()),
            ProviderError::Cancelled,
            ProviderError::Terminal("HTTP 400".into()),
            ProviderError::ContextOverflow {
                message: "too long".into(),
            },
            ProviderError::OutputBudgetExceeded {
                message: "cannot afford".into(),
                affordable_output_tokens: Some(4_096),
            },
            ProviderError::UnknownModel {
                slug: "glm-5.2-turbo".into(),
            },
        ] {
            assert!(!err.is_park_eligible(), "{err}");
        }
    }

    /// Both hint-carrying variants surface their hint through one accessor,
    /// and the variants that carry none say so rather than inventing one.
    #[test]
    fn only_the_waiting_variants_carry_a_server_backoff_hint() {
        assert_eq!(
            ProviderError::RateLimited {
                message: "429".into(),
                retry_after_ms: Some(500),
            }
            .retry_after_hint_ms(),
            Some(500)
        );
        assert_eq!(
            ProviderError::overloaded("529", Some(90_000)).retry_after_hint_ms(),
            Some(90_000)
        );
        assert_eq!(
            ProviderError::overloaded("529", None).retry_after_hint_ms(),
            None
        );
        assert_eq!(
            ProviderError::transport("reset").retry_after_hint_ms(),
            None
        );
    }

    /// The rendered line is for a human: no `Some(90000)` Debug syntax, and
    /// the stated wait is visible when the server gave one.
    #[test]
    fn overloaded_renders_its_hint_without_leaking_rust_syntax() {
        let err =
            ProviderError::overloaded("anthropic HTTP 529 Overloaded: overloaded", Some(90_000));
        let msg = err.to_string();
        assert!(msg.contains("provider overloaded"), "{msg}");
        assert!(msg.contains("retry after 90000ms"), "{msg}");
        assert!(!msg.contains("Some("), "{msg}");

        let hintless = ProviderError::overloaded("z.ai HTTP 529: overloaded", None);
        assert!(!hintless.to_string().contains("retry after"), "{hintless}");
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
