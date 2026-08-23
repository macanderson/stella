//! Shared HTTP plumbing for every provider adapter: a `reqwest` client with a
//! bounded connect timeout, and an idle-timeout wrapper around per-chunk
//! stream reads. Centralized so every provider adapter gets identical
//! timeout-and-retry-classification behavior — a hung TCP connect or a
//! provider that opens a stream and then goes silent must surface as a
//! *retryable* `Transport` error, not an unbounded hang.

use std::time::Duration;

use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use stella_protocol::{CompletionUsage, PartialUsage, ProviderError, tokens::estimate_tokens};

use crate::catalog::Pricing;

/// How long to wait for the initial TCP/TLS connection before giving up. A
/// dead or black-holed provider endpoint should fail fast and retryably, not
/// block the turn on the OS default connect timeout (minutes).
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for the *next* chunk of an already-open stream before
/// treating the connection as dead. Generous enough for slow model thinking
/// between tokens, short enough that a silently-dropped stream doesn't hang
/// the turn forever.
pub(crate) const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long to wait for the **first** chunk of a stream's body before
/// treating the stream itself as broken. Distinct from — and shorter than —
/// [`STREAM_IDLE_TIMEOUT`], because the two waits mean different things: a
/// gap *between* fragments is a model thinking, while a response that has
/// sent its headers and then not one body byte is the signature of a proxy
/// buffering the SSE body — a path fault the same request completes fine
/// over a non-streaming call (issue #2686). 90 seconds matches the zappy
/// comparator's stream watchdog and sits well above any observed
/// time-to-first-token (which grows with prompt-processing time on large
/// cache writes, not with generation length), while discovering a buffering
/// proxy 30s sooner than the idle bound and ~13x sooner than the engine's
/// 816s model deadline. A trip is fallback-eligible: see
/// `crate::stream_recovery`.
pub(crate) const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(90);

