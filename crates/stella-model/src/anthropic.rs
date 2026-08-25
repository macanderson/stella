//! Anthropic adapter — Messages API, SSE streaming, native tool-use.
//! Retires raw-SSE-parsing risk against a second, structurally different
//! dialect from Z.ai's OpenAI-compatible one (`anthropic-tools` vs.
//! `openai-json`).

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use stella_protocol::{
    CompletionMessage, CompletionRequestRef, CompletionResult, FinishReason, MessageRole,
    ProviderError,
};

mod context_edit;
mod stream;
mod unary;

use crate::cache_economics::CacheTtl;
use crate::catalog::{Catalog, Pricing};
use crate::credential::ApiKey;
use crate::http;
use crate::provider::{Provider, ToolCallObserver};
use crate::stream_recovery::StreamRecovery;
use context_edit::{CONTEXT_MANAGEMENT_BETA, ContextManagement};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: ApiKey,
    base_url: String,
    model: String,
    /// List pricing for `model`, resolved from the catalog at construction so
    /// `cost_usd` is computed on the real request path (see `zai.rs`).
    pricing: Option<Pricing>,
    /// Where the PREVIOUS request's conversation-tail breakpoint landed, so
    /// this one can re-stamp it and keep an anchor at a position the cache was
    /// actually written to (#1837). See [`stamp_remembered_tail`].
    ///
    /// Session-scoped because the adapter is constructed once per session. A
    /// `Mutex` rather than a cell: `complete_ref` takes `&self` and concurrent
    /// calls through one provider are ordinary (sibling sub-agents, the
    /// pipeline's management roles). Contention is one pointer write per
    /// request.
    previous_tail: std::sync::Mutex<Option<TailPosition>>,
    /// The session's context-editing policy, or `None` to send no
    /// `context_management` and behave exactly as before the feature existed.
    ///
    /// Session-scoped like `cache_ttl`, and for the same reason: the field is
    /// part of the request prefix the cache is keyed on, so changing it
    /// mid-session would pay a re-write for a prefix the next turn cannot use.
    context_management: Option<ContextManagement>,
    /// The prompt-cache window this session asks for (#1839): the default
    /// 5-minute window sends today's exact bytes; the 1-hour opt-in adds
    /// `ttl: "1h"` to every breakpoint plus the [`EXTENDED_CACHE_TTL_BETA`]
    /// header. Session-scoped like `previous_tail` — mixing windows within a
    /// session would pay the 2x write premium for a prefix the next turn
    /// re-anchors on the short window anyway.
    cache_ttl: CacheTtl,
    /// The streaming→non-streaming fallback latch (#2686, extended to this
    /// dialect by #2746): armed when a stream hangs before its first byte or
    /// comes back empty, consulted per attempt so the retry of a faulted
    /// attempt goes out unary. See [`crate::stream_recovery`] for the state
    /// machine and its bounds.
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
    /// `next_stream_read` takes `idle` as a parameter).
    first_byte_deadline: Duration,
}

