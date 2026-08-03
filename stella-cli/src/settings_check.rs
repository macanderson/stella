//! Launch-time settings validation — correctness checks run once, right
//! after the config resolves and before the first turn, so a mis-formed
//! setting is a clear warning at startup instead of a provider `400` on the
//! first model call.
//!
//! Today the focus is **model slugs**: the class of bug where a settings
//! entry names a model the provider can't serve — an unknown provider, a
//! typo'd slug, or a provider-qualification that resolves to the wrong wire
//! id (e.g. an OpenRouter slug that ends up double-prefixed as
//! `openrouter/openrouter/auto`, which OpenRouter rejects). Each configured
//! reference is resolved to the exact WIRE slug the engine would send and
//! validated against the catalog ([`crate::model_catalog::validate_model_slug`]),
//! so the check sees precisely what the provider will see.
//!
//! Warnings never block launch — a run can proceed on a partially-valid
//! config (a bad judge pin falls back to the worker, etc.); the point is to
//! surface the problem where it's cheap to fix, not to gate. `stella doctor`
//! runs the same pass through [`inspect_model_config`] and *does* gate: a
//! script that wants a hard answer before spending money asks for one.
//!
//! # The precedence this module resolves against
//!
//! Documented here because it is the thing users guess wrong about (#895),
//! and because getting it wrong would make every warning point at the wrong
//! setting. Highest first, as `config.rs` actually implements it:
//!
//! 1. **`--model`**, which is the SAME clap slot as **`STELLA_MODEL`**
//!    (`cli.rs`'s `#[arg(long, global = true, env = "STELLA_MODEL")]`), so
//!    the flag beats the env var and either beats every settings key —
//!    `Config::load` does `model_override.or(engine_default)`.
//! 2. **`agents.default.model`** (with `agents.default.provider` pinning the
//!    provider, in which case the slug is sent verbatim).
//! 3. **`default_model`**.
//! 4. **Auto-detection** — the first [`PROVIDERS`] row with a resolvable
//!    credential, on that provider's own `default_model`; then
//!    settings-defined providers, alphabetically.
//!
//! `--base-url` / `STELLA_BASE_URL` is NOT in that chain at all: it replaces
//! the endpoint of whichever provider the chain picked, without changing the
//! pick or the credential. That is the genuinely surprising one — it is how a
//! Z.ai URL ends up receiving an OpenRouter key and returning a 401 that
//! reads as "OpenRouter rejected the credential" — so
//! [`check_base_url_coherence`] calls it out by name.

use std::path::Path;

use crate::config::{Dialect, LOCAL_PROVIDER, PROVIDERS, ProviderConfig};
use crate::engine_config::{ModelSpec, model_spec_for, parse_model_spec};
use crate::model_catalog::validate_model_slug;
use crate::settings::{AgentEngineConfig, EngineAgentKind};
use stella_model::catalog::Catalog;

/// One flagged settings problem — where it lives, the offending value, and
/// what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsIssue {
    /// The settings location, in the user's own vocabulary (`default_model`,
    /// `agents.judge.model`, `allowed_models[2]`, or `--model`).
    pub location: String,
    /// The configured value that failed.
    pub value: String,
    /// What is wrong and how to fix it.
    pub message: String,
}

impl SettingsIssue {
    /// The one-line form the launch path prints (and tests pin).
    pub fn line(&self) -> String {
        format!("{}: `{}` — {}", self.location, self.value, self.message)
    }
}

/// The `location` the base-URL coherence finding carries. Named because both
/// the reporter (which suppresses the model fix hints for it — no amount of
/// `stella models` fixes a mis-pointed endpoint) and its test refer to it.
const BASE_URL_LOCATION: &str = "--base-url / STELLA_BASE_URL";

/// The ways out of a bad model setting, in the order a user tries them: see
/// what IS valid, re-sync in case the model shipped after the last sync, then
/// the two places the default is actually set. Every model finding — at
/// launch, in `stella doctor`, and on a refused `/model` — ends with these,
/// so "invalid model" is never where the message stops (#895).
pub fn model_fix_hints(provider_id: Option<&str>) -> Vec<String> {
    let scope = provider_id
        .map(|id| format!(" --provider {id}"))
        .unwrap_or_default();
    vec![
        format!("`stella models list{scope}` prints every slug that will be accepted"),
        "`stella models refresh` re-syncs the catalog — try this first if the model shipped \
         recently"
            .to_string(),
        "set the default with `/model <provider>/<slug>` in the deck, or on its SETTINGS tab"
            .to_string(),
    ]
}