/// A `reqwest::Client` with [`CONNECT_TIMEOUT`] applied, plus a per-read
/// stall bound of [`STREAM_IDLE_TIMEOUT`]. The read timeout closes the gap
/// [`next_with_timeout`] cannot see: the wait between a successful connect
/// and the first response byte (headers). Without it, a provider LB that
/// accepts the connection and then black-holes hangs `.send()` forever and
/// the retry engine never fires. The bound matches the stream-idle policy,
/// so it can never kill a stream the idle timeout would have allowed.
/// Falls back to the default client if the builder fails (only possible on
/// a broken TLS backend, which is catastrophic and unrelated to any single
/// request) — never panics on the construction path. Note what that fallback
/// costs: `reqwest::Client::default()` carries NO connect and NO read bound,
/// so the degraded client is precisely the unbounded-hang shape this function
/// exists to prevent. Acceptable only because the trigger is a process-wide
/// TLS failure that fails every request anyway.
///
/// Neither this client nor [`unary_client`] sets a *total* request timeout:
/// the bound is per-read, so a peer that dribbles one byte inside every
/// [`STREAM_IDLE_TIMEOUT`] window is never cut off. That is deliberate for
/// streaming completions (a long generation is legitimate), so a
/// non-completion caller must bring its own wall-clock ceiling —
/// both [`crate::provider_listing`] and [`crate::modelsdev`] (the two
/// third-party fetches, and both on the CLI's blocking startup auto-sync
/// path) wrap every fetch in one.
///
/// This bound fits streaming callers only. A non-streaming caller has no
/// first token to reset the clock, so the whole generation must fit inside
/// one read — see [`unary_client`], which such a caller takes instead.
pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(STREAM_IDLE_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// Read bound for a UNARY provider call. With no stream there is no first
/// token to reset the read clock, so this single bound has to cover the
/// entire model generation — extended thinking, a large `max_output_tokens`,
/// a long tool-argument payload — not just the gap between chunks.
///
/// Ten minutes is chosen to sit above any generation a caller would still
/// want, while staying finite: dropping the bound entirely would let a
/// provider LB that accepts the connection and then black-holes hang the
/// turn forever, since there is no engine-level deadline above the model
/// call to cut it short.
pub(crate) const UNARY_READ_TIMEOUT: Duration = Duration::from_secs(600);

/// A [`client`] for adapters that issue a **unary** request rather than a
/// stream, bounded by [`UNARY_READ_TIMEOUT`] instead of
/// [`STREAM_IDLE_TIMEOUT`].
///
/// `bedrock.rs` is unary by construction: it calls `Converse`, not
/// `ConverseStream`, so the whole generation happens before the first
/// response byte arrives. On the shared client that made every completion
/// slower than [`STREAM_IDLE_TIMEOUT`] fail as `ProviderError::Transport`,
/// whose `is_retryable()` is true — so the driver re-issued the identical
/// too-long request and it timed out again, burning the retry budget and
/// paying for all four attempts (#547).
///
/// Every streaming adapter also holds one, for the fallback the same issue
/// motivates from the other side: when a session's streaming path proves
/// broken, [`crate::stream_recovery`] sends the retried attempt out unary,
/// and that attempt has no first token to reset the clock either.
pub(crate) fn unary_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(UNARY_READ_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// Classify a dispatch failure from a **unary** adapter.
///
/// Splitting timeout from the rest is the other half of #547. On a unary
/// adapter [`UNARY_READ_TIMEOUT`] covers the entire generation, so its expiry
/// means the request was too long to serve — not that the network hiccuped.
/// Classifying that as retryable `Transport` made the driver re-issue the
/// identical too-long request until the retry budget ran out, turning one
/// wedged call into four full read timeouts (~40 minutes) before anything
/// surfaced. Moving #547's bound from 120s to 600s widened that window rather
/// than closing it.
///
/// A **connect** timeout is the opposite case and stays retryable: nothing
/// reached the model, nothing was generated, and the next attempt may well
/// land. Ordinary transport faults (resets, DNS, TLS) stay retryable too.
pub(crate) fn classify_unary_dispatch_error(label: &str, error: &reqwest::Error) -> ProviderError {
    if error.is_connect() {
        return ProviderError::transport(error.to_string());
    }
    if error.is_timeout() {
        return ProviderError::Terminal(format!(
            "{label} did not answer within {}s; retrying would re-issue the \
             identical request and wait again",
            UNARY_READ_TIMEOUT.as_secs()
        ));
    }
    ProviderError::transport(error.to_string())
}

/// Parse a `Retry-After` header (RFC 9110 §10.2.3) into a millisecond hint
/// for the retry policy — `crates/stella-core/src/retry.rs` honors
/// `RateLimited.retry_after_ms` when present. An absent or unparseable value
/// yields `None` rather than an error.
pub(crate) fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    retry_after_ms_at(value, unix_epoch_secs_now())
}

/// Seconds since the Unix epoch, or 0 if the system clock is set before it.
/// Split out so [`retry_after_ms_at`] stays a pure function of its inputs and
/// the HTTP-date arm is testable without a fixed system clock.
fn unix_epoch_secs_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// The pure half of [`parse_retry_after_ms`]: `value` interpreted against a
/// caller-supplied clock.
///
/// Both RFC 9110 forms are accepted. Delta-seconds is what every major API
/// sends directly; the HTTP-date form is what a CDN or reverse proxy in front
/// of one emits — and a deployment fronted by a CDN is exactly the deployment
/// that rate-limits. Ignoring the date form did not fail loudly: the hint came
/// back `None`, the driver fell back to its computed backoff, and the retry
/// landed back inside the window the server had just named.
///
/// A date already in the past yields `Some(0)`; the retry policy floors a zero
/// hint at its own base delay, so "retry now" is expressed rather than
/// discarded. The two obsolete formats RFC 9110 §5.6.7 lists (RFC 850 and
/// asctime) are deliberately not parsed — nothing observed emits them, and
/// they degrade to `None` exactly as the date form used to.
fn retry_after_ms_at(value: &str, now_epoch_secs: i64) -> Option<u64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds.saturating_mul(1000));
    }
    let deadline = imf_fixdate_epoch_secs(value)?;
    Some((deadline - now_epoch_secs).max(0).saturating_mul(1000) as u64)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Seconds since the Unix epoch for an IMF-fixdate — `Sun, 06 Nov 1994
/// 08:49:37 GMT`, the one form RFC 9110 §5.6.7 allows a sender to generate.
/// Strict by design: a header this function cannot read exactly is a header
/// whose meaning we would be guessing at, and `None` costs only the fallback
/// to computed backoff.
fn imf_fixdate_epoch_secs(value: &str) -> Option<i64> {
    // The day-of-week is redundant with the date and is not cross-checked;
    // a sender that disagrees with itself is not a reason to drop the hint.
    let (_weekday, rest) = value.split_once(", ")?;
    let mut fields = rest.split(' ');
    let day: i64 = fields.next()?.parse().ok()?;
    let month_name = fields.next()?;
    let month = MONTHS.iter().position(|name| *name == month_name)? as i64 + 1;
    let year: i64 = fields.next()?.parse().ok()?;
    let mut clock = fields.next()?.split(':');
    // GMT is the only zone the grammar permits, so anything else is a
    // malformed header rather than another time zone to convert from.
    if fields.next()? != "GMT" || fields.next().is_some() {
        return None;
    }
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if clock.next().is_some() {
        return None;
    }
    // A leap second is spelled `:60`; it maps onto the same POSIX epoch
    // second as `:59`, which is the closest a Unix timestamp can represent.
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second.min(59))
}