impl AnthropicProvider {
    pub fn new(api_key: ApiKey, model: impl Into<String>) -> Self {
        let model = model.into();
        // Scope the lookup to the `anthropic` provider: after `stella models
        // refresh` merges the models.dev master list the same slug legitimately
        // appears under several providers (`claude-sonnet-4.5` under both
        // `anthropic` and `openrouter`), and an unscoped `resolve` takes
        // whichever row happens to sit first — costing an Anthropic turn at a
        // gateway's list price with no symptom (see `Catalog::resolve_for`).
        let catalog = Catalog::current();
        let pricing = catalog
            .resolve_for("anthropic", &model)
            .ok()
            .map(|e| e.pricing);
        Self {
            client: http::client(),
            api_key,
            base_url: DEFAULT_BASE_URL.to_string(),
            model,
            pricing,
            previous_tail: std::sync::Mutex::new(None),
            cache_ttl: CacheTtl::default(),
            // Off by default. Context editing trades prompt cache for context
            // room, and a session that never outgrows its window would pay the
            // invalidation and get nothing; the caller who knows the shape of
            // its conversation opts in.
            context_management: None,
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

    /// Shrink the unary read bound so the non-streaming path is testable in
    /// milliseconds instead of ten minutes. Separate from
    /// [`Self::with_first_byte_deadline`] because the two bounds guard
    /// different halves of the fallback: that one the stream's first byte,
    /// this one the whole unary generation — head and body alike, which is
    /// what lets a test stall the response *body* and reach the
    /// classification `complete_unary_attempt` applies to `text()`.
    #[cfg(test)]
    pub(crate) fn with_unary_read_timeout(mut self, timeout: Duration) -> Self {
        self.unary_client = reqwest::Client::builder()
            .read_timeout(timeout)
            .build()
            .expect("the test client builds");
        self
    }

    /// Opt this session into server-side context editing.
    ///
    /// `trigger_tokens` is the input size below which nothing is cleared —
    /// the floor that keeps short conversations fully cached and therefore the
    /// reason enabling this cannot make them more expensive. `thinking_turns`
    /// is `None` to keep every thinking block (cache-optimal, and what a
    /// current model does anyway) or `Some(n)` to keep only the last `n` and
    /// accept the cache invalidation in exchange for the room.
    ///
    /// Session-scoped by construction: the policy is part of the request
    /// prefix the cache is keyed on, so it must not change mid-session.
    #[must_use]
    pub fn with_context_editing(
        mut self,
        trigger_tokens: u32,
        thinking_turns: Option<u32>,
    ) -> Self {
        self.context_management = Some(ContextManagement::new(trigger_tokens, thinking_turns));
        self
    }

    /// Override the base URL — used by conformance tests against a mock
    /// server, and by anyone routing through a private proxy.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Pin the prompt-cache window this session requests (#1839). The default
    /// ([`CacheTtl::FiveMinutes`]) changes nothing on the wire; see the
    /// `cache_ttl` field for what [`CacheTtl::OneHour`] adds.
    #[must_use]
    pub fn with_cache_ttl(mut self, cache_ttl: CacheTtl) -> Self {
        self.cache_ttl = cache_ttl;
        self
    }
}

// ── Wire types (Anthropic Messages API) ─────────────────────────────────

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    /// System prompt as a content-block array rather than a bare string, so
    /// the block can carry the `cache_control` breakpoint that caches the
    /// tools+system prefix tier (prompt caching is opt-in per request on the
    /// Messages API).
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<Vec<AnthropicSystemBlock>>,
    messages: Vec<AnthropicMessage>,
    stream: bool,
    /// Sampling temperature, forwarded from `CompletionRequest.temperature`.
    /// Omitted when `None` so Anthropic applies its own default (dropping it
    /// unconditionally would silently ignore a caller-set temperature).
    /// Omitted entirely on adaptive-thinking models (Claude 4.6+ / the 5
    /// family), which reject any sampling parameter with a 400 whether or not
    /// `thinking` is set. On the legacy shape it is forwarded, except when
    /// `thinking` is on: the Messages API rejects any temperature != 1
    /// alongside extended thinking, and the engine's default (0.0) would fail
    /// every thinking turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    /// Sampling overrides from `CompletionRequest.params`, skipped when
    /// `None` so a request without overrides serializes byte-identically
    /// (the prompt-cache contract). Only the subset the Messages API
    /// speaks — the rest of `GenerationParams` has no slot here and is
    /// silently dropped per the never-fail contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    /// Extended thinking, set only for `CompletionRequest.reasoning ==
    /// Some(true)`. Its wire shape depends on the model generation — adaptive
    /// (`{"type":"adaptive"}`) on current models, `{"type":"enabled",
    /// "budget_tokens":N}` on legacy ones — see [`AnthropicThinking`] and
    /// [`uses_adaptive_thinking`]. `Some(false)`/`None` omit it (thinking is
    /// opt-in per request), keeping the pre-field bytes stable.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<AnthropicThinking>,
    /// Output controls (`{"effort":"low|…|max"}`). On current models this is
    /// the depth/spend knob that replaced `thinking.budget_tokens`; omitted on
    /// legacy models, which reject the field. See [`AnthropicOutputConfig`].
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<AnthropicOutputConfig>,
    /// Server-side context editing — clearing stale thinking blocks and tool
    /// results before the model reads them. Omitted unless the session opted
    /// in, because clearing invalidates the prompt cache from the point of the
    /// edit and a session that never grows past the trigger would pay that
    /// invalidation for nothing. See [`context_edit`].
    #[serde(skip_serializing_if = "Option::is_none")]
    context_management: Option<ContextManagement>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicToolSchema>,
}

/// The Messages API's thinking switch, in one of two wire shapes chosen by the
/// model generation:
///   * `{"type":"adaptive"}` — current models (Claude 4.6+, the 5-family). The
///     model picks its own depth; [`AnthropicOutputConfig`]'s `effort` tunes
///     it. Sending `budget_tokens` here is an HTTP 400.
///   * `{"type":"enabled","budget_tokens":N}` — legacy models (≤ 4.5), where
///     `N` must satisfy `1024 <= N < max_tokens` (see [`thinking_budget_tokens`]).
#[derive(Serialize)]
struct AnthropicThinking {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Present only on the legacy `enabled` shape; omitted (and rejected) on
    /// the adaptive shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_tokens: Option<u32>,
}

impl AnthropicThinking {
    /// `{"type":"adaptive"}` — the current-model shape (no budget field).
    const fn adaptive() -> Self {
        Self {
            kind: "adaptive",
            budget_tokens: None,
        }
    }

