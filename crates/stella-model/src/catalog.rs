//! Model catalog. Binding rule:
//! **a slug not present
//! in the catalog is a hard, immediate, named error, never a silent
//! fallback** (the TS-era phantom `glm-5.2-turbo` slug and gateway
//! slug-drift lessons, L-M1/L-M2). The seed below covers every provider
//! `crates/stella-cli/src/config.rs`'s `PROVIDERS` table can select — it is
//! the compile-time floor, always accepted; its rows live in
//! `catalog/seed/<provider>.rs`. `stella models refresh` pulls
//! the live master list (models.dev) into the on-disk catalog
//! (`stella-store`'s model cards), and `stella-cli` installs the merged
//! result here via [`Catalog::install_runtime`] so every consumer —
//! adapters resolving pricing, the deck's model picker, the engine config —
//! sees one catalog through [`Catalog::current`].

use std::sync::{Arc, OnceLock, RwLock};

use stella_protocol::{CompletionUsage, ProviderError};

mod seed;

/// Per-model list pricing in USD per million tokens.
/// Seed values are day-0 offline approximations of each
/// provider's published list price; `stella models refresh` overlays them
/// with live data (the latest model-card version's pricing configuration).
/// Cached input is billed at its own, cheaper rate — cached tokens are a
/// *subset* of `input_tokens` in the normalized [`CompletionUsage`]
/// envelope, so cost accounting must not double-charge them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pricing {
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    pub cached_input_usd_per_mtok: f64,
    /// What a cache *write* costs per million tokens. Unlike cached reads,
    /// `cache_write_tokens` is reported OUTSIDE `input_tokens`, so this rate is
    /// billed on its own line rather than carved out of the input total.
    /// Providers charge a premium over input for writes (Anthropic-family
    /// 5-minute writes are 1.25x); the implicit-cache providers report zero
    /// writes, so their rate is never exercised and equals their input rate.
    pub cache_write_usd_per_mtok: f64,
}

impl Pricing {
    /// Estimated USD cost for one completion's normalized usage. Non-cached
    /// input (`input_tokens - cached_input_tokens`) is billed at the input
    /// rate, the cached remainder at the cached rate, and output at the
    /// output rate. Never panics and never goes negative — a provider that
    /// reports more cached than total input (shouldn't happen, but is not
    /// worth aborting a turn over) saturates to zero non-cached input.
    ///
    /// `cache_write_tokens` is billed on its own line at
    /// `cache_write_usd_per_mtok`. It is NOT carved out of `input_tokens` the
    /// way cached reads are — both adapters that report writes
    /// (`anthropic`, `bedrock`) count them outside the input total — so adding
    /// it here double-charges nothing. Leaving it unpriced is what issue #97
    /// tracked: it silently under-reported every cache-writing turn by the
    /// full write rate, and made this function disagree by construction with
    /// [`Pricing::cache_savings_usd`], which charges only the *premium* on the
    /// assumption the base rate is billed here.
    pub fn cost_usd(&self, usage: &CompletionUsage) -> f64 {
        let cached = usage.cached_input_tokens.min(usage.input_tokens);
        let uncached_input = usage.input_tokens - cached;
        const PER_MTOK: f64 = 1_000_000.0;
        (uncached_input as f64 / PER_MTOK) * self.input_usd_per_mtok
            + (cached as f64 / PER_MTOK) * self.cached_input_usd_per_mtok
            + (usage.output_tokens as f64 / PER_MTOK) * self.output_usd_per_mtok
            + (usage.cache_write_tokens as f64 / PER_MTOK) * self.cache_write_usd_per_mtok
    }
}

