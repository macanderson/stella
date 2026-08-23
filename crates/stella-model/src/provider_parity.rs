//! Provider feature-parity matrix — the structural guard against
//! per-provider gotchas.
//!
//! Born from a real defect: Anthropic models routed through OpenRouter ran
//! with NO prompt caching (CACHE 0% across a $2+ session) because
//! Anthropic's cache is explicit opt-in while most providers' caches are
//! implicit — a per-provider divergence nothing enforced. DeepSeek had the
//! sibling defect the same week: its native cache-hit field was silently
//! dropped because it spells the telemetry differently. This module makes
//! that class of gap structural instead of tribal: every provider id the
//! CLI can construct declares a row on each axis here, and tests fail when a
//! row is missing (`stella-cli` config tests), duplicated, or names a witness
//! test that no longer exists in the adapter sources.
//!
//! Six axes are guarded today, all born from the same shape of silent
//! per-provider divergence. (This sentence said "three" while listing four
//! for as long as the fourth existed — a count in prose beside the list it
//! counts is a cell that has to be written twice, which is why
//! `all_axes_cover_the_same_provider_ids` checks the tables and this sentence
//! merely introduces them.)
//! - [`CachePosture`] — how a provider's prompt cache is engaged and observed.
//! - [`ReasoningPosture`] — how a provider's reasoning/thinking budget is
//!   controlled on the wire. The reasoning axis has the sibling defect the
//!   cache axis had: only Z.ai (`thinking`) and OpenRouter (`reasoning`)
//!   honored a reasoning preference on the shared chat-completions adapter,
//!   so a pinned `effort` was *silently dropped* for xAI, DeepSeek, and local
//!   — the exact "nothing enforces the omission stays deliberate" gap.
//! - [`OverflowPosture`] — how a provider signals that a request exceeds the
//!   model's context window (#2680). There is no wire standard: each vendor
//!   spells it as its own 400 body, and an unrecognized spelling degrades to
//!   `ProviderError::Terminal` — safe (the turn aborts exactly as before the
//!   overflow-recovery path existed) but unrecovered, so which spellings are
//!   *detected* is a declared per-provider fact, not an assumption.
//! - [`OutputBudgetPosture`] — whether a provider refuses the *requested
//!   output ceiling* rather than billing what is spent, and whether that
//!   refusal is recognised. This diverges hardest at the gateways: a gateway
//!   prices the request against the ceiling the caller asks for, so a 128K
//!   ask is refused against a balance that would fund the real call several
//!   times over. Unrecognised, it aborts the turn — which cost three
//!   benchmark runs, every trial dead against a balance the provider itself
//!   said could afford a smaller ask.
//! - [`StreamFallbackPosture`] — how a provider recovers when its streaming
//!   path is broken before it delivers a byte (#2686).
//! - [`ParallelToolCallPosture`] — whether several tool calls ride one
//!   assistant message, which the engine's concurrent read-only dispatch and
//!   the system prompt's "send independent tool calls together" both depend
//!   on and neither could check (#4163). See [`parallel`] for why this axis
//!   records two facts rather than one.
//!
//! **The law for new providers:** adding a provider id means adding a row on
//! every axis here in the same PR, and a `Controllable`/`OptIn`/`Implicit`
//! row must name a witness test proving the posture on the wire — the opt-in
//! marker is sent, the hit telemetry is parsed into `CompletionUsage`, or the
//! reasoning control reaches the request body. The no-control variants
//! (`NotApplicable`, `Unsupported`, `FixedOn`, `FixedOff`) are allowed only
//! with a note a reviewer can check. The same pattern applies to any future
//! per-provider feature divergence (attachment dialects, tool schemas): when
//! one provider needs something the others don't, record the axis as a
//! matrix, don't leave it as adapter folklore.

pub mod parallel;

pub use parallel::{
    PARALLEL_TOOL_CALL_POSTURE, ParallelAdmission, ParallelToolCallPosture,
    parallel_tool_call_posture,
};

/// Every file a witness test — on any axis — can live in. `include_str!`
/// embeds them at compile time so a renamed or deleted witness fails the
/// build's tests rather than production.
///
/// The `zai/tests/` and `anthropic/tests/` submodules are listed individually
/// because this guard reads SOURCE TEXT, not the module tree: a witness that
/// stays a real, passing test but moves into a split-out module would vanish
/// from this list and fail the guard, which is a false alarm rather than the
/// rotted proof it exists to catch. The parent `tests.rs` files are over the
/// file-size ratchet, so those splits keep happening — the list has to follow
/// them.
///
/// Returned as a slice, not a sized array: the array length was one shared
/// cell every PR adding a source file had to write, and two green PRs (#2748
/// adding `zai/tests/stream_fallback.rs`, #2752 adding `http.rs`) composed
/// into a red `main` when the merge kept both entries and one length — the
/// same shape that removed the spelled-out total from `GATE_STEPS` (#1883).
///
/// Lives at module level rather than inside `mod tests` so the axis
/// submodules can check their own witnesses against the same list; the
/// `include_str!` paths stay relative to this file either way.
#[cfg(test)]
pub(crate) fn adapter_sources() -> &'static [&'static str] {
    &[
        // The overflow axis's witnesses live beside the classifier they
        // prove: overflow detection is shared plumbing
        // (`http::classify_http_status`), and its per-dialect tests exercise
        // each provider's exact body shape there.
        include_str!("http.rs"),
        include_str!("http/tests.rs"),
        include_str!("anthropic/tests.rs"),
        include_str!("anthropic/tests/cache_breakpoints.rs"),
        include_str!("anthropic/tests/thinking.rs"),
        // Beside `tests.rs`, not inside `tests/`: both parent `tests.rs`
        // files sit on the file-size ratchet, so the fan-in witnesses added
        // by #4163 and the stream-fallback witnesses added by #2746 are
        // declared from their adapter modules instead — even the one line
        // that would declare a submodule from `tests.rs` is growth.
        include_str!("anthropic/parallel_tool_calls.rs"),
        include_str!("anthropic/stream_fallback_tests.rs"),
        include_str!("zai/parallel_tool_calls.rs"),
        include_str!("bedrock/tests.rs"),
        include_str!("openai.rs"),
        // The Responses dialect's own `mod tests` lives in `openai.rs`, which
        // is a grandfathered god file closed to growth — so its parallel
        // fan-in witness lives in the sibling that owns the `Provider` impl
        // the test dispatches through.
        include_str!("openai/provider.rs"),
        include_str!("openai/stream_fallback_tests.rs"),
        include_str!("gemini/tests.rs"),
        include_str!("vertex.rs"),
        include_str!("zai/tests.rs"),
        include_str!("zai/tests/error_classify.rs"),
        include_str!("zai/tests/openrouter_effort.rs"),
        include_str!("zai/tests/openrouter_stream.rs"),
        include_str!("zai/tests/stream_fallback.rs"),
        include_str!("zai/tests/stream_frame.rs"),
        include_str!("zai/tests/vision.rs"),
        include_str!("zai/tests/zai_effort.rs"),
    ]
}