    /// `{"type":"enabled","budget_tokens":N}` — the legacy-model shape.
    const fn enabled(budget_tokens: u32) -> Self {
        Self {
            kind: "enabled",
            budget_tokens: Some(budget_tokens),
        }
    }
}

/// The Messages API's `output_config` object. Only `effort` is modeled — the
/// GA depth/spend control on current models (no beta header), which replaces
/// the legacy per-request thinking budget. Rejected by legacy models.
#[derive(Serialize)]
struct AnthropicOutputConfig {
    effort: &'static str,
}

/// Whether `model` speaks the current adaptive-thinking wire shape
/// (`thinking:{type:"adaptive"}` + `output_config.effort`, with `temperature`/
/// `top_p`/`top_k` rejected) rather than the legacy
/// `{type:"enabled",budget_tokens}` shape (which accepts sampling).
///
/// Claude 4.6+ and the "5" family (Fable 5, Mythos 5, Sonnet 5, …) **require**
/// the adaptive shape and answer a stray `budget_tokens` — or any sampling
/// parameter — with an HTTP 400. That 400 is exactly the failure this classifier
/// exists to prevent. The 4.5-and-older generations still use `budget_tokens`.
///
/// The legacy set is closed and shrinking; the modern set is open and growing.
/// So we **denylist** the known legacy generations and default everything else
/// — including models released after this code was written — to the modern
/// shape. An allowlist would silently 400 the next launch (fable-6, opus-5),
/// which is precisely how this bug reached production.
fn uses_adaptive_thinking(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    // Compare whole version SEGMENTS, never substrings: a substring marker
    // like `-4-1` also matches inside `-4-10`/`-4-11`, misreading a two-digit
    // point release of the modern major as legacy — the failure direction
    // that 400s (`budget_tokens` sent to a model that requires adaptive).
    //
    // Slug grammar: dash-separated segments where the first numeric segment
    // is the major generation, the numeric segment after it (if any) the
    // minor, and an 8-digit segment a dated snapshot, not a version
    // (`claude-opus-4-20250514` is a 4.0 snapshot). `claude-2.1` spells its
    // version with a dot, so a segment's major is the part before the first
    // `.`. Legacy = major ≤ 3, or major 4 with minor ≤ 5 (a missing minor is
    // 4.0). Anything without a version segment — including models released
    // after this code was written — stays modern, per the denylist posture
    // above.
    let version = |segment: &str| {
        segment
            .split('.')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
    };
    let segments: Vec<&str> = m.split('-').collect();
    let Some(pos) = segments.iter().position(|s| version(s).is_some()) else {
        return true;
    };
    let major = version(segments[pos]).unwrap_or(0);
    let minor = segments
        .get(pos + 1)
        .and_then(|s| version(s))
        // Two digits at most: anything longer is a dated snapshot segment.
        .filter(|&n| n < 100)
        .unwrap_or(0);
    match major {
        0..=3 => false,
        4 => minor >= 6,
        _ => true,
    }
}

/// Map the engine's effort tiers onto thinking budgets, for the **legacy**
/// `budget_tokens` shape only. Anthropic's older models had no named levels —
/// the budget IS the level — so the tiers are spaced roughly geometrically from
/// the API's 1024-token floor up to a Max that still leaves headroom under
/// typical output caps. `None` defaults to Medium, the same middle-tier default
/// posture as `openai.rs` ("effort":"medium"). Current models use
/// [`map_effort`] instead.
///
/// `pub(crate)` because `bedrock.rs` sends the same `{type:"enabled",
/// budget_tokens}` shape through Converse's `additionalModelRequestFields
/// .reasoning_config` — one mapping, not two spellings that drift.
pub(crate) fn thinking_budget_tokens(effort: Option<stella_protocol::ReasoningEffort>) -> u32 {
    use stella_protocol::ReasoningEffort::*;
    match effort {
        Some(Low) => 2_048,
        None | Some(Medium) => 8_192,
        Some(High) => 16_384,
        Some(Xhigh) => 32_768,
        Some(Max) => 49_152,
    }
}

/// Map the engine's effort tiers onto the Messages API's `output_config.effort`
/// levels for the current adaptive shape. The vocabularies line up 1:1, so this
/// is a direct rename (unlike the legacy [`thinking_budget_tokens`] mapping).
fn map_effort(effort: stella_protocol::ReasoningEffort) -> &'static str {
    use stella_protocol::ReasoningEffort::*;
    match effort {
        Low => "low",
        Medium => "medium",
        High => "high",
        Xhigh => "xhigh",
        Max => "max",
    }
}

/// The opt-in prompt-cache marker (`{"type": "ephemeral"}`, default 5-minute
/// TTL). The pipeline keeps the system prefix byte-stable and rides volatile
/// recall after it (L-E8) precisely so these breakpoints hit; this is the
/// wire half that actually turns the cache on. Reads bill at ~0.1x the input
/// rate, writes at ~1.25x — break-even after two requests, and the agent
/// loop replays its prefix every turn.
///
/// `ttl` is the extended-window opt-in (#1839): `"1h"` widens the window to
/// an hour (writes bill 2x instead of 1.25x) and requires the
/// [`EXTENDED_CACHE_TTL_BETA`] header on the request. `None` — the default —
/// omits the field entirely, so a session on the 5-minute window serializes
/// byte-identically to one built before the knob existed (invariant 7).
#[derive(Serialize, Clone, Copy, Debug)]
struct AnthropicCacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<&'static str>,
}

/// The `anthropic-beta` value that unlocks the `cache_control.ttl` field.
/// Sent only when the session configured the 1-hour window: an unconditional
/// beta header would change every request's shape for users who never asked.
const EXTENDED_CACHE_TTL_BETA: &str = "extended-cache-ttl-2025-04-11";

