//! Z.ai adapter — OpenAI-compatible chat completions + SSE streaming, GLM
//! 5.2's tool-call dialect (`openai-json`: an accumulating `tool_calls`
//! array keyed by index, arguments streamed as string fragments).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stella_protocol::{
    CompletionMessage, CompletionRequestRef, CompletionResult, MessageRole, ProviderError,
    ReasoningEffort,
};
// Used by the assembled result the delivery-path modules now own, but still
// named unqualified throughout the test tree, which imports `super::*`.
#[cfg(test)]
use stella_protocol::{CompletionUsage, FinishReason, ToolCall};

use crate::catalog::{Catalog, Pricing};
use crate::credential::ApiKey;
use crate::http;
use crate::provider::{Provider, ToolCallObserver};
use crate::stream_recovery::{StreamFault, StreamRecovery};

pub(crate) mod effort;
mod stream;
mod unary;
use effort::{xai_reasoning_effort, xai_supports_reasoning_effort, zai_reasoning_effort};
// `map_zai_effort` is asserted on directly by the wire tests, which reach it
// through this module's namespace like every other helper here. Its xAI sibling
// is deliberately absent: nothing outside `effort.rs` calls it, and re-exporting
// it unused fails clippy's `-D warnings`.
#[cfg(test)]
use effort::map_zai_effort;

/// International endpoint. `open.bigmodel.cn` (mainland) is the same wire
/// shape behind a different base URL — `with_base_url` covers both without
/// a second adapter, as does the GLM Coding Plan endpoint
/// (`/api/coding/paas/v4`). Base-URL *policy* — including the
/// `ZAI_GLM_CODING_PLAN=1` toggle — lives in one place,
/// `stella-cli`'s `Config::effective_base_url`; a copy here would silently
/// drift from it, and the factory overrides the constructor's default with
/// the resolved URL on every build anyway.
const DEFAULT_BASE_URL: &str = "https://api.z.ai/api/paas/v4";

pub struct ZaiProvider {
    client: reqwest::Client,
    api_key: ApiKey,
    base_url: String,
    model: String,
    /// List pricing for `model`, resolved from the catalog at construction so
    /// `cost_usd` is computed on the real request path. `None` when the slug
    /// isn't in the catalog — for seeded providers `build_provider`
    /// (`factory.rs`) rejects that case up front; for gateway providers with
    /// free-form slugs (OpenRouter) it is legitimately `None` and the cost
    /// comes from the gateway's own usage accounting instead.
    pricing: Option<Pricing>,
    id: String,
    label: String,
    /// Extra headers sent with every request — OpenRouter's app-attribution
    /// pair (`HTTP-Referer`, `X-Title`). Empty for every other provider.
    extra_headers: Vec<(&'static str, String)>,
    /// Ask the gateway to report the request's actual cost in the final
    /// usage frame (OpenRouter `usage: {"include": true}`). When the frame
    /// carries a cost, it overrides catalog list pricing — the gateway
    /// routed the call, only it knows what the call cost.
    usage_accounting: bool,
    /// Upstreams this provider is pinned to, in preference order, sent as
    /// OpenRouter's `provider.order` with fallbacks refused. Empty means
    /// unpinned — the gateway routes wherever it likes, which is the shipped
    /// default and is only safe when nobody is comparing two runs.
    upstream_pin: Vec<String>,
    /// Session-stable sticky-routing key, sent as OpenRouter's top-level
    /// `session_id` (only for the `openrouter` identity — see the field on
    /// [`ZaiRequest`]). One id per provider construction = one id per agent
    /// run, so every turn of a session pins to the same upstream provider and
    /// reuses the prompt cache the previous turn paid to write; distinct per
    /// construction, so fleet siblings don't serialize on one shard. Mirrors
    /// the OpenAI adapter's `prompt_cache_key` lifecycle. Volatile by design:
    /// it rides as a request parameter and never enters the cached bytes.
    session_id: String,
    /// The streaming→non-streaming fallback latch (#2686): armed when a
    /// stream hangs before its first byte or comes back empty, consulted per
    /// attempt so the retry of a faulted attempt goes out unary. See
    /// [`crate::stream_recovery`] for the state machine and its bounds.
    recovery: StreamRecovery,
    /// The client the unary fallback dispatches through. Separate from
    /// [`Self::client`] because a non-streaming call has no first token to
    /// reset the per-read clock: the whole generation must fit inside one
    /// read, so it needs [`http::unary_client`]'s 600s bound where the
    /// streaming client's 120s per-chunk bound would fail every completion
    /// slower than two minutes as retryable Transport (#547's lesson,
    /// learned on Bedrock).
    unary_client: reqwest::Client,
    /// [`http::FIRST_BYTE_TIMEOUT`] in production; a field so the
    /// hung-stream path is testable in milliseconds (the same reason
    /// `next_with_timeout` takes `idle` as a parameter).
    first_byte_deadline: Duration,
}

/// Process-wide monotonic suffix guaranteeing two [`ZaiProvider`]
/// constructions in the same process — fleet siblings built back-to-back —
/// get distinct session ids even when the nanosecond clock reads identically
/// for both. Without it, a tight builder loop could mint colliding ids and
/// serialize the whole fleet onto one cache shard, the opposite of the point.
static SESSION_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh session id, `stella-<pid>-<nanos>-<seq>`. Well under OpenRouter's
/// 256-char limit. The pid+nanos pair scopes it to this run; the atomic seq
/// makes same-nanos siblings provably distinct.
fn new_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("stella-{}-{nanos:x}-{seq:x}", std::process::id())
}

/// Whether `base_url` addresses OpenRouter — `openrouter.ai` itself or any
/// subdomain of it. Host-exact on purpose: matched on the parsed host, never
/// by substring over the whole URL, so `https://openrouter.ai.evil.example`
/// and a path that merely mentions the gateway both stay non-matches (this
/// gates what the request BODY carries, not where it is sent, but a sloppy
/// match would still leak OpenRouter-only fields onto servers that 400 on
/// unknown keys).
///
/// Public because "is this the gateway?" is asked in two places that must
/// agree: here, gating the request body, and `stella-cli`'s boot notice for a
/// gateway running with routing unpinned. Answering it twice is how the cache
/// opt-in came to be gated on the id alone and silently ran Claude routes
/// uncached through a custom provider entry (invariant 8).
pub fn is_openrouter_endpoint(base_url: &str) -> bool {
    let after_scheme = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Strip userinfo and port; an IPv6 literal contains ':' but can never be
    // openrouter.ai, so the split is safe for the hosts this can match.
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split(':').next().unwrap_or(host);
    host.eq_ignore_ascii_case("openrouter.ai")
        || host.to_ascii_lowercase().ends_with(".openrouter.ai")
}