/// The host of a base URL — lowercased, without scheme, userinfo, or port.
/// Deliberately dependency-free (no `url` crate for one comparison) and
/// deliberately host-only: Z.ai's coding-plan endpoint differs from its
/// standard one by PATH alone (`/api/coding/paas/v4` vs `/api/paas/v4`) and
/// must read as coherent.
fn base_url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // Strip a port only when the tail really is one, so an unbracketed IPv6
    // literal is not truncated to `[`.
    let host = match authority.rsplit_once(':') {
        Some((head, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => authority,
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Detect the provider/base_url incoherence that mislabels its own error
/// (#895 problem 3): `--base-url` replaces the endpoint but never the
/// provider or the credential, so pointing it at a DIFFERENT known provider's
/// host sends provider A's key to provider B — and B's `401` is reported by
/// A's adapter, which is why the prior incident read as "OpenRouter rejected
/// the credential" while the request went to Z.ai.
///
/// Only a host that belongs to another [`PROVIDERS`] row is flagged. A
/// private gateway, a corporate proxy, or an SSH tunnel is a legitimate and
/// common use of the flag, and this check must never cry wolf at one — an
/// unknown host is `None`. Also `None` for `local` (whose base URL *is* its
/// address) and for Vertex/Bedrock (whose table rows carry a display anchor,
/// not a real endpoint — `bedrock` literally contains `<AWS_REGION>`).
pub fn check_base_url_coherence(
    provider: &ProviderConfig,
    base_url_override: Option<&str>,
) -> Option<SettingsIssue> {
    let raw = base_url_override.map(str::trim).filter(|u| !u.is_empty())?;
    if provider.id == LOCAL_PROVIDER.id
        || matches!(provider.dialect, Dialect::Vertex | Dialect::Bedrock)
    {
        return None;
    }
    let host = base_url_host(raw)?;
    if base_url_host(provider.base_url).is_some_and(|own| own == host) {
        return None;
    }
    let other = PROVIDERS.iter().find(|p| {
        p.id != provider.id
            && !matches!(p.dialect, Dialect::Vertex | Dialect::Bedrock)
            && base_url_host(p.base_url).is_some_and(|h| h == host)
    })?;
    Some(SettingsIssue {
        location: BASE_URL_LOCATION.to_string(),
        value: raw.to_string(),
        message: format!(
            "points at {} ({host}) while the resolved provider is {} — your {} credential \
             would be sent to {}, whose rejection is reported as \"{} rejected the API key\". \
             Pin the provider to the host (`--model {}/<slug>`) or drop the override.",
            other.display_name,
            provider.display_name,
            provider.env_var,
            other.display_name,
            provider.display_name,
            other.id,
        ),
    })
}

fn kind_label(kind: EngineAgentKind) -> &'static str {
    match kind {
        EngineAgentKind::Default => "default",
        EngineAgentKind::Worker => "worker",
        EngineAgentKind::Judge => "judge",
        EngineAgentKind::Triage => "triage",
    }
}

/// Whether this provider's seed-catalog ids are vendor-namespaced (carry a
/// `/`, like OpenRouter's `openrouter/auto` and `openai/gpt-5.5`) rather than
/// bare (like Z.ai's `glm-5.2`). For a namespaced provider the wire slug MUST
/// keep its namespace — a bare slug is the fingerprint of an over-eager
/// `provider/slug` split that stripped it.
fn provider_ids_namespaced(provider: &str) -> bool {
    let seed = Catalog::seed();
    let mut ids = seed
        .entries()
        .iter()
        .filter(|e| e.provider == provider)
        .map(|e| e.id.as_str())
        .peekable();
    ids.peek().is_some() && ids.all(|id| id.contains('/'))
}

/// One real seeded id for `provider`, so a message can show the shape it is
/// asking for instead of describing it. Nothing beats a line the user can
/// paste. Ids whose vendor happens to BE the provider name are skipped —
/// `openrouter/openrouter/auto` is a correct setting and a terrible example,
/// because it looks like the doubling the neighbouring check rejects.
fn example_id(provider: &str) -> Option<String> {
    let seed = Catalog::seed();
    let ids = || seed.entries().iter().filter(|e| e.provider == provider);
    ids()
        .find(|e| !e.id.starts_with(&format!("{provider}/")))
        .or_else(|| ids().next())
        .map(|e| e.id.clone())
}

/// The wire-exactness problem with `wire`, if any — checks INDEPENDENT of the
/// catalog's alias-tolerant `resolve`, which happily maps both a doubled and a
/// de-namespaced slug back to the right card and so masks exactly these bugs.
/// The precise fingerprints:
///
/// - **over-qualified**: the slug repeats the provider prefix
///   (`openrouter/openrouter/auto`) — the doubled form providers reject; and
/// - **de-namespaced**: a namespaced provider's slug lost its vendor prefix
///   (`openrouter/auto` mis-split to the wire slug `auto`).
fn wire_shape_issue(provider: &str, wire: &str) -> Option<String> {
    if wire.starts_with(&format!("{provider}/{provider}/")) {
        return Some(format!(
            "over-qualified — the id repeats `{provider}/`; drop one so the wire \
             slug matches the provider's catalog (e.g. `{provider}/auto`, not \
             `{provider}/{provider}/auto`)"
        ));
    }
    if !wire.contains('/') && provider_ids_namespaced(provider) {
        // Deliberately phrased against the SETTING, not the wire slug: this
        // fires on `default_model: "openrouter/auto"`, whose value is echoed
        // beside the message, so "the wire slug should be `openrouter/auto`"
        // read as advice to keep exactly what was already there. What is
        // actually missing is a segment — the vendor between the two.
        let example = example_id(provider)
            .map(|id| format!("{provider}/{id}"))
            .unwrap_or_else(|| format!("{provider}/<vendor>/{wire}"));
        return Some(format!(
            "missing the vendor namespace — `{provider}` model ids are \
             `vendor/model` pairs, so the bare `{wire}` names nothing it \
             serves. Name the vendor too, as `{provider}/<vendor>/{wire}` \
             (a real one looks like `{example}`)"
        ));
    }
    None
}

/// Validate an already-resolved [`ModelSpec`] — the wire slug exactly as the
/// engine would send it — against the provider catalog. `None` means "no
/// problem I can prove" (valid, a provider pin with no model — the provider
/// default answers — or a settings-defined provider whose endpoint is the
/// authority). `value` is the user's original string, echoed back in the
/// warning so it points at what they actually typed.
fn check_resolved_spec(location: &str, value: &str, spec: &ModelSpec) -> Option<SettingsIssue> {
    // A provider pin with no slug rides the provider's own default model.
    if spec.model.is_empty() {
        return None;
    }
    // Only built-in providers have a catalog to validate against; a
    // settings-defined custom endpoint is its own authority (mirrors
    // `validate_model_slug`'s local/never-synced posture) — `?` skips it.
    let provider_config = PROVIDERS.iter().find(|p| p.id == spec.provider)?;
    // Wire-shape checks first — they catch the over-qualified / de-namespaced
    // slugs the alias-tolerant catalog resolve would wave through.
    if let Some(message) = wire_shape_issue(&spec.provider, &spec.model) {
        return Some(SettingsIssue {
            location: location.to_string(),
            value: value.to_string(),
            message,
        });
    }
    match validate_model_slug(provider_config, &spec.model) {
        Ok(()) => None,
        Err(message) => Some(SettingsIssue {
            location: location.to_string(),
            value: value.to_string(),
            message,
        }),
    }
}

/// Validate one configured model STRING (a `provider/slug` or bare slug with
/// no separate `provider` field — `default_model`, `allowed_models`),
/// resolving it to its wire slug exactly as the engine would.
fn check_spec(
    location: &str,
    raw: &str,
    is_provider: &dyn Fn(&str) -> bool,
) -> Option<SettingsIssue> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Some(spec) = parse_model_spec(trimmed, is_provider) else {
        return Some(SettingsIssue {
            location: location.to_string(),
            value: trimmed.to_string(),
            message: "unrecognized model — use `provider/slug` (e.g. `zai/glm-5.2`) or a \
                      bare slug the seed catalog knows"
                .to_string(),
        });
    };
    check_resolved_spec(location, trimmed, &spec)
}

/// Which flat key actually feeds [`AgentEngineConfig::model_for`] for `kind`
/// when the agent itself sets no `model` (issue #273's remaining validator
/// gap): the kind's own `pipeline_<kind>_model` when set, else `default_model`
/// — the same fallback order `model_for` implements. Used only to label a
/// flagged issue with the setting the user actually needs to fix.
fn flat_source_label(engine: &AgentEngineConfig, kind: EngineAgentKind) -> &'static str {
    let flat_specific = match kind {
        EngineAgentKind::Default => None,
        EngineAgentKind::Worker => engine.pipeline_worker_model.as_deref(),
        EngineAgentKind::Judge => engine.pipeline_judge_model.as_deref(),
        EngineAgentKind::Triage => engine.pipeline_triage_model.as_deref(),
    };
    match (kind, flat_specific.is_some()) {
        (EngineAgentKind::Worker, true) => "pipeline_worker_model",
        (EngineAgentKind::Judge, true) => "pipeline_judge_model",
        (EngineAgentKind::Triage, true) => "pipeline_triage_model",
        _ => "default_model",
    }
}