/// Which tool-call dialect a model's provider speaks — the axis that decides
/// which adapter can serve a row, since adapters are per *wire shape*, not
/// per vendor (four vendors share `OpenaiJson`; two Google surfaces share
/// `GeminiFunctions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolDialect {
    AnthropicTools,
    OpenaiJson,
    /// OpenAI's own Responses API (`stella_model::openai::OpenAiProvider`).
    /// Structurally distinct from `OpenaiJson` (Chat Completions and every
    /// OpenAI-*compatible* gateway: Z.ai, xAI, DeepSeek, OpenRouter, local)
    /// despite the name overlap: item-based `input`/`output` arrays with
    /// `function_call`/`function_call_output` items, not a `messages` array
    /// with an accumulating `tool_calls` delta array. Real OpenAI models
    /// (the `gpt-5.5` row in `catalog/seed/openai.rs`) get this variant now
    /// that the real
    /// adapter exists; `OpenaiJson` stays the dialect name for everything
    /// that actually speaks the Chat Completions wire shape.
    OpenaiResponses,
    /// Google's native `generateContent` dialect
    /// (`stella_model::gemini::GeminiProvider` and
    /// `stella_model::vertex::VertexProvider` — identical wire shape,
    /// different auth/addressing): `functionCall`/`functionResponse` parts
    /// correlated by function *name* (no wire call ids), args arriving as
    /// complete JSON objects rather than streamed string fragments, and
    /// Gemini 3 thought signatures riding on call parts
    /// ( `gemini-functions`).
    GeminiFunctions,
    /// Amazon Bedrock's Converse dialect
    /// (`stella_model::bedrock::BedrockProvider`): `toolUse`/`toolResult`
    /// content blocks correlated by `toolUseId`, tool results framed on a
    /// user-role message with an explicit `status` field for failures.
    BedrockConverse,
}

/// One catalog row — provider-native slug, verified against the provider's
/// own `/models` endpoint (the seeded rows are the day-0 offline fallback;
/// refreshed rows carry the latest model-card version's pricing).
///
/// Fields are owned `String`s (not `&'static str`): rows come from two
/// sources now — the compile-time seed and the on-disk model-card catalog
/// installed at startup — and only one of those can borrow from the binary.
///
/// `Eq` is intentionally *not* derived: [`Pricing`] carries `f64` fields, and
/// exact float equality is not a meaningful identity for a catalog row (rows
/// are keyed by `(provider, id)`, deduped by the seed test). `PartialEq` is
/// kept for tests that compare whole entries.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    pub id: String,
    pub provider: String,
    pub family: String,
    pub context_window: u32,
    pub tool_dialect: ToolDialect,
    /// List pricing used to compute `CompletionResult::cost_usd` on the real
    /// request path — each adapter resolves its own row in its constructor.
    pub pricing: Pricing,
    /// Whether this model supports reasoning / extended thinking. `None` is
    /// "unknown": effort settings pass through and the provider stays the
    /// authority. `Some(false)` is a hard "no" from catalog data — the
    /// effort picker hides its levels and the request path drops
    /// effort/reasoning instead of sending a parameter the API rejects.
    pub supports_reasoning: Option<bool>,
    /// The model's own maximum completion length, from `limit.output` on the
    /// model card. `None` is "unknown" and leaves the engine default standing.
    ///
    /// This exists because the engine had no way to ask: `EngineConfig`
    /// carried one global `max_output_tokens` for every model on every
    /// provider, and its comment named per-model caps as "the eventual
    /// refinement" — while the value was already parsed from models.dev,
    /// stored on the model card, read back, and then dropped at the
    /// runtime-catalog assembly in `stella-cli`. The same shape as the
    /// cache-write rate before #97, at the same site.
    ///
    /// A model spends whatever budget it is given, so a cap below the
    /// model's own ceiling decides where work stops rather than the model
    /// doing so — and a step cut off mid-reasoning emits no tool call and
    /// does no work. `bench/READINESS.md` §8.3.2 carries the measurement.
    pub max_output_tokens: Option<u32>,
    /// The model's knowledge/training cutoff, verbatim from the master
    /// list's `knowledge` field (year-month text, e.g. `"2025-04"`).
    /// `None` is "unknown": the session-environment prompt line that reads
    /// this is omitted rather than guessed (#2718). Seed rows carry `None`
    /// by construction — cutoff dates are synced data, not curated code.
    pub knowledge_cutoff: Option<String>,
}

impl CatalogEntry {
    /// A catalog row without the field-by-field ceremony — the seeded rows,
    /// the runtime-catalog assembly in `stella-cli`, and tests all
    /// build entries through this. Capabilities default to unknown; chain
    /// [`CatalogEntry::with_reasoning`] where the data exists.
    pub fn new(
        id: &str,
        provider: &str,
        family: &str,
        context_window: u32,
        tool_dialect: ToolDialect,
        pricing: Pricing,
    ) -> Self {
        Self {
            id: id.to_string(),
            provider: provider.to_string(),
            family: family.to_string(),
            context_window,
            tool_dialect,
            pricing,
            supports_reasoning: None,
            max_output_tokens: None,
            knowledge_cutoff: None,
        }
    }

