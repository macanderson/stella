//! The CLI's model-catalog bridge: the one place that knows BOTH
//! vocabularies — models.dev's provider ids and stella's own provider
//! table — and everything derived from them.
//!
//! What lives here:
//! - **`bootstrap`** — open the user-tier `catalog.db`, lay the seed floor,
//!   opportunistically refresh a stale master list (only after the user has
//!   opted in by running `stella models refresh` at least once — the
//!   no-phone-home rule), and install the merged runtime catalog every
//!   pricing consumer resolves through `Catalog::current()`.
//! - **`validate_model_slug`** — the anti-invalid-slug gate `build_provider`
//!   calls for EVERY provider. Strictness is earned, never assumed: the
//!   seed floor always passes; a provider whose master-list rows are synced
//!   gets hard validation with suggestions; a provider the catalog knows
//!   nothing about keeps today's posture (seeded → seed check, custom
//!   endpoint → the endpoint is the authority).
//! - **`stella models refresh` / `stella models list`** — the sync command
//!   (incremental via the persisted ETag) and the catalog listing.
//! - **`note_wire_model`** — the telemetry-perfection hook: any model
//!   string a provider echoes on the wire that isn't yet joinable gets
//!   matched to its card (version/region-stripped forms) and registered as
//!   a `learned` alias, so telemetry's raw strings always join to exactly
//!   one model card.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use colored::Colorize;
use stella_model::catalog::{Catalog, CatalogEntry, Pricing, ToolDialect};
use stella_model::modelsdev::{self, FetchOutcome, FetchedCatalog};
use stella_store::catalog::{
    AliasForm, CatalogStore, ModelUpsert, RefreshCounts, VersionData, catalog_db_path,
};

use crate::config::{Dialect, LOCAL_PROVIDER, PROVIDERS, ProviderConfig};

/// The `catalog_sync.source` key for the models.dev master list.
pub const SYNC_SOURCE: &str = "models.dev";

/// How stale a previously-synced master list may get before `bootstrap`
/// re-fetches it (conditional request — an unchanged list is one cheap 304).
const AUTO_REFRESH_TTL_SECS: i64 = 24 * 60 * 60;

/// The process-wide store handle. Set exactly once, by [`bootstrap`] or the
/// first `stella models refresh|list` command — NEVER opened implicitly by
/// readers ([`catalog_store`] returns `None` before then), so tests and
/// library-style callers can't accidentally touch the user's real catalog.
static STORE: OnceLock<Option<Arc<CatalogStore>>> = OnceLock::new();

/// The already-opened catalog store, if this process opened one.
pub fn catalog_store() -> Option<Arc<CatalogStore>> {
    STORE.get().and_then(|slot| slot.clone())
}

/// Open (or reuse) the store for an explicit catalog command.
fn store_for_command() -> Result<Arc<CatalogStore>, String> {
    if let Some(store) = catalog_store() {
        return Ok(store);
    }
    let store = CatalogStore::open(&catalog_db_path())
        .map(Arc::new)
        .map_err(|e| format!("cannot open the model catalog: {e}"))?;
    // Best-effort publication: if bootstrap raced us, the existing handle
    // wins and this fresh one just serves the current command.
    let _ = STORE.set(Some(store.clone()));
    Ok(store)
}

// ---------------------------------------------------------------------
// Vocabulary mapping & derivations
// ---------------------------------------------------------------------

/// models.dev provider id → stella provider id. Only the three ids whose
/// stella names differ are mapped; everything else matches verbatim (which
/// is also what lets a settings.json-defined provider named after a
/// models.dev id — `groq`, `mistral`, `together`, … — get validation and
/// pricing for free).
pub(crate) fn stella_provider_id(models_dev_id: &str) -> &str {
    match models_dev_id {
        "google" => "gemini",
        "google-vertex" => "vertex",
        "amazon-bedrock" => "bedrock",
        other => other,
    }
}