impl ZaiProvider {
    pub fn new(api_key: ApiKey, model: impl Into<String>) -> Self {
        let model = model.into();
        // Scoped to `zai`, the identity this constructor mints — the same slug
        // legitimately exists under several providers once `stella models
        // refresh` merges the master list, and an unscoped `resolve` takes
        // whichever row sits first. `with_identity` re-resolves scoped to the
        // identity it switches to, for exactly the same reason.
        let catalog = Catalog::current();
        let pricing = catalog.resolve_for("zai", &model).ok().map(|e| e.pricing);
        Self {
            client: http::client(),
            api_key,
            base_url: DEFAULT_BASE_URL.to_string(),
            model,
            pricing,
            id: "zai".to_string(),
            label: "Z.ai".to_string(),
            extra_headers: Vec::new(),
            usage_accounting: false,
            upstream_pin: Vec::new(),
            session_id: new_session_id(),
            recovery: StreamRecovery::default(),
            unary_client: http::unary_client(),
            first_byte_deadline: http::FIRST_BYTE_TIMEOUT,
        }
    }

    /// Shrink the first-byte deadline so the hung-stream fallback is
    /// testable in milliseconds instead of 90 seconds of wall clock.
    #[cfg(test)]
    pub(crate) fn with_first_byte_deadline(mut self, deadline: Duration) -> Self {
        self.first_byte_deadline = deadline;
        self
    }

    /// The session-stable sticky-routing id minted for this construction.
    /// Test-only: the fleet-distinctness witness inspects it on the builder
    /// (no live gateway), and the wire-gating tests assert it appears only on
    /// the `openrouter` identity's body.
    #[cfg(test)]
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Point this adapter at another OpenAI-compatible endpoint — the
    /// mainland Z.ai host, a gateway, a local server, or a mock in tests.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Whether this instance's requests actually land on OpenRouter — by the
    /// built-in `openrouter` identity, or by a settings-defined custom
    /// provider whose base URL points at the gateway.
    ///
    /// The cache opt-in (`cache_control` + `session_id`) keys off this rather
    /// than the identity string: the OpenRouter cache is explicit opt-in, so
    /// an id-only gate silently ran Claude-via-OpenRouter with ZERO caching
    /// whenever the user reached the gateway through a custom provider entry
    /// — the exact defect the parity matrix (invariant 8) was born from. The
    /// reasoning/attribution fields keep their identity gates: they are
    /// behavior preferences, not billing, and widening them is a separate
    /// decision.
    fn serves_openrouter(&self) -> bool {
        self.id == "openrouter" || is_openrouter_endpoint(&self.base_url)
    }

    /// OpenRouter app attribution: `HTTP-Referer` names the app's site,
    /// `X-Title` its display name (shown on openrouter.ai rankings and in
    /// the user's activity feed). Harmless on any other OpenAI-compatible
    /// endpoint, but only the OpenRouter build path sets it.
    #[must_use]
    pub fn with_attribution(
        mut self,
        referer: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        self.extra_headers.push(("HTTP-Referer", referer.into()));
        self.extra_headers.push(("X-Title", title.into()));
        self
    }

    /// Request per-call cost reporting in the stream's final usage frame
    /// (OpenRouter `usage: {"include": true}`). A reported cost overrides
    /// catalog list pricing in `CompletionResult::cost_usd`, which is what
    /// makes budget metering real for a gateway whose routed models (and
    /// therefore prices) are not in our seed catalog.
    #[must_use]
    pub fn with_usage_accounting(mut self) -> Self {
        self.usage_accounting = true;
        self
    }

    /// Pin the gateway to these upstreams, in preference order, and forbid it
    /// from falling back to any other (OpenRouter `provider.order` +
    /// `allow_fallbacks: false`).
    ///
    /// This is what makes a gateway run *comparable*. Without it the upstream
    /// is chosen per app identity and can differ between two runs — or between
    /// two trials of the same run — while both record the same provider id, so
    /// a head-to-head silently varies the model provider it claims to hold
    /// fixed. Empty order leaves routing to the gateway, which is the default.
    #[must_use]
    pub fn with_upstream_pin(mut self, order: Vec<String>) -> Self {
        self.upstream_pin = order;
        self
    }

    /// Re-identify this adapter for another OpenAI-*compatible* provider it
    /// is serving (xAI, DeepSeek, OpenRouter, a local endpoint): `id` is
    /// what `Provider::id()` reports and `label` is the human name used in
    /// error messages. Without this, every gateway routed through the
    /// shared Chat Completions adapter misreported itself as Z.ai — an
    /// xAI 401 read "Z.ai rejected the API key", pointing the user at the
    /// wrong credential.
    #[must_use]
    pub fn with_identity(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.id = id.into();
        self.label = label.into();
        // Re-resolve pricing scoped to the real provider id: the same bare
        // slug can exist on more than one provider, and a provider-scoped
        // catalog row (e.g. an xAI or DeepSeek row from `stella models
        // refresh`) is authoritative over whatever the unscoped constructor
        // lookup found — or didn't find — under the default `zai` identity.
        let catalog = Catalog::current();
        if let Ok(entry) = catalog.resolve_for(&self.id, &self.model) {
            self.pricing = Some(entry.pricing);
        }
        self
    }
}

// ── Wire types (OpenAI-compatible chat/completions) ─────────────────────

