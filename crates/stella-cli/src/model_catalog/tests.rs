use super::*;
use std::collections::BTreeMap;
use stella_model::modelsdev::{ModelCost, ModelEntry, ModelLimit, ProviderEntry};

#[test]
fn provider_ids_map_google_vertex_bedrock_and_pass_the_rest_through() {
    assert_eq!(stella_provider_id("google"), "gemini");
    assert_eq!(stella_provider_id("google-vertex"), "vertex");
    assert_eq!(stella_provider_id("amazon-bedrock"), "bedrock");
    assert_eq!(stella_provider_id("anthropic"), "anthropic");
    assert_eq!(stella_provider_id("openrouter"), "openrouter");
    assert_eq!(stella_provider_id("groq"), "groq");
}

#[test]
fn model_provider_prefers_the_slugs_own_vendor_namespace() {
    // OpenRouter-style vendor prefix.
    assert_eq!(
        derive_model_provider("openrouter", "anthropic/claude-sonnet-4.5"),
        "anthropic"
    );
    // Bedrock region-prefixed profile.
    assert_eq!(
        derive_model_provider("bedrock", "us.anthropic.claude-sonnet-4-5-20250929-v1:0"),
        "anthropic"
    );
    // Bedrock un-prefixed dotted id.
    assert_eq!(
        derive_model_provider("bedrock", "anthropic.claude-3-haiku-20240307-v1:0"),
        "anthropic"
    );
    // Family prefix when a gateway serves a bare slug.
    assert_eq!(
        derive_model_provider("vertex", "claude-sonnet-4-5"),
        "anthropic"
    );
    assert_eq!(derive_model_provider("groq", "llama-3.3-70b"), "meta");
    // API-provider fallback (mapped to the parent company for Google).
    assert_eq!(derive_model_provider("gemini", "learnlm-2.0"), "google");
    assert_eq!(
        derive_model_provider("anthropic", "claude-fable-5"),
        "anthropic"
    );
    assert_eq!(derive_model_provider("mystery", "zzz-1"), "mystery");
    // A dotted version segment is not a vendor namespace.
    assert_eq!(derive_model_provider("openai", "gpt-3.5-turbo"), "openai");
    assert_eq!(derive_model_provider("zai", "glm-4.6"), "zai");
}

#[test]
fn model_versions_extract_dates_and_bedrock_revisions() {
    assert_eq!(
        extract_model_version("claude-sonnet-4-5-20250929").as_deref(),
        Some("20250929")
    );
    assert_eq!(
        extract_model_version("gpt-4o-2024-08-06").as_deref(),
        Some("2024-08-06")
    );
    assert_eq!(
        extract_model_version("us.anthropic.claude-sonnet-4-5-20250929-v1:0").as_deref(),
        Some("v1:0")
    );
    assert_eq!(extract_model_version("claude-sonnet-4-5"), None);
    assert_eq!(extract_model_version("grok-4"), None);
    assert_eq!(extract_model_version("gemini-2.0-flash-001"), None);
}

#[test]
fn version_stripping_produces_the_base_slug() {
    assert_eq!(
        version_stripped("claude-sonnet-4-5-20250929").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        version_stripped("gpt-4o-2024-08-06").as_deref(),
        Some("gpt-4o")
    );
    // Revision AND date both strip, in order.
    assert_eq!(
        version_stripped("us.anthropic.claude-sonnet-4-5-20250929-v1:0").as_deref(),
        Some("us.anthropic.claude-sonnet-4-5")
    );
    assert_eq!(version_stripped("claude-sonnet-4-5"), None);
    assert_eq!(
        region_stripped("us.anthropic.claude-x").as_deref(),
        Some("anthropic.claude-x")
    );
    assert_eq!(region_stripped("anthropic.claude-x"), None);
    assert_eq!(region_stripped("gpt-4.1"), None);
}