/// Bedrock-style cross-region prefixes (`us.anthropic.…`, `eu.…`).
const REGION_PREFIXES: &[&str] = &["us", "eu", "apac", "jp", "au", "ca", "sa", "mx", "global"];

/// Well-known model-family prefixes → the company that makes the model.
/// Used only when the slug itself carries no vendor namespace.
fn family_vendor(slug: &str) -> Option<&'static str> {
    const FAMILIES: &[(&str, &str)] = &[
        ("claude", "anthropic"),
        ("gpt", "openai"),
        ("chatgpt", "openai"),
        ("codex", "openai"),
        ("o1", "openai"),
        ("o3", "openai"),
        ("o4", "openai"),
        ("gemini", "google"),
        ("gemma", "google"),
        ("grok", "xai"),
        ("glm", "zai"),
        ("deepseek", "deepseek"),
        ("llama", "meta"),
        ("mistral", "mistralai"),
        ("mixtral", "mistralai"),
        ("magistral", "mistralai"),
        ("codestral", "mistralai"),
        ("devstral", "mistralai"),
        ("qwen", "alibaba"),
        ("qwq", "alibaba"),
        ("kimi", "moonshotai"),
        ("nova", "amazon"),
        ("titan", "amazon"),
        ("phi", "microsoft"),
        ("command", "cohere"),
    ];
    let lower = slug.to_ascii_lowercase();
    FAMILIES
        .iter()
        .find(|(prefix, _)| lower.starts_with(prefix))
        .map(|(_, vendor)| *vendor)
}

/// Who MAKES the model named by `slug`, as served by `api_provider`. The
/// slug's own vendor namespace wins (`anthropic/claude-…` on OpenRouter,
/// `us.anthropic.claude-…` on Bedrock); then well-known family prefixes;
/// then the API provider itself (mapped to its parent company for the
/// gateway-style built-ins).
pub(crate) fn derive_model_provider(api_provider: &str, slug: &str) -> String {
    // OpenRouter-style `vendor/model`.
    if let Some((vendor, rest)) = slug.split_once('/')
        && !vendor.is_empty()
        && !rest.is_empty()
    {
        return vendor.to_string();
    }
    // Bedrock-style dotted ids: `us.anthropic.claude-…` / `anthropic.claude-…`.
    let dotted: Vec<&str> = slug.split('.').collect();
    if dotted.len() >= 3 && REGION_PREFIXES.contains(&dotted[0]) {
        return dotted[1].to_string();
    }
    if dotted.len() >= 2
        && !dotted[0].is_empty()
        && dotted[0].chars().all(|c| c.is_ascii_alphabetic())
    {
        return dotted[0].to_string();
    }
    if let Some(vendor) = family_vendor(slug) {
        return vendor.to_string();
    }
    match api_provider {
        "gemini" | "vertex" => "google".to_string(),
        "bedrock" => "amazon".to_string(),
        other => other.to_string(),
    }
}

/// The version token embedded in a slug, when it has one: a Bedrock
/// profile revision (`…-v1:0` → `v1:0`), a compact date snapshot
/// (`…-20250929` → `20250929`), or an OpenAI-style dashed date
/// (`…-2024-08-06` → `2024-08-06`).
pub(crate) fn extract_model_version(slug: &str) -> Option<String> {
    if let Some(idx) = slug.rfind("-v") {
        let tail = &slug[idx + 2..];
        if tail.contains(':')
            && tail
                .split(':')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        {
            return Some(format!("v{tail}"));
        }
    }
    if let Some((_, tail)) = slug.rsplit_once('-')
        && tail.len() == 8
        && tail.starts_with("20")
        && tail.chars().all(|c| c.is_ascii_digit())
    {
        return Some(tail.to_string());
    }
    let parts: Vec<&str> = slug.split('-').collect();
    if parts.len() >= 4 {
        let n = parts.len();
        let (y, m, d) = (parts[n - 3], parts[n - 2], parts[n - 1]);
        if y.len() == 4
            && y.starts_with("20")
            && m.len() == 2
            && d.len() == 2
            && [y, m, d]
                .iter()
                .all(|p| p.chars().all(|c| c.is_ascii_digit()))
        {
            return Some(format!("{y}-{m}-{d}"));
        }
    }
    None
}