#[derive(Serialize)]
struct ZaiRequest<'a> {
    model: &'a str,
    messages: Vec<ZaiMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    /// Sampling overrides from `CompletionRequest.params`, each skipped when
    /// `None` so a request without overrides serializes byte-identical to
    /// what this adapter has always sent (the prompt-cache contract).
    /// `top_p`/`frequency_penalty`/`presence_penalty`/`seed` are standard
    /// Chat Completions parameters; `top_k` and `repetition_penalty` are
    /// non-standard but accepted by OpenRouter and local servers — they are
    /// forwarded whenever `Some` because the user explicitly opted in, and an
    /// honest provider error beats silently dropping an explicit setting.
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repetition_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    /// GLM's native thinking switch (`{"type": "enabled"|"disabled"}`) —
    /// only sent when this adapter is serving Z.ai itself AND the caller set
    /// `CompletionRequest.reasoning`; other Chat Completions servers don't
    /// speak this field and `None` keeps their wire untouched.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ZaiThinking>,
    /// OpenRouter's normalized reasoning control — only sent when this
    /// adapter is re-identified as OpenRouter, which translates it to
    /// whatever the routed upstream vendor calls it. See
    /// [`openrouter_reasoning`] for the effort/enabled shape rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<OpenRouterReasoning>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ZaiToolSchema>,
    /// OpenRouter usage accounting (`{"include": true}`) — omitted entirely
    /// unless the provider was built `with_usage_accounting`, so the shared
    /// adapter never sends a field other Chat Completions servers might
    /// reject.
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<ZaiUsageInclude>,
    /// OpenRouter's request-root automatic prompt-caching switch
    /// (`{"type": "ephemeral"}`): the gateway places a cache breakpoint at
    /// the last cacheable block and advances it as the conversation grows.
    /// Anthropic models routed through OpenRouter get NO prompt caching
    /// without it — their cache is explicit opt-in, unlike the implicit
    /// OpenAI/Gemini/DeepSeek caches — so every agent turn re-billed the
    /// full growing prefix at the uncached input rate and pinned the
    /// cache-hit stat at zero. Providers with implicit caches ignore the
    /// field (OpenRouter normalizes it per upstream), so it is sent on
    /// every OpenRouter request; no other Chat Completions server speaks
    /// it, so any other identity keeps it off the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<ZaiCacheControl>,
    /// OpenRouter's session-stable sticky-routing key (top-level `session_id`,
    /// ≤256 chars): the gateway routes every request carrying the same id to
    /// the same upstream provider + cache shard, so consecutive agent turns
    /// reuse the prompt cache instead of landing on a fresh endpoint that
    /// re-bills the whole prefix. Without it OpenRouter derives a routing key
    /// from message hashing, which drifts as the conversation grows — the
    /// exact reason a session's later turns miss the cache the earlier ones
    /// wrote. Sent only for the `openrouter` identity (same 400-risk gating as
    /// `reasoning`/`cache_control`); every other identity keeps it off the
    /// wire. See the field on [`ZaiProvider`] for the one-per-session,
    /// distinct-per-sibling lifecycle.
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    /// xAI's chat-completions reasoning control: a top-level `reasoning_effort`
    /// string (`low`/`medium`/`high`, OpenAI-compatible). Grok's reasoning
    /// models honor it; only sent when this adapter is serving the `xai`
    /// identity AND the caller pinned an `effort`/`reasoning` preference. Every
    /// other identity behind this shared adapter keeps it off the wire (same
    /// 400-risk gating as `reasoning`/`cache_control`/`session_id`) — GLM,
    /// DeepSeek, and local servers don't speak it, and an unknown key would
    /// risk a hard 400. See [`xai_reasoning_effort`] for the shape rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    /// OpenRouter's routing preferences — sent only when the operator pinned
    /// an upstream, and only when this adapter is actually addressing the
    /// gateway. Absent means "let the gateway choose", which is the right
    /// default for interactive use and the wrong one for a measured run.
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<OpenRouterProviderPin<'a>>,
}

/// OpenRouter's `provider` routing object, in the one shape this adapter
/// sends: an explicit upstream order with fallbacks refused.
///
/// `allow_fallbacks: false` is the whole point and is not configurable. A pin
/// that silently falls back to a second vendor when the first is busy is not
/// a pin — it is a preference, and it would reintroduce exactly the
/// uncontrolled variable the caller asked to remove, at the least convenient
/// moment (under load, mid-run, on some trials and not others). Refusing the
/// call is the honest failure: a 404 from the gateway is a measurable, fixable
/// event, while a quietly re-routed trial is a corrupted data point that looks
/// identical to a clean one.
#[derive(Serialize)]
struct OpenRouterProviderPin<'a> {
    order: &'a [String],
    allow_fallbacks: bool,
}

/// GLM's request-level thinking object: `{"type": "enabled"}` /
/// `{"type": "disabled"}`. Z.ai's default depends on the model, so the field
/// is only sent when the caller expressed an explicit preference —
/// `CompletionRequest.reasoning == None` keeps the provider default AND the
/// pre-field wire bytes.
#[derive(Serialize)]
struct ZaiThinking {
    #[serde(rename = "type")]
    kind: &'static str,
}

/// OpenRouter's `reasoning` object. Exactly one field is set per request:
/// `effort` when the caller pinned a level, `enabled` for a bare on/off —
/// sending both would be redundant (effort implies enabled) and `enabled:
/// false` with an effort would be contradictory.
#[derive(Serialize)]
struct OpenRouterReasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
}

/// Map the engine's one `ReasoningEffort` enum to OpenRouter's
/// `reasoning.effort`.
///
/// This used to collapse `xhigh`/`max` down to `high`, matching
/// `openai.rs::map_reasoning_effort`, because the gateway modelled only
/// `low`/`medium`/`high`. It no longer does: OpenRouter documents the full
/// `none`/`minimal`/`low`/`medium`/`high`/`xhigh`/`max` ladder and normalizes
/// it onto whatever the routed model exposes. Keeping the collapse meant the
/// top two tiers were a lie — a user pinning `xhigh` got `high` on the wire,
/// silently, with nothing to distinguish it from asking for `high`.
///
/// Sending a tier the routed model does not itself advertise under
/// `reasoning.supported_efforts` is safe: the gateway normalizes rather than
/// rejects (verified against `moonshotai/kimi-k3`, which advertises only
/// `max`/`high`/`low` and accepts `xhigh` without error). That matters here
/// because the OpenRouter default posture pins `xhigh` for every routed
/// model, and a 400 on an unadvertised tier would break every turn.
fn map_openrouter_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}

/// The output allowance a request gets when the caller pinned none, matching
/// `anthropic.rs`'s reasoning-aware default so the two adapters cannot
/// disagree about what an un-capped reasoning turn is allowed to spend.
///
/// Reasoning spends from the SAME allowance as the answer. A ceiling sized for
/// a non-thinking reply truncates a thinking model mid-thought, and on the
/// Chat Completions dialects that lands as a `finish_reason: length` turn that
/// carries chain-of-thought and no tool call — the shape that ended 30 of 30
/// cap-hit steps in the 2026-07-31 Terminal-Bench bundle with zero tool calls.
///
/// A caller-set cap is still honored verbatim: this only fills the hole where
/// the caller expressed no preference, which is exactly where a silent 4k-ish
/// provider default would otherwise decide the turn's fate.
fn reasoning_aware_max_tokens(requested: Option<u32>, reasoning: Option<bool>) -> Option<u32> {
    requested.or(match reasoning {
        Some(true) => Some(32_000),
        _ => None,
    })
}

/// Whether a terminal provider error says reasoning cannot be turned off.
/// Matched on the upstream's own wording rather than a status code: the same
/// 400 covers every other malformed-request cause, and only this one is
/// recoverable by resending without the field.
fn rejects_disabled_reasoning(detail: &str) -> bool {
    let detail = detail.to_lowercase();
    detail.contains("reasoning")
        && (detail.contains("cannot be disabled")
            || detail.contains("can not be disabled")
            || detail.contains("is mandatory")
            || detail.contains("mandatory for this endpoint"))
}