/// The cache marker for `ttl`'s window: bare `{"type":"ephemeral"}` on the
/// default 5-minute window, `{"type":"ephemeral","ttl":"1h"}` on the 1-hour
/// opt-in. One constructor so the system-block and both message breakpoints
/// can never disagree about the window they ask for.
const fn ephemeral_cache(ttl: CacheTtl) -> AnthropicCacheControl {
    AnthropicCacheControl {
        kind: "ephemeral",
        ttl: match ttl {
            CacheTtl::FiveMinutes => None,
            CacheTtl::OneHour => Some("1h"),
        },
    }
}

/// Stamp the conversation-tail cache breakpoint: `cache_control` on the
/// LAST content block of the final message, so each agent-loop turn reads
/// the prefix written by the previous turn instead of re-paying the whole
/// replayed history at the full input rate. Pairs with the system-block
/// marker (two of the four allowed breakpoints). Block-level is the
/// placement this adapter sends (never a top-level `cache_control` request
/// field, which Anthropic's unknown-parameter handling would reject with a
/// 400); `tests/live_smoke.rs::anthropic_smoke` tracks whether that holds
/// end-to-end.
/// Where a request's conversation-tail breakpoint landed: `(message, block)`.
///
/// Carried on the provider so the NEXT request can re-stamp it — see
/// [`stamp_tail_cache_breakpoint`] for why one breakpoint is not enough.
type TailPosition = (usize, usize);

/// Re-stamp the position the PREVIOUS request's tail breakpoint landed on.
///
/// This is the second of the two message breakpoints (#1837). Anthropic
/// allows four; this adapter used two — one on the system block (which covers
/// tools via the cache hierarchy) and one on the newest content block. The
/// second is on content that *did not exist* on the previous request, so
/// nothing was anchored at the position the previous turn actually wrote to,
/// and the conversation tier was served only by Anthropic's bounded automatic
/// lookback (~20 content blocks).
///
/// That bound is easy to exceed here: a step that fans out several reads emits
/// ~11 content blocks (one `tool_use` + one `tool_result` per call), so two
/// such steps push the previous write out of lookback and the whole replayed
/// history re-bills at the full input rate and re-writes at 1.25×. Claude Code
/// keeps a rolling pair for exactly this reason; measured, Stella sat at ~76%
/// cache-read against its ~93%.
///
/// A remembered POSITION rather than a content digest, deliberately. The worst
/// case if history shifted under it — compaction rewriting or evicting a
/// message — is that the breakpoint anchors somewhere less useful, which costs
/// a cache write and nothing else. A `cache_control` marker is an anchor, not
/// an assertion: no answer here can be *wrong*, only suboptimal. Paying a hash
/// of every block on every request to slightly improve an anchor would cost
/// more than it saves.
fn stamp_remembered_tail(
    messages: &mut [AnthropicMessage],
    at: TailPosition,
    marker: AnthropicCacheControl,
) {
    let (message, block) = at;
    // Never the newest block: that is the current tail's own position, and
    // spending both message breakpoints on the same anchor is what this
    // function exists to stop.
    if let Some(target) = messages
        .get_mut(message)
        .and_then(|m| m.content.get_mut(block))
    {
        match target {
            AnthropicContentBlock::Text { cache_control, .. }
            | AnthropicContentBlock::ToolResult { cache_control, .. } => {
                *cache_control = Some(marker);
            }
            // The position now holds a block this adapter's schema cannot
            // mark. Leave it: a missing second breakpoint is the old
            // behaviour, and the tail breakpoint below is unaffected.
            AnthropicContentBlock::Image { .. }
            | AnthropicContentBlock::Document { .. }
            | AnthropicContentBlock::ToolUse { .. } => {}
        }
    }
}

fn stamp_tail_cache_breakpoint(
    messages: &mut [AnthropicMessage],
    marker: AnthropicCacheControl,
) -> Option<TailPosition> {
    // Walk BACKWARD to the newest stampable block rather than inspecting only
    // the literal last one. The loop's ordinary shape does end on Text or
    // ToolResult — but a user message that is *only* an attachment ends on a
    // media block (`to_anthropic_messages` appends the text block solely when
    // the trimmed content is non-empty), and giving up there meant the whole
    // conversation tier went unmarked: that step wrote nothing to the cache,
    // and the next step re-paid everything since the previous turn's marker
    // at the full input rate. Stamping the newest Text/ToolResult before the
    // media tail keeps the incremental tier alive at the cost of leaving only
    // the trailing attachment blocks uncached.
    for (mi, message) in messages.iter_mut().enumerate().rev() {
        for (bi, block) in message.content.iter_mut().enumerate().rev() {
            match block {
                AnthropicContentBlock::Text { cache_control, .. }
                | AnthropicContentBlock::ToolResult { cache_control, .. } => {
                    *cache_control = Some(marker);
                    return Some((mi, bi));
                }
                // Media and tool_use blocks don't carry the marker in this
                // adapter's schema — keep walking to the newest block that
                // does. The system-block breakpoint still caches the
                // tools+system tier even if nothing here is stampable.
                AnthropicContentBlock::Image { .. }
                | AnthropicContentBlock::Document { .. }
                | AnthropicContentBlock::ToolUse { .. } => {}
            }
        }
    }
    None
}

