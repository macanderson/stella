//! The provider table: which gateways Stella knows, what credential each
//! reads, and what model it serves by default.
//!
//! Split out of `config.rs` because it is data, not logic — the resolution
//! chain that consumes it is a separate concern, and keeping the two in one
//! file put that file over the 1500-line ratchet
//! (`scripts/check-file-size.sh`) with no room for a new row.

/// One provider's config: id, env var name, display name, default model.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub id: &'static str,
    pub env_var: &'static str,
    /// Alternate env var names accepted for this provider's credential,
    /// tried after `env_var` and before the credentials file (spec §2:
    /// `GEMINI_API_KEY` alias `GOOGLE_API_KEY`).
    pub env_var_aliases: &'static [&'static str],
    pub display_name: &'static str,
    pub default_model: &'static str,
    pub base_url: &'static str,
    /// Which wire adapter serves this provider. `build_provider_parts`
    /// (agent.rs) dispatches on this — never on a hard-coded id match — so
    /// config-defined providers (settings.json) reach the right adapter too.
    pub dialect: Dialect,
    /// Whether this provider's models are curated in the catalog seed.
    /// `true` for the built-in rows (an unknown slug is a hard, named error
    /// — the anti-phantom-slug check exists to catch drift in OUR seed
    /// data); `false` for `local` and settings.json-defined providers,
    /// whose models are whatever the user's endpoint actually serves.
    pub seeded: bool,
}

impl ProviderConfig {
    /// Narrow this to the parts [`stella_model::factory::build_provider`] needs.
    ///
    /// The factory deliberately does not take a whole `ProviderConfig`: env var
    /// names, aliases, and the default model are credential-resolution and
    /// config-UI concerns that belong up here, not in the model layer.
    pub fn factory_spec(&self) -> stella_model::factory::ProviderSpec<'_> {
        stella_model::factory::ProviderSpec {
            id: self.id,
            display_name: self.display_name,
            dialect: self.dialect,
            seeded: self.seeded,
        }
    }
}

/// The wire dialect a provider speaks — which `stella_model` adapter is
/// constructed for it. Serialized form is the settings.json `dialect` field
/// (kebab-case, e.g. `"openai-compatible"`).
///
/// Defined in [`stella_model::factory`], alongside the adapters its variants
/// name and the factory that dispatches on it, so callers below `stella-cli`
/// in the dependency graph can construct providers too. Re-exported here
/// because settings.json parsing and the provider table are this module's job.
pub use stella_model::factory::Dialect;