/// The slug with its version suffix removed (`claude-sonnet-4-5-20250929`
/// → `claude-sonnet-4-5`), or `None` when there is nothing to strip.
pub(crate) fn version_stripped(slug: &str) -> Option<String> {
    let mut base = slug.to_string();
    let mut changed = false;
    // Bedrock revision tail first (`-v1:0` follows the date in profile ids).
    if let Some(idx) = base.rfind("-v") {
        let tail = base[idx + 2..].to_string();
        if tail.contains(':')
            && tail
                .split(':')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        {
            base.truncate(idx);
            changed = true;
        }
    }
    // Compact date tail.
    if let Some(idx) = base.rfind('-') {
        let tail = base[idx + 1..].to_string();
        if tail.len() == 8 && tail.starts_with("20") && tail.chars().all(|c| c.is_ascii_digit()) {
            base.truncate(idx);
            changed = true;
        }
    }
    // Dashed date tail (three segments).
    let parts: Vec<String> = base.split('-').map(str::to_string).collect();
    if parts.len() >= 4 {
        let n = parts.len();
        let (y, m, d) = (&parts[n - 3], &parts[n - 2], &parts[n - 1]);
        if y.len() == 4
            && y.starts_with("20")
            && m.len() == 2
            && d.len() == 2
            && [y, m, d]
                .iter()
                .all(|p| p.chars().all(|c| c.is_ascii_digit()))
        {
            base = parts[..n - 3].join("-");
            changed = true;
        }
    }
    (changed && !base.is_empty()).then_some(base)
}

/// The slug with a Bedrock-style region prefix removed
/// (`us.anthropic.claude-…` → `anthropic.claude-…`).
pub(crate) fn region_stripped(slug: &str) -> Option<String> {
    let (prefix, rest) = slug.split_once('.')?;
    (REGION_PREFIXES.contains(&prefix) && !rest.is_empty()).then(|| rest.to_string())
}

fn push_unique(forms: &mut Vec<AliasForm>, alias: String, model_version: Option<String>) {
    if !alias.is_empty() && forms.iter().all(|f| f.alias != alias) {
        forms.push(AliasForm {
            alias,
            model_version,
            source: "derived".to_string(),
        });
    }
}

/// Every string form one catalog model is registered under: the exact id
/// (source `catalog`), then derived variants — `provider/slug`, the
/// lowercase form, the version-stripped base, and the region-stripped
/// Bedrock form. Insert order matters downstream: exact ids register
/// first, and `INSERT OR IGNORE` guarantees a later derived collision can
/// never displace another model's exact id.
pub(crate) fn alias_forms(api_provider: &str, slug: &str) -> Vec<AliasForm> {
    let version = extract_model_version(slug);
    let mut forms = vec![AliasForm {
        alias: slug.to_string(),
        model_version: version.clone(),
        source: "catalog".to_string(),
    }];
    push_unique(
        &mut forms,
        format!("{api_provider}/{slug}"),
        version.clone(),
    );
    let lower = slug.to_ascii_lowercase();
    if lower != slug {
        push_unique(&mut forms, lower, version.clone());
    }
    if let Some(base) = version_stripped(slug) {
        push_unique(&mut forms, base, None);
    }
    if let Some(region_free) = region_stripped(slug) {
        let region_free_version = extract_model_version(&region_free);
        if let Some(base) = version_stripped(&region_free) {
            push_unique(&mut forms, base, None);
        }
        push_unique(&mut forms, region_free, region_free_version);
    }
    forms
}