/// Days between 1970-01-01 and a proleptic-Gregorian `y-m-d`, by Howard
/// Hinnant's `days_from_civil`. Written out rather than pulling in a date
/// crate for one header: this is the whole of the calendar arithmetic an
/// IMF-fixdate needs, and it handles the leap rules exactly. `m` is 1-based
/// and assumed in range (its caller validates).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400; // [0, 399]
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Best-effort extraction of the human `message` from a provider's
/// structured error body. OpenAI, Anthropic, and every OpenAI-compatible gateway
/// (OpenRouter, xAI, DeepSeek, Z.ai, Gemini direct) nest it under an `error`
/// object — `{"error": {"message": "...", ...}}`; Bedrock/AWS report the
/// same field flat at the top level — `{"message": "...", "__type": ...}`.
/// Tried in that order; `None` when the body is not JSON or carries no
/// non-empty `message`, in which case callers fall back to the raw body.
fn parse_error_reason(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Wrapped {
        error: Inner,
    }
    #[derive(Deserialize, Default)]
    struct Inner {
        #[serde(default)]
        message: Option<String>,
    }
    #[derive(Deserialize, Default)]
    struct Flat {
        #[serde(default)]
        message: Option<String>,
    }

    let reason = serde_json::from_str::<Wrapped>(body)
        .ok()
        .and_then(|w| w.error.message)
        .or_else(|| {
            serde_json::from_str::<Flat>(body)
                .ok()
                .and_then(|f| f.message)
        });

    reason
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

/// The machine `code` from a provider's structured error body, when it is a
/// string. OpenAI and every OpenAI-compatible gateway put it at `error.code`
/// (`"context_length_exceeded"`); a numeric code there (OpenRouter echoes the
/// HTTP status) carries no classification signal and yields `None`, as does a
/// non-JSON body.
fn parse_error_code(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Wrapped {
        error: Inner,
    }
    #[derive(Deserialize, Default)]
    struct Inner {
        #[serde(default)]
        code: Option<serde_json::Value>,
    }
    serde_json::from_str::<Wrapped>(body)
        .ok()
        .and_then(|w| w.error.code)
        .and_then(|code| match code {
            serde_json::Value::String(code) => Some(code),
            _ => None,
        })
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
}

/// Whether a non-success response is the provider saying "this request
/// exceeds the model's context window" — the one 4xx the engine can repair
/// by compacting and re-issuing (#2680), so it must classify as
/// `ProviderError::ContextOverflow` rather than fall into the terminal
/// catch-all.
///
/// Providers share no wire standard for this, so detection is per-dialect
/// signatures over the machine `code` and the human message — each declared
/// per provider on the parity matrix's overflow axis
/// (`crate::provider_parity::OVERFLOW_POSTURE`, invariant 8) rather than
/// assumed:
///
/// - **HTTP 413** is unconditional: Payload Too Large has no other meaning
///   on a completion endpoint.
/// - **OpenAI / OpenAI-compatible** (and the gateways that forward it):
///   `error.code == "context_length_exceeded"`, or the classic message
///   "This model's maximum context length is N tokens", or the Responses
///   API's "exceeds the context window".
/// - **Anthropic**: 400 `invalid_request_error` with
///   "prompt is too long: N tokens > M maximum".
/// - **Bedrock**: `ValidationException` with "Input is too long for
///   requested model" / "too many input tokens".
/// - **Gemini / Vertex**: 400 `INVALID_ARGUMENT` with "input token count
///   (N) exceeds the maximum number of tokens allowed".
///
/// Phrases are deliberately narrow — each is the vendor's documented
/// overflow sentence, not a generic word like "context" or "long" — because
/// a false positive here makes the engine compact and retry a request that
/// failed for a different reason. An unmatched overflow shape degrades to
/// `Terminal`, which is exactly today's abort: safe, just unrecovered.
fn is_context_overflow(status: reqwest::StatusCode, body: &str, reason: Option<&str>) -> bool {
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        return true;
    }
    if status != reqwest::StatusCode::BAD_REQUEST {
        return false;
    }
    if parse_error_code(body).as_deref() == Some("context_length_exceeded") {
        return true;
    }
    let haystack = reason.unwrap_or(body).to_lowercase();
    [
        "prompt is too long",                    // Anthropic
        "maximum context length",                // OpenAI chat completions
        "exceeds the context window",            // OpenAI Responses API
        "context_length_exceeded",               // code echoed in prose
        "input is too long for requested model", // Bedrock Converse
        "too many input tokens",                 // Bedrock (alternate)
        "exceeds the maximum number of tokens",  // Gemini / Vertex
    ]
    .iter()
    .any(|phrase| haystack.contains(phrase))
}