    /// Set the reasoning capability (builder-style, so the many existing
    /// `new` call sites stay untouched).
    #[must_use]
    pub fn with_reasoning(mut self, supports_reasoning: Option<bool>) -> Self {
        self.supports_reasoning = supports_reasoning;
        self
    }

    /// Set the model's own completion ceiling (builder-style, same reason as
    /// [`CatalogEntry::with_reasoning`] — the seed rows and the many `new`
    /// call sites stay untouched, and a row without the data keeps `None`).
    #[must_use]
    pub fn with_max_output_tokens(mut self, max_output_tokens: Option<u32>) -> Self {
        self.max_output_tokens = max_output_tokens;
        self
    }

    /// Set the knowledge cutoff (builder-style, same reason as the two
    /// above; a row without the data keeps `None` and the consumer omits
    /// its line rather than guessing).
    #[must_use]
    pub fn with_knowledge_cutoff(mut self, knowledge_cutoff: Option<String>) -> Self {
        self.knowledge_cutoff = knowledge_cutoff;
        self
    }
}

/// The process-wide catalog installed by `stella-cli` at startup (seed rows
/// merged with the on-disk model-card catalog). `None` until installed;
/// [`Catalog::current`] falls back to the seed so library consumers and
/// tests behave identically without an install.
static RUNTIME_CATALOG: RwLock<Option<Arc<Catalog>>> = RwLock::new(None);

/// The seed, built once — [`Catalog::current`]'s fallback must not
/// reallocate the table on every adapter construction.
fn seed_arc() -> &'static Arc<Catalog> {
    static SEED: OnceLock<Arc<Catalog>> = OnceLock::new();
    SEED.get_or_init(|| Arc::new(Catalog::seed()))
}

/// The model catalog. Curated, versioned data — not code that
/// call sites reach past. `Catalog::resolve` is the only sanctioned way to
/// turn a user-supplied slug into a usable model reference.
pub struct Catalog {
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    /// The in-binary seed: one row per provider `config.rs::PROVIDERS` can
    /// select, keyed to that table's `default_model`. `stella models
    /// refresh` grows the *runtime* catalog with live master-list data; the
    /// seed stays the offline floor.
    /// The in-binary seed: one row per provider `config.rs::PROVIDERS` can
    /// select, keyed to that table's `default_model`. `stella models
    /// refresh` grows the *runtime* catalog with live master-list data; the
    /// seed stays the offline floor.
    ///
    /// The rows live in `catalog/seed/<provider>.rs` and are assembled by
    /// `seed::rows` — see `catalog/seed.rs` for why they are not in this
    /// file (#3862).
    pub fn seed() -> Self {
        Self {
            entries: seed::rows(),
        }
    }

    /// A catalog over explicit rows — how `stella-cli` assembles the runtime
    /// catalog (seed rows first, then the on-disk model-card rows, so seed
    /// lookups keep their exact pre-refresh results).
    pub fn with_entries(entries: Vec<CatalogEntry>) -> Self {
        Self { entries }
    }

    /// Install the process-wide catalog [`Catalog::current`] serves.
    /// Idempotent and replaceable — the last install wins (a mid-session
    /// `stella models refresh` re-installs with the new rows).
    pub fn install_runtime(catalog: Catalog) {
        let mut slot = RUNTIME_CATALOG.write().unwrap_or_else(|e| e.into_inner());
        *slot = Some(Arc::new(catalog));
    }

    /// The catalog every consumer resolves against: the installed runtime
    /// catalog when `stella-cli` has loaded one, otherwise the seed. Library
    /// users (and tests) that never install see exactly the seed.
    pub fn current() -> Arc<Catalog> {
        let slot = RUNTIME_CATALOG.read().unwrap_or_else(|e| e.into_inner());
        slot.clone().unwrap_or_else(|| Arc::clone(seed_arc()))
    }