/// A fetched master list, mapped into store upserts: provider ids
/// translated to stella's, model makers and version tokens derived, alias
/// forms generated. Every provider in the document is kept — a custom
/// (settings.json) provider whose id matches a models.dev id gets strict
/// validation and pricing the moment it's defined.
pub(crate) fn build_upserts(fetched: &FetchedCatalog) -> Vec<ModelUpsert> {
    let mut out = Vec::new();
    for (models_dev_id, provider) in &fetched.providers {
        let api_provider = stella_provider_id(models_dev_id);
        for model in provider.models.values() {
            if model.id.is_empty() {
                continue;
            }
            let cost = model.cost.unwrap_or_default();
            let limit = model.limit.unwrap_or_default();
            out.push(ModelUpsert {
                api_provider: api_provider.to_string(),
                model_provider: derive_model_provider(api_provider, &model.id),
                slug: model.id.clone(),
                display_name: model.name.clone(),
                family: model.family.clone(),
                source: SYNC_SOURCE.to_string(),
                version: VersionData {
                    input_usd_per_mtok: cost.input,
                    output_usd_per_mtok: cost.output,
                    cached_input_usd_per_mtok: cost.cache_read,
                    cache_write_usd_per_mtok: cost.cache_write,
                    context_window: limit.context,
                    max_output_tokens: limit.output,
                    release_date: model.release_date.clone(),
                    last_updated: model.last_updated.clone(),
                },
                aliases: alias_forms(api_provider, &model.id),
            });
        }
    }
    out
}

/// The seed catalog as store upserts — the offline floor laid at bootstrap.
fn seed_upserts() -> Vec<ModelUpsert> {
    Catalog::seed()
        .entries()
        .iter()
        .map(|entry| ModelUpsert {
            api_provider: entry.provider.clone(),
            model_provider: derive_model_provider(&entry.provider, &entry.id),
            slug: entry.id.clone(),
            display_name: None,
            family: Some(entry.family.clone()),
            source: "seed".to_string(),
            version: VersionData {
                input_usd_per_mtok: Some(entry.pricing.input_usd_per_mtok),
                output_usd_per_mtok: Some(entry.pricing.output_usd_per_mtok),
                cached_input_usd_per_mtok: Some(entry.pricing.cached_input_usd_per_mtok),
                cache_write_usd_per_mtok: None,
                context_window: Some(entry.context_window as u64),
                max_output_tokens: None,
                release_date: None,
                last_updated: None,
            },
            aliases: alias_forms(&entry.provider, &entry.id),
        })
        .collect()
}

/// Lay the seed floor: insert seed cards that don't exist yet. A card the
/// master list already owns is never touched — otherwise every bootstrap
/// would ping-pong versions between seed pricing and live pricing.
fn ensure_seed_floor(store: &CatalogStore) {
    let missing: Vec<ModelUpsert> = seed_upserts()
        .into_iter()
        .filter(|m| matches!(store.resolve(&m.api_provider, &m.slug), Ok(None)))
        .collect();
    if !missing.is_empty() {
        let _ = store.apply_batch(&missing);
    }
}

// ---------------------------------------------------------------------
// Bootstrap & runtime catalog
// ---------------------------------------------------------------------

/// Which tool dialect a provider's catalog rows carry — from the
/// provider's wire dialect (the catalog field is informational; adapter
/// dispatch stays on `ProviderConfig::dialect`).
fn tool_dialect_for(dialect: Dialect) -> ToolDialect {
    match dialect {
        Dialect::OpenaiCompatible => ToolDialect::OpenaiJson,
        Dialect::OpenaiResponses => ToolDialect::OpenaiResponses,
        Dialect::Anthropic => ToolDialect::AnthropicTools,
        Dialect::Gemini | Dialect::Vertex => ToolDialect::GeminiFunctions,
        Dialect::Bedrock => ToolDialect::BedrockConverse,
    }
}