#[test]
fn alias_forms_register_exact_id_first_then_derived_variants() {
    let forms = alias_forms("bedrock", "us.anthropic.claude-sonnet-4-5-20250929-v1:0");
    assert_eq!(
        forms[0].alias,
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0"
    );
    assert_eq!(forms[0].source, "catalog");
    assert_eq!(forms[0].model_version.as_deref(), Some("v1:0"));
    let aliases: Vec<&str> = forms.iter().map(|f| f.alias.as_str()).collect();
    assert!(aliases.contains(&"bedrock/us.anthropic.claude-sonnet-4-5-20250929-v1:0"));
    assert!(aliases.contains(&"us.anthropic.claude-sonnet-4-5"));
    assert!(aliases.contains(&"anthropic.claude-sonnet-4-5-20250929-v1:0"));
    assert!(aliases.contains(&"anthropic.claude-sonnet-4-5"));
    // No duplicates.
    let mut deduped = aliases.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(deduped.len(), aliases.len());

    // A plain undated slug: exact + provider-prefixed only.
    let forms = alias_forms("zai", "glm-5.2");
    let aliases: Vec<&str> = forms.iter().map(|f| f.alias.as_str()).collect();
    assert_eq!(aliases, vec!["glm-5.2", "zai/glm-5.2"]);
}

#[test]
fn build_upserts_maps_provider_ids_costs_and_limits() {
    let mut models = BTreeMap::new();
    models.insert(
        "gemini-3-pro".to_string(),
        ModelEntry {
            id: "gemini-3-pro".to_string(),
            name: Some("Gemini 3 Pro".to_string()),
            family: Some("gemini".to_string()),
            cost: Some(ModelCost {
                input: Some(1.25),
                output: Some(10.0),
                cache_read: Some(0.31),
                cache_write: None,
            }),
            limit: Some(ModelLimit {
                context: Some(1_000_000),
                output: Some(65_536),
            }),
            release_date: Some("2025-11-18".to_string()),
            last_updated: None,
            knowledge: Some("2025-08".to_string()),
            reasoning: Some(true),
            tool_call: Some(true),
        },
    );
    let mut providers = BTreeMap::new();
    providers.insert(
        "google".to_string(),
        ProviderEntry {
            id: "google".to_string(),
            name: Some("Google".to_string()),
            models,
        },
    );
    let fetched = FetchedCatalog {
        etag: Some("\"e\"".to_string()),
        payload_hash: "h".to_string(),
        providers,
    };

    let upserts = build_upserts(&fetched);
    assert_eq!(upserts.len(), 1);
    let up = &upserts[0];
    assert_eq!(
        up.api_provider, "gemini",
        "models.dev `google` is stella `gemini`"
    );
    assert_eq!(up.model_provider, "google");
    assert_eq!(up.slug, "gemini-3-pro");
    assert_eq!(up.source, SYNC_SOURCE);
    assert_eq!(up.version.input_usd_per_mtok, Some(1.25));
    assert_eq!(up.version.cached_input_usd_per_mtok, Some(0.31));
    assert_eq!(up.version.context_window, Some(1_000_000));
    assert_eq!(up.version.release_date.as_deref(), Some("2025-11-18"));
    assert_eq!(up.version.knowledge.as_deref(), Some("2025-08"));
    assert_eq!(up.version.supports_reasoning, Some(true));
    assert_eq!(up.version.supports_tools, Some(true));
    assert!(up.aliases.iter().any(|a| a.alias == "gemini/gemini-3-pro"));
}