    /// Resolve a slug against the catalog. Returns `ProviderError::UnknownModel`
    /// (never a fallback to a default model) when the slug isn't present —
    /// the loud, named error the spec requires. When the same slug exists on
    /// several providers (e.g. `gemini-3-pro` on both `gemini` and
    /// `vertex`), the first row wins; use [`Catalog::resolve_for`] when the
    /// provider is known.
    pub fn resolve(&self, slug: &str) -> Result<&CatalogEntry, ProviderError> {
        self.entries
            .iter()
            .find(|entry| entry.id == slug)
            .ok_or_else(|| ProviderError::UnknownModel {
                slug: slug.to_string(),
            })
    }

    /// Resolve a slug for a specific provider — the form `build_provider`
    /// uses, since the same model genuinely exists on more than one
    /// provider (Gemini on `gemini` and `vertex`; most things on
    /// `openrouter`) and a slug that exists on provider A must still be a
    /// hard error when requested from provider B.
    pub fn resolve_for(&self, provider: &str, slug: &str) -> Result<&CatalogEntry, ProviderError> {
        self.entries
            .iter()
            .find(|entry| entry.provider == provider && entry.id == slug)
            .ok_or_else(|| ProviderError::UnknownModel {
                slug: format!("{provider}/{slug}"),
            })
    }

    /// Every row, in install order (seed rows first when `stella-cli`
    /// assembled the runtime catalog). For enumeration — the model picker,
    /// `stella models list`, the seed's own invariant tests. Turning a
    /// user-supplied slug into a usable model still goes through
    /// [`Catalog::resolve`] / [`Catalog::resolve_for`], never a scan here.
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    /// Serializes the tests that install a runtime catalog.
    ///
    /// The runtime catalog is a process-global and `cargo test` runs these in
    /// parallel threads. Installing a strict superset of the seed — which
    /// `install_runtime_extends_current_without_disturbing_seed_rows` does —
    /// makes concurrent lookups of SEED rows safe, but not lookups of the
    /// synthetic row a test just added: that row exists only in that test's
    /// catalog, so a second install landing between the first test's install
    /// and its assertion replaces the catalog out from under it and the row
    /// is simply gone.
    ///
    /// Observed, not theorized: adding an unrelated test to this module
    /// perturbed the scheduling enough for
    /// `a_row_carries_the_models_own_output_ceiling_and_defaults_to_unknown`
    /// to fail once with its own row missing, then pass six runs in a row.
    /// A flake that rare is worse than a failure — it lands as "the gate is
    /// being flaky" rather than as a bug report.
    static RUNTIME_CATALOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    use super::*;

    /// A GLM pipeline needs a verifier and a triage model that are *not* the
    /// worker: a head-to-head pins the worker to the opponent's model, and the
    /// authored-witness tier refuses when the verifier resolves to the worker.
    ///
    /// Seeding is what makes them reachable. A benchmark container runs with
    /// `STELLA_CATALOG_AUTO_REFRESH=0`, so an unseeded slug is not merely
    /// unpriced there — it is unresolvable, and the run dies at startup having
    /// emitted no events at all. That failure cost a whole measured match and
    /// is invisible from the trial logs, which is why it is pinned here.
    #[test]
    fn the_zai_pipeline_roles_resolve_offline() {
        let catalog = Catalog::seed();
        for slug in ["glm-5.2", "glm-5.1", "glm-4.5-air"] {
            assert!(
                catalog.resolve_for("zai", slug).is_ok(),
                "`{slug}` must resolve from the offline seed"
            );
        }
        // The gateway route carries the same worker model. It exists because
        // the z.ai plan's per-minute cap cannot support a concurrent
        // head-to-head, and an unseeded row there fails exactly as loudly as
        // an unseeded row anywhere else: not at all, until the run dies.
        for slug in ["z-ai/glm-5.2", "z-ai/glm-5.1", "z-ai/glm-4.5-air"] {
            assert!(
                catalog.resolve_for("openrouter", slug).is_ok(),
                "the metered GLM route needs `{slug}` too — a pipeline is only a \
                 pipeline if every role resolves"
            );
        }
    }