#[derive(Serialize)]
struct AnthropicSystemBlock {
    #[serde(rename = "type")]
    kind: &'static str,
    /// Owned rather than borrowed: the hoisted system prompt is built by
    /// [`to_anthropic_messages`] inside the body builder, so a borrow here
    /// would tie the request to a local that dies before it is sent.
    text: String,
    cache_control: AnthropicCacheControl,
}

#[derive(Serialize)]
struct AnthropicToolSchema {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
        /// Set only on the final block of the last message — the
        /// conversation-tail cache breakpoint ([`stamp_tail_cache_breakpoint`]).
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl>,
    },
    /// A user-attached image (`{"type":"image","source":{...}}`).
    Image { source: AnthropicMediaSource },
    /// A user-attached PDF (`{"type":"document","source":{...}}`).
    Document { source: AnthropicMediaSource },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
        /// Same conversation-tail breakpoint slot as `Text::cache_control`.
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl>,
    },
}

/// The base64 payload envelope shared by image and document blocks.
#[derive(Serialize, Debug)]
struct AnthropicMediaSource {
    #[serde(rename = "type")]
    kind: &'static str,
    media_type: String,
    data: String,
}

impl AnthropicMediaSource {
    fn base64(media_type: impl Into<String>, data: String) -> Self {
        Self {
            kind: "base64",
            media_type: media_type.into(),
            data,
        }
    }
}

/// The Messages API ingests images and PDFs natively; audio, video, and
/// arbitrary binaries degrade to descriptive text notes.
const ANTHROPIC_CAPS: crate::attachment::DialectCaps = crate::attachment::DialectCaps {
    images: true,
    pdfs: true,
    audio: false,
    video: false,
};

/// Map a user message's attachments to Anthropic content blocks. Media
/// blocks precede text (the documented preferred ordering for vision).
fn attachment_blocks(message: &CompletionMessage) -> Vec<AnthropicContentBlock> {
    crate::attachment::wire_parts(&message.attachments, ANTHROPIC_CAPS)
        .into_iter()
        .map(attachment_block)
        .collect()
}

/// One resolved part as a Messages content block.
fn attachment_block(part: crate::attachment::WirePart) -> AnthropicContentBlock {
    match part {
        crate::attachment::WirePart::Image { media_type, base64 } => AnthropicContentBlock::Image {
            source: AnthropicMediaSource::base64(media_type, base64),
        },
        crate::attachment::WirePart::Pdf { base64, .. } => AnthropicContentBlock::Document {
            source: AnthropicMediaSource::base64("application/pdf", base64),
        },
        crate::attachment::WirePart::Text { text } => AnthropicContentBlock::Text {
            text,
            cache_control: None,
        },
        // Audio/video are switched off in ANTHROPIC_CAPS, so wire_parts has
        // already degraded them before this sees them — audio to a Text note,
        // video to sampled Image parts plus the note saying they are frames
        // (`crate::keyframes`). Turning either cap on without adding a block
        // arm lands here — degrade, never abort the turn.
        part @ (crate::attachment::WirePart::Audio { .. }
        | crate::attachment::WirePart::Video { .. }) => AnthropicContentBlock::Text {
            text: crate::attachment::unsupported_part_note(&part, "Anthropic Messages API"),
            cache_control: None,
        },
    }
}

/// Streamed SSE payloads from the Messages API's `content_block_delta`
/// events. Anthropic's stream sends several event *types*
/// (`message_start`, `content_block_start`, `content_block_delta`,
/// `message_delta`, `message_stop`); this adapter aggregates text deltas
/// and the final usage block, and requires the terminal `message_stop`.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicStreamEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: AnthropicMessageStart },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        #[serde(default)]
        index: usize,
        content_block: AnthropicStartBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        #[serde(default)]
        index: usize,
        delta: AnthropicDelta,
    },
    /// A content block finished streaming. For a `tool_use` block this is
    /// the earliest moment its complete input is known — the hook that lets
    /// [`stream::aggregate_anthropic_stream`] announce the call to a
    /// [`ToolCallObserver`] while the rest of the message still streams.
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {
        #[serde(default)]
        index: usize,
    },
    #[serde(rename = "message_delta")]
    MessageDelta {
        /// Carries `stop_reason` — `"max_tokens"` when the model was cut off
        /// at the output-token limit. Tracked so a tool call whose argument
        /// JSON was truncated mid-stream surfaces an actionable error instead
        /// of a silent `Null` (see [`crate::http::truncated_tool_input_error`]).
        #[serde(default)]
        delta: AnthropicMessageDeltaBody,
        usage: Option<AnthropicUsage>,
    },
    /// The stream's terminal event — the Messages API always ends a healthy
    /// stream with it. Modeled explicitly (not left to `Other`) because its
    /// absence at EOF is the only evidence that a *clean* connection close
    /// (close-delimited proxies, LB idle-reaps) cut the message short.
    #[serde(rename = "message_stop")]
    MessageStop,
    /// A mid-stream error event. The Messages API can send
    /// `event: error` / `data: {"type":"error","error":{...}}` after already
    /// streaming content — modeled explicitly so it aborts the turn with a
    /// typed error instead of falling into `Other` and being swallowed,
    /// returning truncated text as a bogus success.
    #[serde(rename = "error")]
    Error { error: AnthropicStreamError },
    #[serde(other)]
    Other,
}