/// Longest raw response body (in characters) echoed back in a classified
/// HTTP error. A provider fronted by a CDN or a reverse proxy answers a 5xx
/// with a multi-kilobyte HTML error page, and a gateway can dump an equally
/// large JSON blob on a 400 — pasting either verbatim into the turn's error
/// floods the transcript, the log, and the user's terminal for no diagnostic
/// gain over the opening paragraph. Generous enough that every real
/// structured error body (which is a sentence or two) survives untouched.
const ERROR_BODY_SNIPPET_CHARS: usize = 800;

/// `body` bounded to [`ERROR_BODY_SNIPPET_CHARS`], with an explicit marker
/// naming how much was elided so nobody mistakes the cut for the provider's
/// whole answer. Character-bounded, not byte-bounded: a body is
/// environment-controlled text and a byte slice could land mid-character.
///
/// `pub(crate)` because the vendor pre-checks that run *ahead* of
/// [`classify_http_status`] build their own message and would otherwise echo
/// an unbounded body. Three such callers today: `gemini.rs`'s
/// `API_KEY_INVALID`-on-400 arm, `zai.rs`'s billing-vs-throttle 429
/// classifier, and `bedrock.rs`'s throttle arm. Every path that puts a
/// provider body into an error message must go through here — each of those
/// pre-checks classifies on the FULL body and bounds only the message it
/// surfaces.
pub(crate) fn body_snippet(body: &str) -> String {
    let total = body.chars().count();
    if total <= ERROR_BODY_SNIPPET_CHARS {
        return body.to_string();
    }
    let head: String = body.chars().take(ERROR_BODY_SNIPPET_CHARS).collect();
    let elided = total - ERROR_BODY_SNIPPET_CHARS;
    format!("{head}… (+{elided} more chars)")
}

/// Bucket an HTTP 403's reason text into the right remediation hint. A 403
/// means the key itself is valid — the account/key/model combination is
/// refused for one of three provider-observed reasons, and each has a
/// different fix: billing/credits exhausted, this specific model not
/// enabled for the key/org, or a bare permission/scope refusal with neither
/// signal present. Matched on lowercase text since providers share no
/// stable machine code for this (the same body-sniffing tradeoff
/// `zai.rs`'s 429 billing classifier makes) — a false negative that falls
/// through to the generic hint beats a wrong diagnosis.
///
/// The model-enablement arm deliberately does NOT match on bare "access":
/// "access denied" / "you do not have access" is how providers phrase the
/// *generic* permission refusal, so keying on that word sent nearly every
/// plain 403 to the "enable this model for your key" hint — a confident
/// wrong diagnosis, which is the one outcome this bucketing exists to avoid.
/// "model" and "enable" stay, because both really are enablement language.
fn forbidden_hint(haystack: &str) -> &'static str {
    if haystack.contains("credit")
        || haystack.contains("balance")
        || haystack.contains("insufficient")
        || haystack.contains("quota")
        || haystack.contains("spend")
        || haystack.contains("billing")
    {
        "the key is valid but the account is out of credits or over its spend cap — add \
         credit or raise the cap, then retry"
    } else if haystack.contains("model") || haystack.contains("enable") {
        "the key is valid but isn't enabled for this model — enable it for this key/org, or \
         switch models on the SETTINGS tab / with `--model provider/slug`"
    } else {
        "the key is valid but lacks permission for this request — check the key's scopes \
         and organization on the provider's dashboard"
    }
}