    #[test]
    fn resolve_known_slug_succeeds() {
        let catalog = Catalog::seed();
        let entry = catalog.resolve("glm-5.2").expect("glm-5.2 is seeded");
        assert_eq!(entry.provider, "zai");
        assert_eq!(entry.tool_dialect, ToolDialect::OpenaiJson);
    }

    #[test]
    fn resolve_unknown_slug_is_a_named_hard_error_never_a_fallback() {
        let catalog = Catalog::seed();
        let err = catalog.resolve("glm-5.2-turbo").unwrap_err();
        match err {
            ProviderError::UnknownModel { slug } => assert_eq!(slug, "glm-5.2-turbo"),
            other => panic!("expected UnknownModel, got {other:?}"),
        }
    }

    #[test]
    fn seed_catalog_has_no_duplicate_provider_id_pairs() {
        // Keyed on (provider, id), not id alone: the same model genuinely
        // exists on more than one provider (gemini-3-pro on gemini and
        // vertex), which is exactly why resolve_for exists.
        let catalog = Catalog::seed();
        let mut pairs: Vec<(&str, &str)> = catalog
            .entries()
            .iter()
            .map(|e| (e.provider.as_str(), e.id.as_str()))
            .collect();
        let before = pairs.len();
        pairs.sort_unstable();
        pairs.dedup();
        assert_eq!(
            pairs.len(),
            before,
            "catalog seed must not contain duplicate (provider, slug) pairs"
        );
    }

    #[test]
    fn resolve_for_scopes_the_slug_to_the_named_provider() {
        let catalog = Catalog::seed();
        let entry = catalog
            .resolve_for("vertex", "gemini-3-pro")
            .expect("vertex row is seeded");
        assert_eq!(entry.provider, "vertex");
        assert_eq!(entry.tool_dialect, ToolDialect::GeminiFunctions);

        // A slug that exists — but on a different provider — is still a
        // hard error for the provider actually requested.
        let err = catalog.resolve_for("bedrock", "gemini-3-pro").unwrap_err();
        match err {
            ProviderError::UnknownModel { slug } => {
                assert_eq!(slug, "bedrock/gemini-3-pro")
            }
            other => panic!("expected UnknownModel, got {other:?}"),
        }
    }

    #[test]
    fn install_runtime_extends_current_without_disturbing_seed_rows() {
        let _guard = RUNTIME_CATALOG_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The runtime catalog is a process-global; this test installs a
        // strict SUPERSET of the seed so any concurrently-running test that
        // resolves seed rows through `current()` sees identical results
        // regardless of test ordering.
        let mut entries = Catalog::seed().entries.clone();
        entries.push(CatalogEntry::new(
            "test-only-model",
            "anthropic",
            "claude",
            100_000,
            ToolDialect::AnthropicTools,
            Pricing {
                input_usd_per_mtok: 1.0,
                output_usd_per_mtok: 2.0,
                cached_input_usd_per_mtok: 0.1,
                cache_write_usd_per_mtok: 1.25,
            },
        ));
        Catalog::install_runtime(Catalog::with_entries(entries));

        let current = Catalog::current();
        // Seed row unchanged, new row visible.
        assert_eq!(
            current.resolve_for("zai", "glm-5.2").unwrap().pricing,
            Catalog::seed().resolve("glm-5.2").unwrap().pricing,
        );
        assert!(current.resolve_for("anthropic", "test-only-model").is_ok());
    }

    #[test]
    fn a_row_carries_the_models_own_output_ceiling_and_defaults_to_unknown() {
        let _guard = RUNTIME_CATALOG_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The value models.dev publishes as `limit.output`. It was parsed,
        // written to the model card's `max_output_tokens` column and read
        // back, then dropped when the runtime catalog was assembled — the
        // same site, and the same shape, as the cache-write rate before #97.
        // With nowhere to carry it, the engine fell back to one global 16384
        // for every model, and truncated steps the model had room to finish.
        let entry = CatalogEntry::new(
            "test-only-ceiling-model",
            "anthropic",
            "claude",
            200_000,
            ToolDialect::AnthropicTools,
            Pricing {
                input_usd_per_mtok: 3.0,
                output_usd_per_mtok: 15.0,
                cached_input_usd_per_mtok: 0.3,
                cache_write_usd_per_mtok: 3.75,
            },
        );
        // Absent by default: a row without the data must not invent a ceiling.
        assert_eq!(entry.max_output_tokens, None);

        let entry = entry.with_max_output_tokens(Some(64_000));
        assert_eq!(entry.max_output_tokens, Some(64_000));

        // And it survives the lookup the engine actually performs. Superset of
        // the seed, per `install_runtime_extends_current_without_disturbing_seed_rows`.
        let mut entries = Catalog::seed().entries.clone();
        entries.push(entry);
        Catalog::install_runtime(Catalog::with_entries(entries));

        assert_eq!(
            Catalog::current()
                .resolve_for("anthropic", "test-only-ceiling-model")
                .unwrap()
                .max_output_tokens,
            Some(64_000),
        );
    }