/// Validate every model reference in the engine settings. Per-agent `model`
/// entries are resolved through the engine's own [`model_spec_for`], so the
/// check honors the agent's explicit `provider` field (a set `provider` sends
/// `model` VERBATIM as the wire slug — no `provider/slug` split) and validates
/// against the exact provider the request will hit. `default_model` and each
/// `allowed_models` candidate are plain `provider/slug` strings, parsed as
/// `--model` semantics.
pub fn check_engine_settings(
    engine: &AgentEngineConfig,
    is_provider: &dyn Fn(&str) -> bool,
) -> Vec<SettingsIssue> {
    let mut issues = Vec::new();
    if let Some(model) = &engine.default_model
        && let Some(issue) = check_spec("default_model", model, is_provider)
    {
        issues.push(issue);
    }
    for kind in [
        EngineAgentKind::Default,
        EngineAgentKind::Worker,
        EngineAgentKind::Judge,
        EngineAgentKind::Triage,
    ] {
        let Some(agent) = engine.agent(kind) else {
            continue;
        };
        // The agent's OWN explicit `model` pin, if it set one.
        let own_model = agent
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty());
        if let Some(trimmed) = own_model {
            let location = format!("agents.{}.model", kind_label(kind));
            // Resolve exactly as the engine does — honoring `agent.provider`.
            match model_spec_for(engine, kind, is_provider) {
                Some(spec) => {
                    if let Some(issue) = check_resolved_spec(&location, trimmed, &spec) {
                        issues.push(issue);
                    }
                }
                // No pinned provider and the string names no known provider /
                // seed slug: fall back to the plain-string diagnostic so a
                // typo'd per-agent slug still surfaces as `unrecognized`.
                None => {
                    if let Some(issue) = check_spec(&location, trimmed, is_provider) {
                        issues.push(issue);
                    }
                }
            }
            continue;
        }
        // Issue #273: the agent sets no `model` of its own, but a `provider`
        // pin still stands — its effective model comes from a flat key
        // (`pipeline_<kind>_model` or `default_model`). That flat string is
        // ALREADY validated on its own as a plain `provider/slug` spec
        // (`default_model` above, or nowhere at all for the pipeline-specific
        // keys), but never combined with THIS provider pin — the exact
        // combination the engine actually sends. Skip `Default`: its flat key
        // IS `default_model`, and the resolved wire model is separately
        // backstopped by `check_resolved_model` at launch, so adding it here
        // would only risk a duplicate (the launch-time dedup matches on
        // identical `value`, not location).
        if matches!(
            kind,
            EngineAgentKind::Worker | EngineAgentKind::Judge | EngineAgentKind::Triage
        ) && agent
            .provider
            .as_deref()
            .map(str::trim)
            .is_some_and(|p| !p.is_empty())
            && let Some(raw) = engine.model_for(kind)
            && let Some(spec) = model_spec_for(engine, kind, is_provider)
            && let Some(issue) = check_resolved_spec(
                &format!(
                    "agents.{} (provider pin over {})",
                    kind_label(kind),
                    flat_source_label(engine, kind)
                ),
                raw,
                &spec,
            )
        {
            issues.push(issue);
        }
    }
    for (i, model) in engine.allowed_models().iter().enumerate() {
        if let Some(issue) = check_spec(&format!("allowed_models[{i}]"), model, is_provider) {
            issues.push(issue);
        }
    }
    issues
}

