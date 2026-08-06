//! Amazon Bedrock adapter — the Converse API
//! ("Bedrock | AWS chain | Converse/ConverseStream |
//! catalog-driven … Model Garden by ARN"). Two deliberate v1 scopings, both
//! consistent with postures already recorded elsewhere in this crate:
//!
//! - **Non-streaming `Converse`, not `ConverseStream`.** The streaming
//!   variant speaks `application/vnd.amazon.eventstream` — a binary framing
//!   with per-message CRC32 prologues, not SSE — an entirely separate
//!   transport decoder. `Converse` returns an identical `CompletionResult`,
//!   so nothing downstream of the adapter can tell the difference; what the
//!   scoping does cost is everything that needs the response *while it is
//!   still arriving*. [`BedrockProvider::complete_observed_ref`] recovers
//!   the half of that which does not need incremental arrival: the answer
//!   is announced as one terminal `text_delta`, so an observing host
//!   renders a completed answer instead of nothing at all (#1132). What
//!   stays out of reach is genuine token-by-token preview (the deck fills
//!   in one write rather than progressively, #612) and mid-stream tool-call
//!   announcement, so the engine's speculative execution never fires on
//!   this adapter — a call announced only once the whole body has landed
//!   would start speculation microseconds before dispatch asks for it,
//!   which is overhead wearing a fix's clothing. The event-stream decoder
//!   is what closes both; until then the whole generation has to fit inside
//!   one read, which is why this adapter takes `crate::http::unary_client`
//!   rather than the shared streaming client.
//! - **Explicit credentials, not the full AWS chain.** The adapter takes
//!   access key / secret / optional session token directly;
//!   [`crate::credential::BedrockCredentials`] resolves the secret, session
//!   token, and region under the standard `AWS_SECRET_ACCESS_KEY` /
//!   `AWS_SESSION_TOKEN` / `AWS_REGION` names, alongside
//!   [`crate::credential::ApiKey`] for `AWS_ACCESS_KEY_ID`.
//!
//!   Those names are resolved by whatever chain the *host* owns, handed down
//!   as a [`crate::credential::AuxCredentials`] set
//!   ([`crate::credential::BedrockCredentials::resolve_with`]), with the
//!   process environment as the fallback for a caller that has no chain of
//!   its own. That is what lets `stella auth set bedrock` persist the whole
//!   set, and what lets a sealed process — a benchmark trial whose design
//!   keeps provider secrets out of the environment entirely — reach Bedrock
//!   at all (#1301).
//!
//!   Profile files, SSO caches, IMDS, and the container-role endpoints stay
//!   out on purpose rather than as a gap: each resolves an identity
//!   *ambiently*, so a process would authenticate as whatever the host
//!   happens to be carrying. `stella-cli`'s `config::aux` module states the
//!   supported and excluded sources in full.
//!
//! Requests are signed with SigV4 implemented in `sigv4` below — pure
//! functions over explicit inputs, pinned by golden vectors generated from
//! botocore's reference implementation (see `sigv4::tests`), because
//! request signing is exactly the kind of code that "looks right" while
//! producing signatures a real endpoint rejects.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stella_protocol::{
    CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage, FinishReason,
    MessageRole, ProviderError, ToolCall, ToolCallObserver,
};

use crate::catalog::{Catalog, Pricing};
use crate::credential::ApiKey;
use crate::http;
use crate::provider::Provider;

pub struct BedrockProvider {
    client: reqwest::Client,
    access_key: ApiKey,
    secret_key: ApiKey,
    session_token: Option<ApiKey>,
    region: String,
    model: String,
    base_url_override: Option<String>,
    /// List pricing for `model`, resolved from the catalog at construction so
    /// `cost_usd` is computed on the real request path — never a hard-coded
    /// zero (which would silently disable budget enforcement for Bedrock).
    pricing: Option<Pricing>,
}

impl BedrockProvider {
    /// Build an adapter for `model` (a catalog-resolved Bedrock model id or
    /// inference-profile id, e.g. `us.anthropic.claude-sonnet-4-5-20250929-v1:0`)
    /// in `region`.
    pub fn new(
        access_key: ApiKey,
        secret_key: ApiKey,
        session_token: Option<ApiKey>,
        region: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let model = model.into();
        let catalog = Catalog::current();
        let pricing = catalog
            .resolve_for("bedrock", &model)
            .ok()
            .map(|e| e.pricing);
        Self {
            // Unary `Converse`, not `ConverseStream`: the whole generation
            // precedes the first response byte, so the shared streaming
            // client's per-read bound would cap the generation itself (#547).
            client: http::unary_client(),
            access_key,
            secret_key,
            session_token,
            region: region.into(),
            model,
            base_url_override: None,
            pricing,
        }
    }

    /// Override the scheme+host — used by conformance tests against a mock
    /// server, and by anyone routing through a private proxy. Signing is
    /// unaffected beyond the `host` header (a mock server never verifies
    /// signatures; a proxy forwards them).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url_override = Some(base_url.into());
        self
    }

    /// The wire path for this model's Converse call. Bedrock model ids
    /// routinely contain `:` (version suffixes) and ARNs contain `/` — both
    /// must be percent-encoded in the request path, exactly as the AWS SDKs
    /// send them, and the canonical URI signs the same encoded form.
    fn wire_path(&self) -> String {
        format!("/model/{}/converse", sigv4::uri_encode(&self.model))
    }

    fn base_url(&self) -> String {
        match &self.base_url_override {
            Some(base) => base.clone(),
            None => format!("https://bedrock-runtime.{}.amazonaws.com", self.region),
        }
    }
}