    #[test]
    fn the_seed_carries_a_ceiling_where_the_frozen_catalog_is_all_there_is() {
        // A refreshed model card is not available everywhere the ceiling is
        // needed. `stella models refresh` populates the card store, but a
        // benchmark container runs frozen (`STELLA_CATALOG_AUTO_REFRESH=0`)
        // and a fresh install has never refreshed at all — in both, the seed
        // *is* the catalog. An unseeded ceiling is `None` there, and the
        // engine falls back to its global default.
        //
        // That is not hypothetical: before these rows carried a ceiling, a
        // trial-shaped run of the real binary against a recording endpoint
        // put `max_tokens: 16384` on the wire for a model whose ceiling is
        // 64000. Same run after: 64000.
        //
        // Per model, not one shared number. These rows carried an identical
        // 64000 until Fable's was corrected to its real 128000 (#1211 6.2) —
        // it had been copied from the Sonnet row, and a copied ceiling is
        // exactly the drift this test is here to catch. Two claims live here
        // and they decouple the moment two models differ: every benchmarked
        // model carries SOME ceiling in the seed, and it is THE MODEL'S.
        //
        // Correcting Fable's raised the obvious question about the row it had
        // been copied FROM, and the answer was the same defect: Sonnet 5's
        // 64000 was the comparator's measured stopping point, not the model's
        // ceiling. The provider says 128000 for both (#1290), so the two agree
        // again — but they now agree because each was looked up, which is a
        // different thing from the coincidence they started as. Haiku is the
        // row that proves the difference: it is 64000 on purpose.
        for (slug, ceiling) in [
            ("claude-sonnet-5", 128_000),
            ("claude-sonnet-4-6", 128_000),
            ("claude-opus-5", 128_000),
            ("claude-fable-5", 128_000),
        ] {
            assert_eq!(
                Catalog::seed()
                    .resolve_for("anthropic", slug)
                    .unwrap()
                    .max_output_tokens,
                Some(ceiling),
                "{slug} must carry its own ceiling in the seed, not only after a refresh",
            );
        }
        // The same models reached through the gateway are different rows, and
        // the ceiling has to be on both. `resolve_for` matches (provider, id)
        // exactly, so the first-party row above cannot answer for a run routed
        // over OpenRouter: without its own ceiling that run silently drops to
        // the engine's global 16384 and truncates before it emits a tool call.
        // Choosing a route is not supposed to change the model's ceiling.
        for (slug, ceiling) in [
            ("anthropic/claude-sonnet-5", 128_000),
            ("anthropic/claude-opus-5", 128_000),
            ("anthropic/claude-fable-5", 128_000),
            // Lower than its siblings, on purpose and from the provider —
            // the row that keeps this loop from passing on a shared constant.
            ("anthropic/claude-haiku-4.5", 64_000),
        ] {
            assert_eq!(
                Catalog::seed()
                    .resolve_for("openrouter", slug)
                    .unwrap()
                    .max_output_tokens,
                Some(ceiling),
                "{slug} must carry its ceiling on the gateway route too",
            );
        }
    }