/// Build OpenRouter's `reasoning` object from the request's reasoning/effort
/// pair. Rules: an explicit off (`reasoning == Some(false)`) always wins and
/// sends `{"enabled": false}` — a pinned effort must not resurrect thinking
/// the caller suppressed; otherwise a pinned effort sends
/// `{"effort": …}` (implicitly enabling); a bare `Some(true)` with no effort
/// sends `{"enabled": true}` and lets the gateway pick the level; and
/// neither set keeps the field off the wire entirely (provider default,
/// byte-stable with the pre-field body).
fn openrouter_reasoning(
    reasoning: Option<bool>,
    effort: Option<ReasoningEffort>,
) -> Option<OpenRouterReasoning> {
    match (reasoning, effort) {
        (Some(false), _) => Some(OpenRouterReasoning {
            effort: None,
            enabled: Some(false),
        }),
        (_, Some(effort)) => Some(OpenRouterReasoning {
            effort: Some(map_openrouter_effort(effort)),
            enabled: None,
        }),
        (Some(true), None) => Some(OpenRouterReasoning {
            effort: None,
            enabled: Some(true),
        }),
        (None, None) => None,
    }
}

#[derive(Serialize)]
struct ZaiUsageInclude {
    include: bool,
}

/// OpenRouter's cache-control object, `{"type": "ephemeral"}` — the 5-minute
/// default TTL. The 1-hour variant costs 2x input on writes (vs 1.25x) and
/// only pays off for sessions idle between turns, which an agent loop never
/// is.
#[derive(Serialize)]
struct ZaiCacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct ZaiMessage {
    role: &'static str,
    content: ZaiContent,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ZaiOutboundToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

/// Message content in the OpenAI-compatible chat dialect: a plain string for
/// text-only turns (byte-stable with what this adapter has always sent —
/// the prompt-cache contract), or a part array when a user turn carries
/// attachments.
#[derive(Serialize, Debug, PartialEq)]
#[serde(untagged)]
enum ZaiContent {
    Text(String),
    Parts(Vec<ZaiContentPart>),
}

impl ZaiContent {
    /// The plain-text form, for assertions and logging; a parts array is not
    /// plain text and yields `""`.
    #[cfg(test)]
    fn as_text(&self) -> &str {
        match self {
            ZaiContent::Text(text) => text,
            ZaiContent::Parts(_) => "",
        }
    }
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ZaiContentPart {
    Text { text: String },
    ImageUrl { image_url: ZaiImageUrl },
}

#[derive(Serialize, Debug, PartialEq)]
struct ZaiImageUrl {
    /// A `data:` URI carrying the base64 payload.
    url: String,
}

/// GLM *vision* models ingest images via `image_url` parts; PDFs, audio, and
/// video degrade to descriptive text notes in this dialect.
///
/// `images` is deliberately NOT a constant. This dialect is OpenAI-compatible
/// and serves two very different populations — Z.ai's own GLM slugs and every
/// free-form slug OpenRouter routes — and the image cap is a property of the
/// *model*, not the wire format. Z.ai's text-only GLM endpoints reject a
/// content array outright (`1210: messages.content.type is invalid, allowed
/// values: ['text']`), which is a **terminal** provider error: one pasted
/// screenshot kills the whole turn. See [`caps_for`].
fn caps_for(model: &str) -> crate::attachment::DialectCaps {
    crate::attachment::DialectCaps {
        images: model_ingests_images(model),
        pdfs: false,
        audio: false,
        video: false,
    }
}

/// Whether `model` can carry image parts on this dialect.
///
/// The rule is asymmetric on purpose, because the two failure modes are not
/// symmetric. Sending an image to a text-only model is a terminal HTTP 400 —
/// the turn dies and the user's prompt is lost. Withholding one from a model
/// that could have seen it degrades to a note that *names the file*, so the
/// model can still open it with a tool and the turn survives. So: deny where
/// we positively know, permit everywhere else.
///
/// "Positively know" is the GLM family, whose vision variants are marked in
/// the slug by a `v` in the version segment (`glm-4v`, `glm-4.1v`,
/// `glm-4.5v`). A GLM slug without one is a text model — `glm-4.6`, `glm-5`,
/// `glm-5.2` — and gets the deny. Every non-GLM slug on this dialect arrives
/// via a gateway that accepts image parts for its vision models (Anthropic,
/// OpenAI, Gemini) and would regress if we guessed, so it stays permitted.
///
/// The prefix match is on the *last* path segment so gateway-qualified slugs
/// (`zai/glm-5.2`, `openrouter/z-ai/glm-4.5v`) classify the same as bare ones.
fn model_ingests_images(model: &str) -> bool {
    let slug = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    let Some(version) = slug.strip_prefix("glm-") else {
        // Not a GLM model — some other provider behind an OpenAI-compatible
        // gateway. Unknown, so permitted (see the asymmetry above).
        return true;
    };
    // The version segment runs up to the first `-` (`4.5v-flash` → `4.5v`).
    // A trailing `v` there is the vision marker.
    version
        .split('-')
        .next()
        .is_some_and(|v| v.ends_with('v') || v.ends_with("v0"))
}

/// Content for a user turn: a plain string when there are no attachments,
/// otherwise a parts array with media before text.
fn user_content(message: &CompletionMessage, model: &str) -> ZaiContent {
    if message.attachments.is_empty() {
        return ZaiContent::Text(message.content.clone());
    }
    let mut parts: Vec<ZaiContentPart> =
        crate::attachment::wire_parts(&message.attachments, caps_for(model))
            .into_iter()
            .map(attachment_part)
            .collect();
    if !message.content.is_empty() {
        parts.push(ZaiContentPart::Text {
            text: message.content.clone(),
        });
    }
    ZaiContent::Parts(parts)
}

/// One resolved part as a GLM content part.
fn attachment_part(part: crate::attachment::WirePart) -> ZaiContentPart {
    match part {
        crate::attachment::WirePart::Image { media_type, base64 } => ZaiContentPart::ImageUrl {
            image_url: ZaiImageUrl {
                url: format!("data:{media_type};base64,{base64}"),
            },
        },
        crate::attachment::WirePart::Text { text } => ZaiContentPart::Text { text },
        // Excluded by `caps_for` today; turning one of those caps on without
        // adding a part arm lands here — degrade, never abort the turn.
        part @ (crate::attachment::WirePart::Pdf { .. }
        | crate::attachment::WirePart::Audio { .. }
        | crate::attachment::WirePart::Video { .. }) => ZaiContentPart::Text {
            text: crate::attachment::unsupported_part_note(&part, "Z.ai chat API"),
        },
    }
}

/// An assistant-authored tool call echoed back in conversation history
/// (OpenAI-compatible dialect requires the assistant message to carry the
/// calls its tool results answer).
#[derive(Serialize)]
struct ZaiOutboundToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: ZaiOutboundFunction,
}

#[derive(Serialize)]
struct ZaiOutboundFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ZaiToolSchema {
    #[serde(rename = "type")]
    kind: &'static str,
    function: ZaiFunctionSchema,
}

#[derive(Serialize)]
struct ZaiFunctionSchema {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Deserialize, Debug)]
struct ZaiStreamChunk {
    #[serde(default)]
    choices: Vec<ZaiStreamChoice>,
    #[serde(default)]
    usage: Option<ZaiUsage>,
    /// The upstream OpenRouter routed this call to, which it names on every
    /// chunk. Absent on every direct endpoint. Recorded rather than merely
    /// trusted, because "which silicon served this trial?" must be answerable
    /// from the trace and not from the config that was *meant* to be in force.
    #[serde(default)]
    provider: Option<String>,
    /// An in-band error frame. The OpenAI-compatible gateways can emit
    /// `data: {"error":{...}}` mid-stream after already sending some content
    /// deltas — without this field it deserialized into an all-default,
    /// empty `ZaiStreamChunk` and was silently swallowed, returning the
    /// truncated text so far as a bogus success.
    #[serde(default)]
    error: Option<ZaiStreamError>,
}