#[test]
fn native_upserts_overlay_missing_fields_from_the_existing_card() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = CatalogStore::open(&dir.path().join("catalog.db")).expect("open");
    let anthropic = PROVIDERS
        .iter()
        .find(|p| p.id == "anthropic")
        .expect("anthropic row");

    // The master list already priced this model and knows it reasons.
    store
        .apply_batch(&[ModelUpsert {
            api_provider: "anthropic".to_string(),
            model_provider: "anthropic".to_string(),
            slug: "claude-fable-5".to_string(),
            display_name: None,
            family: Some("claude".to_string()),
            source: SYNC_SOURCE.to_string(),
            version: VersionData {
                input_usd_per_mtok: Some(3.0),
                output_usd_per_mtok: Some(15.0),
                cached_input_usd_per_mtok: Some(0.3),
                cache_write_usd_per_mtok: None,
                context_window: Some(200_000),
                max_output_tokens: Some(64_000),
                release_date: Some("2026-01-15".to_string()),
                last_updated: None,
                supports_reasoning: Some(true),
                supports_tools: Some(true),
                knowledge: Some("2026-01".to_string()),
            },
            aliases: alias_forms("anthropic", "claude-fable-5"),
        }])
        .expect("master-list row");

    // Anthropic's own /v1/models reports ids + display names only. The
    // merged upsert must keep every master-list fact…
    let native = [ProviderModel {
        id: "claude-fable-5".to_string(),
        display_name: Some("Claude Fable 5".to_string()),
        ..ProviderModel::default()
    }];
    let ups = native_upserts(anthropic, &native, &store);
    assert_eq!(ups.len(), 1);
    assert_eq!(ups[0].source, NATIVE_SOURCE);
    assert_eq!(ups[0].version.input_usd_per_mtok, Some(3.0));
    assert_eq!(ups[0].version.supports_reasoning, Some(true));
    assert_eq!(ups[0].version.release_date.as_deref(), Some("2026-01-15"));
    assert_eq!(
        ups[0].version.knowledge.as_deref(),
        Some("2026-01"),
        "a native re-sync must preserve the master list's cutoff"
    );
    // …which also means the merged version hashes identically and a
    // native re-sync appends NO new pricing version.
    let counts = store.apply_batch(&ups).expect("native apply");
    assert_eq!(
        counts.versions_added, 0,
        "no-new-information sync is version-silent"
    );
    assert_eq!(counts.cards_added, 0);

    // A model the master list has never heard of (released today) still
    // lands as a fresh card.
    let brand_new = [ProviderModel {
        id: "claude-brand-new".to_string(),
        ..ProviderModel::default()
    }];
    let counts = store
        .apply_batch(&native_upserts(anthropic, &brand_new, &store))
        .expect("new-model apply");
    assert_eq!(counts.cards_added, 1);
}

#[test]
fn native_sync_fires_on_first_run_but_master_list_never_does() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = CatalogStore::open(&dir.path().join("catalog.db")).expect("open");

    // Native listings (traffic to the user's own provider) DO fire on
    // the never-synced case — that's how new installs discover models.
    assert!(
        native_sync_is_due(&store, &native_sync_source("openrouter")),
        "never synced native → due (BYOK-clean)"
    );

    // models.dev (a third party) must NOT auto-fetch until the user has
    // explicitly refreshed at least once — the no-phone-home rule.
    assert!(
        !master_list_auto_due(&store),
        "never synced master list → NOT due (no phone home on a fresh install)"
    );

    // After an explicit refresh recorded a sync row, it may auto-refresh
    // once stale — but a just-recorded sync is still fresh.
    store
        .record_sync(SYNC_SOURCE, None, None)
        .expect("record sync");
    assert!(
        !master_list_auto_due(&store),
        "just refreshed → not due until the TTL passes"
    );
    store
        .record_sync(&native_sync_source("openrouter"), None, None)
        .expect("record native sync");
    assert!(
        !native_sync_is_due(&store, &native_sync_source("openrouter")),
        "just synced native → not due until the TTL passes"
    );
}

#[test]
fn every_builtin_except_vertex_and_bedrock_has_a_native_listing() {
    for provider in PROVIDERS {
        let expected = !matches!(provider.dialect, Dialect::Vertex | Dialect::Bedrock);
        assert_eq!(
            has_native_listing(provider),
            expected,
            "native-listing coverage drifted for `{}`",
            provider.id
        );
    }
}

#[test]
fn seed_floor_covers_every_seed_row_with_its_pricing() {
    let ups = seed_upserts();
    assert_eq!(ups.len(), Catalog::seed().entries().len());
    let sonnet = ups
        .iter()
        .find(|u| u.api_provider == "bedrock")
        .expect("bedrock seed row present");
    assert_eq!(sonnet.model_provider, "anthropic");
    assert_eq!(sonnet.version.input_usd_per_mtok, Some(3.0));
    assert_eq!(sonnet.source, "seed");
}
