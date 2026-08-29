//! OpenAI adapter — the Responses API (`POST /responses`), not the
//! Chat Completions API. Routing `OPENAI_API_KEY` through an OpenAI-compatible
//! shim works only because `/v1/chat/completions` also exists on OpenAI's
//! account, but it is not the wire shape and is not structurally distinct from
//! Z.ai's dialect. The Responses API is genuinely different: an `input` *items*
//! array instead of a flat `messages` array, an `output` items array instead
//! of `choices`, and `function_call`/`function_call_output` items instead of
//! an accumulating `tool_calls` delta array — see the wire types below.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use stella_protocol::{
    CompletionMessage, CompletionRequestRef, CompletionResult, MessageRole, ProviderError,
    ReasoningEffort, ServiceTier, ToolCallObserver, Verbosity,
};

use crate::catalog::{Catalog, Pricing};
use crate::credential::ApiKey;
use crate::http;

mod stream;
mod stream_error;
mod unary;
use crate::stream_recovery::StreamRecovery;
use stream_error::classify_openai_stream_error;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: ApiKey,
    base_url: String,
    model: String,
    /// List pricing for `model`, resolved from the catalog at construction so
    /// `cost_usd` is computed on the real request path (see `zai.rs`).
    pricing: Option<Pricing>,
    /// OpenAI's prompt caching is implicit (no `cache_control` equivalent to
    /// opt into), but `prompt_cache_key` steers cache *routing*: requests
    /// sharing a key land on the same cache shard, so an agent loop's turns
    /// — which all replay the same growing prefix — reliably find the writes
    /// their earlier turns made. One key per provider instance = one key per
    /// session. The value is volatile by design; it rides as a request
    /// parameter and never enters the cached prompt bytes.
    prompt_cache_key: String,
    /// The streaming→non-streaming fallback latch (#2686, extended to this
    /// dialect by #2746): armed when a stream hangs before its first byte or
    /// comes back empty, consulted per attempt so the retry of a faulted
    /// attempt goes out unary. See [`crate::stream_recovery`].
    recovery: StreamRecovery,
    /// The client the unary fallback dispatches through — [`http::unary_client`]'s
    /// 600s bound, because a non-streaming call has no first token to reset
    /// the per-read clock (#547).
    unary_client: reqwest::Client,
    /// [`http::FIRST_BYTE_TIMEOUT`] in production; a field so the hung-stream
    /// path is testable in milliseconds.
    first_byte_deadline: Duration,
}

/// Process-wide monotonic suffix guaranteeing two [`OpenAiProvider`]
/// constructions in the same process — fleet siblings built back-to-back —
/// get distinct cache-routing keys even when the nanosecond clock reads
/// identically for both. Without it, a tight builder loop could mint colliding
/// keys and serialize the whole fleet onto one cache shard, the opposite of
/// the point. Same construction (and same reason) as `zai.rs`'s `SESSION_SEQ`.
static CACHE_KEY_SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh cache-routing key, `stella-<pid>-<nanos>-<seq>`. The pid+nanos pair
/// scopes it to this run; the atomic seq makes same-nanos siblings provably
/// distinct.
fn new_prompt_cache_key() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    prompt_cache_key_at(nanos)
}

/// The formatting half, split from the clock read so a test can mint two keys
/// from the SAME instant — the collision the atomic exists to prevent, and the
/// one a real clock will not reproduce on demand.
fn prompt_cache_key_at(nanos: u128) -> String {
    let seq = CACHE_KEY_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("stella-{}-{nanos:x}-{seq:x}", std::process::id())
}

impl OpenAiProvider {
    /// Build an adapter for `model` (a catalog-resolved slug, e.g.
    /// `gpt-5.5` — never a literal chosen at the call site).
    pub fn new(api_key: ApiKey, model: impl Into<String>) -> Self {
        let model = model.into();
        // Scope the lookup to the `openai` provider: after `stella models
        // refresh` merges the models.dev master list the same slug can appear
        // under several providers (gateways re-serve OpenAI models), and an
        // unscoped `resolve` takes whichever row happens to sit first — costing
        // an OpenAI turn at a gateway's list price with no symptom (see
        // `Catalog::resolve_for`).
        let catalog = Catalog::current();
        let pricing = catalog
            .resolve_for("openai", &model)
            .ok()
            .map(|e| e.pricing);
        Self {
            client: http::client(),
            api_key,
            base_url: DEFAULT_BASE_URL.to_string(),
            model,
            pricing,
            prompt_cache_key: new_prompt_cache_key(),
            recovery: StreamRecovery::default(),
            unary_client: http::unary_client(),
            first_byte_deadline: http::FIRST_BYTE_TIMEOUT,
        }
    }