/// An OpenAI-compatible in-band error object. Classification reads
/// `type`/`message` text plus `code` — the gateways don't share a stable
/// machine code *vocabulary*, so human-readable text is still the primary
/// signal, but when a numeric status is present it is the most reliable part
/// of the frame and must not be dropped.
#[derive(Deserialize, Debug, Default)]
struct ZaiStreamError {
    #[serde(default)]
    message: String,
    #[serde(default, rename = "type")]
    kind: String,
    /// OpenRouter puts the upstream HTTP status here and nowhere else:
    /// `{"error":{"message":"Provider returned an empty response","code":502}}`.
    /// Without this field the status never reaches the haystack below, so the
    /// `contains("502")` arm could not fire and a transient upstream 5xx fell
    /// through to non-retryable `Terminal` — burning the turn on attempt 1
    /// with all three configured retries unused. Untyped (`Value`) because
    /// the gateways disagree on whether it is a number or a string.
    #[serde(default)]
    code: Option<serde_json::Value>,
}

impl ZaiStreamError {
    /// The lowercased prose the word-based classification matches against:
    /// `type` and `message` only. Numeric statuses are matched against
    /// [`Self::code_text`] exactly — never against this text, where a message
    /// quoting a token count ("maximum context length is 500000 tokens")
    /// would substring-match "500" and turn a terminal request error into a
    /// retry storm that re-pays the prompt on every attempt.
    fn haystack(&self) -> String {
        format!("{} {}", self.kind, self.message).to_lowercase()
    }

    /// The stringified `code`, unquoted so `"502"` and `502` compare equal.
    fn code_text(&self) -> String {
        match &self.code {
            Some(serde_json::Value::String(s)) => s.trim().to_lowercase(),
            Some(other) => other.to_string(),
            None => String::new(),
        }
    }
}

/// Classify an in-band OpenAI-compatible error frame into a typed
/// `ProviderError`. Transient/server-side conditions (overloaded, 5xx,
/// unavailable, timeout, aborted) are **retryable** `Transport`; an explicit
/// rate limit is `RateLimited`; everything else is `Terminal`. The gateways don't
/// share a stable machine code, so this matches on the human-readable
/// `type`/`message` text — deliberately conservative: unknown ⇒ terminal, so
/// a genuine failure is never retried into an infinite loop. `label` names
/// the concrete provider (Z.ai / xAI / DeepSeek / …) so the surfaced message
/// points at the right credential and endpoint.
fn classify_zai_stream_error(err: &ZaiStreamError, label: &str) -> ProviderError {
    let haystack = err.haystack();
    let code = err.code_text();
    let detail = if err.message.is_empty() {
        format!("{label} stream error ({})", err.kind)
    } else {
        format!("{label} stream error: {}", err.message)
    };
    if haystack.contains("overload")
        || haystack.contains("server_error")
        || haystack.contains("unavailable")
        || haystack.contains("timeout")
        // An abort is the upstream connection dropping mid-stream, not a
        // decision about the request — so it is transient by construction and
        // the identical retry can succeed. OpenRouter phrases it "The
        // operation was aborted" and attaches NO `code`, so neither the words
        // above nor the statuses below could catch it: the same shape as the
        // `code: 502` frame this arm's sibling was added for, one rung further
        // out, and it burned the turn on attempt 1 with all retries unused.
        || haystack.contains("aborted")
        || code == "500"
        || code == "502"
        || code == "503"
        || code == "529"
    {
        ProviderError::transport(detail)
    } else if code == "429" || (haystack.contains("rate") && haystack.contains("limit")) {
        // A `code: 429` frame carries no "rate limit" prose on some gateways,
        // so the numeric status is the only signal that this is throttling
        // rather than a permanent rejection.
        ProviderError::RateLimited {
            message: detail,
            retry_after_ms: None,
        }
    } else {
        ProviderError::Terminal(detail)
    }
}

/// The `error` object Z.ai returns in a *non-streamed* HTTP error body:
/// `{"error":{"code":"1113","message":"Insufficient balance ..."}}`. Distinct
/// from [`ZaiStreamError`] (the in-band SSE frame) — this is the top-level
/// envelope on a plain JSON 4xx response.
#[derive(Deserialize, Debug)]
struct ZaiErrorEnvelope {
    error: ZaiErrorBody,
}

#[derive(Deserialize, Debug, Default)]
struct ZaiErrorBody {
    #[serde(default)]
    message: String,
}

/// Classify an HTTP 429 by its body. Z.ai returns 429 both for real rate
/// limiting AND for account-balance/quota exhaustion (code 1113,
/// "Insufficient balance or no resource package. Please recharge") — the
/// latter is **terminal**: no amount of backoff refills an empty account, so
/// retrying it just delays a clear error and, worse, reports it as a rate
/// limit. Billing/quota text ⇒ [`ProviderError::Terminal`]; anything else ⇒
/// retryable [`ProviderError::RateLimited`], honoring a `Retry-After` hint
/// when the server sent one. The provider's own message is always carried
/// through so the real reason is visible instead of a hard-coded string —
/// bounded by [`http::body_snippet`], since the fallback when the body is not
/// the documented envelope is the RAW body, and a gateway fronted by a CDN
/// answers a 429 with a multi-kilobyte HTML page. Classification still reads
/// the full text; only the surfaced message is cut.
///
/// The billing-vs-throttle split generalizes past Z.ai — OpenAI-compatible
/// gateways answer an exhausted account with the same 429 and the same
/// vocabulary ("exceeded your current quota") — so this pre-check runs for
/// every identity on the shared adapter, and takes `label` for the same
/// reason [`ZaiProvider::with_identity`] exists: an OpenRouter throttle that
/// reported itself as "Z.ai HTTP 429" would point the user at a credential
/// and an endpoint they never configured.
fn classify_zai_429(label: &str, body: &str, retry_after_ms: Option<u64>) -> ProviderError {
    let detail = serde_json::from_str::<ZaiErrorEnvelope>(body)
        .ok()
        .map(|e| e.error.message)
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| body.trim().to_string());
    let haystack = detail.to_lowercase();
    let message = if detail.is_empty() {
        format!("{label} HTTP 429")
    } else {
        format!("{label} HTTP 429: {}", http::body_snippet(&detail))
    };
    // Balance/quota exhaustion — never self-clears, so terminal not retryable.
    if haystack.contains("balance")
        || haystack.contains("recharge")
        || haystack.contains("resource package")
        || haystack.contains("insufficient")
        || haystack.contains("quota")
        || haystack.contains("arrears")
    {
        ProviderError::Terminal(message)
    } else {
        ProviderError::RateLimited {
            message,
            retry_after_ms,
        }
    }
}