/// The `delta` object of a `message_delta` event. Only `stop_reason` is
/// modeled — it is how the Messages API signals *why* generation ended, and
/// `"max_tokens"` specifically means the output was cut off at the token
/// limit (potentially mid-tool-call).
#[derive(Deserialize, Debug, Default)]
struct AnthropicMessageDeltaBody {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct AnthropicStreamError {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: String,
}

/// Map an Anthropic in-stream error to a typed `ProviderError`.
/// `overloaded_error` is Anthropic's own brownout frame and classifies as the
/// park-eligible `Overloaded`, the same class the status-line 529 gets: which
/// side of the response boundary the provider shed load on is invisible to
/// the user and says nothing about recoverability (#3859). `api_error` and
/// `timeout_error` are transient server-side conditions with no such waiting
/// story, so they stay retryable `Transport`; `rate_limit_error` is
/// `RateLimited`; everything else (`invalid_request_error`,
/// `authentication_error`, `permission_error`, `not_found_error`, …) is
/// `Terminal`.
///
/// The caller pipes the result through [`http::attach_partial`], which now
/// decorates both retryable classes — so an overload three frames into a
/// stream keeps the input tokens that stream already reported.
fn classify_anthropic_stream_error(err: &AnthropicStreamError) -> ProviderError {
    let detail = if err.message.is_empty() {
        format!("Anthropic stream error ({})", err.kind)
    } else {
        format!("Anthropic stream error: {}", err.message)
    };
    match err.kind.as_str() {
        "overloaded_error" => ProviderError::overloaded(detail, None),
        "api_error" | "timeout_error" => ProviderError::transport(detail),
        "rate_limit_error" => ProviderError::RateLimited {
            message: detail,
            retry_after_ms: None,
        },
        _ => ProviderError::Terminal(detail),
    }
}

#[derive(Deserialize, Debug, Default)]
struct AnthropicMessageStart {
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicStartBlock {
    ToolUse {
        id: String,
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    /// Extended-thinking content. Announced to the observer on the reasoning
    /// channel so the transcript can show it live, collapsed and dimmed —
    /// never folded into the answer text.
    ThinkingDelta {
        thinking: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    /// Tokens served from the prompt cache. Anthropic reports these
    /// *separately* from `input_tokens` (they are NOT already folded in, as
    /// they are for OpenAI), so the adapter must add them back to keep the
    /// normalized `cached_input_tokens` a subset of `input_tokens` and bill
    /// them at the cheaper cache rate rather than dropping them.
    #[serde(default)]
    cache_read_input_tokens: u64,
    /// Tokens WRITTEN to the prompt cache by this call — also reported
    /// separately from `input_tokens`. Surfaced as the normalized
    /// `cache_write_tokens` (telemetry, `stella stats`) but deliberately NOT
    /// folded into `input_tokens`: the catalog prices writes on their own
    /// line (`cache_write_usd_per_mtok`, 1.25x input on the Anthropic rows),
    /// so folding them in would misprice them as plain input (see
    /// `Pricing::cost_usd`).
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

fn to_anthropic_messages(
    messages: &[CompletionMessage],
) -> (Option<String>, Vec<AnthropicMessage>) {
    // Every system turn is hoisted, not just the last one: the Messages API
    // has a single `system` slot, so overwriting it would silently drop a
    // second system turn (a skill preamble, an injected policy block) with no
    // error and no log line. Accumulate and join with a blank line, exactly as
    // `openai.rs::to_openai_input` and `gemini.rs::to_gemini_request_parts` do,
    // so the same conversation carries the same instructions on every dialect.
    let mut system: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for message in messages {
        match message.role {
            MessageRole::System => system.push(message.content.clone()),
            // The Anthropic API rejects a text content block whose text is
            // empty or whitespace-only with a 400 — and because the whole
            // conversation is replayed on every turn, one such block bricks
            // the session permanently (every retry re-sends it). So a text
            // block is emitted only when it carries non-whitespace content,
            // and a message that ends up with zero blocks is dropped rather
            // than padded with an empty block. Attachment blocks (images,
            // documents, inlined files) precede the typed text.
            MessageRole::User => {
                let mut content = attachment_blocks(message);
                if !message.content.trim().is_empty() {
                    content.push(AnthropicContentBlock::Text {
                        text: message.content.clone(),
                        cache_control: None,
                    });
                }
                if !content.is_empty() {
                    out.push(AnthropicMessage {
                        role: "user",
                        content,
                    });
                }
            }
            MessageRole::Assistant => {
                let mut content = Vec::new();
                if !message.content.trim().is_empty() {
                    content.push(AnthropicContentBlock::Text {
                        text: message.content.clone(),
                        cache_control: None,
                    });
                }
                for call in &message.tool_calls {
                    content.push(AnthropicContentBlock::ToolUse {
                        id: call.call_id.clone(),
                        name: call.name.clone(),
                        input: call.input.clone(),
                    });
                }
                // A content-less assistant turn (no text, no tool calls) is
                // dropped, not sent as an empty text block: it carries no
                // information and there is no tool_use to orphan.
                if !content.is_empty() {
                    out.push(AnthropicMessage {
                        role: "assistant",
                        content,
                    });
                }
            }
            // Anthropic dialect: tool results are content blocks inside a
            // `user` message, each keyed by `tool_use_id`.
            MessageRole::Tool => {
                let content: Vec<AnthropicContentBlock> = message
                    .tool_results
                    .iter()
                    .map(|result| {
                        let (text, is_error) = match &result.output {
                            stella_protocol::ToolOutput::Ok { content, .. } => {
                                (content.clone(), false)
                            }
                            stella_protocol::ToolOutput::Error { message, .. } => {
                                (message.clone(), true)
                            }
                        };
                        AnthropicContentBlock::ToolResult {
                            tool_use_id: result.call_id.clone(),
                            content: text,
                            is_error,
                            cache_control: None,
                        }
                    })
                    .collect();
                if !content.is_empty() {
                    out.push(AnthropicMessage {
                        role: "user",
                        content,
                    });
                }
            }
        }
    }
    let system = if system.is_empty() {
        None
    } else {
        Some(system.join("\n\n"))
    };
    (system, out)
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

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

impl AnthropicProvider {
    /// The one request/deliver/assemble body behind both `complete_ref` and
    /// `complete_observed_ref` — the observer is threaded down to the stream
    /// aggregator, which announces each tool call at its
    /// `content_block_stop`.
    ///
    /// Streams by default, but consults the fallback latch first: the retry
    /// of an attempt whose stream hung before its first byte or came back
    /// empty goes out as a **unary** request for the same payload instead
    /// (#2686, #2746) — see [`crate::stream_recovery`] for the latch's states
    /// and bounds, and [`unary`] for that path.
    async fn complete_inner(
        &self,
        req: CompletionRequestRef<'_>,
        observer: Option<&dyn ToolCallObserver>,
    ) -> Result<CompletionResult, ProviderError> {
        if self.recovery.use_unary() {
            return self.complete_unary_attempt(req).await;
        }
        let body = self.build_body(req, true);
        let response = self.dispatch(&self.client, &body, false).await?;
        let outcome = stream::aggregate_anthropic_stream(
            response,
            observer,
            self.pricing.as_ref(),
            self.first_byte_deadline,
        )
        .await
        .map_err(|fault| self.recovery.absorb(fault))?;
        let cost_usd = self
            .pricing
            .map(|p| p.cost_usd(&outcome.usage))
            .unwrap_or(0.0);
        Ok(CompletionResult {
            text: outcome.text,
            tool_calls: outcome.tool_calls,
            usage: outcome.usage,
            model: self.model.clone(),
            cost_usd,
            finish_reason: map_stop_reason(outcome.stop_reason.as_deref()),
            // A direct endpoint: the provider id is already the whole answer
            // to "who served this?", so there is no upstream to name.
            upstream_provider: None,
        })
    }

    /// The one request body both delivery paths serialize — `stream` is the
    /// only field on which they differ, so the unary fallback re-issues the
    /// byte-identical payload minus the stream flag.
    ///
    /// Stamping the two cache breakpoints belongs here rather than at the
    /// send site: the remembered tail is per-request state, and a body built
    /// without it would silently drop the conversation cache tier.
    fn build_body(&self, req: CompletionRequestRef<'_>, stream: bool) -> AnthropicRequest<'_> {
        let (system, mut messages) = to_anthropic_messages(req.messages);
        // Two message breakpoints, not one (#1837): the previous request's
        // tail — a position the cache was genuinely written to — and this
        // request's own tail. Anthropic allows four and the system block takes
        // one, so the second message breakpoint was simply unused.
        //
        // Previous first: `stamp_tail_cache_breakpoint` walks backward to the
        // newest stampable block, and re-stamping the old position afterwards
        // could otherwise land on the same block and spend both on one anchor.
        let marker = ephemeral_cache(self.cache_ttl);
        let previous = *self.previous_tail.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(at) = previous {
            stamp_remembered_tail(&mut messages, at, marker);
        }
        if let Some(tail) = stamp_tail_cache_breakpoint(&mut messages, marker) {
            *self.previous_tail.lock().unwrap_or_else(|p| p.into_inner()) = Some(tail);
        }
        let reasoning_on = req.reasoning == Some(true);
        let params = req.params.unwrap_or_default();

        // Two thinking dialects, chosen by model generation. Current models
        // (Claude 4.6+, the 5-family) take `thinking:{type:"adaptive"}` plus
        // `output_config.effort`, and REJECT `budget_tokens` and every sampling
        // parameter with a 400 — the failure this fix repairs. Legacy models
        // (≤ 4.5) keep the old `{type:"enabled",budget_tokens}` shape and accept
        // sampling. `uses_adaptive_thinking` denylists the closed legacy set and
        // defaults everything else (incl. future launches) to the modern shape.
        let (max_tokens, thinking, output_config, temperature, top_p, top_k) =
            if uses_adaptive_thinking(&self.model) {
                // Adaptive thinking spends from the SAME output allowance as the
                // answer, so an un-capped reasoning turn needs far more headroom
                // than the old 4096 no-thinking default — 4096 would truncate a
                // max-effort verifier mid-verdict (returning empty text with
                // stop_reason=max_tokens). We already stream, so a high ceiling
                // costs nothing; a caller-set cap is still honored as-is.
                let max_tokens =
                    req.max_output_tokens
                        .unwrap_or(if reasoning_on { 32_000 } else { 4096 });
                (
                    max_tokens,
                    reasoning_on.then(AnthropicThinking::adaptive),
                    // Effort is a GA control independent of thinking; forward it
                    // whenever the caller pinned one, defaulting to the API's own
                    // (high) when unset.
                    req.effort.map(|effort| AnthropicOutputConfig {
                        effort: map_effort(effort),
                    }),
                    // Sampling params 400 on these models — never send them.
                    None,
                    None,
                    None,
                )
            } else {
                // Legacy shape: budget_tokens is coupled to max_tokens (the API
                // requires budget < max_tokens), and the 4096 default would leave
                // no room for any budget above Low — so when thinking is on and
                // the caller set no cap, the floor rises to budget + 8192. A
                // caller-set cap is honored and the budget clamps to it: at most
                // max_tokens - 1024, never below the 1024 floor; a cap at or below
                // the floor leaves NO legal budget, so thinking is omitted rather
                // than sent as a 400.
                let thinking_budget = reasoning_on.then(|| thinking_budget_tokens(req.effort));
                let max_tokens = match (req.max_output_tokens, thinking_budget) {
                    (Some(cap), _) => cap,
                    (None, Some(budget)) => budget + 8_192,
                    (None, None) => 4096,
                };
                let thinking = thinking_budget.and_then(|budget| {
                    (max_tokens > 1024).then(|| {
                        AnthropicThinking::enabled(budget.min(max_tokens - 1024).max(1024))
                    })
                });
                // The API rejects temperature != 1 with thinking enabled; omit it
                // entirely (rather than special-casing 1.0) and let the API apply
                // its own thinking-compatible default.
                let temperature = if thinking.is_some() {
                    None
                } else {
                    req.temperature
                };
                (
                    max_tokens,
                    thinking,
                    None,
                    temperature,
                    params.top_p,
                    params.top_k,
                )
            };

        AnthropicRequest {
            model: &self.model,
            max_tokens,
            system: system.map(|text| {
                vec![AnthropicSystemBlock {
                    kind: "text",
                    text,
                    cache_control: marker,
                }]
            }),
            messages,
            stream,
            temperature,
            top_p,
            top_k,
            thinking,
            output_config,
            context_management: self.context_management.clone(),
            tools: req
                .tools
                .iter()
                .map(|tool| AnthropicToolSchema {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.input_schema.clone(),
                })
                .collect(),
        }
    }

    /// POST `body` to the Messages endpoint and run the shared non-success
    /// ladder. Returns the successful response for the caller — streaming or
    /// unary — to consume. The two delivery paths differ in their client's
    /// read bound AND in what that bound's expiry means, so both ride as
    /// parameters.
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
        body: &AnthropicRequest<'_>,
        unary: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        let mut request = client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", self.api_key.reveal())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json");
        // Beta flags are accumulated into ONE comma-joined `anthropic-beta`
        // header rather than several headers, and each rides only when the
        // request actually carries the field it gates — so a session using
        // neither feature sends byte-identical headers to before either
        // existed, which is what keeps the prompt-cache prefix stable.
        let mut betas: Vec<&str> = Vec::new();
        // The `ttl` field is beta-gated; it rides only when a marker actually
        // carries the field.
        if self.cache_ttl == CacheTtl::OneHour {
            betas.push(EXTENDED_CACHE_TTL_BETA);
        }
        if body.context_management.is_some() {
            betas.push(CONTEXT_MANAGEMENT_BETA);
        }
        if !betas.is_empty() {
            request = request.header("anthropic-beta", betas.join(","));
        }
        let response = request.json(body).send().await.map_err(|e| {
            if unary {
                http::classify_unary_dispatch_error("Anthropic", &e)
            } else {
                ProviderError::transport(e.to_string())
            }
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_after_ms = http::parse_retry_after_ms(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(http::classify_http_status(
                "Anthropic",
                status,
                retry_after_ms,
                &body,
                &self.model,
            ));
        }
        Ok(response)
    }
}

/// Normalize the Messages API's `stop_reason` vocabulary onto the
/// provider-neutral [`FinishReason`] — the driver's truncation diagnostics
/// (`driver.rs`) only fire when `Length` actually reaches it. Unknown or
/// unreported reasons stay `None` per the `CompletionResult` contract.
fn map_stop_reason(stop_reason: Option<&str>) -> Option<FinishReason> {
    match stop_reason? {
        "end_turn" | "stop_sequence" | "pause_turn" => Some(FinishReason::Stop),
        "max_tokens" => Some(FinishReason::Length),
        "tool_use" => Some(FinishReason::ToolCalls),
        "refusal" => Some(FinishReason::ContentFilter),
        _ => None,
    }
}

#[cfg(test)]
mod parallel_tool_calls;
#[cfg(test)]
mod stream_fallback_tests;
#[cfg(test)]
mod tests;