    /// Override the base URL — used by conformance tests against a mock
    /// server, and by anyone routing through a private proxy.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Shrink the first-byte deadline so the hung-stream fallback is testable
    /// in milliseconds instead of 90 seconds of wall clock.
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
    /// what lets a test stall the response *body*.
    #[cfg(test)]
    pub(crate) fn with_unary_read_timeout(mut self, timeout: Duration) -> Self {
        self.unary_client = reqwest::Client::builder()
            .read_timeout(timeout)
            .build()
            .expect("the test client builds");
        self
    }
}

// ── Wire types (OpenAI Responses API) ────────────────────────────────────

/// The Responses API request body.
#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    input: Vec<OpenAiInputItem>,
    /// Always `false`. The Responses API defaults `store` to `true`, which
    /// retains every request server-side — the whole replayed conversation:
    /// the user's source files, tool output, and system prompt — and lists it
    /// in the org's dashboard. A BYOK agent must not leave that residue by
    /// default, so every request opts out explicitly. The cost is deliberate:
    /// `store: false` also disables the encrypted-reasoning-item round-trip a
    /// later multi-turn reasoning optimization would want; that optimization
    /// must find another shape, not quietly re-enable retention.
    store: bool,
    /// The Responses API's dedicated system/developer-prompt field. We pick
    /// this over framing the system prompt as an `input` item with
    /// `role: "system"` — both are accepted, but `instructions` is the
    /// documented, stable mechanism specifically for "the model's
    /// persistent behavior" and keeps the system prompt out of the item
    /// array we're otherwise using purely for conversation turns and tool
    /// I/O, which is easier to reason about when building `input` from our
    /// one internal message list.
    ///
    /// Owned rather than borrowed: the hoisted system prompt is built by
    /// [`to_openai_input`] inside the body builder, so a borrow here would
    /// tie the request to a local that dies before it is sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    /// Nucleus sampling from `CompletionRequest.params`, skipped when `None`
    /// so a request without overrides serializes byte-identical to before
    /// (the prompt-cache contract). Gated exactly like `temperature`: the
    /// reasoning-model families reject sampling parameters with HTTP 400.
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    /// Processing tier from `params.service_tier` ("auto"/"default"/"flex"/
    /// "priority") — a routing hint, not a sampling one, so it is never
    /// gated by model family.
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<&'static str>,
    /// Response-detail control from `params.verbosity`, wrapped in the
    /// Responses API's `text` object (`{"verbosity": "low|medium|high"}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<OpenAiText>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiToolSchema>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<OpenAiReasoning>,
    /// Session-stable cache-routing key — see the field on
    /// [`OpenAiProvider`] for why this maximizes implicit-cache hit rate.
    prompt_cache_key: &'a str,
}

#[derive(Serialize)]
struct OpenAiReasoning {
    effort: &'static str,
}

/// The Responses API's `text` configuration object. Only `verbosity` is
/// modeled — the object exists solely to carry it, and it is omitted
/// entirely when the caller expressed no preference.
#[derive(Serialize)]
struct OpenAiText {
    verbosity: &'static str,
}

/// Map the engine's `Verbosity` enum to the API's lowercase token.
fn map_verbosity(verbosity: Verbosity) -> &'static str {
    match verbosity {
        Verbosity::Low => "low",
        Verbosity::Medium => "medium",
        Verbosity::High => "high",
    }
}

/// Map the engine's `ServiceTier` enum to the API's lowercase token.
fn map_service_tier(tier: ServiceTier) -> &'static str {
    match tier {
        ServiceTier::Auto => "auto",
        ServiceTier::Default => "default",
        ServiceTier::Flex => "flex",
        ServiceTier::Priority => "priority",
    }
}

/// The Responses API's function-tool shape: flat (`name`/`description`/
/// `parameters` at the top level), unlike Chat Completions' nested
/// `{"type":"function","function":{...}}` wrapper that `zai.rs` speaks.
#[derive(Serialize)]
struct OpenAiToolSchema {
    #[serde(rename = "type")]
    kind: &'static str,
    name: String,
    description: String,
    parameters: Value,
}