#[derive(Deserialize, Debug)]
struct ZaiStreamChoice {
    #[serde(default)]
    delta: ZaiStreamDelta,
    /// Why generation ended for this choice. `"length"` is the OpenAI-
    /// compatible signal that the output was cut off at `max_tokens` — the
    /// truncation marker a partially-streamed tool call needs so it can be
    /// reported rather than silently nulled.
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct ZaiStreamDelta {
    #[serde(default)]
    content: Option<String>,
    /// GLM streams chain-of-thought under `reasoning_content`, separate from
    /// the answer in `content`. Without deserializing it, a turn that spends
    /// its whole output budget reasoning (and is cut off before emitting any
    /// `content`) looks empty — the adapter returns no text and no tool call,
    /// and the driver used to record that as a clean completion (the "turn
    /// ends with no feedback" defect). Captured so it can be surfaced.
    #[serde(default)]
    reasoning_content: Option<String>,
    /// OpenRouter's normalized name for the same thing: reasoning models
    /// routed through the gateway stream chain-of-thought under `reasoning`,
    /// whatever the upstream vendor calls it. Folded into the same buffer as
    /// `reasoning_content` so a reasoning-only turn is visible regardless of
    /// which endpoint served it.
    #[serde(default)]
    reasoning: Option<String>,
    /// `Option` so an explicit `"tool_calls": null` is tolerated. Bare
    /// `#[serde(default)]` covers a MISSING field only — an explicit null
    /// fails the whole chunk, which is how a tool call turns into a silent
    /// no-op (see [`ZaiStreamToolCallDelta`]).
    #[serde(default)]
    tool_calls: Option<Vec<ZaiStreamToolCallDelta>>,
}

/// GLM 5.2's streamed tool-call shape: fragments keyed by `index`, with
/// `function.arguments` arriving as a partial JSON string across many
/// chunks — the exact dialect quirk this adapter exists to prove out.
///
/// `index` is optional purely defensively. It was the ONE non-defaulted field
/// in this whole tree, so a frame that omitted it — or sent it as a string —
/// failed to deserialize, and the frame was then dropped whole. A dropped
/// frame carrying tool calls is indistinguishable from a turn that simply had
/// none, and the driver ends a turn on `tool_calls.is_empty()`: the run would
/// report a clean, successful completion having executed nothing. Falling back
/// to the fragment's position in the chunk keeps the sequential-index contract
/// that `announce_completed_below` relies on.
#[derive(Deserialize, Debug)]
struct ZaiStreamToolCallDelta {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ZaiStreamFunctionDelta>,
}

#[derive(Deserialize, Debug, Default)]
struct ZaiStreamFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct ZaiUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    /// OpenAI-compatible cache detail: prompt tokens served from Z.ai's
    /// implicit (server-side, no opt-in) prompt cache. Unlike Anthropic's
    /// envelope these are already folded into `prompt_tokens`, so they map
    /// straight onto the normalized subset invariant (cached ⊆ input) and
    /// bill at the catalog's cheaper cached rate instead of full input.
    #[serde(default)]
    prompt_tokens_details: Option<ZaiPromptTokensDetails>,
    /// OpenRouter usage accounting: the request's actual cost in USD
    /// credits, present only when the request opted in (`usage.include`).
    /// Authoritative over catalog list pricing — the gateway routed the
    /// call, only it knows which upstream (at which price) served it.
    #[serde(default)]
    cost: Option<f64>,
    /// DeepSeek's native cache-hit field: the platform reports prompt-cache
    /// hits as TOP-LEVEL `prompt_cache_hit_tokens` (paired with
    /// `prompt_cache_miss_tokens`; hits + misses = `prompt_tokens`) and does
    /// NOT send an OpenAI-style `prompt_tokens_details` object. Absent on
    /// every other endpoint this adapter serves, where `serde(default)`
    /// keeps it 0.
    #[serde(default)]
    prompt_cache_hit_tokens: u64,
    /// The reasoning share of `completion_tokens`, when the endpoint breaks it
    /// out. `Option` all the way down: absent means the server never reported
    /// it, which is a different fact from a reported zero and must not
    /// collapse into one.
    #[serde(default)]
    completion_tokens_details: Option<ZaiCompletionTokensDetails>,
}

#[derive(Deserialize, Debug, Default)]
struct ZaiCompletionTokensDetails {
    /// Tokens spent thinking, already counted inside `completion_tokens`.
    /// Served by OpenRouter for every reasoning model and by the OpenAI-shaped
    /// endpoints that implement the same detail object; absent elsewhere.
    #[serde(default)]
    reasoning_tokens: Option<u64>,
}

#[derive(Deserialize, Debug, Default)]
struct ZaiPromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
    /// OpenRouter-only sibling of `cached_tokens`: tokens WRITTEN to the
    /// upstream's prompt cache by this call (Anthropic bills them at 1.25x
    /// input). Absent on every other OpenAI-compatible endpoint, where
    /// `serde(default)` keeps it 0 — matching the normalized envelope's
    /// "0 when never reported" contract.
    #[serde(default)]
    cache_write_tokens: u64,
}