    /// Route is not a model property: the direct and gateway rows for one
    /// Opus 5 resolves on both routes, at its own price.
    ///
    /// The model was missing from the seed entirely, which is a launch-time
    /// refusal rather than a wrong answer — `STELLA_CATALOG_AUTO_REFRESH=0` on
    /// the benchmark adapter, so a role pinned to an unlisted slug stops the
    /// run. Safe, and it made the model unusable for the head-to-head panel
    /// that wants Stella's worker on Opus-tier.
    ///
    /// The price is asserted against a literal on purpose. A missing row fails
    /// loudly; a row copied from the Fable 5 block beside it would resolve
    /// fine and silently double the reported spend on every Opus turn, because
    /// `cost_usd` is computed from this table. Opus 5 is half Fable's price at
    /// the same context window, so the two are exactly the pair a copy-paste
    /// gets wrong.
    #[test]
    fn opus_5_is_seeded_on_both_routes_at_its_own_price() {
        let seed = Catalog::seed();
        for (provider, slug) in [
            ("anthropic", "claude-opus-5"),
            ("openrouter", "anthropic/claude-opus-5"),
        ] {
            let entry = seed
                .resolve_for(provider, slug)
                .unwrap_or_else(|_| panic!("{provider}/{slug} is not in the seed catalog"));
            assert_eq!(entry.pricing.input_usd_per_mtok, 5.00, "{slug} input price");
            assert_eq!(
                entry.pricing.output_usd_per_mtok, 25.00,
                "{slug} output price"
            );
            assert_eq!(entry.context_window, 1_000_000, "{slug} context window");
            assert_eq!(
                entry.max_output_tokens,
                Some(128_000),
                "{slug} output ceiling",
            );
        }
    }

    /// model must agree on its ceiling.
    ///
    /// Asserted as equality between the two rows rather than against a
    /// literal, so it keeps holding when a ceiling is corrected. The failure
    /// it catches is a real one — the rows are edited separately, and a model
    /// whose ceiling was raised on one route only would answer at full length
    /// or at half length depending on how the run happened to be booked.
    #[test]
    fn a_models_ceiling_does_not_depend_on_the_route_it_is_reached_through() {
        let seed = Catalog::seed();
        for (direct, gateway) in [
            ("claude-sonnet-5", "anthropic/claude-sonnet-5"),
            ("claude-opus-5", "anthropic/claude-opus-5"),
            ("claude-fable-5", "anthropic/claude-fable-5"),
        ] {
            assert_eq!(
                seed.resolve_for("anthropic", direct)
                    .unwrap()
                    .max_output_tokens,
                seed.resolve_for("openrouter", gateway)
                    .unwrap()
                    .max_output_tokens,
                "{direct} answers at a different length depending on its route",
            );
        }
    }

    /// The benchmark's roles must all resolve on one provider. A trial carries
    /// exactly one credential, so a verifier or triage model that only exists on
    /// a second provider is unreachable at run time — and an unresolvable
    /// verifier pin silently degrades to "verifier is the worker", which is the
    /// weaker claim #1147 exists to refuse.
    #[test]
    fn every_benchmark_role_model_resolves_on_the_gateway_provider() {
        let catalog = Catalog::seed();
        for slug in [
            "anthropic/claude-sonnet-5",  // worker
            "anthropic/claude-fable-5",   // verifier, arm A
            "moonshotai/kimi-k3",         // verifier, arm B
            "anthropic/claude-haiku-4.5", // triage, both arms
        ] {
            catalog
                .resolve_for("openrouter", slug)
                .unwrap_or_else(|_| panic!("`{slug}` must be reachable on the openrouter route"));
        }
    }

    #[test]
    fn pricing_bills_cached_input_at_its_own_rate_and_never_double_charges() {
        let pricing = Pricing {
            input_usd_per_mtok: 3.00,
            output_usd_per_mtok: 15.00,
            cached_input_usd_per_mtok: 0.30,
            cache_write_usd_per_mtok: 3.75,
        };
        // 1M input tokens of which 400k are cached, plus 200k output:
        //   uncached input = 600k @ $3/M    = 1.80
        //   cached input   = 400k @ $0.30/M = 0.12
        //   output         = 200k @ $15/M   = 3.00
        //                                     ------
        //                                      4.92
        let usage = CompletionUsage {
            reported: true,
            input_tokens: 1_000_000,
            output_tokens: 200_000,
            cached_input_tokens: 400_000,
            cache_write_tokens: 0,
            reasoning_tokens: None,
        };
        assert!((pricing.cost_usd(&usage) - 4.92).abs() < 1e-9);
    }