/// Push `issue` unless the same offending VALUE was already reported —
/// `default_model` and the resolved wire model are usually the same string
/// seen from two angles, and saying it twice reads as two problems.
fn push_unique(issues: &mut Vec<SettingsIssue>, issue: SettingsIssue) {
    if !issues.iter().any(|i| i.value == issue.value) {
        issues.push(issue);
    }
}

/// Every provider id this workspace can legally name: the built-ins, `local`,
/// and whatever `settings.providers` defines.
fn provider_ids(settings: &crate::settings::Settings) -> Vec<String> {
    PROVIDERS
        .iter()
        .map(|p| p.id.to_string())
        .chain(std::iter::once(LOCAL_PROVIDER.id.to_string()))
        .chain(settings.providers.keys().cloned())
        .collect()
}

/// The launch entry point: validate every configured model reference, the
/// resolved default model, and the provider/base_url pairing. Best-effort —
/// a settings load failure yields no issues here (the config path already
/// surfaced it), and the caller treats the result as advisory warnings, never
/// a launch gate.
pub fn validate_at_launch(cfg: &crate::config::Config) -> Vec<SettingsIssue> {
    let mut issues = Vec::new();
    if let Ok(settings) = crate::settings::Settings::load(&cfg.workspace_root) {
        let ids = provider_ids(&settings);
        let is_provider = |id: &str| ids.iter().any(|p| p == id);
        if let Some(engine) = &settings.agent_engine_config {
            issues.extend(check_engine_settings(engine, &is_provider));
        }
    }
    // The effective wire model — deduped against the settings checks so an
    // issue already reported for `default_model` isn't repeated here.
    if let Some(issue) = check_resolved_model(&cfg.provider, &cfg.model_id) {
        push_unique(&mut issues, issue);
    }
    // Nothing to dedup against: an endpoint is not a model.
    if let Some(issue) = check_base_url_coherence(&cfg.provider, cfg.base_url_override.as_deref()) {
        issues.push(issue);
    }
    issues
}

/// Run [`validate_at_launch`] and print it: one `⚠ settings:` line per
/// finding, then the fix hints ONCE. Owning the printing here rather than at
/// each call site is what keeps the two paths that resolve their own config —
/// `main` and `stella ingest <paths>` — saying the same thing (#895 problem
/// 2, where ingest had no advisory surface at all).
///
/// The hints are model-shaped, so they are suppressed when the only finding
/// is a mis-pointed endpoint: no amount of `stella models` fixes that, and
/// [`check_base_url_coherence`]'s message already carries its own remedy.
pub fn report_at_launch(cfg: &crate::config::Config) {
    let issues = validate_at_launch(cfg);
    for issue in &issues {
        eprintln!("⚠ settings: {}", issue.line());
    }
    if issues.iter().any(|i| i.location != BASE_URL_LOCATION) {
        for hint in model_fix_hints(Some(cfg.provider.id)) {
            eprintln!("  → {hint}");
        }
    }
}