/// `model` is threaded through only so user turns can resolve their image cap
/// against the model that will actually receive them ([`caps_for`]).
fn to_zai_messages(messages: &[CompletionMessage], model: &str) -> Vec<ZaiMessage> {
    let mut out = Vec::new();
    for message in messages {
        match message.role {
            MessageRole::System => out.push(ZaiMessage {
                role: "system",
                content: ZaiContent::Text(message.content.clone()),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }),
            MessageRole::User => out.push(ZaiMessage {
                role: "user",
                content: user_content(message, model),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }),
            MessageRole::Assistant => out.push(ZaiMessage {
                role: "assistant",
                content: ZaiContent::Text(message.content.clone()),
                tool_calls: message
                    .tool_calls
                    .iter()
                    .map(|call| ZaiOutboundToolCall {
                        id: call.call_id.clone(),
                        kind: "function",
                        function: ZaiOutboundFunction {
                            name: call.name.clone(),
                            arguments: call.input.to_string(),
                        },
                    })
                    .collect(),
                tool_call_id: None,
            }),
            // OpenAI-compatible dialect: one `role: "tool"` message per
            // result, each carrying the `tool_call_id` it answers.
            MessageRole::Tool => {
                for result in &message.tool_results {
                    let content = match &result.output {
                        stella_protocol::ToolOutput::Ok { content, .. } => content.clone(),
                        stella_protocol::ToolOutput::Error { message, .. } => {
                            format!("ERROR: {message}")
                        }
                    };
                    out.push(ZaiMessage {
                        role: "tool",
                        content: ZaiContent::Text(content),
                        tool_calls: Vec::new(),
                        tool_call_id: Some(result.call_id.clone()),
                    });
                }
            }
        }
    }
    out
}

fn to_zai_tools(tools: &[stella_protocol::tool::ToolSchema]) -> Vec<ZaiToolSchema> {
    tools
        .iter()
        .map(|tool| ZaiToolSchema {
            kind: "function",
            function: ZaiFunctionSchema {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            },
        })
        .collect()
}

#[async_trait]
impl Provider for ZaiProvider {
    fn id(&self) -> &str {
        &self.id
    }

    /// The slug this adapter was configured with, so a failed call can name
    /// it (#2831). For a gateway id (OpenRouter) this is the slug Stella
    /// ASKED for, which is the honest pre-dispatch answer — what the gateway
    /// actually routed to is only knowable from a result, and is not pinned
    /// or recorded here at all.
    fn model(&self) -> Option<&str> {
        Some(&self.model)
    }

    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.complete_inner(req, None).await
    }

    async fn complete_observed_ref(
        &self,
        req: CompletionRequestRef<'_>,
        observer: &dyn ToolCallObserver,
    ) -> Result<CompletionResult, ProviderError> {
        self.complete_inner(req, Some(observer)).await
    }
}

impl ZaiProvider {
    /// The one request/stream/aggregate body behind both `complete_ref` and
    /// `complete_observed_ref` — the observer is threaded down to the stream
    /// aggregator, which announces each tool call as soon as the next one
    /// starts streaming. That is the only per-call boundary this dialect
    /// emits mid-stream, and it leaves the turn's last call unannounced —
    /// see `ToolCallAccumulator::announced` (`zai/stream.rs`) for what that
    /// costs.
    async fn complete_inner(
        &self,
        req: CompletionRequestRef<'_>,
        observer: Option<&dyn ToolCallObserver>,
    ) -> Result<CompletionResult, ProviderError> {
        // Some upstreams OpenRouter fronts make reasoning mandatory and answer
        // `reasoning:{enabled:false}` with a hard 400. The caller's "off" is an
        // economy preference, never a correctness requirement — losing it must
        // not lose the call. Resend once, letting the endpoint apply its own
        // default. Same family of provider-quirk gate as the sampling-param and
        // thinking-shape omissions in the sibling adapters.
        let disables_reasoning = self.id == "openrouter" && req.reasoning == Some(false);
        // Keeping the request for a possible resend used to mean retaining a
        // copy of the whole replayed message history, so the fast path was
        // written to avoid paying for it. Since #921 the request is a borrowed
        // `Copy` view, so both arms are the same nothing and the branch stays
        // only because the *resend* is what needs guarding, not the copy.
        if !disables_reasoning {
            return self.complete_attempt(req, observer, false).await;
        }
        match self.complete_attempt(req, observer, false).await {
            Err(ProviderError::Terminal(detail)) if rejects_disabled_reasoning(&detail) => {
                self.complete_attempt(req, observer, true).await
            }
            other => other,
        }
    }

    /// One request/stream/aggregate cycle. `force_default_reasoning` drops the
    /// `reasoning` object from the body so an endpoint that mandates reasoning
    /// applies its own default instead of rejecting the request.
    ///
    /// Streams by default, but consults the fallback latch first: the retry
    /// of an attempt whose stream hung before its first byte or came back
    /// empty goes out as a **unary** request for the same payload instead
    /// (#2686) — see [`crate::stream_recovery`] for the latch's states and
    /// bounds, and [`unary`] for that path.
    async fn complete_attempt(
        &self,
        req: CompletionRequestRef<'_>,
        observer: Option<&dyn ToolCallObserver>,
        force_default_reasoning: bool,
    ) -> Result<CompletionResult, ProviderError> {
        if self.recovery.use_unary() {
            return self
                .complete_unary_attempt(req, force_default_reasoning)
                .await;
        }
        let body = self.build_body(req, force_default_reasoning, true);
        let response = self.dispatch(&self.client, &body, false).await?;
        let outcome = stream::aggregate_zai_stream(
            response,
            &self.label,
            observer,
            self.pricing.as_ref(),
            self.first_byte_deadline,
        )
        .await
        .map_err(|fault| self.absorb_stream_fault(fault))?;
        // A gateway-reported cost (OpenRouter usage accounting) is
        // authoritative; catalog list pricing is the estimate for providers
        // that don't report one.
        let cost_usd = outcome.reported_cost_usd.unwrap_or_else(|| {
            self.pricing
                .map(|p| p.cost_usd(&outcome.usage))
                .unwrap_or(0.0)
        });
        Ok(CompletionResult {
            text: outcome.text,
            tool_calls: outcome.tool_calls,
            usage: outcome.usage,
            model: self.model.clone(),
            cost_usd,
            finish_reason: outcome.finish_reason,
            upstream_provider: outcome.upstream_provider,
        })
    }

    /// Route a [`StreamFault`] back into the retry ladder. An eligible fault
    /// (hung before its first byte / empty stream) arms the fallback latch —
    /// the caller's retry of this attempt will go out unary — and its error
    /// is retryable Transport, so the ordinary retry machinery both drives
    /// that switch and bills this discarded attempt through its
    /// `UsageIncomplete` observer, exactly like every other doomed attempt.
    /// The message names the switch only when THIS fault armed the latch, so
    /// an error never promises a fallback some other attempt already made.
    fn absorb_stream_fault(&self, fault: StreamFault) -> ProviderError {
        if fault.fallback_eligible && self.recovery.note_stream_fault() {
            return match fault.error {
                ProviderError::Transport { message, partial } => ProviderError::Transport {
                    message: format!("{message}; retrying this attempt as a non-streaming request"),
                    partial,
                },
                other => other,
            };
        }
        fault.error
    }

