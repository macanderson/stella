//! Snapshot builders for the SETTINGS tab's two panels (ENGINE, TOOLS).
//!
//! Split out of `command_deck.rs` (closed to growth) the way `pr_observe.rs`
//! and `issues.rs` were. Both builders are shared across several call sites —
//! session boot, `/reload`, the panel handlers in `settings_io.rs`, and an
//! in-deck session switch (`session_override.rs`) — which is why they live
//! here rather than beside any one caller.

use stella_tui::Inbound;

use crate::config::Config;

// ── Agent-engine config (the SETTINGS tab's config panel) ─────────────────────

/// Build an [`Inbound::EngineConfig`] snapshot: the freshly merged
/// `agent_engine_config` from the settings scope chain, plus the picker
/// vocabularies — every provider whose credential currently resolves, and
/// the catalog's `provider/slug` list as the model-picker fallback when
/// `allowed_models` is empty. The model list is scoped to those same
/// credentialed providers (plus the session's active one): a model you
/// have no key for is not an option, and offering it anyway was exactly
/// the "selectable but unusable" bug. Re-reading the chain (rather than
/// caching) is what makes the overlay reflect a hand edit, and show what a
/// save at one scope means under the others.
pub(super) fn engine_config_inbound(cfg: &Config, status: Option<String>) -> Inbound {
    let engine = crate::settings::Settings::load(&cfg.workspace_root)
        .ok()
        .and_then(|s| s.agent_engine_config)
        .unwrap_or_default();
    let providers: Vec<String> = crate::config::discover_configured_providers()
        .into_iter()
        .map(|p| p.config.id.to_string())
        .collect();
    // The session's provider is always usable — its credential resolved at
    // startup (possibly interactively, which discovery never does).
    let mut usable: std::collections::HashSet<&str> =
        providers.iter().map(String::as_str).collect();
    usable.insert(cfg.provider.id);
    let catalog = stella_model::catalog::Catalog::current();
    let mut catalog_models: Vec<String> = Vec::new();
    let mut model_efforts: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for entry in catalog
        .entries()
        .iter()
        .filter(|entry| usable.contains(entry.provider.as_str()))
    {
        let spec = format!("{}/{}", entry.provider, entry.id);
        let levels = crate::engine_config::effort_levels(
            &entry.provider,
            crate::config::PROVIDERS
                .iter()
                .find(|p| p.id == entry.provider)
                .map(|p| p.dialect)
                .unwrap_or(crate::config::Dialect::OpenaiCompatible),
            entry.supports_reasoning,
        );
        model_efforts.insert(spec.clone(), levels.iter().map(|s| s.to_string()).collect());
        catalog_models.push(spec);
    }
    // `allowed_models` specs are picker entries too — give each its effort
    // vocabulary so the effort row is model-aware under a restriction.
    for raw in engine.allowed_models() {
        if model_efforts.contains_key(raw) {
            continue;
        }
        if let Some(spec) = crate::engine_config::parse_model_spec(raw, &|id| usable.contains(id)) {
            let levels = crate::engine_config::effort_levels_for_spec(&spec.provider, &spec.model);
            model_efforts.insert(raw.clone(), levels.iter().map(|s| s.to_string()).collect());
        }
    }
    let roles = crate::config_wiring::deck_rows(cfg, &providers);
    // What is installed, not what core knows: the seat list is the union of the
    // roles installed plugins declare, so a session with none shows the default
    // model and nothing else (`doc:roleless-core` §8.4).
    let declared = crate::agent::seats::installed_seats(&cfg.workspace_root);
    Inbound::EngineConfig {
        state: crate::engine_config::state_from_settings(
            &engine,
            providers,
            catalog_models,
            model_efforts,
            roles,
            &declared,
        ),
        status,
    }
}

// ── Tool switches (the SETTINGS tab's TOOLS panel) ─────────────────────────

/// Build an [`Inbound::ToolPolicy`] from the session's live tool surface and
/// the settings scope chain.
///
/// `names` is enumerated at the call site because only the driver loop holds
/// the assembled stack: MCP tools appear the moment the background connect
/// lands, and custom tools come from the workspace's manifests. The scope
/// chain is re-read every time (cheap local files) so the panel attributes a
/// switch to the file that carries it *now*, not when the session started.
///
/// The effective posture is re-derived from disk rather than read off
/// [`Config::tool_policy`], which was resolved once at session start: a save
/// has to be visible in the very next snapshot, and the panel is a *settings*
/// editor — it shows what the files say. (The running session keeps the stack
/// it resolved; the status line says so.)
///
/// A scope-read failure is reported as the panel's status rather than dropped:
/// an editor that silently showed "nothing is off" over an unreadable managed
/// file would misstate the posture in the most dangerous direction.
pub(super) fn tool_policy_inbound(
    cfg: &Config,
    names: &[String],
    status: Option<String>,
) -> Inbound {
    let root = &cfg.workspace_root;
    let mut notes: Vec<String> = status.into_iter().collect();
    let mut note_failure = |e: String| notes.push(format!("settings unreadable: {e}"));

    let effective = match crate::settings::Settings::load(root) {
        Ok(settings) => settings.tool_policy(),
        Err(e) => {
            note_failure(e);
            cfg.tool_policy.clone()
        }
    };
    let scopes = match crate::settings::Settings::load_tool_scopes(root) {
        Ok(scopes) => scopes,
        Err(e) => {
            note_failure(e);
            crate::settings::ToolScopePolicies::default()
        }
    };
    Inbound::ToolPolicy {
        state: crate::tool_switches::tool_policy_state(names, &effective, &scopes),
        status: (!notes.is_empty()).then(|| notes.join(" · ")),
    }
}