/// All supported providers, in preference order. Order matters twice over:
/// auto-detection picks the first row with a resolvable credential, which is
/// why Bedrock (keyed on the generic `AWS_ACCESS_KEY_ID` that plenty of
/// non-Bedrock users have exported) sits last — it must only ever be
/// auto-picked when nothing else is configured; `--model bedrock/…` pins it
/// explicitly regardless.
///
/// OpenRouter sits FIRST for the mirror-image reason: its key is
/// gateway-specific (nobody exports `OPENROUTER_API_KEY` by accident), so an
/// instance holding one has said something deliberate about how it routes.
/// [`crate::engine_config::provider_engine_baseline`] gives that instance a
/// whole default engine posture rather than only this default model.
pub static PROVIDERS: &[ProviderConfig] = &[
    ProviderConfig {
        id: "openrouter",
        env_var: "OPENROUTER_API_KEY",
        env_var_aliases: &[],
        display_name: "OpenRouter",
        // Named, not `openrouter/auto`: the gateway's dynamic router picks a
        // different model per turn, breaking prompt-cache prefixes and making
        // no two turns comparable. See `provider_engine_baseline`.
        default_model: "moonshotai/kimi-k3",
        base_url: "https://openrouter.ai/api/v1",
        dialect: Dialect::OpenaiCompatible,
        // Unseeded on purpose, like `local`: OpenRouter fronts hundreds of
        // `vendor/model` slugs that change weekly — a curated seed can only
        // veto real models (`anthropic/claude-…` was a hard error here). A
        // typo'd slug fails fast with OpenRouter's own named 400/404, and
        // cost metering doesn't need list prices: the adapter requests the
        // gateway's usage accounting and takes the reported per-call cost.
        seeded: false,
    },
    ProviderConfig {
        id: "zai",
        env_var: "ZAI_API_KEY",
        env_var_aliases: &[],
        display_name: "Z.ai (GLM 5.2)",
        default_model: "glm-5.2",
        base_url: "https://api.z.ai/api/paas/v4",
        dialect: Dialect::OpenaiCompatible,
        seeded: true,
    },
    ProviderConfig {
        id: "anthropic",
        env_var: "ANTHROPIC_API_KEY",
        env_var_aliases: &[],
        display_name: "Anthropic (Claude)",
        default_model: "claude-fable-5",
        base_url: "https://api.anthropic.com",
        dialect: Dialect::Anthropic,
        seeded: true,
    },
    ProviderConfig {
        id: "openai",
        env_var: "OPENAI_API_KEY",
        env_var_aliases: &[],
        display_name: "OpenAI (GPT)",
        default_model: "gpt-5.5",
        base_url: "https://api.openai.com/v1",
        dialect: Dialect::OpenaiResponses,
        seeded: true,
    },
    ProviderConfig {
        id: "xai",
        env_var: "XAI_API_KEY",
        env_var_aliases: &[],
        display_name: "xAI (Grok)",
        default_model: "grok-4",
        base_url: "https://api.x.ai/v1",
        dialect: Dialect::OpenaiCompatible,
        seeded: true,
    },
    ProviderConfig {
        id: "deepseek",
        env_var: "DEEPSEEK_API_KEY",
        env_var_aliases: &[],
        display_name: "DeepSeek",
        default_model: "deepseek-chat",
        base_url: "https://api.deepseek.com/v1",
        dialect: Dialect::OpenaiCompatible,
        seeded: true,
    },
    ProviderConfig {
        id: "gemini",
        env_var: "GEMINI_API_KEY",
        // Spec §2: "GEMINI_API_KEY (alias GOOGLE_API_KEY)" — the name most
        // Google tooling exports.
        env_var_aliases: &["GOOGLE_API_KEY"],
        display_name: "Google Gemini",
        default_model: "gemini-3-pro",
        // Gemini's native generateContent surface
        // (`stella_model::gemini::GeminiProvider`). This row previously
        // pointed at Google's OpenAI-compatibility shim
        // (`…/v1beta/openai`) served by the generic Chat Completions
        // adapter as a stand-in until the native adapter existed.
        base_url: "https://generativelanguage.googleapis.com/v1beta",
        dialect: Dialect::Gemini,
        seeded: true,
    },
    // Vertex and Bedrock are appended LAST so auto-detection (the no-`--model`
    // path picks the first provider with a resolvable credential) never
    // prefers them over an explicitly-configured provider — AWS_ACCESS_KEY_ID
    // in particular is commonly present in a shell for unrelated reasons.
    // Both speak a native, non-OpenAI wire shape, so `build_provider`
    // (agent.rs) routes them to their own adapters rather than the generic
    // Chat Completions client.
    ProviderConfig {
        id: "vertex",
        // Deliberately Vertex-specific (not a generic Google var) so
        // auto-detection is an explicit opt-in; documented as
        // `export VERTEX_ACCESS_TOKEN=$(gcloud auth print-access-token)`.
        // Also requires VERTEX_PROJECT_ID (or GOOGLE_CLOUD_PROJECT) and
        // honors VERTEX_LOCATION — resolved in `build_provider`.
        env_var: "VERTEX_ACCESS_TOKEN",
        env_var_aliases: &[],
        display_name: "Google Vertex AI",
        default_model: "gemini-3-pro",
        // Display anchor: the real endpoint is project/location-scoped and
        // built per request by the adapter.
        base_url: "https://aiplatform.googleapis.com",
        dialect: Dialect::Vertex,
        seeded: true,
    },
    ProviderConfig {
        id: "bedrock",
        // The standard AWS chain vars; AWS_SECRET_ACCESS_KEY (and optional
        // AWS_SESSION_TOKEN / AWS_REGION) are resolved in `build_provider`.
        // Last in preference order on purpose — see the doc comment above.
        env_var: "AWS_ACCESS_KEY_ID",
        env_var_aliases: &[],
        display_name: "Amazon Bedrock",
        default_model: "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        // Display anchor: the real host is region-scoped
        // (`bedrock-runtime.<AWS_REGION>.amazonaws.com`), built per request
        // by the adapter.
        base_url: "https://bedrock-runtime.<AWS_REGION>.amazonaws.com",
        dialect: Dialect::Bedrock,
        seeded: true,
    },
];

/// The handful of provider key variables named in the first-run "no API key
/// found" error. Deliberately a short subset of [`PROVIDERS`]: the full
/// enumeration lives behind `stella models`, where it is already tabulated
/// with per-provider key status, instead of hard-wrapping across an
/// 80-column terminal inside an error message.
const COMMON_KEY_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "ZAI_API_KEY",
    "OPENROUTER_API_KEY",
];

/// The first-run "no key anywhere" message. This is the single most-hit error
/// in the product, so it names the two commands built for it — `stella auth
/// set` writes `~/.stella/credentials.toml` safely (masked prompt, `--stdin`
/// mode, owner-only perms) and `stella models` tabulates every provider with
/// its key status — instead of telling the user to hand-edit TOML and then
/// hard-wrapping all thirteen provider rows across their terminal.
pub(crate) fn no_api_key_error() -> String {
    format!(
        "no API key found.\n\n\
         Set a provider key, e.g. one of:\n  {}\n\
         Or run: stella auth set <provider>  (writes ~/.stella/credentials.toml)\n\
         See every provider and its key status: stella models",
        COMMON_KEY_ENV_VARS.join(", ")
    )
}

/// The `local` pseudo-provider: any OpenAI-compatible endpoint the user
/// points `--base-url` at (Ollama, vLLM, LM Studio, llama.cpp server, or
/// anything else speaking Chat Completions). Not in [`PROVIDERS`]: it is
/// never auto-detected
/// (there is no ambient signal a local server exists), has no default model
/// (the server's models are whatever the user pulled), and its API key is
/// optional (`LOCAL_API_KEY`, defaulting to a placeholder — most local
/// servers ignore auth entirely).
pub static LOCAL_PROVIDER: ProviderConfig = ProviderConfig {
    id: "local",
    env_var: "LOCAL_API_KEY",
    env_var_aliases: &[],
    display_name: "Local (OpenAI-compatible)",
    default_model: "",
    base_url: "",
    dialect: Dialect::OpenaiCompatible,
    seeded: false,
};