// ── Wire types (Bedrock Converse API) ────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConverseRequest {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<BedrockSystemBlock>,
    messages: Vec<BedrockMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inference_config: Option<BedrockInferenceConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_config: Option<BedrockToolConfig>,
    /// Converse's model-specific passthrough. The one field this adapter
    /// sends is Claude's extended-thinking switch — see
    /// [`BedrockAdditionalFields`]. Omitted entirely when reasoning is off,
    /// so the no-reasoning request body is byte-identical to the
    /// pre-passthrough shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_model_request_fields: Option<BedrockAdditionalFields>,
}

/// The `additionalModelRequestFields` payload. Its CONTENTS are the model
/// vendor's vocabulary, not Converse's camelCase — Claude-on-Bedrock takes
/// `reasoning_config` with the Anthropic legacy thinking shape
/// (`{"type":"enabled","budget_tokens":N}`), so these two structs stay
/// snake_case on the wire.
#[derive(Serialize, Debug)]
struct BedrockAdditionalFields {
    reasoning_config: BedrockReasoningConfig,
}

#[derive(Serialize, Debug)]
struct BedrockReasoningConfig {
    #[serde(rename = "type")]
    kind: &'static str,
    budget_tokens: u32,
}

#[derive(Serialize, Debug)]
struct BedrockTextBlock {
    text: String,
}

/// Converse's opt-in prompt-cache marker (`{"cachePoint": {"type": "default"}}`).
/// Same role as the Anthropic adapter's `cache_control` breakpoints: without
/// one, Bedrock caches nothing and every turn re-pays the replayed prefix at
/// the full input rate. Placed after the system blocks (tools+system tier)
/// and at the tail of the last message (conversation-incremental tier).
#[derive(Serialize, Debug, Clone, Copy)]
struct BedrockCachePoint {
    #[serde(rename = "type")]
    kind: &'static str,
}

const CACHE_POINT: BedrockCachePoint = BedrockCachePoint { kind: "default" };

/// One `system` array entry — a union of a text block or a cache point,
/// same one-field-set convention as [`BedrockContentBlock`].
#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct BedrockSystemBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_point: Option<BedrockCachePoint>,
}

#[derive(Serialize, Debug)]
struct BedrockMessage {
    role: &'static str,
    content: Vec<BedrockContentBlock>,
}

/// One content block. Exactly one field is set per block (the wire treats
/// them as a union) — same shape convention as `gemini.rs`'s outbound part.
#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct BedrockContentBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<BedrockMediaBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document: Option<BedrockDocumentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    video: Option<BedrockMediaBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use: Option<BedrockToolUse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_result: Option<BedrockToolResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_point: Option<BedrockCachePoint>,
}

/// An image or video block: a Converse format token plus base64 bytes.
#[derive(Serialize, Debug)]
struct BedrockMediaBlock {
    format: &'static str,
    source: BedrockMediaSource,
}

#[derive(Serialize, Debug)]
struct BedrockDocumentBlock {
    format: &'static str,
    /// Converse restricts document names to alphanumerics, whitespace,
    /// hyphens, parentheses, and square brackets — see
    /// [`sanitize_document_name`].
    name: String,
    source: BedrockMediaSource,
}

#[derive(Serialize, Debug)]
struct BedrockMediaSource {
    /// Base64 in the HTTP JSON encoding of the Converse API.
    bytes: String,
}

/// Converse ingests images, PDFs, and video natively; audio degrades to a
/// descriptive text note.
const BEDROCK_CAPS: crate::attachment::DialectCaps = crate::attachment::DialectCaps {
    images: true,
    pdfs: true,
    audio: false,
    video: true,
};

/// Converse image `format` token for a MIME type — only the documented set;
/// anything else degrades rather than being rejected by the API.
fn bedrock_image_format(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpeg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// Converse video `format` token for a MIME type (documented set only).
fn bedrock_video_format(media_type: &str) -> Option<&'static str> {
    match media_type {
        "video/mp4" => Some("mp4"),
        "video/quicktime" => Some("mov"),
        "video/webm" => Some("webm"),
        "video/x-matroska" => Some("mkv"),
        "video/mpeg" => Some("mpeg"),
        "video/3gpp" => Some("three_gp"),
        "video/x-ms-wmv" => Some("wmv"),
        "video/x-flv" => Some("flv"),
        _ => None,
    }
}

/// Clamp a document name to Converse's allowed character set.
fn sanitize_document_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " -()[]".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.trim().is_empty() {
        "document".to_string()
    } else {
        cleaned
    }
}

/// Map a user message's attachments to Converse blocks (media before text).
/// A media type outside Converse's format allowlists degrades to a note
/// instead of a hard API rejection.
fn attachment_blocks(message: &CompletionMessage) -> Vec<BedrockContentBlock> {
    crate::attachment::wire_parts(&message.attachments, BEDROCK_CAPS)
        .into_iter()
        .map(attachment_block)
        .collect()
}