/// One item in the Responses API's `input` array. This replaces the flat
/// `messages` array every other adapter here uses — text turns are
/// `message` items, an assistant's tool call is its own `function_call`
/// item, and a tool result is its own `function_call_output` item
/// correlated back by `call_id`.
#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiInputItem {
    Message {
        role: &'static str,
        content: Vec<OpenAiContentPart>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiContentPart {
    InputText {
        text: String,
    },
    OutputText {
        text: String,
    },
    /// A user-attached image. The Responses API takes the payload as a data
    /// URI in a plain string `image_url` (unlike Chat Completions' object).
    InputImage {
        image_url: String,
    },
    /// A user-attached PDF, inlined as a `file_data` data URI.
    InputFile {
        filename: String,
        file_data: String,
    },
}

/// The Responses API ingests images and PDFs; audio and video degrade to
/// descriptive text notes (audio input rides separate model families and
/// endpoints, not the Responses text path).
const OPENAI_CAPS: crate::attachment::DialectCaps = crate::attachment::DialectCaps {
    images: true,
    pdfs: true,
    audio: false,
    video: false,
};

/// Map a user message's attachments to input parts (media before text).
fn attachment_parts(message: &CompletionMessage) -> Vec<OpenAiContentPart> {
    crate::attachment::wire_parts(&message.attachments, OPENAI_CAPS)
        .into_iter()
        .map(attachment_part)
        .collect()
}

/// One resolved part as a Responses input part.
fn attachment_part(part: crate::attachment::WirePart) -> OpenAiContentPart {
    match part {
        crate::attachment::WirePart::Image { media_type, base64 } => {
            OpenAiContentPart::InputImage {
                image_url: format!("data:{media_type};base64,{base64}"),
            }
        }
        crate::attachment::WirePart::Pdf { name, base64 } => OpenAiContentPart::InputFile {
            filename: name,
            file_data: format!("data:application/pdf;base64,{base64}"),
        },
        crate::attachment::WirePart::Text { text } => OpenAiContentPart::InputText { text },
        // Excluded by OPENAI_CAPS today; turning either cap on without adding
        // a part arm lands here — degrade, never abort the turn.
        part @ (crate::attachment::WirePart::Audio { .. }
        | crate::attachment::WirePart::Video { .. }) => OpenAiContentPart::InputText {
            text: crate::attachment::unsupported_part_note(&part, "OpenAI Responses API"),
        },
    }
}

/// Streamed SSE payloads from the Responses API. Unlike Chat Completions'
/// single chunk shape, this dialect sends many named event *types*
/// (`response.created`, `response.output_item.added`,
/// `response.output_text.delta`, `response.function_call_arguments.delta`,
/// `response.completed`, …). We model only what we aggregate and tolerate
/// everything else via `#[serde(other)]`, matching `zai.rs`'s "tolerate
/// keep-alive/ping frames" posture — a new event type OpenAI adds later
/// must never turn into a hard failure of the turn.
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum OpenAiStreamEvent {
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded {
        #[serde(default)]
        output_index: usize,
        item: OpenAiOutputItem,
    },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: String },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta {
        #[serde(default)]
        output_index: usize,
        delta: String,
    },
    /// One `function_call` item's arguments are complete. Modeled only so
    /// [`ToolCallObserver`] has a precise per-call boundary to announce on;
    /// the accumulated arguments remain the source of truth, so the wire's
    /// own copy of them is not read here.
    ///
    /// The chat-completions dialects have no such event and must announce a
    /// call when the *next* one starts, which leaves a stream's last call
    /// unannounced. This one does not have that gap.
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone {
        #[serde(default)]
        output_index: usize,
    },
    #[serde(rename = "response.completed")]
    Completed { response: OpenAiResponseObject },
    /// The response terminated in failure. The `response.error` object
    /// carries the code/message — modeled explicitly so it aborts the turn
    /// instead of falling into `Other` and returning truncated text as a
    /// bogus success.
    #[serde(rename = "response.failed")]
    Failed { response: OpenAiResponseObject },
    /// The response stopped before completing (e.g. `max_output_tokens`,
    /// `content_filter`). Returning the partial text as success would be a
    /// silent truncation, so this is surfaced as a terminal error.
    #[serde(rename = "response.incomplete")]
    Incomplete { response: OpenAiResponseObject },
    /// A top-level stream error frame (`event: error`), distinct from a
    /// `response.failed` wrapper.
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },
    #[serde(other)]
    Other,
}