    /// The one request body both delivery paths serialize — `stream` is the
    /// only field on which they differ, so the unary fallback re-issues the
    /// byte-identical payload minus the stream flag.
    fn build_body(
        &self,
        req: CompletionRequestRef<'_>,
        force_default_reasoning: bool,
        stream: bool,
    ) -> ZaiRequest<'_> {
        // "Include" semantics: every override is `None` unless the caller
        // set it, and `None` never reaches the wire — a request without
        // params serializes byte-identical to the pre-params body.
        let params = req.params.unwrap_or_default();
        ZaiRequest {
            model: &self.model,
            messages: to_zai_messages(req.messages, &self.model),
            stream,
            max_tokens: reasoning_aware_max_tokens(req.max_output_tokens, req.reasoning),
            temperature: req.temperature,
            top_p: params.top_p,
            top_k: params.top_k,
            frequency_penalty: params.frequency_penalty,
            presence_penalty: params.presence_penalty,
            repetition_penalty: params.repetition_penalty,
            seed: params.seed,
            // Reasoning routes by identity: only Z.ai itself speaks GLM's
            // `thinking` object and only OpenRouter speaks the normalized
            // `reasoning` object. Any other identity behind this shared
            // adapter (xAI, DeepSeek, local, settings-defined) gets neither —
            // there is no portable Chat Completions reasoning field, and an
            // unknown key would risk a hard 400 on servers the user never
            // opted into experimenting with.
            thinking: (self.id == "zai")
                .then(|| {
                    req.reasoning.map(|enabled| ZaiThinking {
                        kind: if enabled { "enabled" } else { "disabled" },
                    })
                })
                .flatten(),
            reasoning: (self.id == "openrouter" && !force_default_reasoning)
                .then(|| openrouter_reasoning(req.reasoning, req.effort))
                .flatten(),
            // Two identities speak the top-level `reasoning_effort`, for
            // different reasons, so they map through different tables:
            //
            // * xAI's Grok reasoning models take it as their ONLY reasoning
            //   control, and only some of them accept it (the original grok-4
            //   400s; see [`xai_supports_reasoning_effort`]).
            // * Z.ai takes it as a *depth* control alongside the `thinking`
            //   on/off object above. Verified live against `glm-5.2`
            //   (2026-08-04): the field is honoured, not ignored — `minimal`
            //   drives `reasoning_tokens` to 0. Without it the effort tier was
            //   accepted, hashed into the published posture, and then dropped
            //   on the floor, so a run pinned to `medium` silently reasoned at
            //   GLM's own default depth. See [`zai_reasoning_effort`].
            //
            // Every other identity behind this shared adapter still sends
            // nothing, so no server the user never opted into experimenting
            // with sees a key it might reject.
            reasoning_effort: match self.id.as_str() {
                "xai" if xai_supports_reasoning_effort(&self.model) => {
                    xai_reasoning_effort(req.reasoning, req.effort)
                }
                "zai" => zai_reasoning_effort(req.reasoning, req.effort),
                _ => None,
            },
            tools: to_zai_tools(req.tools),
            usage: self
                .usage_accounting
                .then_some(ZaiUsageInclude { include: true }),
            // Gated on "actually talking to OpenRouter", not on the identity
            // string alone: a settings-defined custom provider pointing its
            // base URL at the gateway is OpenRouter in every way that matters,
            // and keying the cache opt-in off the id alone re-created the
            // motivating incident (Claude via OpenRouter, CACHE 0%, every
            // token at the full input rate) one config file over (#1285).
            cache_control: self
                .serves_openrouter()
                .then_some(ZaiCacheControl { kind: "ephemeral" }),
            session_id: self.serves_openrouter().then_some(self.session_id.as_str()),
            // Same "actually talking to OpenRouter" gate as the cache opt-in,
            // and additionally silent unless an upstream was pinned: no other
            // Chat Completions server speaks `provider`, and an unpinned
            // request must keep its pre-field bytes so the prompt cache is
            // unaffected for everyone who never asked for a pin.
            provider: (self.serves_openrouter() && !self.upstream_pin.is_empty()).then_some(
                OpenRouterProviderPin {
                    order: &self.upstream_pin,
                    allow_fallbacks: false,
                },
            ),
        }
    }

    /// POST `body` and run the shared non-success ladder (vendor 429
    /// pre-check first). Returns the successful response for the caller —
    /// streaming or unary — to consume. The two delivery paths differ in
    /// their client's read bound AND in what that bound's expiry means, so
    /// both ride as parameters.
    ///
    /// `unary` selects the send-error classification (#547's other half): on
    /// the unary client the read bound covers the ENTIRE generation, so its
    /// expiry means the request was too long to serve — Terminal, because
    /// re-issuing the identical request just waits out the full bound again
    /// once per retry. On the streaming client the same expiry is only a
    /// header stall (the first token would have reset the clock), which the
    /// next attempt may well clear — retryable, as it always was.
    async fn dispatch(
        &self,
        client: &reqwest::Client,
        body: &ZaiRequest<'_>,
        unary: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        // Capture what this run actually asks for, when a rig asked to see it
        // (`STELLA_WIRE_LOG`). The body is the only place the reasoning
        // budget, the output ceiling and the tool schemas appear; `step_usage`
        // records what a call cost but never what it requested. Off, and free,
        // unless the variable names a file. The credential is in the header
        // below, never in the body, so nothing capturable is secret.
        crate::wire_log::record_request(&url, &self.label, body);
        let mut request = client.post(&url).bearer_auth(self.api_key.reveal());
        for (name, value) in &self.extra_headers {
            request = request.header(*name, value);
        }
        let response = request.json(body).send().await.map_err(|e| {
            if unary {
                http::classify_unary_dispatch_error(&self.label, &e)
            } else {
                ProviderError::transport(e.to_string())
            }
        })?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            // Vendor pre-check ahead of the shared ladder — Z.ai overloads
            // HTTP 429: besides genuine throttling it also returns 429 for
            // BILLING problems — an account with no credit answers with
            // `{"error":{"code":"1113","message":"Insufficient balance or no
            // resource package. Please recharge."}}`. Blindly mapping every
            // 429 to a retryable `RateLimited` both mislabels that as a rate
            // limit (the user sees "provider rate limited" on their very
            // first call, which can't be a real throttle) AND burns three
            // pointless retries on a condition backoff will never clear.
            // Read the body and classify: a real throttle stays retryable, a
            // billing/quota failure is terminal, and either way the
            // provider's own message is surfaced instead of a hard-coded
            // string — under `self.label`, so a gateway on this shared
            // adapter never reports its throttle as Z.ai's.
            let retry_after_ms = http::parse_retry_after_ms(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(classify_zai_429(&self.label, &body, retry_after_ms));
        }
        if !response.status().is_success() {
            let status = response.status();
            let retry_after_ms = http::parse_retry_after_ms(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(http::classify_http_status(
                &self.label,
                status,
                retry_after_ms,
                &body,
                &self.model,
            ));
        }
        Ok(response)
    }
}

#[cfg(test)]
mod parallel_tool_calls;
#[cfg(test)]
mod tests;