/// Open the catalog, lay the seed floor, refresh a stale master list (only
/// ever after the explicit first `stella models refresh` — see
/// [`maybe_auto_refresh`]), and install the merged runtime catalog. Called
/// once at startup, before any provider is resolved; every failure
/// degrades silently to today's seed-only behavior (the catalog is an
/// upgrade, never a new way to break a turn).
pub fn bootstrap() {
    let store = match CatalogStore::open(&catalog_db_path()) {
        Ok(store) => Some(Arc::new(store)),
        Err(e) => {
            eprintln!(
                "  {} model catalog unavailable ({e}) — using the built-in seed only",
                "warning:".yellow()
            );
            None
        }
    };
    if let Some(store) = &store {
        ensure_seed_floor(store);
        maybe_auto_refresh(store);
        install_runtime_catalog(store);
    }
    let _ = STORE.set(store);
}

/// Re-fetch the master list when it is stale — but ONLY if a sync row
/// already exists: the first fetch is always the user's explicit `stella
/// models refresh` (the no-phone-home rule), and `STELLA_CATALOG_AUTO_REFRESH=0`
/// switches the re-fetch off entirely. Best-effort: offline just means
/// the catalog stays as of the last sync.
fn maybe_auto_refresh(store: &Arc<CatalogStore>) {
    if std::env::var("STELLA_CATALOG_AUTO_REFRESH").as_deref() == Ok("0") {
        return;
    }
    let Ok(Some(age)) = store.seconds_since_sync(SYNC_SOURCE) else {
        return;
    };
    if age < AUTO_REFRESH_TTL_SECS {
        return;
    }
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let _ = rt.block_on(refresh_with_store(store, false));
}

/// Assemble and install the runtime catalog: every seed row (verbatim, so
/// pre-refresh behavior is bit-identical), plus the latest-version card of
/// every model the store knows for a *selectable* provider (built-ins +
/// settings.json-defined). Selectable-only keeps unscoped adapter pricing
/// lookups from matching a card on some provider stella can't even route.
fn install_runtime_catalog(store: &CatalogStore) {
    let mut providers: Vec<(String, ToolDialect)> = PROVIDERS
        .iter()
        .map(|p| (p.id.to_string(), tool_dialect_for(p.dialect)))
        .collect();
    if let Ok(workspace) = std::env::current_dir()
        && let Ok(settings) = crate::settings::Settings::load(&workspace)
    {
        for (id, entry) in &settings.providers {
            if id == LOCAL_PROVIDER.id || providers.iter().any(|(p, _)| p == id) {
                continue;
            }
            let dialect = entry.dialect.unwrap_or(Dialect::OpenaiCompatible);
            providers.push((id.clone(), tool_dialect_for(dialect)));
        }
    }

    let mut entries = Catalog::seed().entries().to_vec();
    let mut known: HashSet<(String, String)> = entries
        .iter()
        .map(|e| (e.provider.clone(), e.id.clone()))
        .collect();
    for (provider_id, dialect) in providers {
        let Ok(listings) = store.models_for_provider(Some(&provider_id)) else {
            continue;
        };
        for listing in listings {
            let key = (listing.api_provider.clone(), listing.slug.clone());
            if known.contains(&key) {
                continue;
            }
            let family = listing
                .family
                .clone()
                .unwrap_or_else(|| listing.model_provider.clone());
            entries.push(CatalogEntry::new(
                &listing.slug,
                &listing.api_provider,
                &family,
                listing
                    .pricing
                    .context_window
                    .unwrap_or(0)
                    .min(u32::MAX as u64) as u32,
                dialect,
                Pricing {
                    input_usd_per_mtok: listing.pricing.input_usd_per_mtok.unwrap_or(0.0),
                    output_usd_per_mtok: listing.pricing.output_usd_per_mtok.unwrap_or(0.0),
                    cached_input_usd_per_mtok: listing
                        .pricing
                        .cached_input_usd_per_mtok
                        .unwrap_or(0.0),
                },
            ));
            known.insert(key);
        }
    }
    Catalog::install_runtime(Catalog::with_entries(entries));
}