/// How a provider's prompt cache is engaged and observed.
#[derive(Debug)]
pub enum CachePosture {
    /// The adapter must SEND an explicit opt-in marker or the provider
    /// caches nothing (Anthropic `cache_control`, Bedrock `cachePoint`,
    /// OpenRouter's request-root `cache_control` for Claude routes).
    OptIn {
        /// The wire mechanism, for humans reading the matrix.
        mechanism: &'static str,
        /// Name of the test function that proves the marker reaches the
        /// wire; checked for existence by this module's tests.
        witness: &'static str,
    },
    /// The provider caches implicitly — nothing to send, but the adapter
    /// must PARSE the provider's hit telemetry or cached tokens bill at the
    /// full input rate and the cache stat pins at zero.
    Implicit {
        /// Where the hits are reported on this provider's usage envelope.
        telemetry: &'static str,
        /// Name of the test function that proves the telemetry lands in
        /// `CompletionUsage`; checked for existence by this module's tests.
        witness: &'static str,
    },
    /// No billable prompt cache exists to opt into or meter.
    NotApplicable { reason: &'static str },
}

/// One row per provider id constructible by the CLI (`config.rs
/// PROVIDERS` + `LOCAL_PROVIDER`; the completeness check lives in
/// `stella-cli`'s config tests, which see both lists). Settings-defined
/// custom providers inherit the shared OpenAI-compatible adapter and its
/// `prompt_tokens_details` parsing — they need no row of their own.
pub static CACHE_POSTURE: &[(&str, CachePosture)] = &[
    (
        "anthropic",
        CachePosture::OptIn {
            mechanism: "Messages API cache_control breakpoints (system + conversation tail), \
                        with a configurable 5m/1h TTL window (#1839): the 1-hour opt-in adds \
                        ttl: \"1h\" to every marker plus the extended-cache-ttl beta header, \
                        witnessed in anthropic/tests/cache_breakpoints.rs from both sides — \
                        the pair reaches the wire when configured, and the default window \
                        stays byte-identical",
            witness: "request_serializes_both_cache_breakpoints",
        },
    ),
    (
        "bedrock",
        CachePosture::OptIn {
            mechanism: "Converse cachePoint blocks, gated to supporting model families",
            witness: "complete_sends_cache_points_for_claude_models",
        },
    ),
    (
        "openrouter",
        CachePosture::OptIn {
            mechanism: "request-root cache_control {type: ephemeral} — required for Claude \
                        routes, ignored by implicit-cache upstreams — plus a session-stable \
                        top-level session_id requesting that every turn of a session reach \
                        the same upstream provider + cache shard (sticky routing). The \
                        session_id is a HINT the gateway may decline, not a pin: measured \
                        over one 17-minute session it held for the last 17 calls and not \
                        the first 32, leaving 20 of 69 calls reading zero cached tokens \
                        for 54% of the spend. Only a non-empty upstream_pin \
                        (provider.order + allow_fallbacks:false) actually fixes the route",
            witness: "openrouter_identity_sends_root_level_cache_control",
        },
    ),
    (
        "openai",
        CachePosture::Implicit {
            telemetry: "input_tokens_details.cached_tokens, plus session-stable \
                        prompt_cache_key cache-shard routing",
            witness: "complete_sends_a_session_stable_prompt_cache_key",
        },
    ),
    (
        "gemini",
        CachePosture::Implicit {
            telemetry: "usageMetadata.cachedContentTokenCount",
            witness: "complete_streams_and_aggregates_text_excluding_thought_parts",
        },
    ),
    (
        "vertex",
        CachePosture::Implicit {
            telemetry: "usageMetadata.cachedContentTokenCount (shared gemini aggregator)",
            witness: "complete_sends_a_bearer_token_to_the_project_scoped_path",
        },
    ),
    (
        "zai",
        CachePosture::Implicit {
            telemetry: "prompt_tokens_details.cached_tokens",
            witness: "complete_surfaces_cached_tokens_and_bills_them_at_the_cached_rate",
        },
    ),
    (
        "xai",
        CachePosture::Implicit {
            telemetry: "prompt_tokens_details.cached_tokens (shared chat-completions parse path)",
            witness: "xai_identity_surfaces_cached_tokens_in_usage",
        },
    ),
    (
        "deepseek",
        CachePosture::Implicit {
            telemetry: "top-level prompt_cache_hit_tokens — DeepSeek's native spelling; \
                        it sends no prompt_tokens_details object",
            witness: "deepseek_native_cache_hit_tokens_surface_as_cached_input",
        },
    ),
    (
        "local",
        CachePosture::NotApplicable {
            reason: "local servers prefix-cache for latency, not price — there is no \
                     billed cache to opt into; OpenAI-shape cached_tokens still parse \
                     via the shared adapter when a server reports them",
        },
    ),
];

/// The declared cache posture for `provider_id`, or `None` for an id the
/// matrix doesn't know — which the `stella-cli` completeness test turns
/// into a hard failure for any seeded provider.
pub fn cache_posture(provider_id: &str) -> Option<&'static CachePosture> {
    CACHE_POSTURE
        .iter()
        .find(|(id, _)| *id == provider_id)
        .map(|(_, posture)| posture)
}

