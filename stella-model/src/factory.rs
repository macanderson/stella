//! The per-dialect provider factory — one place that turns "which wire shape
//! does this provider speak" into a constructed [`Provider`].
//!
//! ## Why this lives in `stella-model` and not in the CLI
//!
//! Every arm of this factory constructs a `stella_model` adapter, and every
//! variant of [`Dialect`] names one. Hosting the match one crate up meant the
//! only callable copy sat behind `stella-cli`'s binary-only target, so two
//! things that should have shared it could not:
//!
//! - **a second host of the engine** — anything embedding Stella's model layer
//!   without the CLI had to re-derive the dialect→adapter mapping, including
//!   the OpenRouter attribution/usage-accounting special case;
//! - **`stella-model`'s own live smoke tests** — which sit *below* `stella-cli`
//!   in the dependency graph and so could never call the factory at all. They
//!   hand-rolled nine construction sites instead, and because the factory is
//!   also where the anti-phantom-slug check runs, those nine sites silently
//!   skipped it.
//!
//! Moving the factory down fixes both: the dispatch and the slug floor now
//! live where the adapters live, reachable by every caller above them.
//!
//! ## The slug gate is layered, on purpose
//!
//! [`build_provider`] enforces the **catalog floor** unconditionally: a slug
//! that is not in [`Catalog::current`] for a `seeded` provider is a hard, named
//! error before any wire call. With no runtime catalog installed, `current()`
//! is the compiled [`Catalog::seed`], so a bare `stella-model` embedder or a
//! live smoke test gets exactly the seed floor with no on-disk state; when the
//! CLI has installed a runtime catalog (seed + synced model cards), a
//! newly-released synced model passes here too, matching the CLI ladder's
//! store-hit allow rather than silently narrowing it.
//!
//! The *escalation* — vetoing a slug against the user's synced master list,
//! with suggestions — needs `stella-store`'s catalog database, which sits above
//! this crate. It stays in `stella_cli::model_catalog::validate_model_slug`,
//! which runs before delegating here. So the CLI gets the full ladder and a
//! bare `stella-model` embedder still cannot construct a phantom-slug provider.

use stella_protocol::provider::Provider;

use crate::catalog::Catalog;
use crate::credential::ApiKey;

/// The wire dialect a provider speaks — which adapter is constructed for it.
/// Serialized form is the settings.json `dialect` field (kebab-case, e.g.
/// `"openai-compatible"`).
// `Serialize` so a settings document can be written back out (the JSON→TOML
// migration re-emits every provider entry it read). The kebab-case rename
// applies in both directions, so a round-trip is byte-stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Dialect {
    /// OpenAI Chat Completions shape ([`crate::zai::ZaiProvider`],
    /// re-identified per provider) — Z.ai, xAI, DeepSeek, OpenRouter,
    /// local endpoints, and the default for config-defined providers.
    OpenaiCompatible,
    /// OpenAI Responses API ([`crate::openai::OpenAiProvider`]).
    OpenaiResponses,
    /// Anthropic Messages API ([`crate::anthropic::AnthropicProvider`]).
    Anthropic,
    /// Gemini generateContent ([`crate::gemini::GeminiProvider`]).
    Gemini,
    /// Vertex generateContent with project/location addressing. Built-in
    /// only: it needs `VERTEX_PROJECT_ID`/`VERTEX_LOCATION` resolution that
    /// a settings.json entry has no way to express.
    Vertex,
    /// Bedrock Converse with SigV4. Built-in only, same reasoning.
    Bedrock,
}

impl Dialect {
    /// Human-readable label for `stella config` / `stella models`.
    pub fn label(self) -> &'static str {
        match self {
            Dialect::OpenaiCompatible => "OpenAI-compatible",
            Dialect::OpenaiResponses => "OpenAI Responses",
            Dialect::Anthropic => "Anthropic Messages",
            Dialect::Gemini | Dialect::Vertex => "Gemini generateContent",
            Dialect::Bedrock => "Bedrock Converse",
        }
    }
}

/// The parts of a provider's configuration the factory actually needs, so the
/// signature does not drag the CLI's whole `ProviderConfig` (env var names,
/// aliases, default model) down into this crate. `stella_cli::config::
/// ProviderConfig` converts into one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSpec<'a> {
    /// Stable provider id — `"anthropic"`, `"openrouter"`, `"local"`, or a
    /// settings.json-defined one. Becomes the adapter's `Provider::id()`.
    pub id: &'a str,
    /// Human label used in the OpenAI-compatible arm's error messages when the
    /// id is not one of the known gateways.
    pub display_name: &'a str,
    /// Which adapter to construct.
    pub dialect: Dialect,
    /// Whether this provider's models are curated in the catalog seed. `true`
    /// for the built-in rows — an unknown slug is a hard, named error. `false`
    /// for `local` and settings.json-defined providers, whose models are
    /// whatever the user's endpoint actually serves.
    pub seeded: bool,
}