/// What `stella doctor` needs to say about this workspace's model
/// configuration, resolved WITHOUT credentials and WITHOUT network so the
/// command keeps its "no provider, no API key, no network" contract.
#[derive(Debug, Clone, Default)]
pub struct ModelConfigReport {
    /// The `provider/slug` the next run would send, when one resolves.
    pub resolved: Option<String>,
    /// Which rung of the precedence chain produced it, in the user's own
    /// vocabulary — the setting they would edit to change it.
    pub source: String,
    /// The provider the default routes to, for scoping the fix hints.
    pub provider: Option<String>,
    /// An honest caveat on a PASS: what this offline check could not prove.
    pub note: Option<String>,
    /// Everything provably wrong, most-configured-first.
    pub issues: Vec<SettingsIssue>,
}

/// The offline check's blind spot, stated out loud. An UNSEEDED provider
/// (OpenRouter, a settings-defined gateway) has no curated slug list, so
/// until its own `/models` listing has been synced the only thing provable
/// without network is the slug's SHAPE. A pass must not read as "verified"
/// when that is all it means.
fn unsynced_note(provider: &ProviderConfig) -> Option<String> {
    if provider.seeded {
        return None;
    }
    let synced = crate::model_catalog::catalog_store().map_or(0, |store| {
        store
            .provider_model_count(provider.id, Some(crate::model_catalog::SYNC_SOURCE))
            .unwrap_or(0)
            + store
                .provider_model_count(provider.id, Some(crate::model_catalog::NATIVE_SOURCE))
                .unwrap_or(0)
    });
    (synced == 0).then(|| {
        format!(
            "{} has no synced model list yet, so only the slug's shape could be checked — \
             `stella models refresh` turns this into a real catalog check",
            provider.display_name
        )
    })
}

/// Resolve and validate the default model the way a run would, but with no
/// credentials prompted and no network touched — the form `stella doctor`
/// needs. The precedence walked here is the one documented in the module
/// header, and it is the REAL one: `model_override` carries `--model` and
/// `STELLA_MODEL` alike (one clap slot), and it outranks settings.
pub fn inspect_model_config(
    workspace_root: &Path,
    model_override: Option<&str>,
    base_url_override: Option<&str>,
) -> ModelConfigReport {
    let settings = crate::settings::Settings::load(workspace_root).unwrap_or_default();
    let ids = provider_ids(&settings);
    let is_provider = |id: &str| ids.iter().any(|p| p == id);

    let mut report = ModelConfigReport {
        issues: settings
            .agent_engine_config
            .as_ref()
            .map(|engine| check_engine_settings(engine, &is_provider))
            .unwrap_or_default(),
        ..Default::default()
    };

    let mut spec = None;
    if let Some(raw) = model_override.map(str::trim).filter(|m| !m.is_empty()) {
        // Rung 1. Not covered by `check_engine_settings` — it is not a
        // setting — so it is validated here, as its own location.
        if let Some(issue) = check_spec("--model", raw, &is_provider) {
            push_unique(&mut report.issues, issue);
        }
        report.source = "--model / STELLA_MODEL".to_string();
        spec = parse_model_spec(raw, &is_provider);
    } else if let Some(engine) = &settings.agent_engine_config
        && let Some(resolved) = model_spec_for(engine, EngineAgentKind::Default, &is_provider)
        && !resolved.model.is_empty()
    {
        // Rungs 2 and 3, told apart by which key actually answered.
        report.source = match engine
            .agent(EngineAgentKind::Default)
            .and_then(|a| a.model.as_deref())
        {
            Some(m) if !m.trim().is_empty() => "agents.default.model",
            _ => flat_source_label(engine, EngineAgentKind::Default),
        }
        .to_string();
        spec = Some(resolved);
    }
    if spec.is_none()
        && let Some(first) = crate::config::discover_configured_providers()
            .into_iter()
            .next()
    {
        // Rung 4. `discover_configured_providers` never prompts, so this
        // stays credential-free in the sense that matters: it reads what is
        // already there and asks the user for nothing.
        report.source = format!(
            "auto-detected — {} is the first provider with a credential",
            first.config.display_name
        );
        spec = Some(ModelSpec {
            provider: first.config.id.to_string(),
            model: first.config.default_model.to_string(),
        });
    }

    let Some(spec) = spec else {
        return report;
    };
    let value = format!("{}/{}", spec.provider, spec.model);
    if let Some(issue) = check_resolved_spec("resolved model", &value, &spec) {
        push_unique(&mut report.issues, issue);
    }
    if let Some(provider) = PROVIDERS.iter().find(|p| p.id == spec.provider) {
        if let Some(issue) = check_base_url_coherence(provider, base_url_override) {
            report.issues.push(issue);
        }
        report.note = unsynced_note(provider);
    }
    report.provider = Some(spec.provider);
    report.resolved = Some(value);
    report
}