/// One resolved part as a Converse content block.
fn attachment_block(part: crate::attachment::WirePart) -> BedrockContentBlock {
    match part {
        crate::attachment::WirePart::Text { text } => BedrockContentBlock {
            text: Some(text),
            ..Default::default()
        },
        crate::attachment::WirePart::Image { media_type, base64 } => {
            match bedrock_image_format(&media_type) {
                Some(format) => BedrockContentBlock {
                    image: Some(BedrockMediaBlock {
                        format,
                        source: BedrockMediaSource { bytes: base64 },
                    }),
                    ..Default::default()
                },
                None => unsupported_format_note("image", &media_type),
            }
        }
        crate::attachment::WirePart::Video { media_type, base64 } => {
            match bedrock_video_format(&media_type) {
                Some(format) => BedrockContentBlock {
                    video: Some(BedrockMediaBlock {
                        format,
                        source: BedrockMediaSource { bytes: base64 },
                    }),
                    ..Default::default()
                },
                None => unsupported_format_note("video", &media_type),
            }
        }
        crate::attachment::WirePart::Pdf { name, base64 } => BedrockContentBlock {
            document: Some(BedrockDocumentBlock {
                format: "pdf",
                name: sanitize_document_name(&name),
                source: BedrockMediaSource { bytes: base64 },
            }),
            ..Default::default()
        },
        // Excluded by BEDROCK_CAPS today; turning audio on without adding a
        // block arm lands here — degrade, never abort the turn.
        part @ crate::attachment::WirePart::Audio { .. } => BedrockContentBlock {
            text: Some(crate::attachment::unsupported_part_note(
                &part,
                "Bedrock Converse API",
            )),
            ..Default::default()
        },
    }
}