/// How a provider's reasoning / thinking budget is controlled from a
/// completion request. The sibling of [`CachePosture`]: reasoning had the
/// same per-provider divergence and no guard — a pinned `effort` reached
/// only Z.ai and OpenRouter and was silently dropped everywhere else.
#[derive(Debug)]
pub enum ReasoningPosture {
    /// The adapter translates the engine's `effort`/`reasoning` preference
    /// into this provider's native reasoning control and puts it on the wire
    /// (Anthropic thinking budget, OpenRouter `reasoning`, OpenAI/xAI
    /// `reasoning[_]effort`, Gemini `thinkingLevel`, GLM `thinking`).
    Controllable {
        /// The wire mechanism, for humans reading the matrix.
        mechanism: &'static str,
        /// Name of the test function that proves the control reaches the
        /// request body; checked for existence by this module's tests.
        witness: &'static str,
        /// Effort tiers this adapter cannot express distinctly, as
        /// `(requested, served)` — empty when every tier reaches the wire as
        /// itself.
        ///
        /// `Controllable` used to mean "the effort is honoured", and four of
        /// the eight rows quietly did not honour it: they map the finer tiers
        /// onto a coarser set the routed model is guaranteed to accept, which
        /// is the right wire posture and the wrong thing to stay silent about
        /// (#1499). A user who pinned `max` on OpenAI was told nothing and
        /// served `high`.
        ///
        /// That matters more here than it would in most codebases: effort was
        /// the single largest measured variable in this repo's own paid arena
        /// runs, so serving a different tier than the one configured makes a
        /// benchmark comparison quietly untrue rather than merely surprising.
        ///
        /// Declared rather than computed because the mappings live in each
        /// adapter as separate functions over different wire vocabularies.
        /// This module's `collapsed_effort_tiers_match_the_adapters` test
        /// calls those functions and pins this list against what they really
        /// return, so a declaration and its adapter cannot drift apart.
        collapses: &'static [(&'static str, &'static str)],
    },
    /// The provider always reasons and the depth cannot be pinned from the
    /// request. Declared for taxonomy completeness — no id in the current
    /// fleet is classified here (DeepSeek's always-on reasoner is filed under
    /// [`ReasoningPosture::Unsupported`] instead, so a dropped effort still
    /// surfaces a notice), but a future reasoning-only model with no dial
    /// belongs here rather than pretending it is controllable.
    FixedOn { note: &'static str },
    /// The provider has no reasoning mode at all for the models this id
    /// serves. Declared for taxonomy completeness (see [`FixedOn`]).
    ///
    /// [`FixedOn`]: ReasoningPosture::FixedOn
    FixedOff { note: &'static str },
    /// The shared adapter deliberately drops `effort`/`reasoning` for this id:
    /// there is no portable Chat Completions reasoning field and an unknown
    /// key risks a hard 400. Honest degradation, not a silent one — a pinned
    /// effort against an `Unsupported` provider surfaces a one-line transcript
    /// notice (`stella-cli` boot chrome) rather than vanishing.
    Unsupported { note: &'static str },
}

/// One reasoning row per provider id constructible by the CLI — same
/// completeness contract as [`CACHE_POSTURE`] (enforced by `stella-cli`'s
/// config tests, which see both `PROVIDERS` and `LOCAL_PROVIDER`).
/// Settings-defined custom providers inherit the shared OpenAI-compatible
/// adapter, which sends no reasoning field, so they behave as `Unsupported`
/// and need no row of their own.
pub static REASONING_POSTURE: &[(&str, ReasoningPosture)] = &[
    (
        "anthropic",
        ReasoningPosture::Controllable {
            mechanism: "extended-thinking budget (thinking.budget_tokens) + output_config.effort \
                        — all five effort tiers map to distinct budgets",
            witness: "reasoning_true_enables_thinking_raises_max_tokens_and_omits_temperature",
            collapses: &[],
        },
    ),
    (
        "bedrock",
        ReasoningPosture::Controllable {
            mechanism: "additionalModelRequestFields.reasoning_config ({type:\"enabled\", \
                        budget_tokens}) — the Anthropic legacy thinking shape, effort tiers \
                        mapped to budgets by the same anthropic.rs mapping",
            witness: "reasoning_true_sends_reasoning_config_and_raises_max_tokens",
            collapses: &[],
        },
    ),
    (
        "openrouter",
        ReasoningPosture::Controllable {
            mechanism: "normalized reasoning object ({effort} / {enabled}), translated by the \
                        gateway to whatever the routed upstream vendor calls it",
            witness: "openrouter_identity_maps_reasoning_to_the_gateway_object",
            collapses: &[],
        },
    ),
    (
        "openai",
        ReasoningPosture::Controllable {
            mechanism: "Responses API reasoning.effort (low/medium/high; finer model-dependent \
                        tiers — minimal/xhigh/max — collapse to the universally-safe high)",
            witness: "reasoning_true_without_effort_defaults_to_medium",
            collapses: &[("xhigh", "high"), ("max", "high")],
        },
    ),
    (
        "gemini",
        ReasoningPosture::Controllable {
            mechanism: "thinkingConfig.thinkingLevel (low/high; medium/minimal exist only on some \
                        Gemini 3.x models, so the adapter maps to the portable low/high pair)",
            witness: "complete_sends_generation_config_params_on_the_wire",
            collapses: &[("medium", "high"), ("xhigh", "high"), ("max", "high")],
        },
    ),
    (
        "vertex",
        ReasoningPosture::Controllable {
            mechanism: "thinkingConfig.thinkingLevel via the shared gemini generation-config \
                        builder",
            witness: "complete_sends_shared_generation_config_params_on_the_wire",
            collapses: &[("medium", "high"), ("xhigh", "high"), ("max", "high")],
        },
    ),
    (
        "zai",
        ReasoningPosture::Controllable {
            mechanism: "GLM thinking object ({type: enabled|disabled}) for on/off, PLUS the \
                        top-level reasoning_effort (low/medium/high) for depth — verified \
                        accepted and honored by glm-5.2 on 2026-08-04. `minimal` is reachable \
                        only via the off switch, never from an effort tier: it returns zero \
                        reasoning tokens. A caller who pins no effort still sends no field, so \
                        the model keeps its own default depth",
            witness: "zai_identity_maps_pinned_effort_to_reasoning_effort",
            collapses: &[("xhigh", "high"), ("max", "high")],
        },
    ),
    (
        "xai",
        ReasoningPosture::Controllable {
            mechanism: "chat-completions top-level reasoning_effort (low/medium/high), gated to \
                        the xai identity on the shared adapter — and, within xai, skipped for the \
                        original grok-4, which reasons but 400s on the param (retiring 2026-08-15)",
            witness: "xai_identity_maps_effort_to_reasoning_effort",
            collapses: &[("xhigh", "high"), ("max", "high")],
        },
    ),
    (
        "deepseek",
        ReasoningPosture::Unsupported {
            note: "deepseek-reasoner reasons unconditionally and DeepSeek's chat-completions API \
                   exposes no request-level effort control; a pinned effort is dropped (surfaced \
                   as a boot notice) — the model reasons at its own fixed depth",
        },
    ),
    (
        "local",
        ReasoningPosture::Unsupported {
            note: "local OpenAI-compatible servers have no portable reasoning field; a pinned \
                   effort is dropped rather than guessed at — an unknown key risks a 400 on a \
                   server the user never opted into experimenting with",
        },
    ),
];

/// How a provider signals a context-window overflow on the wire, and whether
/// the shared classifier (`crate::http::classify_http_status`) detects it as
/// [`stella_protocol::ProviderError::ContextOverflow`] — the classification
/// the engine's reactive overflow recovery keys on (#2680).
#[derive(Debug)]
pub enum OverflowPosture {
    /// The provider's documented overflow signature is matched by the shared
    /// classifier, so an overflow rejection reaches the engine as
    /// `ContextOverflow` and recovery fires.
    Detected {
        /// The wire signature, for humans reading the matrix.
        signature: &'static str,
        /// Name of the test function that proves this provider's exact body
        /// shape classifies as `ContextOverflow`; checked for existence by
        /// this module's tests.
        witness: &'static str,
    },
    /// No signature verified against this provider's own wire. Its errors
    /// still funnel through the shared classifier, so an overflow phrased in
    /// one of the detected dialects is caught opportunistically; anything
    /// else degrades to `Terminal` — today's abort, safe but unrecovered.
    /// Verifying the real wire shape upgrades the row to [`Detected`].
    ///
    /// [`Detected`]: OverflowPosture::Detected
    BestEffort { note: &'static str },
}

/// One overflow row per provider id constructible by the CLI — same
/// completeness contract as [`CACHE_POSTURE`] (enforced by `stella-cli`'s
/// config tests). Settings-defined custom providers inherit the shared
/// OpenAI-compatible adapter and behave as `openai`-signature best-effort;
/// they need no row of their own.
pub static OVERFLOW_POSTURE: &[(&str, OverflowPosture)] = &[
    (
        "anthropic",
        OverflowPosture::Detected {
            signature: "HTTP 400 invalid_request_error, message \
                        `prompt is too long: N tokens > M maximum`",
            witness: "an_anthropic_prompt_too_long_400_classifies_as_context_overflow",
        },
    ),
    (
        "openai",
        OverflowPosture::Detected {
            signature: "HTTP 400 with error.code `context_length_exceeded` (chat completions) \
                        or message `...exceeds the context window` (Responses API)",
            witness: "an_openai_context_length_exceeded_400_classifies_as_context_overflow",
        },
    ),
    (
        "gemini",
        OverflowPosture::Detected {
            signature: "HTTP 400 INVALID_ARGUMENT, message `The input token count (N) exceeds \
                        the maximum number of tokens allowed (M)`",
            witness: "a_gemini_token_count_overflow_400_classifies_as_context_overflow",
        },
    ),
    (
        "vertex",
        OverflowPosture::Detected {
            signature: "same INVALID_ARGUMENT prose as gemini (shared error funnel)",
            witness: "a_gemini_token_count_overflow_400_classifies_as_context_overflow",
        },
    ),
    (
        "bedrock",
        OverflowPosture::Detected {
            signature: "HTTP 400 ValidationException, flat top-level message \
                        `Input is too long for requested model` / `too many input tokens`",
            witness: "a_bedrock_input_too_long_validation_400_classifies_as_context_overflow",
        },
    ),
    (
        "openrouter",
        OverflowPosture::BestEffort {
            note: "the gateway forwards the routed upstream vendor's own error body, so the \
                   anthropic/openai signatures usually match via the shared funnel — but the \
                   passthrough is not verified against the live gateway per upstream",
        },
    ),
    (
        "zai",
        OverflowPosture::BestEffort {
            note: "OpenAI-compatible error dialect on the shared funnel; GLM's exact overflow \
                   phrase is unverified on the wire",
        },
    ),
    (
        "xai",
        OverflowPosture::BestEffort {
            note: "OpenAI-compatible error dialect on the shared funnel; xAI's exact overflow \
                   phrase is unverified on the wire",
        },
    ),
    (
        "deepseek",
        OverflowPosture::BestEffort {
            note: "OpenAI-compatible error dialect on the shared funnel; DeepSeek's exact \
                   overflow phrase is unverified on the wire",
        },
    ),
    (
        "local",
        OverflowPosture::BestEffort {
            note: "local OpenAI-compatible servers each spell overflow their own way \
                   (llama.cpp, vLLM, ollama); the OpenAI signatures catch the compatible \
                   ones and the rest abort exactly as before",
        },
    ),
];

/// The declared overflow posture for `provider_id`, or `None` for an id the
/// matrix doesn't know — which the `stella-cli` completeness test turns into
/// a hard failure for any seeded provider.
pub fn overflow_posture(provider_id: &str) -> Option<&'static OverflowPosture> {
    OVERFLOW_POSTURE
        .iter()
        .find(|(id, _)| *id == provider_id)
        .map(|(_, posture)| posture)
}

/// Whether a provider's refusal to fund the *requested output ceiling* is
/// recognised as one, so the engine's clamp-and-re-ask recovery fires
/// instead of aborting the turn.
///
/// The output-side twin of [`OverflowPosture`], and a separate axis for the
/// same reason that one is: the failure is a distinct wire signature with a
/// distinct repair, and a provider can support one and not the other. It
/// diverges hardest at the gateways, because a gateway prices a request
/// against the ceiling the caller *asks for* — so it is the surface that
/// refuses a 128K ask against a balance that would fund the actual call
/// several times over. Direct vendor APIs bill what is spent and generally
/// do not reject on the ask at all.
#[derive(Debug)]
pub enum OutputBudgetPosture {
    /// The provider's affordability rejection is matched by the shared
    /// classifier, so it reaches the engine as
    /// `ProviderError::OutputBudgetExceeded` and the clamp ladder fires.
    Detected {
        /// The wire signature, for humans reading the matrix.
        signature: &'static str,
        /// Name of the test function that proves this provider's exact body
        /// shape classifies as `OutputBudgetExceeded`; checked for existence
        /// by this module's tests.
        witness: &'static str,
    },
    /// No affordability rejection verified against this provider's own wire.
    /// Its errors still funnel through the shared classifier, so a rejection
    /// phrased in a detected dialect is caught opportunistically; anything
    /// else stays `Terminal` — an abort, safe but unrecovered. Verifying the
    /// real wire shape upgrades the row to [`Detected`].
    ///
    /// [`Detected`]: OutputBudgetPosture::Detected
    BestEffort { note: &'static str },
}

/// One output-budget row per provider id constructible by the CLI — same
/// completeness contract as [`CACHE_POSTURE`] and [`OVERFLOW_POSTURE`].
pub static OUTPUT_BUDGET_POSTURE: &[(&str, OutputBudgetPosture)] = &[
    (
        "openrouter",
        OutputBudgetPosture::Detected {
            signature: "HTTP 402, message `This request requires more credits, or fewer \
                        max_tokens. You requested up to N tokens, but can only afford M.`",
            witness: "classify_http_status_402_naming_an_affordable_ceiling_is_recoverable",
        },
    ),
    (
        "anthropic",
        OutputBudgetPosture::BestEffort {
            note: "bills what is spent rather than pricing the ask; no affordability \
                   rejection observed on the wire",
        },
    ),
    (
        "openai",
        OutputBudgetPosture::BestEffort {
            note: "bills what is spent rather than pricing the ask; a hard-quota refusal \
                   arrives as 429 insufficient_quota, which is a different failure",
        },
    ),
    (
        "gemini",
        OutputBudgetPosture::BestEffort {
            note: "no affordability rejection observed; quota refusals are 429",
        },
    ),
    (
        "vertex",
        OutputBudgetPosture::BestEffort {
            note: "billed against the GCP project, not a prepaid balance",
        },
    ),
    (
        "bedrock",
        OutputBudgetPosture::BestEffort {
            note: "billed against the AWS account, not a prepaid balance",
        },
    ),
    (
        "zai",
        OutputBudgetPosture::BestEffort {
            note: "signals balance exhaustion as a 429 with code 1113 (see `zai.rs`), which \
                   is exhaustion rather than an unaffordable ask",
        },
    ),
    (
        "xai",
        OutputBudgetPosture::BestEffort {
            note: "OpenAI-compatible error dialect on the shared funnel; no affordability \
                   rejection verified on the wire",
        },
    ),
    (
        "deepseek",
        OutputBudgetPosture::BestEffort {
            note: "OpenAI-compatible error dialect on the shared funnel; no affordability \
                   rejection verified on the wire",
        },
    ),
    (
        "local",
        OutputBudgetPosture::BestEffort {
            note: "a local server bills nothing and has no balance to price an ask against",
        },
    ),
];

/// The declared output-budget posture for `provider_id`, or `None` for an id
/// the matrix doesn't know — which the `stella-cli` completeness test turns
/// into a hard failure for any seeded provider.
pub fn output_budget_posture(provider_id: &str) -> Option<&'static OutputBudgetPosture> {
    OUTPUT_BUDGET_POSTURE
        .iter()
        .find(|(id, _)| *id == provider_id)
        .map(|(_, posture)| posture)
}

/// The declared reasoning posture for `provider_id`, or `None` for an id the
/// matrix doesn't know — which the `stella-cli` completeness test turns into a
/// hard failure for any seeded provider.
pub fn reasoning_posture(provider_id: &str) -> Option<&'static ReasoningPosture> {
    REASONING_POSTURE
        .iter()
        .find(|(id, _)| *id == provider_id)
        .map(|(_, posture)| posture)
}

/// How a provider recovers when its *streaming path* is broken — the stream
/// hangs before its first byte (a proxy buffering the SSE body) or comes
/// back empty (a gateway answering 200 with no data). The third axis of the
/// matrix (#2686), with the same law as the other two: behavior that
/// diverges per provider is declared, never assumed.
#[derive(Debug)]
pub enum StreamFallbackPosture {
    /// The adapter arms a bounded per-session latch on a fallback-eligible
    /// stream fault and re-issues the retried attempt as a unary
    /// (non-streaming) request for the same payload — see
    /// `crate::stream_recovery` for the state machine.
    UnaryFallback {
        /// The wire mechanism, for humans reading the matrix.
        mechanism: &'static str,
        /// Name of the test function that proves the fallback: the faulted
        /// streaming attempt fails retryably and the retry completes over
        /// `stream: false`. Checked for existence by this module's tests.
        witness: &'static str,
    },
    /// The adapter streams and has no unary fallback path (yet): a broken
    /// streaming path fails the attempt with its ordinary classification.
    /// Allowed only with a note a reviewer can check.
    StreamingOnly { note: &'static str },
    /// The adapter is already unary — there is no stream to fall back from.
    AlwaysUnary { note: &'static str },
}

/// One stream-fallback row per provider id constructible by the CLI — same
/// completeness contract as the other two axes. Settings-defined custom
/// providers inherit the shared OpenAI-compatible adapter and its fallback,
/// so they need no row of their own.
pub static STREAM_FALLBACK_POSTURE: &[(&str, StreamFallbackPosture)] = &[
    (
        "anthropic",
        StreamFallbackPosture::UnaryFallback {
            mechanism: "Messages: retried attempt re-issues the byte-identical body with \
                        stream: false through the unary read bound (http::unary_client)",
            witness: "an_anthropic_stream_hung_before_its_first_byte_falls_back_to_a_unary_request",
        },
    ),
    (
        "bedrock",
        StreamFallbackPosture::AlwaysUnary {
            note: "the adapter calls Converse, not ConverseStream — every completion is \
                   already unary, so there is no stream to fall back from",
        },
    ),
    (
        "openrouter",
        StreamFallbackPosture::UnaryFallback {
            mechanism: "shared chat-completions adapter: retried attempt re-issues the \
                        byte-identical body with stream: false through the unary read bound",
            witness: "an_empty_stream_falls_back_to_a_non_streaming_request",
        },
    ),
    (
        "openai",
        StreamFallbackPosture::UnaryFallback {
            mechanism: "Responses: retried attempt re-issues the byte-identical body with \
                        stream: false through the unary read bound (http::unary_client)",
            witness: "an_openai_stream_hung_before_its_first_byte_falls_back_to_a_unary_request",
        },
    ),
    (
        "gemini",
        StreamFallbackPosture::StreamingOnly {
            note: "streamGenerateContent has no unary parse path yet (generateContent would \
                   be the fallback); tracked in #2746",
        },
    ),
    (
        "vertex",
        StreamFallbackPosture::StreamingOnly {
            note: "shares gemini's streaming aggregator and its gap; tracked in #2746",
        },
    ),
    (
        "zai",
        StreamFallbackPosture::UnaryFallback {
            mechanism: "retried attempt re-issues the byte-identical body with stream: false \
                        through the unary read bound (http::unary_client)",
            witness: "a_stream_hung_before_its_first_byte_falls_back_to_a_non_streaming_request",
        },
    ),
    (
        "xai",
        StreamFallbackPosture::UnaryFallback {
            mechanism: "shared chat-completions adapter fallback (see the zai row)",
            witness: "a_stream_hung_before_its_first_byte_falls_back_to_a_non_streaming_request",
        },
    ),
    (
        "deepseek",
        StreamFallbackPosture::UnaryFallback {
            mechanism: "shared chat-completions adapter fallback (see the zai row)",
            witness: "a_stream_hung_before_its_first_byte_falls_back_to_a_non_streaming_request",
        },
    ),
    (
        "local",
        StreamFallbackPosture::UnaryFallback {
            mechanism: "shared chat-completions adapter fallback (see the zai row) — local \
                        gateways and proxies are where SSE buffering is most likely",
            witness: "an_empty_stream_falls_back_to_a_non_streaming_request",
        },
    ),
];

/// The declared stream-fallback posture for `provider_id`, or `None` for an
/// id the matrix doesn't know — which the `stella-cli` completeness test
/// turns into a hard failure for any seeded provider.
pub fn stream_fallback_posture(provider_id: &str) -> Option<&'static StreamFallbackPosture> {
    STREAM_FALLBACK_POSTURE
        .iter()
        .find(|(id, _)| *id == provider_id)
        .map(|(_, posture)| posture)
}

/// The tier `effort` is really served as by `provider_id`, when the adapter
/// cannot put that tier on the wire distinctly — `None` when it reaches the
/// wire as itself.
///
/// The question `Controllable` alone could not answer (#1499). A caller that
/// reads only the posture learns the provider *has* a reasoning control, not
/// whether the level it asked for survived the mapping; four of the eight
/// controllable rows collapse the finer tiers onto a coarser set the routed
/// model is guaranteed to accept. This is the difference between "honoured"
/// and "accepted and quietly downgraded", and it is the input a notice needs.
///
/// `None` also for an unknown id and for every non-`Controllable` posture: a
/// provider with no reasoning control at all does not *downgrade* an effort,
/// it drops it, which is [`ReasoningPosture::Unsupported`]'s story to tell.
#[must_use]
pub fn downgraded_effort(provider_id: &str, effort: &str) -> Option<&'static str> {
    match reasoning_posture(provider_id)? {
        ReasoningPosture::Controllable { collapses, .. } => collapses
            .iter()
            .find(|(requested, _)| *requested == effort)
            .map(|(_, served)| *served),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every witness named in the cache matrix must exist as a test function
    /// in the adapter sources — a row whose proof rotted (test renamed or
    /// deleted) fails here, not as a production surprise.
    #[test]
    fn every_witness_test_exists_in_the_adapter_sources() {
        let sources = adapter_sources();
        for (id, posture) in CACHE_POSTURE {
            let witness = match posture {
                CachePosture::OptIn { witness, .. } | CachePosture::Implicit { witness, .. } => {
                    witness
                }
                CachePosture::NotApplicable { .. } => continue,
            };
            let needle = format!("fn {witness}(");
            assert!(
                sources.iter().any(|source| source.contains(&needle)),
                "cache-posture witness for `{id}` not found in adapter sources: {witness}"
            );
        }
    }

    /// Every declared effort collapse is what the adapter really sends.
    ///
    /// The matrix says which tiers a provider cannot express; the adapters
    /// decide it. Declaring by hand is how the two drift, so this calls the
    /// real mapping functions and compares. It fails in both directions: an
    /// undeclared collapse (the #1499 bug — a tier silently downgraded with
    /// nothing to notify from) and a declared one the adapter does not
    /// actually make (a notice that would lie the other way).
    #[test]
    fn collapsed_effort_tiers_match_the_adapters() {
        use stella_protocol::ReasoningEffort;

        const TIERS: &[(&str, ReasoningEffort)] = &[
            ("low", ReasoningEffort::Low),
            ("medium", ReasoningEffort::Medium),
            ("high", ReasoningEffort::High),
            ("xhigh", ReasoningEffort::Xhigh),
            ("max", ReasoningEffort::Max),
        ];

        /// The tier→tier renames we can call directly.
        ///
        /// `None` for the rows with no string mapping to compare: anthropic
        /// and bedrock map onto token *budgets* (five distinct values, so no
        /// tier is lost), and openrouter hands the tier to the gateway
        /// untouched. Those must therefore declare no collapses at all, which
        /// is asserted below rather than skipped.
        fn served(provider: &str, effort: ReasoningEffort) -> Option<&'static str> {
            match provider {
                "openai" => Some(crate::openai::map_reasoning_effort(effort)),
                "gemini" | "vertex" => Some(crate::gemini::map_thinking_level(effort)),
                "zai" => Some(crate::zai::effort::map_zai_effort(effort)),
                "xai" => Some(crate::zai::effort::map_xai_effort(effort)),
                _ => None,
            }
        }

        for (provider, posture) in REASONING_POSTURE {
            let ReasoningPosture::Controllable { collapses, .. } = posture else {
                continue;
            };

            if served(provider, ReasoningEffort::Low).is_none() {
                assert!(
                    collapses.is_empty(),
                    "{provider} has no tier→tier rename to lose a level in, so it must \
                     declare no collapses; it declares {collapses:?}"
                );
                continue;
            }

            for (name, tier) in TIERS {
                let actual = served(provider, *tier).expect("checked directly above");
                let declared = downgraded_effort(provider, name);
                if actual == *name {
                    assert_eq!(
                        declared, None,
                        "{provider}: '{name}' reaches the wire as itself, but the matrix \
                         declares it collapsed — a notice here would be wrong"
                    );
                } else {
                    assert_eq!(
                        declared,
                        Some(actual),
                        "{provider}: '{name}' is really served as '{actual}'. An \
                         undeclared collapse is the #1499 bug: the user is told nothing \
                         and gets a tier they did not pin"
                    );
                }
            }
        }
    }

    /// The reasoning-axis sibling: every `Controllable` row must name a test
    /// that exists in the adapter sources, proving the reasoning control
    /// reaches the wire. The no-control variants carry a note, not a witness.
    #[test]
    fn every_reasoning_witness_test_exists_in_the_adapter_sources() {
        let sources = adapter_sources();
        for (id, posture) in REASONING_POSTURE {
            let witness = match posture {
                ReasoningPosture::Controllable { witness, .. } => witness,
                ReasoningPosture::FixedOn { .. }
                | ReasoningPosture::FixedOff { .. }
                | ReasoningPosture::Unsupported { .. } => continue,
            };
            let needle = format!("fn {witness}(");
            assert!(
                sources.iter().any(|source| source.contains(&needle)),
                "reasoning-posture witness for `{id}` not found in adapter sources: {witness}"
            );
        }
    }

    /// The overflow-axis sibling: every `Detected` row must name a test that
    /// exists in the adapter sources, proving that provider's exact overflow
    /// body classifies as `ContextOverflow`. `BestEffort` rows carry a note,
    /// not a witness — they declare the absence of wire verification.
    #[test]
    fn every_overflow_witness_test_exists_in_the_adapter_sources() {
        let sources = adapter_sources();
        for (id, posture) in OVERFLOW_POSTURE {
            let witness = match posture {
                OverflowPosture::Detected { witness, .. } => witness,
                OverflowPosture::BestEffort { .. } => continue,
            };
            let needle = format!("fn {witness}(");
            assert!(
                sources.iter().any(|source| source.contains(&needle)),
                "overflow-posture witness for `{id}` not found in adapter sources: {witness}"
            );
        }
    }

    #[test]
    fn overflow_provider_ids_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (id, _) in OVERFLOW_POSTURE {
            assert!(seen.insert(id), "duplicate overflow-posture row for `{id}`");
        }
    }

    /// The output-budget axis, enforced exactly as its overflow twin is: a
    /// `Detected` row names a test proving that provider's affordability
    /// rejection reaches the engine as `OutputBudgetExceeded`, and a row
    /// whose witness has been renamed or deleted fails here rather than in a
    /// bench run that pays for it.
    #[test]
    fn every_output_budget_witness_test_exists_in_the_adapter_sources() {
        let sources = adapter_sources();
        for (id, posture) in OUTPUT_BUDGET_POSTURE {
            let witness = match posture {
                OutputBudgetPosture::Detected { witness, .. } => witness,
                OutputBudgetPosture::BestEffort { .. } => continue,
            };
            let needle = format!("fn {witness}(");
            assert!(
                sources.iter().any(|source| source.contains(&needle)),
                "output-budget witness for `{id}` not found in adapter sources: {witness}"
            );
        }
    }

    #[test]
    fn output_budget_provider_ids_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (id, _) in OUTPUT_BUDGET_POSTURE {
            assert!(
                seen.insert(id),
                "duplicate output-budget-posture row for `{id}`"
            );
        }
    }