/// The item announced by `response.output_item.added`. We only need to act
/// on `function_call` items (to learn the `call_id`/`name` before argument
/// deltas start arriving); `message` items and anything else are ignored —
/// their text arrives via `response.output_text.delta` regardless of which
/// item it belongs to, which is all the single-turn aggregation needs.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiOutputItem {
    FunctionCall {
        call_id: String,
        #[serde(default)]
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug, Default)]
struct OpenAiResponseObject {
    #[serde(default)]
    usage: Option<OpenAiUsage>,
    /// Present on `response.failed`.
    #[serde(default)]
    error: Option<OpenAiResponseError>,
    /// Present on `response.incomplete`.
    #[serde(default)]
    incomplete_details: Option<OpenAiIncompleteDetails>,
}

#[derive(Deserialize, Debug, Default)]
struct OpenAiResponseError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct OpenAiIncompleteDetails {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct OpenAiUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<OpenAiInputTokensDetails>,
    /// The Responses API's reasoning breakdown — the same fact the
    /// chat-completions dialect spells `completion_tokens_details`. Absent
    /// means never reported, which is not a reported zero.
    #[serde(default)]
    output_tokens_details: Option<OpenAiOutputTokensDetails>,
}

#[derive(Deserialize, Debug, Default)]
struct OpenAiInputTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Deserialize, Debug, Default)]
struct OpenAiOutputTokensDetails {
    /// Tokens spent thinking, already counted inside `output_tokens`.
    #[serde(default)]
    reasoning_tokens: Option<u64>,
}

/// Map the engine's one `ReasoningEffort` enum to the Responses API's
/// `reasoning.effort` parameter. Audited against the vendor docs (2026-07):
/// `reasoning.effort` now documents a model-dependent set that can include
/// `none`/`minimal`/`low`/`medium`/`high`/`xhigh`/`max`, but which values a
/// given model accepts varies per model. The adapter maps to the
/// `low`/`medium`/`high` tiers every current gpt-5/o-series reasoning model
/// accepts, and collapses `Xhigh`/`Max` to `"high"` rather than sending a tier
/// the routed model might reject — the same "never send a value the model
/// rejects" posture as the other adapters. (Offering the finer tiers would
/// require per-model capability gating the picker vocabulary does not yet
/// carry.)
pub(crate) fn map_reasoning_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High | ReasoningEffort::Xhigh | ReasoningEffort::Max => "high",
    }
}

/// Whether `model` is an OpenAI reasoning model (gpt-5 family or the
/// o-series). Their Responses API rejects the `temperature` sampling
/// parameter with HTTP 400; the caller omits it for these models.
fn is_reasoning_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("gpt-5") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
}

fn to_openai_input(messages: &[CompletionMessage]) -> (Option<String>, Vec<OpenAiInputItem>) {
    let mut instructions: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for message in messages {
        match message.role {
            MessageRole::System => instructions.push(message.content.clone()),
            MessageRole::User => {
                let mut content = attachment_parts(message);
                if !message.content.is_empty() || content.is_empty() {
                    content.push(OpenAiContentPart::InputText {
                        text: message.content.clone(),
                    });
                }
                out.push(OpenAiInputItem::Message {
                    role: "user",
                    content,
                });
            }
            MessageRole::Assistant => {
                if !message.content.is_empty() {
                    out.push(OpenAiInputItem::Message {
                        role: "assistant",
                        content: vec![OpenAiContentPart::OutputText {
                            text: message.content.clone(),
                        }],
                    });
                }
                for call in &message.tool_calls {
                    out.push(OpenAiInputItem::FunctionCall {
                        call_id: call.call_id.clone(),
                        name: call.name.clone(),
                        arguments: call.input.to_string(),
                    });
                }
            }
            // Responses API dialect: each tool result is its own
            // `function_call_output` item, correlated back to the call
            // solely by `call_id` — there is no wrapping "tool message".
            MessageRole::Tool => {
                for result in &message.tool_results {
                    let output = match &result.output {
                        stella_protocol::ToolOutput::Ok { content, .. } => content.clone(),
                        stella_protocol::ToolOutput::Error { message, .. } => {
                            format!("ERROR: {message}")
                        }
                    };
                    out.push(OpenAiInputItem::FunctionCallOutput {
                        call_id: result.call_id.clone(),
                        output,
                    });
                }
            }
        }
    }
    let instructions = if instructions.is_empty() {
        None
    } else {
        Some(instructions.join("\n\n"))
    };
    (instructions, out)
}