/// The degrade note for a media type Converse ingests as a *kind* but whose
/// specific format is outside its allowlist (a TIFF image, an AVI video).
/// Distinct from [`crate::attachment::unsupported_part_note`], which covers a
/// kind this dialect cannot carry at all: here the block shape exists and only
/// the `format` token is missing, so sending it would be a hard API rejection
/// where a note keeps the turn alive.
fn unsupported_format_note(kind: &str, media_type: &str) -> BedrockContentBlock {
    BedrockContentBlock {
        text: Some(format!(
            "[the user attached a {kind} of type {media_type}, which Bedrock's Converse API \
             does not accept; acknowledge it and suggest a supported format]"
        )),
        ..Default::default()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct BedrockToolUse {
    tool_use_id: String,
    name: String,
    #[serde(default)]
    input: Value,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct BedrockToolResult {
    tool_use_id: String,
    content: Vec<BedrockTextBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'static str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BedrockInferenceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    /// Nucleus sampling from `CompletionRequest.params` — `topP` is the only
    /// sampling override Converse's model-agnostic `inferenceConfig` speaks.
    /// The rest of `GenerationParams` (top_k, penalties, seed, …) would need
    /// per-model spellings under `additionalModelRequestFields` (which this
    /// adapter uses only for the reasoning switch, whose Claude spelling is
    /// documented); per the never-fail contract those are silently dropped
    /// rather than guessed at.
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
}

#[derive(Serialize)]
struct BedrockToolConfig {
    tools: Vec<BedrockToolEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BedrockToolEntry {
    tool_spec: BedrockToolSpec,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BedrockToolSpec {
    name: String,
    description: String,
    input_schema: BedrockInputSchema,
}

#[derive(Serialize)]
struct BedrockInputSchema {
    json: Value,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ConverseResponse {
    #[serde(default)]
    output: Option<ConverseOutput>,
    #[serde(default)]
    usage: Option<BedrockUsage>,
    /// Converse's stop vocabulary (`end_turn`, `tool_use`, `max_tokens`,
    /// `stop_sequence`, `guardrail_intervened`, `content_filtered`).
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ConverseOutput {
    #[serde(default)]
    message: Option<ConverseOutputMessage>,
}

#[derive(Deserialize, Debug)]
struct ConverseOutputMessage {
    #[serde(default)]
    content: Vec<InboundContentBlock>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct InboundContentBlock {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tool_use: Option<BedrockToolUse>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct BedrockUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    /// Tokens written to the prompt cache by this call (Converse
    /// `cacheWriteInputTokens`) — reported separately from `inputTokens`,
    /// surfaced as the normalized `cache_write_tokens` and never folded into
    /// `input_tokens` — `Pricing::cost_usd` bills them at the catalog's
    /// `cache_write_usd_per_mtok` instead.
    #[serde(default)]
    cache_write_input_tokens: u64,
}

/// Translate the engine's one message list into Converse `system` +
/// `messages`. Dialect rules: system turns hoist into the dedicated
/// `system` array; an assistant's tool calls are `toolUse` blocks on its
/// own message; tool results are `toolResult` blocks on a **user**-role
/// message (Converse's framing for "the environment answered"), each
/// correlated by `toolUseId` and carrying `status: "error"` for failed
/// tools instead of the text-prefix convention the OpenAI dialects use.
fn to_bedrock_messages(
    messages: &[CompletionMessage],
) -> (Vec<BedrockTextBlock>, Vec<BedrockMessage>) {
    let mut system = Vec::new();
    let mut out: Vec<BedrockMessage> = Vec::new();
    for message in messages {
        match message.role {
            MessageRole::System => system.push(BedrockTextBlock {
                text: message.content.clone(),
            }),
            MessageRole::User => {
                let mut content = attachment_blocks(message);
                if !message.content.is_empty() || content.is_empty() {
                    content.push(BedrockContentBlock {
                        text: Some(message.content.clone()),
                        ..Default::default()
                    });
                }
                out.push(BedrockMessage {
                    role: "user",
                    content,
                });
            }
            MessageRole::Assistant => {
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(BedrockContentBlock {
                        text: Some(message.content.clone()),
                        ..Default::default()
                    });
                }
                for call in &message.tool_calls {
                    content.push(BedrockContentBlock {
                        tool_use: Some(BedrockToolUse {
                            tool_use_id: call.call_id.clone(),
                            name: call.name.clone(),
                            input: call.input.clone(),
                        }),
                        ..Default::default()
                    });
                }
                if !content.is_empty() {
                    out.push(BedrockMessage {
                        role: "assistant",
                        content,
                    });
                }
            }
            MessageRole::Tool => {
                let content: Vec<BedrockContentBlock> = message
                    .tool_results
                    .iter()
                    .map(|result| {
                        let (text, status) = match &result.output {
                            stella_protocol::ToolOutput::Ok { content } => (content.clone(), None),
                            stella_protocol::ToolOutput::Error { message } => {
                                (message.clone(), Some("error"))
                            }
                        };
                        BedrockContentBlock {
                            tool_result: Some(BedrockToolResult {
                                tool_use_id: result.call_id.clone(),
                                content: vec![BedrockTextBlock { text }],
                                status,
                            }),
                            ..Default::default()
                        }
                    })
                    .collect();
                if !content.is_empty() {
                    out.push(BedrockMessage {
                        role: "user",
                        content,
                    });
                }
            }
        }
    }
    (system, out)
}

fn to_bedrock_tool_config(
    tools: &[stella_protocol::tool::ToolSchema],
) -> Option<BedrockToolConfig> {
    if tools.is_empty() {
        return None;
    }
    Some(BedrockToolConfig {
        tools: tools
            .iter()
            .map(|tool| BedrockToolEntry {
                tool_spec: BedrockToolSpec {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: BedrockInputSchema {
                        json: tool.input_schema.clone(),
                    },
                },
            })
            .collect(),
    })
}

/// Whether the target model family supports Converse `cachePoint` blocks.
/// Bedrock rejects cache points on models without prompt-caching support
/// (ValidationException), so the markers are added only for families with
/// documented support — Anthropic models and Amazon Nova. The catalog's
/// bedrock entries are all Claude inference profiles today; the gate keeps
/// a custom-routed model id from breaking every request.
///
/// Matched on the vendor token as well as the family names, because Bedrock
/// ids spell the family several ways (`anthropic.claude-…` foundation
/// models, `us.anthropic.…` cross-region profiles) and every
/// Anthropic-vendored model on Bedrock supports caching — a gate that
/// recognized only "claude" would run a differently-named Anthropic id with
/// caching silently OFF, the exact defect class the parity matrix
/// (invariant 8) exists to prevent. The known blind spot is an
/// application-inference-profile ARN, which is opaque by construction: the
/// family is not recoverable from the string, and defaulting to sending the
/// marker would 400 every request on a non-supporting model — the worse
/// failure. Route such profiles through a catalog entry whose id names the
/// family.
fn supports_cache_points(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    ["anthropic", "claude", "fable", "mythos", "nova"]
        .iter()
        .any(|family| model.contains(family))
}

#[async_trait]
impl Provider for BedrockProvider {
    fn id(&self) -> &str {
        "bedrock"
    }

    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let (system_text, mut messages) = to_bedrock_messages(req.messages);
        let cache = supports_cache_points(&self.model);
        let mut system: Vec<BedrockSystemBlock> = system_text
            .into_iter()
            .map(|block| BedrockSystemBlock {
                text: Some(block.text),
                ..Default::default()
            })
            .collect();
        if cache && !system.is_empty() {
            // Tools+system cache tier — mirrors the Anthropic adapter's
            // system-block `cache_control` breakpoint.
            system.push(BedrockSystemBlock {
                cache_point: Some(CACHE_POINT),
                ..Default::default()
            });
        }
        if cache {
            // Conversation-tail cache tier: each turn reads the prefix the
            // previous turn wrote instead of re-paying the replayed history.
            if let Some(last) = messages.last_mut() {
                last.content.push(BedrockContentBlock {
                    cache_point: Some(CACHE_POINT),
                    ..Default::default()
                });
            }
        }
        // Only `top_p` has a Converse `inferenceConfig` slot — see the field
        // doc on [`BedrockInferenceConfig`] for why the rest of the params
        // are dropped here.
        let top_p = req.params.and_then(|p| p.top_p);
        // The reasoning switch rides `additionalModelRequestFields
        // .reasoning_config` — the Anthropic legacy `{type:"enabled",
        // budget_tokens}` shape (Converse has no adaptive/effort dialect), so
        // the effort→budget mapping and the budget↔max_tokens coupling mirror
        // `anthropic.rs`'s legacy branch exactly: the API requires
        // `budget < max_tokens`, so an uncapped thinking turn raises the
        // ceiling to budget + 8192, a caller cap clamps the budget to
        // `cap - 1024` (never below the 1024 floor), and a cap at or below
        // the floor leaves NO legal budget — thinking is omitted rather than
        // sent as a ValidationException. Temperature is likewise omitted
        // whenever thinking is on (the API rejects it alongside thinking).
        let reasoning_on = req.reasoning == Some(true);
        let thinking_budget =
            reasoning_on.then(|| crate::anthropic::thinking_budget_tokens(req.effort));
        let max_tokens = match (req.max_output_tokens, thinking_budget) {
            (Some(cap), _) => Some(cap),
            (None, Some(budget)) => Some(budget + 8_192),
            (None, None) => None,
        };
        let reasoning_config = thinking_budget.and_then(|budget| {
            let ceiling = max_tokens.unwrap_or(0);
            (ceiling > 1024).then(|| BedrockReasoningConfig {
                kind: "enabled",
                budget_tokens: budget.min(ceiling - 1024).max(1024),
            })
        });
        let temperature = if reasoning_config.is_some() {
            None
        } else {
            req.temperature
        };
        let inference_config = if max_tokens.is_none() && temperature.is_none() && top_p.is_none() {
            None
        } else {
            Some(BedrockInferenceConfig {
                max_tokens,
                temperature,
                top_p,
            })
        };
        let body = ConverseRequest {
            system,
            messages,
            inference_config,
            tool_config: to_bedrock_tool_config(req.tools),
            additional_model_request_fields: reasoning_config
                .map(|reasoning_config| BedrockAdditionalFields { reasoning_config }),
        };
        let payload =
            serde_json::to_vec(&body).map_err(|e| ProviderError::Malformed(e.to_string()))?;

        let base_url = self.base_url();
        let wire_path = self.wire_path();
        let url = format!("{base_url}{wire_path}");
        let host = sigv4::host_from_url(&url)
            .ok_or_else(|| ProviderError::transport(format!("unparseable Bedrock URL: {url}")))?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| ProviderError::transport(e.to_string()))?
            .as_secs();
        let amz_date = sigv4::format_amz_date(now as i64);

        let signing = sigv4::sign(&sigv4::SigningInput {
            method: "POST",
            wire_path: &wire_path,
            canonical_query: "",
            host: &host,
            content_type: "application/json",
            payload: &payload,
            region: &self.region,
            service: "bedrock",
            access_key: self.access_key.reveal(),
            secret_key: self.secret_key.reveal(),
            session_token: self.session_token.as_ref().map(|t| t.reveal()),
            amz_date: &amz_date,
        });

        let mut request = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("x-amz-date", &amz_date)
            .header("authorization", signing.authorization)
            .body(payload);
        if let Some(token) = &self.session_token {
            request = request.header("x-amz-security-token", token.reveal());
        }

        let response = request
            .send()
            .await
            .map_err(|e| http::classify_unary_dispatch_error("Bedrock", &e))?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            // Vendor pre-check ahead of the shared ladder: surface the
            // server's own throttle message plus any Retry-After hint, so
            // the driver can honor the provider's stated backoff. The echoed
            // body goes through the same bound the shared ladder applies —
            // a throttle answered by an API-Gateway/CDN error page is a
            // multi-kilobyte document, and this message reaches the
            // transcript, the log, and the user's terminal.
            let retry_after_ms = http::parse_retry_after_ms(response.headers());
            let text = response.text().await.unwrap_or_default();
            let message = if text.trim().is_empty() {
                "Bedrock throttled the request".to_string()
            } else {
                format!(
                    "Bedrock throttled the request: {}",
                    http::body_snippet(text.trim())
                )
            };
            return Err(ProviderError::RateLimited {
                message,
                retry_after_ms,
            });
        }
        if !status.is_success() {
            let retry_after_ms = http::parse_retry_after_ms(response.headers());
            let body = response.text().await.unwrap_or_default();
            return Err(http::classify_http_status(
                "Bedrock",
                status,
                retry_after_ms,
                &body,
                &self.model,
            ));
        }

        let parsed: ConverseResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Malformed(e.to_string()))?;

        let mut text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        if let Some(message) = parsed.output.and_then(|o| o.message) {
            for block in message.content {
                if let Some(t) = block.text {
                    text.push_str(&t);
                }
                if let Some(tool_use) = block.tool_use {
                    // A no-argument Converse tool call omits `input` on the
                    // wire, so the `#[serde(default)]` field deserializes to
                    // `Value::Null` — which is the malformed-call sentinel
                    // `driver.rs::execute_with_repair` checks for. Normalize
                    // it to an empty object so a perfectly valid no-arg call
                    // is never "repaired", and so a tool deserializing its
                    // input as an object is never handed `null` (the same
                    // rule `gemini.rs` applies to its own absent `args`).
                    let input = if tool_use.input.is_null() {
                        serde_json::json!({})
                    } else {
                        tool_use.input
                    };
                    tool_calls.push(ToolCall {
                        call_id: tool_use.tool_use_id,
                        name: tool_use.name,
                        input,
                    });
                }
            }
        }
        let usage_reported = parsed.usage.is_some();
        let usage = parsed.usage.unwrap_or_default();
        let usage = CompletionUsage {
            reported: usage_reported,
            // Converse's `inputTokens` EXCLUDES cache reads (same meter split
            // as Anthropic's native API), so reads are folded back in to keep
            // the normalized subset invariant (cached ⊆ input) that
            // `Pricing::cost_usd` relies on — otherwise cached tokens would
            // be double-subtracted and the turn under-billed.
            input_tokens: usage.input_tokens + usage.cache_read_input_tokens,
            output_tokens: usage.output_tokens,
            cached_input_tokens: usage.cache_read_input_tokens,
            cache_write_tokens: usage.cache_write_input_tokens,
            // Converse reports no reasoning breakdown — thinking rides inside
            // `outputTokens` exactly as it does on Anthropic's native API.
            // `None` = not reported, never a measured zero.
            reasoning_tokens: None,
        };
        let cost_usd = self.pricing.map(|p| p.cost_usd(&usage)).unwrap_or(0.0);
        // Normalize Converse's stop vocabulary so the driver's truncation
        // diagnostics fire on Bedrock turns too; unknown values stay `None`
        // per the `CompletionResult` contract.
        let finish_reason = match parsed.stop_reason.as_deref() {
            Some("end_turn" | "stop_sequence") => Some(FinishReason::Stop),
            Some("max_tokens") => Some(FinishReason::Length),
            Some("tool_use") => Some(FinishReason::ToolCalls),
            Some("guardrail_intervened" | "content_filtered") => Some(FinishReason::ContentFilter),
            _ => None,
        };

        Ok(CompletionResult {
            text,
            tool_calls,
            usage,
            model: self.model.clone(),
            cost_usd,
            finish_reason,
        })
    }

    /// The whole answer as one terminal `text_delta`.
    ///
    /// Every sibling adapter threads the observer through an SSE parse loop
    /// and emits a delta per chunk. Converse is not a stream — it answers
    /// with one JSON body — so there is no finer-grained emission to make,
    /// and this is an honest single-chunk announcement rather than a
    /// masked bug: the `text_delta` contract is explicitly best-effort and
    /// lossy, with `CompletionResult::text` as the definitive answer.
    ///
    /// Emitting *something* matters because the alternative is the trait's
    /// default, which discards the observer entirely. A host watching the
    /// observed path — `stella-tui`'s live rendering, `stella-serve`'s
    /// `RemoteProvider` forwarding deltas to a browser — then sees total
    /// silence for the whole call and renders as if the process hung
    /// (#1132).
    ///
    /// Empty text is not announced: a tool-call-only turn has no answer
    /// text, and a zero-length delta would make an observer counting
    /// deltas believe an answer arrived. Tool calls are deliberately not
    /// announced either — see the module docs.
    async fn complete_observed_ref(
        &self,
        req: CompletionRequestRef<'_>,
        observer: &dyn ToolCallObserver,
    ) -> Result<CompletionResult, ProviderError> {
        let result = self.complete_ref(req).await?;
        if !result.text.is_empty() {
            observer.text_delta(&result.text);
        }
        Ok(result)
    }
}

/// AWS Signature Version 4 over explicit inputs — no ambient clock, no
/// ambient env — so every step is unit-testable and the composed result can
/// be pinned against botocore-generated golden vectors. Only what Converse
/// needs is implemented (POST, empty query string, four signed headers);
/// growing it further should extend the golden-vector suite in lockstep.
pub(crate) mod sigv4 {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::{Digest, Sha256};

    type HmacSha256 = Hmac<Sha256>;

    pub(crate) struct SigningInput<'a> {
        pub method: &'a str,
        /// The request path exactly as sent on the wire (already
        /// percent-encoded once — see [`uri_encode`]). Per the SigV4 spec,
        /// every service except S3 signs a canonical URI whose segments are
        /// URI-encoded *twice*; [`sign`] derives that second encoding from
        /// this wire form (`%3A` → `%253A`), matching botocore — the
        /// golden-vector tests pin this exact behavior, which a plausible
        /// "sign what you send" implementation gets wrong.
        pub wire_path: &'a str,
        pub canonical_query: &'a str,
        pub host: &'a str,
        pub content_type: &'a str,
        pub payload: &'a [u8],
        pub region: &'a str,
        pub service: &'a str,
        pub access_key: &'a str,
        pub secret_key: &'a str,
        pub session_token: Option<&'a str>,
        /// `YYYYMMDD'T'HHMMSS'Z'` — from [`format_amz_date`] in production,
        /// fixed in tests.
        pub amz_date: &'a str,
    }

    pub(crate) struct SigningOutput {
        pub authorization: String,
    }

    /// Percent-encode one URI path segment per RFC 3986: unreserved
    /// characters (`A-Z a-z 0-9 - . _ ~`) pass through, everything else —
    /// including `:` in Bedrock version suffixes and `/` in ARNs — becomes
    /// uppercase `%XX`.
    pub(crate) fn uri_encode(segment: &str) -> String {
        let mut out = String::with_capacity(segment.len());
        for byte in segment.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(byte as char)
                }
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        out
    }

    /// The canonical URI derived from a wire path: the SigV4 second
    /// encoding pass for non-S3 services. `/` passes through (it separates
    /// segments), unreserved bytes pass through, and everything else —
    /// including the `%` of existing escapes — is re-encoded.
    fn canonical_uri_from_wire_path(wire_path: &str) -> String {
        let mut out = String::with_capacity(wire_path.len());
        for byte in wire_path.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                    out.push(byte as char)
                }
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        out
    }

    /// `host[:port]` from a URL, the exact value reqwest will send as the
    /// `Host` header (port included only when non-default) — the canonical
    /// headers must sign precisely what goes on the wire.
    pub(crate) fn host_from_url(url: &str) -> Option<String> {
        let parsed: reqwest::Url = url.parse().ok()?;
        let host = parsed.host_str()?;
        match parsed.port() {
            Some(port) => Some(format!("{host}:{port}")),
            None => Some(host.to_string()),
        }
    }

    /// Format seconds-since-epoch as SigV4's `YYYYMMDD'T'HHMMSS'Z'`.
    /// Implemented directly (days-to-civil-date, Howard Hinnant's
    /// algorithm) rather than pulling a date crate for one format.
    pub(crate) fn format_amz_date(epoch_secs: i64) -> String {
        let days = epoch_secs.div_euclid(86_400);
        let secs_of_day = epoch_secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        let hour = secs_of_day / 3_600;
        let minute = (secs_of_day % 3_600) / 60;
        let second = secs_of_day % 60;
        format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
    }

    /// Days since 1970-01-01 → (year, month, day) in the proleptic
    /// Gregorian calendar.
    fn civil_from_days(days: i64) -> (i64, u32, u32) {
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let year = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if month <= 2 { year + 1 } else { year }, month, day)
    }

    fn sha256_hex(data: &[u8]) -> String {
        hex(&Sha256::digest(data))
    }

    fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Derive the per-day signing key: the documented
    /// `HMAC(HMAC(HMAC(HMAC("AWS4"+secret, date), region), service), "aws4_request")`
    /// chain.
    pub(crate) fn derive_signing_key(
        secret_key: &str,
        date: &str,
        region: &str,
        service: &str,
    ) -> Vec<u8> {
        // `Zeroizing`: `AWS4` + the raw secret is a fresh copy of the AWS
        // secret access key that outlives nothing but would otherwise be left
        // legible in freed heap once per request.
        let seed = zeroize::Zeroizing::new(format!("AWS4{secret_key}"));
        let k_date = hmac_sha256(seed.as_bytes(), date.as_bytes());
        let k_region = hmac_sha256(&k_date, region.as_bytes());
        let k_service = hmac_sha256(&k_region, service.as_bytes());
        hmac_sha256(&k_service, b"aws4_request")
    }

    pub(crate) fn sign(input: &SigningInput<'_>) -> SigningOutput {
        // `amz_date` is `YYYYMMDDTHHMMSSZ` and its first 8 bytes are the
        // credential-scope date. `get` rather than a `[..8]` slice: the field
        // is caller-supplied on a `pub(crate)` struct, so a short or
        // non-char-boundary value would panic mid-turn instead of producing a
        // signature the service simply rejects. Well-formed dates are
        // byte-identical either way (pinned by the botocore golden vectors).
        let date = input.amz_date.get(..8).unwrap_or(input.amz_date);
        let payload_hash = sha256_hex(input.payload);
        let canonical_uri = canonical_uri_from_wire_path(input.wire_path);

        // Canonical headers: lowercase names, trimmed values, sorted by
        // name, one per line. The fixed set Converse needs sorts as
        // content-type < host < x-amz-date < x-amz-security-token.
        let mut canonical_headers = format!(
            "content-type:{}\nhost:{}\nx-amz-date:{}\n",
            input.content_type, input.host, input.amz_date
        );
        let mut signed_headers = "content-type;host;x-amz-date".to_string();
        if let Some(token) = input.session_token {
            canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
            signed_headers.push_str(";x-amz-security-token");
        }

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            input.method,
            canonical_uri,
            input.canonical_query,
            canonical_headers,
            signed_headers,
            payload_hash
        );

        let scope = format!("{date}/{}/{}/aws4_request", input.region, input.service);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
            input.amz_date,
            sha256_hex(canonical_request.as_bytes())
        );

        let signing_key = derive_signing_key(input.secret_key, date, input.region, input.service);
        let signature = hex(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));

        SigningOutput {
            authorization: format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                input.access_key
            ),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn uri_encode_passes_unreserved_and_encodes_the_rest_uppercase() {
            assert_eq!(
                uri_encode("us.anthropic.claude-sonnet-4-5-20250929-v1:0"),
                "us.anthropic.claude-sonnet-4-5-20250929-v1%3A0"
            );
            assert_eq!(uri_encode("a/b c~d_e"), "a%2Fb%20c~d_e");
        }

        #[test]
        fn format_amz_date_handles_epoch_and_a_known_recent_instant() {
            assert_eq!(format_amz_date(0), "19700101T000000Z");
            // 2026-07-11 12:34:56 UTC — cross-checked with
            // `date -u -r 1783773296 +%Y%m%dT%H%M%SZ`.
            assert_eq!(format_amz_date(1_783_773_296), "20260711T123456Z");
        }

        #[test]
        fn derive_signing_key_matches_the_documented_aws_example() {
            // The worked example from AWS's SigV4 documentation
            // ("Deriving a signing key"): secret wJalr…, 20120215,
            // us-east-1, iam.
            let key = derive_signing_key(
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                "20120215",
                "us-east-1",
                "iam",
            );
            assert_eq!(
                hex(&key),
                "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d"
            );
        }

        #[test]
        fn host_from_url_includes_non_default_ports_and_drops_default_ones() {
            assert_eq!(
                host_from_url("https://bedrock-runtime.us-east-1.amazonaws.com/model/x/converse"),
                Some("bedrock-runtime.us-east-1.amazonaws.com".to_string())
            );
            assert_eq!(
                host_from_url("http://127.0.0.1:9099/model/x/converse"),
                Some("127.0.0.1:9099".to_string())
            );
        }

        #[test]
        fn canonical_uri_re_encodes_the_wire_paths_existing_escapes() {
            // The SigV4 double-encoding rule for non-S3 services: the `%3A`
            // sent on the wire signs as `%253A`. Botocore does this; a
            // "sign what you send" implementation fails against the real
            // endpoint with SignatureDoesNotMatch.
            assert_eq!(
                canonical_uri_from_wire_path(
                    "/model/us.anthropic.claude-sonnet-4-5-20250929-v1%3A0/converse"
                ),
                "/model/us.anthropic.claude-sonnet-4-5-20250929-v1%253A0/converse"
            );
        }

        /// Golden vector generated with botocore 1.43.46's `SigV4Auth` (the
        /// AWS reference implementation) for this exact request shape, with
        /// the clock frozen to `20260711T123456Z`. Pins the full composition:
        /// canonical request (double-encoded model id in the path), string to
        /// sign, key derivation, and header formatting.
        #[test]
        fn sign_matches_botocore_golden_vector_without_session_token() {
            let output = sign(&SigningInput {
                method: "POST",
                wire_path: "/model/us.anthropic.claude-sonnet-4-5-20250929-v1%3A0/converse",
                canonical_query: "",
                host: "bedrock-runtime.us-east-1.amazonaws.com",
                content_type: "application/json",
                payload: br#"{"messages":[{"role":"user","content":[{"text":"hi"}]}]}"#,
                region: "us-east-1",
                service: "bedrock",
                access_key: "AKIDEXAMPLE",
                secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                session_token: None,
                amz_date: "20260711T123456Z",
            });
            assert_eq!(
                output.authorization,
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260711/us-east-1/bedrock/aws4_request, \
                 SignedHeaders=content-type;host;x-amz-date, \
                 Signature=ae768b88a6982fa3a8811e2286c4360bf143e55da1d1aac37851bbf7a0b78773"
            );
        }

        /// Same request signed with a session token — the token joins both
        /// the canonical headers and the signed-headers list.
        #[test]
        fn sign_matches_botocore_golden_vector_with_session_token() {
            let output = sign(&SigningInput {
                method: "POST",
                wire_path: "/model/us.anthropic.claude-sonnet-4-5-20250929-v1%3A0/converse",
                canonical_query: "",
                host: "bedrock-runtime.us-east-1.amazonaws.com",
                content_type: "application/json",
                payload: br#"{"messages":[{"role":"user","content":[{"text":"hi"}]}]}"#,
                region: "us-east-1",
                service: "bedrock",
                access_key: "AKIDEXAMPLE",
                secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                session_token: Some("IQoJb3JpZ2luX2VjEXAMPLETOKEN"),
                amz_date: "20260711T123456Z",
            });
            assert_eq!(
                output.authorization,
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260711/us-east-1/bedrock/aws4_request, \
                 SignedHeaders=content-type;host;x-amz-date;x-amz-security-token, \
                 Signature=97c2b7044a3c4687c95e5e23e82d9d895c638ad815ab2d13743d6ad1e1d7b4ac"
            );
        }

        /// `amz_date` is a caller-supplied field on a `pub(crate)` struct, so
        /// the credential-scope date must be taken defensively: a value
        /// shorter than 8 bytes, or one whose 8th byte lands inside a
        /// multi-byte character, used to panic the signer mid-turn. The
        /// signature it produces for such a date is meaningless — the service
        /// rejects it — but a typed rejection beats an aborted process.
        #[test]
        fn sign_does_not_panic_on_a_malformed_amz_date() {
            // "", too short, and one whose byte 8 falls inside a 3-byte char.
            for amz_date in ["", "2026", "202607\u{20ac}1"] {
                let output = sign(&SigningInput {
                    method: "POST",
                    wire_path: "/model/m/converse",
                    canonical_query: "",
                    host: "bedrock-runtime.us-east-1.amazonaws.com",
                    content_type: "application/json",
                    payload: b"{}",
                    region: "us-east-1",
                    service: "bedrock",
                    access_key: "AKIDEXAMPLE",
                    secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                    session_token: None,
                    amz_date,
                });
                assert!(output.authorization.starts_with("AWS4-HMAC-SHA256 "));
            }
        }
    }
}

#[cfg(test)]
mod tests;