/// Validate the RESOLVED wire model the default agent will actually send —
/// the last line of defense, catching a bad slug however it was configured
/// (`--model`, auto-detect, or a settings path this module can't see).
pub fn check_resolved_model(provider: &ProviderConfig, model_id: &str) -> Option<SettingsIssue> {
    let issue = |message: String| SettingsIssue {
        location: "resolved model".to_string(),
        value: format!("{}/{}", provider.id, model_id),
        message,
    };
    if let Some(message) = wire_shape_issue(provider.id, model_id) {
        return Some(issue(message));
    }
    validate_model_slug(provider, model_id).err().map(issue)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The seed catalog is deterministic, so these assertions hold without any
    // synced store: `zai/glm-5.2` and `openrouter/auto` are seeded; `auto`
    // (bare) and doubled forms are not.
    fn is_seed_provider(id: &str) -> bool {
        PROVIDERS.iter().any(|p| p.id == id)
    }

    fn openrouter() -> &'static ProviderConfig {
        PROVIDERS.iter().find(|p| p.id == "openrouter").unwrap()
    }

    #[test]
    fn a_seeded_slug_passes() {
        assert!(check_spec("default_model", "zai/glm-5.2", &is_seed_provider).is_none());
    }

    #[test]
    fn the_correct_openrouter_qualified_form_passes() {
        // `openrouter/openrouter/auto` decodes to the wire slug
        // `openrouter/auto`, which the seed catalog knows — this is the
        // CORRECT setting form and must NOT be flagged.
        assert!(
            check_spec(
                "default_model",
                "openrouter/openrouter/auto",
                &is_seed_provider
            )
            .is_none()
        );
    }

    #[test]
    fn a_singly_qualified_openrouter_slug_is_flagged() {
        // `openrouter/auto` resolves to the wire slug `auto`, which OpenRouter
        // does not serve — the natural-looking but wrong form.
        let issue = check_spec("default_model", "openrouter/auto", &is_seed_provider)
            .expect("bare `auto` wire slug must be flagged");
        assert_eq!(issue.location, "default_model");
    }

    #[test]
    fn an_over_qualified_slug_gets_the_double_prefix_note() {
        // The doubled wire slug that actually reaches the provider as a 400.
        let issue = check_resolved_model(openrouter(), "openrouter/openrouter/auto")
            .expect("doubled wire slug must be flagged");
        assert!(
            issue.message.contains("over-qualified"),
            "expected the double-prefix note: {}",
            issue.message
        );
    }

    #[test]
    fn an_unknown_provider_qualification_is_unrecognized() {
        let issue = check_spec("agents.judge.model", "notaprovider/x", &is_seed_provider)
            .expect("unknown provider prefix must be flagged");
        assert!(issue.message.contains("unrecognized"), "{}", issue.message);
    }

    #[test]
    fn a_valid_resolved_model_is_not_flagged() {
        assert!(check_resolved_model(openrouter(), "openrouter/auto").is_none());
    }

    #[test]
    fn per_agent_provider_pin_is_sent_verbatim_not_split() {
        // A judge pinned to OpenRouter with a slug that itself contains `/`:
        // the engine sends `openai/gpt-6` VERBATIM to OpenRouter (unseeded →
        // its endpoint is the authority), so the check must NOT re-split the
        // slug and validate the phantom `openai/gpt-6` against the OpenAI
        // catalog (where `gpt-6` does not exist) — that was a false positive.
        let engine: AgentEngineConfig = serde_json::from_str(
            r#"{ "agents": { "judge": { "provider": "openrouter", "model": "openai/gpt-6" } } }"#,
        )
        .unwrap();
        assert!(
            check_engine_settings(&engine, &is_seed_provider).is_empty(),
            "an OpenRouter-pinned verbatim slug must not be flagged"
        );
    }

    #[test]
    fn per_agent_provider_pin_validates_the_pinned_provider() {
        // With no explicit `provider`, `openai/nope` splits to the OpenAI
        // catalog and is correctly flagged (the string carries its own
        // routing).
        let engine: AgentEngineConfig =
            serde_json::from_str(r#"{ "agents": { "judge": { "model": "openai/nope" } } }"#)
                .unwrap();
        let issues = check_engine_settings(&engine, &is_seed_provider);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].location, "agents.judge.model");
        assert_eq!(issues[0].value, "openai/nope");
    }

    #[test]
    fn provider_pin_over_a_flat_key_is_validated_issue_273() {
        // The remaining validator gap (#273): the judge sets ONLY `provider`
        // (no `agents.judge.model`), so its effective model rides
        // `pipeline_judge_model` — a bare, de-namespaced OpenRouter slug that
        // reaches the wire as `auto`, which OpenRouter does not serve. Before
        // the fix this was invisible: the per-kind loop only looked at the
        // agent's OWN `model` field.
        let engine: AgentEngineConfig = serde_json::from_str(
            r#"{ "pipeline_judge_model": "auto",
                 "agents": { "judge": { "provider": "openrouter" } } }"#,
        )
        .unwrap();
        let issues = check_engine_settings(&engine, &is_seed_provider);
        assert_eq!(
            issues.len(),
            1,
            "a mis-shaped flat-key model under a provider pin must be flagged: {issues:?}"
        );
        assert_eq!(
            issues[0].location,
            "agents.judge (provider pin over pipeline_judge_model)"
        );
        assert_eq!(issues[0].value, "auto");
        assert!(
            issues[0].message.contains("vendor namespace"),
            "{}",
            issues[0].message
        );
    }

    #[test]
    fn provider_pin_over_default_model_is_labeled_by_its_real_source() {
        // Same gap, but the judge's flat fallback is `default_model` (no
        // `pipeline_judge_model` set) — the location must name the key that
        // actually fed the resolution, not always `pipeline_judge_model`.
        // `default_model` is a valid seeded bare slug on its own (clean at
        // the top-level check), so the ONLY issue is the new provider-pin
        // combination — proving it, not an unrelated `default_model` typo.
        let engine: AgentEngineConfig = serde_json::from_str(
            r#"{ "default_model": "glm-5.2",
                 "agents": { "judge": { "provider": "openrouter" } } }"#,
        )
        .unwrap();
        let issues = check_engine_settings(&engine, &is_seed_provider);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(
            issues[0].location,
            "agents.judge (provider pin over default_model)"
        );
    }

    #[test]
    fn a_verbatim_pin_with_its_own_model_stays_clean_issue_273() {
        // Regression guard: the existing verbatim-pin case (agent sets BOTH
        // `provider` and its own `model`) must stay untouched by the new
        // branch — it is handled entirely by the pre-existing own-model path
        // and produces no false positive.
        let engine: AgentEngineConfig = serde_json::from_str(
            r#"{ "agents": { "judge": { "provider": "openrouter", "model": "openai/gpt-6" } } }"#,
        )
        .unwrap();
        assert!(
            check_engine_settings(&engine, &is_seed_provider).is_empty(),
            "a verbatim per-agent pin with its own model must stay clean"
        );
    }

    #[test]
    fn default_kind_provider_pin_over_default_model_is_not_double_checked() {
        // Default is deliberately excluded from the new branch — its flat key
        // IS `default_model` and its resolved wire model is separately
        // backstopped by `check_resolved_model` at launch (issue #273's
        // scoping note). `default_model` alone (`glm-5.2`, a seeded bare
        // slug) is clean; ONLY combining it with the `agents.default`
        // provider pin (openrouter, which needs a `vendor/` namespace) would
        // produce an issue — so if the new branch covered Default too, this
        // would flag. It must not: the combination stays clean here, exactly
        // because Default is excluded and left to the launch-time backstop.
        let engine: AgentEngineConfig = serde_json::from_str(
            r#"{ "default_model": "glm-5.2",
                 "agents": { "default": { "provider": "openrouter" } } }"#,
        )
        .unwrap();
        assert!(
            check_engine_settings(&engine, &is_seed_provider).is_empty(),
            "Default is backstopped elsewhere, not doubly-checked here"
        );
    }

    fn zai() -> &'static ProviderConfig {
        PROVIDERS.iter().find(|p| p.id == "zai").unwrap()
    }

    /// #895 problem 3, the mislabeled 401: an OpenRouter pin whose endpoint
    /// is forced to Z.ai sends the OpenRouter key to Z.ai, and OpenRouter's
    /// adapter reports the rejection — so the error blames the wrong party.
    /// Only the pairing can catch it, and the message has to name both sides
    /// or it repeats the mistake it is diagnosing.
    #[test]
    fn a_base_url_pointing_at_another_provider_is_flagged_with_both_names() {
        let issue = check_base_url_coherence(openrouter(), Some("https://api.z.ai/api/paas/v4"))
            .expect("a cross-provider endpoint must be flagged");
        assert_eq!(issue.location, BASE_URL_LOCATION);
        assert!(
            issue.message.contains("Z.ai") && issue.message.contains("OpenRouter"),
            "both sides must be named: {}",
            issue.message
        );
        assert!(
            issue.message.contains("OPENROUTER_API_KEY"),
            "the credential that would reach the wrong host is named: {}",
            issue.message
        );
        assert!(
            issue.message.contains("--model zai/"),
            "the fix must be a command, not a diagnosis: {}",
            issue.message
        );
    }

    #[test]
    fn a_coherent_base_url_is_not_flagged() {
        // The provider's own endpoint, obviously.
        assert!(
            check_base_url_coherence(openrouter(), Some("https://openrouter.ai/api/v1")).is_none()
        );
        // Z.ai's coding-plan endpoint differs from its standard one by PATH
        // only, which is why the comparison is host-scoped: flagging this
        // would fire on a supported, documented configuration.
        assert!(
            check_base_url_coherence(zai(), Some("https://api.z.ai/api/coding/paas/v4")).is_none()
        );
        // No override at all.
        assert!(check_base_url_coherence(openrouter(), None).is_none());
    }

    #[test]
    fn an_unknown_host_is_a_private_gateway_not_a_mistake() {
        // A proxy, a tunnel, or a corporate gateway is the ordinary use of
        // `--base-url`. A check that flagged these would be ignored within a
        // day, and then it would not catch the real one either.
        for url in [
            "https://llm-gateway.internal.example/v1",
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080",
        ] {
            assert!(
                check_base_url_coherence(openrouter(), Some(url)).is_none(),
                "`{url}` is a legitimate private endpoint"
            );
        }
    }

    #[test]
    fn base_url_host_strips_scheme_userinfo_and_port_but_not_an_ipv6_literal() {
        assert_eq!(
            base_url_host("https://API.Z.ai/api/paas/v4").as_deref(),
            Some("api.z.ai")
        );
        assert_eq!(
            base_url_host("https://user:pw@openrouter.ai:443/api/v1").as_deref(),
            Some("openrouter.ai")
        );
        // A port-shaped tail that is not a port must survive intact.
        assert_eq!(
            base_url_host("http://[::1]:8080/v1").as_deref(),
            Some("[::1]")
        );
    }

    /// The three ways out, named. This is the acceptance criterion of #895 in
    /// one assertion: an unknown or misrouted slug must print the valid-slug
    /// listing, `stella models`, and the SETTINGS tab.
    #[test]
    fn the_fix_hints_name_all_three_ways_out() {
        let hints = model_fix_hints(Some("openrouter")).join("\n");
        assert!(
            hints.contains("stella models list --provider openrouter"),
            "{hints}"
        );
        assert!(hints.contains("stella models refresh"), "{hints}");
        assert!(hints.contains("SETTINGS tab"), "{hints}");
        // Unscoped when the provider is unknown — never a dangling flag.
        assert!(
            model_fix_hints(None)
                .iter()
                .all(|hint| !hint.contains("--provider")),
            "{:?}",
            model_fix_hints(None)
        );
    }

    /// A scratch workspace holding `.stella/settings.json`. The user scope is
    /// already inert under `cfg(test)` (`settings::user_home_dir` reads a
    /// thread-local nobody sets here), so what this file says is what
    /// `Settings::load` sees.
    fn workspace(settings: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(dir.path().join(".stella")).expect("dot dir");
        std::fs::write(dir.path().join(".stella/settings.json"), settings).expect("settings");
        dir
    }

    #[test]
    fn a_bad_default_model_is_reported_once_and_attributed_to_its_setting() {
        let dir = workspace(r#"{"agent_engine_config": {"default_model": "openrouter/auto"}}"#);
        let report = inspect_model_config(dir.path(), None, None);
        assert_eq!(
            report.issues.len(),
            1,
            "`default_model` and the wire model it resolves to are one problem \
             seen twice, not two: {:?}",
            report.issues
        );
        assert_eq!(report.issues[0].location, "default_model");
        assert_eq!(report.resolved.as_deref(), Some("openrouter/auto"));
        assert_eq!(report.source, "default_model");
        assert_eq!(report.provider.as_deref(), Some("openrouter"));
    }

    /// #895 problem 4 assumed a settings-pinned model silently outranks the
    /// override. It does not — `--model` and `STELLA_MODEL` are one clap slot
    /// and `Config::load` does `model_override.or(engine_default)` — so a good
    /// setting cannot rescue a bad flag, and the finding must be attributed to
    /// the flag rather than to the innocent setting underneath it.
    #[test]
    fn the_override_outranks_the_setting_and_is_named_as_the_source() {
        let dir = workspace(r#"{"agent_engine_config": {"default_model": "zai/glm-5.2"}}"#);
        let report = inspect_model_config(dir.path(), Some("openrouter/auto"), None);
        assert_eq!(report.resolved.as_deref(), Some("openrouter/auto"));
        assert_eq!(report.source, "--model / STELLA_MODEL");
        assert_eq!(report.issues.len(), 1, "{:?}", report.issues);
        assert_eq!(report.issues[0].location, "--model");
    }

    #[test]
    fn a_valid_default_reports_the_model_and_no_issues() {
        let dir = workspace(r#"{"agent_engine_config": {"default_model": "zai/glm-5.2"}}"#);
        let report = inspect_model_config(dir.path(), None, None);
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert_eq!(report.resolved.as_deref(), Some("zai/glm-5.2"));
        assert_eq!(report.source, "default_model");
        // Z.ai is seeded, so the check is complete and has nothing to caveat.
        assert_eq!(report.note, None);
    }

    #[test]
    fn an_agents_default_pin_is_named_as_the_source_not_the_flat_key() {
        // Both keys are set and both are valid; only the one that actually
        // answers may be named, or the user edits the wrong line.
        let dir = workspace(
            r#"{"agent_engine_config": {
                 "default_model": "zai/glm-5.2",
                 "agents": {"default": {"model": "openai/gpt-5.5"}}
               }}"#,
        );
        let report = inspect_model_config(dir.path(), None, None);
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert_eq!(report.resolved.as_deref(), Some("openai/gpt-5.5"));
        assert_eq!(report.source, "agents.default.model");
    }

    #[test]
    fn issue_line_is_readable() {
        let issue = SettingsIssue {
            location: "default_model".into(),
            value: "openrouter/auto".into(),
            message: "not served".into(),
        };
        assert_eq!(
            issue.line(),
            "default_model: `openrouter/auto` — not served"
        );
    }
}