    /// The axes must cover the same providers: a provider declared on one
    /// and missing from the other is the silent gap invariant 8 exists to
    /// prevent, and checking it here means neither table can drift alone.
    #[test]
    fn the_output_budget_axis_covers_every_provider_the_overflow_axis_does() {
        let overflow: std::collections::BTreeSet<_> =
            OVERFLOW_POSTURE.iter().map(|(id, _)| *id).collect();
        let budget: std::collections::BTreeSet<_> =
            OUTPUT_BUDGET_POSTURE.iter().map(|(id, _)| *id).collect();
        assert_eq!(overflow, budget, "the two axes cover different providers");
    }

    #[test]
    fn provider_ids_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (id, _) in CACHE_POSTURE {
            assert!(seen.insert(id), "duplicate cache-posture row for `{id}`");
        }
    }

    #[test]
    fn reasoning_provider_ids_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (id, _) in REASONING_POSTURE {
            assert!(
                seen.insert(id),
                "duplicate reasoning-posture row for `{id}`"
            );
        }
    }

    /// The stream-fallback axis's sibling check, which it did not have.
    ///
    /// AGENTS.md describes every axis as enforced from both sides, and this
    /// one was enforced from neither: no witness-existence test here and no
    /// completeness test in `stella-cli` (that half landed in the same PR).
    /// So a `UnaryFallback` row could name a test that had been renamed away
    /// and nothing would notice — the exact rot the other four axes are
    /// guarded against. The no-fallback variants carry a note, not a witness.
    #[test]
    fn every_stream_fallback_witness_test_exists_in_the_adapter_sources() {
        let sources = adapter_sources();
        for (id, posture) in STREAM_FALLBACK_POSTURE {
            let witness = match posture {
                StreamFallbackPosture::UnaryFallback { witness, .. } => witness,
                StreamFallbackPosture::StreamingOnly { .. }
                | StreamFallbackPosture::AlwaysUnary { .. } => continue,
            };
            let needle = format!("fn {witness}(");
            assert!(
                sources.iter().any(|source| source.contains(&needle)),
                "stream-fallback witness for `{id}` not found in adapter sources: {witness}"
            );
        }
    }

    #[test]
    fn stream_fallback_provider_ids_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (id, _) in STREAM_FALLBACK_POSTURE {
            assert!(
                seen.insert(id),
                "duplicate stream-fallback-posture row for `{id}`"
            );
        }
    }

    /// All axes must cover exactly the same set of provider ids — a provider
    /// present on one axis but not another is a matrix hole.
    ///
    /// Compared against `cache` pairwise rather than by intersecting all six,
    /// so a failure names *which* axis drifted instead of reporting that the
    /// set sizes disagree.
    #[test]
    fn all_axes_cover_the_same_provider_ids() {
        let cache: std::collections::BTreeSet<_> =
            CACHE_POSTURE.iter().map(|(id, _)| *id).collect();
        for (axis, ids) in [
            (
                "reasoning",
                REASONING_POSTURE
                    .iter()
                    .map(|(id, _)| *id)
                    .collect::<std::collections::BTreeSet<_>>(),
            ),
            (
                "overflow",
                OVERFLOW_POSTURE.iter().map(|(id, _)| *id).collect(),
            ),
            (
                "output-budget",
                OUTPUT_BUDGET_POSTURE.iter().map(|(id, _)| *id).collect(),
            ),
            (
                "stream-fallback",
                STREAM_FALLBACK_POSTURE.iter().map(|(id, _)| *id).collect(),
            ),
            (
                "parallel-tool-call",
                PARALLEL_TOOL_CALL_POSTURE
                    .iter()
                    .map(|(id, _)| *id)
                    .collect(),
            ),
        ] {
            assert_eq!(
                cache, ids,
                "the cache and {axis} matrices cover different provider ids"
            );
        }
    }

    #[test]
    fn lookup_finds_every_row_and_rejects_unknown_ids() {
        for (id, _) in CACHE_POSTURE {
            assert!(cache_posture(id).is_some());
        }
        assert!(cache_posture("no-such-provider").is_none());
        for (id, _) in REASONING_POSTURE {
            assert!(reasoning_posture(id).is_some());
        }
        assert!(reasoning_posture("no-such-provider").is_none());
        for (id, _) in OVERFLOW_POSTURE {
            assert!(overflow_posture(id).is_some());
        }
        assert!(overflow_posture("no-such-provider").is_none());
    }
}