/// The output ceiling a 402 says the account *can* afford, when the body is
/// the affordability rejection rather than a bare out-of-credits one.
///
/// OpenRouter's exact wire text, recorded off a bench trial that died of it:
///
/// ```text
/// This request requires more credits, or fewer max_tokens. You requested
/// up to 128000 tokens, but can only afford 117676. To increase, visit …
/// ```
///
/// Matched on the two clauses that carry the meaning — "fewer max_tokens"
/// (this is about the ceiling, not the balance) and "afford N" (the number)
/// — rather than on the sentence as a whole, so a reworded prefix or a
/// changed URL does not silently turn a recoverable rejection back into a
/// dead turn. Returns `None` for a 402 that is genuinely out of credit,
/// which must stay terminal: no ceiling is small enough to fund it.
fn affordable_output_tokens(body: &str, reason: Option<&str>) -> Option<u32> {
    let haystack = reason.unwrap_or(body).to_lowercase();
    if !haystack.contains("max_tokens") {
        return None;
    }
    let (_, tail) = haystack.split_once("afford")?;
    let digits: String = tail
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// Whether a non-success body appears to be about the model the caller
/// configured — either literally (the gateway echoes back the rejected
/// slug, e.g. OpenRouter's `"{slug} is not a valid model ID"`) or generically
/// (any mention of "model" in a 4xx we already know isn't an auth/rate-limit
/// failure). Used to gate the invalid-model recovery hint (issue #271) so it
/// fires on "unknown model" without also firing on an unrelated malformed
/// request that happens to 400 for some other reason.
fn mentions_configured_model(body: &str, model: &str) -> bool {
    (!model.trim().is_empty() && body.contains(model)) || body.to_lowercase().contains("model")
}

/// The non-standard "site is overloaded" status Anthropic and Z.ai return
/// while shedding load. `reqwest::StatusCode` has no constant for it (it is
/// not an IANA-registered code), so the classifier compares the raw u16.
pub(crate) const HTTP_OVERLOADED: u16 = 529;

/// The mechanical non-success ladder shared by every adapter, applied AFTER
/// any vendor-specific pre-check (Z.ai's billing-encoded 429s, Google's
/// API_KEY_INVALID-on-400). `model` is the wire slug this call was sent
/// with, used only to sharpen the invalid-model hint below — every adapter
/// already holds it as `self.model`.
///
/// - 401 → non-retryable `Auth` ("authenticate": the key itself is rejected
///   — revoked, mistyped, or never loaded). The provider's own reason
///   ([`parse_error_reason`]) is folded in when present, plus a hint to
///   check the SETTINGS tab / `api_key`/`api_key_env` / `--base-url`.
/// - 403 → non-retryable `Auth` ("authorize": the key is valid but the
///   request is refused). [`forbidden_hint`] distinguishes credits/billing,
///   model-not-enabled, and bare permission refusal so the three don't read
///   as one undifferentiated "credentials failed" message (issue #250).
/// - 402 → three failures share the status and get three classifications:
///   a refused *output ceiling* is non-retryable `OutputBudgetExceeded`
///   (the engine clamps and re-asks); a balance committed to the caller's
///   own concurrent calls ("retry after in-flight requests settle") is
///   retryable `RateLimited`; a bare out-of-credit body is non-retryable
///   `Terminal`, called out explicitly as a billing failure (some gateways
///   use Payment Required for out-of-credits rather than folding it into a
///   403).
/// - 429 → retryable `RateLimited` carrying the Retry-After hint.
/// - 529 → retryable `Overloaded` carrying the Retry-After hint. Anthropic
///   and Z.ai return this non-standard status to shed load, and it is the
///   one 5xx whose recovery is a function of *waiting*, so it is classified
///   apart from `Transport` to reach the engine's parked wait (#2742).
/// - every other 5xx → retryable `Transport`. Without this a momentary blip
///   aborts the whole turn (`Terminal.is_retryable() == false`).
/// - a 400 carrying a provider's context-overflow signature, or a bare 413
///   ([`is_context_overflow`]) → non-retryable `ContextOverflow`, the one
///   4xx split out of the catch-all because the engine repairs it (forced
///   compaction + re-issue, #2680) instead of aborting on it.
/// - anything else (400/404/422/...) → non-retryable `Terminal` — this call
///   can never succeed by retrying it verbatim (issue #271), so the
///   step-driver's retry loop never re-derives a classification here, it
///   only ever asks `is_retryable()` and gets `false` on the first attempt.
///   When the body appears to name the configured model
///   ([`mentions_configured_model`]), a one-line recovery hint is appended:
///   change it on the SETTINGS tab, relaunch with `--model provider/slug`,
///   or edit settings.json.
///
/// Wherever the raw body is echoed (the 5xx and catch-all arms) it goes
/// through [`body_snippet`] first, so a CDN error page can never flood the
/// transcript; classification itself always reads the full body.
pub(crate) fn classify_http_status(
    label: &str,
    status: reqwest::StatusCode,
    retry_after_ms: Option<u64>,
    body: &str,
    model: &str,
) -> ProviderError {
    use reqwest::StatusCode;
    let reason = parse_error_reason(body);
    let reason_suffix = reason
        .as_deref()
        .map(|r| format!(": {r}"))
        .unwrap_or_default();
    match status {
        StatusCode::UNAUTHORIZED => ProviderError::Auth(format!(
            "{label} rejected the credential (HTTP 401){reason_suffix} — the key looks \
             revoked, mistyped, or was never loaded; re-check it on the SETTINGS tab, the \
             `api_key`/`api_key_env` value, and that `--base-url` actually points at {label}"
        )),
        StatusCode::FORBIDDEN => {
            let haystack = reason.as_deref().unwrap_or(body).to_lowercase();
            let hint = forbidden_hint(&haystack);
            ProviderError::Auth(format!(
                "{label} rejected the credential (HTTP 403){reason_suffix} — {hint}"
            ))
        }
        StatusCode::PAYMENT_REQUIRED => {
            // Three different failures share this status, and only one is
            // terminal. "Out of credit" ends the turn; "you asked for a
            // ceiling you cannot afford" is repaired by asking for less,
            // and terminating on it is what killed three bench runs whose
            // balance could still have funded dozens of ordinary calls.
            let haystack = reason.as_deref().unwrap_or(body).to_lowercase();
            if affordable_output_tokens(body, reason.as_deref()).is_some()
                || haystack.contains("fewer max_tokens")
            {
                return ProviderError::OutputBudgetExceeded {
                    message: format!(
                        "{label} cannot afford the requested output ceiling (HTTP \
                         402){reason_suffix}"
                    ),
                    affordable_output_tokens: affordable_output_tokens(body, reason.as_deref()),
                };
            }
            // The third: the balance is committed to the caller's *other*
            // calls, and the provider itself says to wait ("Retry after
            // in-flight requests settle"). Keyed on the clause the way the
            // affordability arm keys on `fewer max_tokens`. Classified as
            // the 429 it behaves like so it reaches the retry ladder and
            // the parked wait — as `Terminal` it aborted turns mid-flight
            // and told the user the account was out of credit while the
            // same key funded further calls a minute later (#4380).
            if haystack.contains("in-flight requests") {
                return ProviderError::RateLimited {
                    message: format!(
                        "{label} deferred the request until its in-flight calls settle (HTTP \
                         402){reason_suffix}"
                    ),
                    retry_after_ms,
                };
            }
            ProviderError::Terminal(format!(
                "{label} rejected the request (HTTP 402: payment required){reason_suffix} — the \
                 account is out of credits; add credit or switch to a funded provider/model"
            ))
        }
        StatusCode::TOO_MANY_REQUESTS => ProviderError::RateLimited {
            message: format!("{label} rate limit"),
            retry_after_ms,
        },
        s if s.as_u16() == HTTP_OVERLOADED => ProviderError::overloaded(
            format!("{label} HTTP {status}: {}", body_snippet(body)),
            retry_after_ms,
        ),
        s if s.is_server_error() => {
            ProviderError::transport(format!("{label} HTTP {status}: {}", body_snippet(body)))
        }
        _ => {
            if is_context_overflow(status, body, reason.as_deref()) {
                return ProviderError::ContextOverflow {
                    message: format!(
                        "{label} rejected the request as exceeding the model's context window \
                         (HTTP {status}): {}",
                        reason
                            .as_deref()
                            .map_or_else(|| body_snippet(body), str::to_string)
                    ),
                };
            }
            let mut message = format!("{label} HTTP {status}: {}", body_snippet(body));
            if mentions_configured_model(body, model) {
                message.push_str(
                    " — change the model on the deck's SETTINGS tab, relaunch with \
                     `--model provider/slug`, or edit settings.json",
                );
            }
            ProviderError::Terminal(message)
        }
    }
}

/// One bounded stream read, with the four outcomes kept distinct. The
/// distinction [`next_with_timeout`] collapses — a stall versus a transport
/// fault — is exactly the bit the streaming→non-streaming fallback needs: a
/// stall before the first byte is a broken *streaming path* (fallback
/// material), while a reset says nothing about streaming specifically.
pub(crate) enum StreamRead<T> {
    /// The next item arrived within the bound.
    Item(T),
    /// Clean end of stream.
    End,
    /// Nothing arrived within the bound — the stream is stalled.
    Idle,
    /// The transport failed mid-read.
    Failed(String),
}

/// Await the next stream item, bounded by `idle`, reporting the outcome as a
/// [`StreamRead`]. `idle` is a parameter (rather than reading
/// [`STREAM_IDLE_TIMEOUT`] directly) purely so the timeout path is
/// unit-testable in milliseconds.
pub(crate) async fn next_stream_read<S, T>(stream: &mut S, idle: Duration) -> StreamRead<T>
where
    S: Stream<Item = reqwest::Result<T>> + Unpin,
{
    match tokio::time::timeout(idle, stream.next()).await {
        Ok(Some(Ok(item))) => StreamRead::Item(item),
        Ok(Some(Err(e))) => StreamRead::Failed(e.to_string()),
        Ok(None) => StreamRead::End,
        Err(_elapsed) => StreamRead::Idle,
    }
}

/// Await the next stream item, bounded by `idle`. Maps a stalled stream (no
/// item within `idle`) and any transport error to a **retryable**
/// `ProviderError::Transport`, and a clean end-of-stream to `Ok(None)`.
/// A thin classification over [`next_stream_read`] for the adapters that
/// don't need to tell the two failure shapes apart.
pub(crate) async fn next_with_timeout<S, T>(
    stream: &mut S,
    idle: Duration,
) -> Result<Option<T>, ProviderError>
where
    S: Stream<Item = reqwest::Result<T>> + Unpin,
{
    match next_stream_read(stream, idle).await {
        StreamRead::Item(item) => Ok(Some(item)),
        StreamRead::End => Ok(None),
        StreamRead::Failed(message) => Err(ProviderError::transport(message)),
        StreamRead::Idle => Err(ProviderError::transport(format!(
            "stream idle timeout: no data for {}s",
            idle.as_secs()
        ))),
    }
}

/// Longest raw-snippet prefix (in characters) echoed back in a truncated
/// tool-input error — enough to identify the call without dumping a
/// multi-kilobyte partial payload into the log.
const TOOL_INPUT_SNIPPET_CHARS: usize = 200;

/// Build the terminal error for a streamed tool call whose argument JSON was
/// cut off at the output-token limit before it finished. Shared by every
/// adapter that assembles tool calls from stream fragments: `provider` names
/// the concrete backend, `limit_signal` the wire-level evidence
/// (`stop_reason=max_tokens`, `finish_reason=length`). Non-retryable:
/// re-issuing the identical request re-truncates identically (the very
/// "stuck-loop" this error replaces), so the actionable fix — a larger
/// `max_output_tokens` or a smaller payload — is surfaced in the message
/// along with a bounded snippet of the RAW accumulated string.
pub(crate) fn truncated_tool_input_error(
    provider: &str,
    name: &str,
    raw: &str,
    limit_signal: &str,
) -> ProviderError {
    let total_chars = raw.chars().count();
    let snippet: String = raw.chars().take(TOOL_INPUT_SNIPPET_CHARS).collect();
    let ellipsis = if total_chars > TOOL_INPUT_SNIPPET_CHARS {
        "…"
    } else {
        ""
    };
    ProviderError::Terminal(format!(
        "{provider} tool call `{name}` was cut off at the output-token limit \
         ({limit_signal}) before its arguments finished streaming — the \
         accumulated JSON is incomplete and cannot be parsed ({total_chars} chars: \
         `{snippet}{ellipsis}`). Raise max_output_tokens or have the model emit a \
         smaller payload, then retry."
    ))
}

/// Build the retryable error for a stream whose body never delivered a first
/// byte inside `deadline`. One wording for every dialect: the fault is a
/// property of the streaming *path* (a proxy buffering the SSE body), not of
/// the request, so an adapter has nothing of its own to add beyond its name.
/// The caller classifies it as fallback-eligible — see
/// [`crate::stream_recovery`].
pub(crate) fn hung_before_first_byte(provider: &str, deadline: Duration) -> ProviderError {
    ProviderError::transport(format!(
        "{provider} stream hung before its first byte: no data within the {}s \
         first-byte deadline",
        deadline.as_secs()
    ))
}

/// Build the retryable error for a stream that reached EOF without its
/// protocol's terminal event (`terminal_event` names it: `message_stop`,
/// `response.completed`, `[DONE]`). A clean EOF is how close-delimited
/// HTTP/1.1 proxies, local gateways, and idle-reaping load balancers surface
/// a dropped connection — whatever accumulated is a half-answer, so it must
/// never be committed as a successful completion. Shared by every streaming
/// adapter so the disconnect classifies as retryable `Transport` uniformly.
pub(crate) fn stream_ended_before_terminal(provider: &str, terminal_event: &str) -> ProviderError {
    ProviderError::transport(format!(
        "{provider} stream ended before {terminal_event} — connection closed mid-response"
    ))
}

/// Package whatever accounting a dying stream had already accumulated, so it
/// rides out on the error instead of dying with the aggregator's stack frame.
///
/// This is the recovery half of the two error paths every streaming adapter
/// has — a mid-stream transport fault ([`next_with_timeout`]) and a clean EOF
/// with no terminal event ([`stream_ended_before_terminal`]). Both used to
/// discard a `CompletionUsage` that, on the Anthropic-shaped dialects, already
/// held the provider's own exact input and cache figures from the opening
/// frame. The turn was then recorded as if the attempt had been free.
///
/// Two deliberate choices about truth:
///
/// - `reported` is forced to `false`. The dialects that stream a *running*
///   output count (Anthropic's `message_delta`) set it mid-flight, so a
///   partial can arrive here already flagged as authoritative; letting that
///   through would launder a half-answer into the settled-accounting path.
/// - A zero `output_tokens` is replaced by an estimate over the text that did
///   arrive. Those tokens were generated and are billable, and a zero we know
///   to be wrong is worse than an approximation we can label — the estimate is
///   never mistaken for provider truth because `input_reported` describes only
///   the input side and `reported` stays false regardless.
///
/// Returns `None` when nothing was observed at all. That case is real and
/// common on the OpenAI-shaped dialects, which send usage only in the final
/// chunk: a stream cut early there has no counts and no text. Attaching a
/// zeroed envelope would be strictly worse than attaching nothing, because
/// downstream it is indistinguishable from a genuine call that cost nothing —
/// the exact misreading this whole change exists to stop.
pub(crate) fn partial_usage(
    usage: &CompletionUsage,
    text: &str,
    pricing: Option<&Pricing>,
) -> Option<PartialUsage> {
    if usage.input_tokens == 0 && usage.output_tokens == 0 && text.is_empty() {
        return None;
    }
    let input_reported = usage.input_tokens > 0;
    let mut observed = *usage;
    observed.reported = false;
    if observed.output_tokens == 0 {
        observed.output_tokens = estimate_tokens(text);
    }
    Some(PartialUsage {
        cost_usd: pricing.map_or(0.0, |p| p.cost_usd(&observed)),
        usage: observed,
        input_reported,
    })
}

/// Hang whatever [`partial_usage`] salvaged onto `error`, leaving it untouched
/// when there was nothing to salvage. The one call every streaming adapter
/// makes at its two loss sites.
pub(crate) fn attach_partial(
    error: ProviderError,
    usage: &CompletionUsage,
    text: &str,
    pricing: Option<&Pricing>,
) -> ProviderError {
    match partial_usage(usage, text, pricing) {
        Some(partial) => error.with_partial(partial),
        None => error,
    }
}

#[cfg(test)]
mod tests;