fn to_openai_tools(tools: &[stella_protocol::tool::ToolSchema]) -> Vec<OpenAiToolSchema> {
    tools
        .iter()
        .map(|tool| OpenAiToolSchema {
            kind: "function",
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
        })
        .collect()
}

/// The `Provider` impl itself — see the module's own docs for why it is not
/// in this file.
mod provider;

impl OpenAiProvider {
    /// Shared body of [`crate::provider::Provider::complete_ref`] and
    /// [`crate::provider::Provider::complete_observed_ref`] — both of which
    /// live in `openai/provider.rs`. The only difference between them is
    /// whether anything is told about the parts as they land.
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
        let outcome = stream::aggregate_openai_stream(
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
        let finish_reason =
            stream::final_finish_reason(outcome.truncated_at_limit, !outcome.tool_calls.is_empty());
        Ok(CompletionResult {
            text: outcome.text,
            tool_calls: outcome.tool_calls,
            usage: outcome.usage,
            model: self.model.clone(),
            cost_usd,
            finish_reason: Some(finish_reason),
            upstream_provider: None,
        })
    }

    /// The one request body both delivery paths serialize — `stream` is the
    /// only field on which they differ, so the unary fallback re-issues the
    /// byte-identical payload minus the stream flag.
    fn build_body(&self, req: CompletionRequestRef<'_>, stream: bool) -> OpenAiRequest<'_> {
        let (instructions, input) = to_openai_input(req.messages);
        let params = req.params.unwrap_or_default();
        OpenAiRequest {
            model: &self.model,
            input,
            store: false,
            instructions,
            stream,
            max_output_tokens: req.max_output_tokens,
            // gpt-5 family and the o-series are reasoning models whose
            // Responses API rejects `temperature` with HTTP 400. The engine's
            // default temperature (Some(0.0)) would otherwise fail every real
            // OpenAI turn Terminal, so omit it for those models.
            temperature: if is_reasoning_model(&self.model) {
                None
            } else {
                req.temperature
            },
            // Same 400-avoidance gate as temperature: the reasoning families
            // reject `top_p` too, and an ungated caller override would fail
            // every turn Terminal on exactly the models people set effort on.
            top_p: if is_reasoning_model(&self.model) {
                None
            } else {
                params.top_p
            },
            service_tier: params.service_tier.map(map_service_tier),
            text: params.verbosity.map(|verbosity| OpenAiText {
                verbosity: map_verbosity(verbosity),
            }),
            tools: to_openai_tools(req.tools),
            // `reasoning == Some(false)` suppresses the reasoning object even
            // when an effort is pinned — an explicit off must win. A bare
            // `Some(true)` with no effort turns thinking on at the API's
            // middle tier; otherwise a pinned effort maps as it always has,
            // and (None, None) keeps the field (and the pre-field bytes) off
            // the wire.
            reasoning: match (req.reasoning, req.effort) {
                (Some(false), _) => None,
                (_, Some(effort)) => Some(OpenAiReasoning {
                    effort: map_reasoning_effort(effort),
                }),
                (Some(true), None) => Some(OpenAiReasoning { effort: "medium" }),
                (None, None) => None,
            },
            prompt_cache_key: &self.prompt_cache_key,
        }
    }

    /// POST `body` to `/responses` and run the shared non-success ladder.
    /// Returns the successful response for the caller — streaming or unary —
    /// to consume. The two delivery paths differ in their client's read bound
    /// AND in what that bound's expiry means, so both ride as parameters.
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
        body: &OpenAiRequest<'_>,
        unary: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        let response = client
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(self.api_key.reveal())
            .json(body)
            .send()
            .await
            .map_err(|e| {
                if unary {
                    http::classify_unary_dispatch_error("OpenAI", &e)
                } else {
                    ProviderError::transport(e.to_string())
                }
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_after_ms = http::parse_retry_after_ms(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(http::classify_http_status(
                "OpenAI",
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
mod tests;