// ---------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------

/// The anti-invalid-slug gate — called by `build_provider_parts` for every
/// provider before any wire call. The decision ladder:
///
/// 1. `local` is exempt: a local server's models are whatever the user
///    pulled into it.
/// 2. The seed floor always passes (offline behavior is a strict superset
///    of pre-catalog stella).
/// 3. A hit in the on-disk catalog — any registered string form — passes.
/// 4. A miss, for a provider whose master-list rows ARE synced, is a hard
///    named error with suggestions: the catalog is authoritative there,
///    and this is what closes the "anyone can type an invalid slug" hole
///    for the previously-unvalidated providers (OpenRouter, custom
///    gateways with a known id).
/// 5. A miss for a provider the master list doesn't cover keeps today's
///    posture: seeded providers fail the seed check (unchanged), custom
///    endpoints stay permissive (their server is the authority — and the
///    error hint tells the user how to opt into strictness).
pub fn validate_model_slug(provider: &ProviderConfig, model_id: &str) -> Result<(), String> {
    if provider.id == LOCAL_PROVIDER.id {
        return Ok(());
    }
    if Catalog::seed().resolve_for(provider.id, model_id).is_ok() {
        return Ok(());
    }
    if let Some(store) = catalog_store() {
        if let Ok(Some(_)) = store.resolve(provider.id, model_id) {
            return Ok(());
        }
        let synced = store
            .provider_model_count(provider.id, Some(SYNC_SOURCE))
            .unwrap_or(0);
        if synced > 0 {
            return Err(unknown_model_message(&store, provider.id, model_id));
        }
    }
    if provider.seeded {
        return Catalog::seed()
            .resolve_for(provider.id, model_id)
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    Ok(())
}

/// The hard-error message for a slug the synced catalog vetoes: what was
/// asked, when the master list was last refreshed, the closest real slugs,
/// and the two commands that resolve it.
fn unknown_model_message(store: &CatalogStore, provider_id: &str, model_id: &str) -> String {
    let listings = store
        .models_for_provider(Some(provider_id))
        .unwrap_or_default();
    let needle = model_id.to_ascii_lowercase();
    let mut suggestions: Vec<&str> = listings
        .iter()
        .map(|l| l.slug.as_str())
        .filter(|slug| {
            let hay = slug.to_ascii_lowercase();
            hay.contains(&needle) || needle.contains(&hay)
        })
        .take(5)
        .collect();
    if suggestions.is_empty() {
        // Longest shared prefix, best three — enough to catch a typo'd tail.
        let mut ranked: Vec<(usize, &str)> = listings
            .iter()
            .map(|l| {
                let hay = l.slug.to_ascii_lowercase();
                let shared = hay
                    .bytes()
                    .zip(needle.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                (shared, l.slug.as_str())
            })
            .filter(|(shared, _)| *shared >= 3)
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        suggestions = ranked.into_iter().take(3).map(|(_, slug)| slug).collect();
    }
    let refreshed = store
        .sync_info(SYNC_SOURCE)
        .ok()
        .flatten()
        .map(|info| info.refreshed_at)
        .unwrap_or_else(|| "never".to_string());
    let mut message = format!(
        "unknown model `{provider_id}/{model_id}` — not in the model catalog \
         (master list refreshed {refreshed} UTC)"
    );
    if !suggestions.is_empty() {
        message.push_str(&format!("\n  did you mean: {}?", suggestions.join(", ")));
    }
    message.push_str(&format!(
        "\n  `stella models list --provider {provider_id}` shows every valid slug; if this \
         model launched recently, run `stella models refresh` first"
    ));
    message
}

// ---------------------------------------------------------------------
// Telemetry alias learning
// ---------------------------------------------------------------------

/// Register the model string a provider echoed on the wire, if it isn't
/// joinable yet. Called from the telemetry write path — must never slow it
/// down or fail it: one in-process dedupe set keeps it to a single lookup
/// per distinct (provider, wire string) per session, and every store
/// outcome is best-effort.
pub(crate) fn note_wire_model(provider_id: &str, wire_model: &str) {
    if wire_model.is_empty() {
        return;
    }
    static SEEN: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    {
        let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
        let mut guard = seen.lock().unwrap_or_else(|p| p.into_inner());
        if !guard.insert((provider_id.to_string(), wire_model.to_string())) {
            return;
        }
    }
    let Some(store) = catalog_store() else {
        return;
    };
    match store.resolve(provider_id, wire_model) {
        Ok(Some(_)) => return, // already joinable
        Ok(None) => {}
        Err(_) => return,
    }
    // The wire form usually differs from the catalog id only by a version
    // stamp, a region prefix, or case — try those bases, most specific
    // first, and learn the wire form against the first card that matches.
    let mut candidates: Vec<String> = Vec::new();
    if let Some(base) = version_stripped(wire_model) {
        candidates.push(base);
    }
    if let Some(region_free) = region_stripped(wire_model) {
        if let Some(base) = version_stripped(&region_free) {
            candidates.push(base);
        }
        candidates.push(region_free);
    }
    let lower = wire_model.to_ascii_lowercase();
    if lower != wire_model {
        candidates.push(lower);
    }
    for candidate in candidates {
        if let Ok(Some(hit)) = store.resolve(provider_id, &candidate) {
            let version = extract_model_version(wire_model);
            let _ = store.insert_learned_alias(
                provider_id,
                wire_model,
                hit.model_card_id,
                version.as_deref(),
            );
            return;
        }
    }
}

// ---------------------------------------------------------------------
// Commands: `stella models refresh` / `stella models list` / status
// ---------------------------------------------------------------------

/// One refresh pass: conditional fetch (persisted ETag), map, batch-apply,
/// record the sync. `force` drops the validator to re-download even an
/// unchanged list (recovery hatch for a corrupted catalog).
async fn refresh_with_store(
    store: &CatalogStore,
    force: bool,
) -> Result<(bool, usize, RefreshCounts), String> {
    let previous = store.sync_info(SYNC_SOURCE).map_err(|e| e.to_string())?;
    let etag = if force {
        None
    } else {
        previous.as_ref().and_then(|info| info.etag.clone())
    };
    match modelsdev::fetch_catalog(modelsdev::MODELS_DEV_URL, etag.as_deref()).await? {
        FetchOutcome::NotModified => {
            // Same list — re-stamp the sync time so the staleness clock
            // restarts, keeping the ETag/hash that still validate.
            let (etag, hash) = previous
                .map(|info| (info.etag, info.payload_hash))
                .unwrap_or_default();
            store
                .record_sync(SYNC_SOURCE, etag.as_deref(), hash.as_deref())
                .map_err(|e| e.to_string())?;
            Ok((true, 0, RefreshCounts::default()))
        }
        FetchOutcome::Fetched(fetched) => {
            let upserts = build_upserts(&fetched);
            let counts = store.apply_batch(&upserts).map_err(|e| e.to_string())?;
            store
                .record_sync(
                    SYNC_SOURCE,
                    fetched.etag.as_deref(),
                    Some(&fetched.payload_hash),
                )
                .map_err(|e| e.to_string())?;
            Ok((false, fetched.providers.len(), counts))
        }
    }
}

/// `stella models refresh [--force]`.
pub async fn run_refresh(force: bool) -> Result<(), String> {
    let store = store_for_command()?;
    ensure_seed_floor(&store);
    println!(
        "{}\n",
        "Stella — Model Catalog Refresh (models.dev)"
            .yellow()
            .bold()
    );
    let (not_modified, providers, counts) = refresh_with_store(&store, force).await?;
    if not_modified {
        println!(
            "  {} master list unchanged (ETag match) — catalog already current",
            "✓".green()
        );
    } else {
        println!(
            "  {} synced {} models across {} providers",
            "✓".green(),
            counts.models_seen,
            providers
        );
        println!(
            "    {} new model cards, {} new pricing versions, {} new aliases",
            counts.cards_added, counts.versions_added, counts.aliases_added
        );
    }
    let (cards, versions, aliases) = store.counts().map_err(|e| e.to_string())?;
    println!(
        "  catalog now holds {cards} model cards, {versions} pricing versions, {aliases} aliases"
    );
    println!(
        "\n  Model slugs now validate against this master list; re-run to pick up new releases."
    );
    Ok(())
}

/// Render an optional USD-per-Mtok rate: `-` when unknown (never `$0.00`,
/// which would read as "free" — a zero rate only renders as zero when the
/// catalog really says zero, e.g. gateway-priced meta-models).
fn rate(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("${v:.2}"),
        None => "-".to_string(),
    }
}

/// `stella models list [--provider <id>]`.
pub fn run_list(provider: Option<&str>) -> Result<(), String> {
    let store = store_for_command()?;
    ensure_seed_floor(&store);
    let listings = store
        .models_for_provider(provider)
        .map_err(|e| e.to_string())?;
    if listings.is_empty() {
        match provider {
            Some(p) => println!(
                "no models in the catalog for provider `{p}` — run `stella models refresh` \
                 to sync the master list"
            ),
            None => println!("the model catalog is empty — run `stella models refresh`"),
        }
        return Ok(());
    }
    println!("{}\n", "Stella — Model Catalog".yellow().bold());
    println!(
        "  {}",
        "provider/slug  ·  $in / $out / $cached-in per Mtok  ·  context  ·  maker  [pricing v]"
            .dimmed()
    );
    for l in &listings {
        let ctx = l
            .pricing
            .context_window
            .map(|c| format!("{}k", c / 1000))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  {}/{}  {} / {} / {}  ctx {}  {}  [v{}]",
            l.api_provider.bright_magenta(),
            l.slug.bright_white(),
            rate(l.pricing.input_usd_per_mtok),
            rate(l.pricing.output_usd_per_mtok),
            rate(l.pricing.cached_input_usd_per_mtok),
            ctx,
            l.model_provider.dimmed(),
            l.version,
        );
    }
    println!(
        "\n  {} models. Pricing shown is each card's latest version.",
        listings.len()
    );
    match store.sync_info(SYNC_SOURCE).map_err(|e| e.to_string())? {
        Some(info) => println!(
            "  master list last refreshed {} UTC — `stella models refresh` to update",
            info.refreshed_at
        ),
        None => println!(
            "  seed data only — `stella models refresh` pulls the live master list (models.dev)"
        ),
    }
    Ok(())
}

/// The one-line catalog status appended to `stella models`.
pub fn print_catalog_status() {
    let Ok(store) = CatalogStore::open(&catalog_db_path()) else {
        return;
    };
    let (cards, _, aliases) = store.counts().unwrap_or((0, 0, 0));
    match store.sync_info(SYNC_SOURCE) {
        Ok(Some(info)) => println!(
            "\n  Model catalog: {cards} models / {aliases} aliases — master list refreshed {} \
             UTC (`stella models list` to browse, `stella models refresh` to update).",
            info.refreshed_at
        ),
        _ => println!(
            "\n  Model catalog: seed only — `stella models refresh` pulls the live master list \
             (models.dev) and turns on strict slug validation for every provider."
        ),
    }
}

#[cfg(test)]
mod tests {
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
        assert!(up.aliases.iter().any(|a| a.alias == "gemini/gemini-3-pro"));
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
}
