//! Classification for the Responses dialect's in-band failures — the
//! `response.failed` error object and the top-level `error` frame.
//!
//! Split out of `openai.rs` for the reason `provider.rs` was: that file is a
//! grandfathered god file closed to growth (AGENTS.md § God files), and #3859
//! adds lines here. Nothing else moved with it — this is a pure function over
//! borrowed text, and its unit tests stay in the parent's `mod tests`.

use stella_protocol::ProviderError;

/// Classify an OpenAI Responses-API error into a typed [`ProviderError`].
///
/// An overload is the park-eligible [`ProviderError::Overloaded`], the same
/// class the status-line 529 gets: a brownout signalled three frames into a
/// stream is the same condition waiting fixes, and the caller pipes the result
/// through `http::attach_partial`, which decorates that class too — so the
/// input tokens the stream already reported are not the price of parking
/// (#3859). Other server-side/timeout conditions are retryable
/// [`ProviderError::Transport`]; an explicit rate limit is
/// [`ProviderError::RateLimited`]; everything else is
/// [`ProviderError::Terminal`], so an unrecognised failure fails closed to
/// "do not retry" rather than looping on something that will never succeed.
pub(super) fn classify_openai_stream_error(code: Option<&str>, message: &str) -> ProviderError {
    let haystack = format!("{} {}", code.unwrap_or(""), message).to_lowercase();
    let detail = match code {
        Some(c) if !c.is_empty() && !message.is_empty() => {
            format!("OpenAI stream error [{c}]: {message}")
        }
        Some(c) if !c.is_empty() => format!("OpenAI stream error [{c}]"),
        _ if !message.is_empty() => format!("OpenAI stream error: {message}"),
        _ => "OpenAI stream error".to_string(),
    };
    if haystack.contains("overloaded") {
        ProviderError::overloaded(detail, None)
    } else if haystack.contains("server_error")
        || haystack.contains("unavailable")
        || haystack.contains("timeout")
    {
        ProviderError::transport(detail)
    } else if haystack.contains("rate_limit")
        || (haystack.contains("rate") && haystack.contains("limit"))
    {
        ProviderError::RateLimited {
            message: detail,
            retry_after_ms: None,
        }
    } else {
        ProviderError::Terminal(detail)
    }
}