/// The provider id whose models are whatever the user pulled into their local
/// server — exempt from the slug floor.
const LOCAL_PROVIDER_ID: &str = "local";

/// Enforce the catalog floor: reject a slug that a `seeded` provider's catalog
/// does not know, before any wire call.
///
/// Resolves against [`Catalog::current`], not [`Catalog::seed`]: when
/// `stella-cli` has installed a runtime catalog (the seed merged with the
/// on-disk model cards synced from each provider's live `/models` listing), a
/// newly-released model that is present in that runtime catalog but not yet in
/// the compiled seed must pass — otherwise this floor would silently veto slugs
/// the CLI's fuller ladder (`validate_model_slug`) already allowed via its
/// store-hit path, blocking every seeded provider's auto-synced models. A bare
/// `stella-model` embedder or a live smoke test never installs a runtime
/// catalog, so `current()` falls back to the seed and the anti-phantom-slug
/// floor is exactly the seed there.
///
/// Exemptions mirror the CLI's fuller ladder: `local` is always exempt, and an
/// unseeded provider (custom endpoint) keeps its endpoint-is-the-authority
/// posture. See the module docs for why the synced-catalog escalation with
/// suggestions is not here.
pub fn check_seed_floor(spec: &ProviderSpec<'_>, model_id: &str) -> Result<(), String> {
    if spec.id == LOCAL_PROVIDER_ID || !spec.seeded {
        return Ok(());
    }
    Catalog::current()
        .resolve_for(spec.id, model_id)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Construct the adapter for `spec`'s dialect, over already-resolved parts.
///
/// `effective_base_url` is the base URL requests go to (override-or-default);
/// `base_url_override` is the raw `--base-url`, which only the Vertex/Bedrock
/// arms consume (they build region/project-scoped URLs themselves).
///
/// Runs [`check_seed_floor`] first — no caller can skip the anti-phantom-slug
/// floor by going through this function.
pub fn build_provider(
    spec: &ProviderSpec<'_>,
    model_id: &str,
    api_key: ApiKey,
    effective_base_url: String,
    base_url_override: Option<&str>,
) -> Result<Box<dyn Provider>, String> {
    check_seed_floor(spec, model_id)?;

    match spec.dialect {
        Dialect::OpenaiResponses => {
            let provider = crate::openai::OpenAiProvider::new(api_key, model_id.to_string())
                .with_base_url(effective_base_url);
            Ok(Box::new(provider))
        }
        Dialect::Anthropic => {
            let provider = crate::anthropic::AnthropicProvider::new(api_key, model_id.to_string())
                .with_base_url(effective_base_url);
            Ok(Box::new(provider))
        }
        Dialect::Gemini => {
            let provider = crate::gemini::GeminiProvider::new(api_key, model_id.to_string())
                .with_base_url(effective_base_url);
            Ok(Box::new(provider))
        }
        Dialect::Vertex => {
            // The access token is `api_key` (VERTEX_ACCESS_TOKEN via the
            // credential chain); project and location are Vertex-specific
            // addressing, owned by `crate::credential` so a second host of the
            // engine gets the same variable names, the same fallback order,
            // and the same named errors without copying them.
            let addressing =
                crate::credential::VertexAddressing::resolve().map_err(|e| e.to_string())?;
            let mut provider = crate::vertex::VertexProvider::new(
                api_key,
                model_id.to_string(),
                addressing.project,
                addressing.location,
            );
            if let Some(override_url) = base_url_override {
                provider = provider.with_base_url(override_url.to_string());
            }
            Ok(Box::new(provider))
        }
        Dialect::Bedrock => {
            // `api_key` is AWS_ACCESS_KEY_ID via the credential chain; the rest
            // of the standard AWS env set is resolved by `crate::credential`,
            // alongside the adapter that needs it. A missing secret is a named
            // error pointing at the exact var, not a doomed unsigned request.
            let aws =
                crate::credential::BedrockCredentials::resolve().map_err(|e| e.to_string())?;
            let mut provider = crate::bedrock::BedrockProvider::new(
                api_key,
                aws.secret_access_key,
                aws.session_token,
                aws.region,
                model_id.to_string(),
            );
            if let Some(override_url) = base_url_override {
                provider = provider.with_base_url(override_url.to_string());
            }
            Ok(Box::new(provider))
        }
        // Z.ai, xAI, DeepSeek, OpenRouter, local, and config-defined providers
        // (settings.json) — the shared Chat Completions adapter, re-identified
        // per provider so its `Provider::id()` and error messages name the
        // surface actually being called.
        Dialect::OpenaiCompatible => {
            let label = match spec.id {
                "zai" => "Z.ai",
                "xai" => "xAI",
                "deepseek" => "DeepSeek",
                "openrouter" => "OpenRouter",
                "local" => "the local endpoint",
                _ => spec.display_name,
            };
            let mut provider = crate::zai::ZaiProvider::new(api_key, model_id.to_string())
                .with_base_url(effective_base_url)
                .with_identity(spec.id, label);
            if spec.id == "openrouter" {
                // First-class OpenRouter: app attribution on every request, and
                // the gateway's own usage accounting so
                // `CompletionResult::cost_usd` is the routed call's real price
                // (its slugs are unseeded, so there is no catalog list price to
                // fall back on).
                provider = provider
                    .with_attribution("https://stella.oxagen.sh", "Stella")
                    .with_usage_accounting();
            }
            Ok(Box::new(provider))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &'static str, dialect: Dialect, seeded: bool) -> ProviderSpec<'static> {
        ProviderSpec {
            id,
            display_name: id,
            dialect,
            seeded,
        }
    }

    fn key() -> ApiKey {
        ApiKey::new("test-key".to_string())
    }

    #[test]
    fn each_dialect_constructs_the_adapter_that_names_it() {
        // The seeded arms need a real slug to clear the floor; the id each
        // adapter reports is the contract callers dispatch on downstream.
        let cases: &[(&'static str, Dialect, &str, &str)] = &[
            (
                "anthropic",
                Dialect::Anthropic,
                "claude-fable-5",
                "anthropic",
            ),
            ("openai", Dialect::OpenaiResponses, "gpt-5.5", "openai"),
            ("gemini", Dialect::Gemini, "gemini-3-pro", "gemini"),
        ];
        for (id, dialect, slug, want_id) in cases {
            let provider = build_provider(
                &spec(id, *dialect, true),
                slug,
                key(),
                "https://example.invalid".to_string(),
                None,
            )
            .unwrap_or_else(|e| panic!("{id}/{slug} should construct: {e}"));
            assert_eq!(&provider.id(), want_id, "{id} adapter reports a wrong id");
        }
    }

    #[test]
    fn openai_compatible_relabels_known_gateways_and_falls_back_otherwise() {
        // Unseeded, so the floor is skipped and any slug constructs.
        for (id, _label) in [("zai", "Z.ai"), ("xai", "xAI"), ("deepseek", "DeepSeek")] {
            let provider = build_provider(
                &spec(id, Dialect::OpenaiCompatible, false),
                "whatever-the-endpoint-serves",
                key(),
                "https://example.invalid".to_string(),
                None,
            )
            .expect("openai-compatible arm constructs for any slug when unseeded");
            assert_eq!(provider.id(), id);
        }
    }

    #[test]
    fn the_seed_floor_rejects_a_phantom_slug_for_a_seeded_provider() {
        // `expect_err` would need `Box<dyn Provider>: Debug`, which a trait
        // object cannot supply — destructure instead.
        let Err(err) = build_provider(
            &spec("anthropic", Dialect::Anthropic, true),
            "claude-does-not-exist-9",
            key(),
            "https://example.invalid".to_string(),
            None,
        ) else {
            panic!("a phantom slug must not construct a provider");
        };
        assert!(
            err.contains("claude-does-not-exist-9"),
            "the error must name the rejected slug, got: {err}"
        );
    }

    #[test]
    fn local_and_unseeded_providers_stay_permissive() {
        // The endpoint is the authority for these — a slug the seed has never
        // heard of is not an error.
        for (id, seeded) in [("local", false), ("my-custom-gateway", false)] {
            check_seed_floor(
                &spec(id, Dialect::OpenaiCompatible, seeded),
                "some-locally-pulled-model",
            )
            .unwrap_or_else(|e| panic!("{id} must stay permissive, got: {e}"));
        }
    }

    #[test]
    fn label_covers_every_dialect() {
        for dialect in [
            Dialect::OpenaiCompatible,
            Dialect::OpenaiResponses,
            Dialect::Anthropic,
            Dialect::Gemini,
            Dialect::Vertex,
            Dialect::Bedrock,
        ] {
            assert!(!dialect.label().is_empty());
        }
    }

    #[test]
    fn dialect_deserializes_from_the_settings_json_kebab_case_form() {
        let parsed: Dialect = serde_json::from_str("\"openai-compatible\"").unwrap();
        assert_eq!(parsed, Dialect::OpenaiCompatible);
        let parsed: Dialect = serde_json::from_str("\"openai-responses\"").unwrap();
        assert_eq!(parsed, Dialect::OpenaiResponses);
    }
}