    #[test]
    fn pricing_saturates_when_cached_exceeds_reported_input() {
        // Defensive: a provider reporting more cached than total input must
        // never produce a negative uncached-input charge.
        let pricing = Pricing {
            input_usd_per_mtok: 3.00,
            output_usd_per_mtok: 15.00,
            cached_input_usd_per_mtok: 0.30,
            cache_write_usd_per_mtok: 3.75,
        };
        let usage = CompletionUsage {
            reported: true,
            input_tokens: 100,
            output_tokens: 0,
            cached_input_tokens: 1_000,
            cache_write_tokens: 0,
            reasoning_tokens: None,
        };
        // All 100 input tokens billed as cached (clamped), never negative.
        let expected = (100.0 / 1_000_000.0) * 0.30;
        assert!((pricing.cost_usd(&usage) - expected).abs() < 1e-12);
        assert!(pricing.cost_usd(&usage) >= 0.0);
    }

    #[test]
    fn every_priced_provider_default_has_nonzero_input_and_output_pricing() {
        // OpenRouter `auto` is deliberately zero (gateway-priced); every
        // other seeded model must carry a real, positive list price so
        // `cost_usd` is never a silent no-op on the real request path.
        let catalog = Catalog::seed();
        for entry in catalog.entries() {
            if entry.id == "openrouter/auto" {
                continue;
            }
            assert!(
                entry.pricing.input_usd_per_mtok > 0.0 && entry.pricing.output_usd_per_mtok > 0.0,
                "model `{}` has zero pricing — budget metering would be a no-op",
                entry.id
            );
            // A zero write rate is the #97 defect in miniature: the counter is
            // reported, the meter charges nothing, and the receipt understates
            // real spend on every cache-writing turn.
            assert!(
                entry.pricing.cache_write_usd_per_mtok >= entry.pricing.input_usd_per_mtok,
                "model `{}` bills cache writes at {} but input at {} — writes are \
                 never cheaper than input, so this under-reports spend",
                entry.id,
                entry.pricing.cache_write_usd_per_mtok,
                entry.pricing.input_usd_per_mtok
            );
        }
    }

    /// Witness for #97: a turn that only writes the cache must cost more than
    /// $0. Before the `cache_write_usd_per_mtok` column, `cost_usd` read three
    /// of the four usage counters and this returned exactly zero.
    #[test]
    fn cache_write_tokens_are_billed_not_free() {
        let pricing = Catalog::seed()
            .resolve_for("anthropic", "claude-fable-5")
            .unwrap()
            .pricing;
        // 100k tokens written, nothing read, nothing generated.
        let write_only = CompletionUsage {
            reported: true,
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            cache_write_tokens: 100_000,
            reasoning_tokens: None,
        };
        // Derived from the entry's OWN rate rather than a copied constant: a
        // hardcoded figure re-fails this test every time a price is corrected,
        // which teaches the reader that the correction was the mistake. The
        // property under test is that the cache-write counter is READ at all —
        // before the `cache_write_usd_per_mtok` column, `cost_usd` consulted
        // three of the four counters and returned exactly zero here.
        let expected = 100_000.0 / 1e6 * pricing.cache_write_usd_per_mtok;
        let cost = pricing.cost_usd(&write_only);
        assert!(expected > 0.0, "the fixture must exercise a priced rate");
        assert!(
            (cost - expected).abs() < 1e-12,
            "expected a 100k-token cache write to cost ${expected}, got {cost}"
        );
    }

    #[test]
    fn seed_covers_every_provider_stella_cli_can_select() {
        // crates/stella-cli/src/config.rs::PROVIDERS lists these providers; this
        // test doesn't import that crate (stella-cli depends on
        // stella-model, not the reverse) but pins the provider id set here
        // so the two can't silently drift apart again — the actual
        // cross-check lives in stella-cli's own test suite (config::tests).
        let catalog = Catalog::seed();
        for provider in [
            "zai",
            "anthropic",
            "openai",
            "xai",
            "deepseek",
            "gemini",
            "openrouter",
            "vertex",
            "bedrock",
        ] {
            assert!(
                catalog.entries().iter().any(|e| e.provider == provider),
                "no catalog entry for provider `{provider}`"
            );
        }
    }
}
