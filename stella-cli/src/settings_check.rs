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
//! surface the problem where it's cheap to fix, not to gate.
//!
//! [`save_role_model`] is the check's recovery companion: the settings
//! write behind the `/model-<role> <spec>` chat commands, validated by the
//! same wire-shape rules and performed with NO model call — when the
//! configured model itself is what's broken, fixing it must not depend on
//! it.

use crate::config::{PROVIDERS, ProviderConfig};
use crate::engine_config::{ModelSpec, model_spec_for, parse_model_spec};
use crate::model_catalog::validate_model_slug;
use crate::settings::{AgentEngineConfig, EngineAgentKind, Settings};
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
        return Some(format!(
            "missing the vendor namespace — `{provider}` model ids carry a \
             `vendor/` prefix, so the wire slug should be e.g. \
             `{provider}/{wire}`, not the bare `{wire}`"
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
        // Only validate an agent's OWN explicit `model` pin here; the flat /
        // `default_model` fallbacks are covered by their own locations above.
        let Some(agent) = engine.agent(kind) else {
            continue;
        };
        let Some(raw) = agent.model.as_deref() else {
            continue;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let location = format!("agents.{}.model", kind_label(kind));
        // Resolve exactly as the engine does — honoring `agent.provider`.
        match model_spec_for(engine, kind, is_provider) {
            Some(spec) => {
                if let Some(issue) = check_resolved_spec(&location, trimmed, &spec) {
                    issues.push(issue);
                }
            }
            // No pinned provider and the string names no known provider /
            // seed slug: fall back to the plain-string diagnostic so a typo'd
            // per-agent slug still surfaces as `unrecognized`.
            None => {
                if let Some(issue) = check_spec(&location, trimmed, is_provider) {
                    issues.push(issue);
                }
            }
        }
    }
    for (i, model) in engine.allowed_models().iter().enumerate() {
        if let Some(issue) = check_spec(&format!("allowed_models[{i}]"), model, is_provider) {
            issues.push(issue);
        }
    }
    issues
}

/// The launch entry point: validate every configured model reference plus
/// the resolved default model. Best-effort — a settings load failure yields
/// no issues here (the config path already surfaced it), and the caller
/// treats the result as advisory warnings, never a launch gate.
pub fn validate_at_launch(cfg: &crate::config::Config) -> Vec<SettingsIssue> {
    let mut issues = Vec::new();
    if let Ok(settings) = crate::settings::Settings::load(&cfg.workspace_root) {
        let ids: Vec<String> = PROVIDERS
            .iter()
            .map(|p| p.id.to_string())
            .chain(std::iter::once(
                crate::config::LOCAL_PROVIDER.id.to_string(),
            ))
            .chain(settings.providers.keys().cloned())
            .collect();
        let is_provider = |id: &str| ids.iter().any(|p| p == id);
        if let Some(engine) = &settings.agent_engine_config {
            issues.extend(check_engine_settings(engine, &is_provider));
        }
    }
    // The effective wire model — deduped against the settings checks so an
    // issue already reported for `default_model` isn't repeated here.
    if let Some(issue) = check_resolved_model(&cfg.provider, &cfg.model_id)
        && !issues.iter().any(|i| i.value == issue.value)
    {
        issues.push(issue);
    }
    issues
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

/// The flat settings key that carries `kind`'s model — where
/// [`save_role_model`] writes and what its status line names.
pub fn flat_key(kind: EngineAgentKind) -> &'static str {
    match kind {
        EngineAgentKind::Default => "default_model",
        EngineAgentKind::Worker => "pipeline_worker_model",
        EngineAgentKind::Judge => "pipeline_judge_model",
        EngineAgentKind::Triage => "pipeline_triage_model",
    }
}

/// Set `kind`'s model to `raw` (`provider/slug`, or a bare slug the seed
/// catalog knows) in the settings file where the write actually takes
/// effect: the project scope when its own engine config already answers
/// `model_for(kind)` — a user-scope write would lose that merge — and the
/// user scope otherwise.
///
/// In the target scope the role's flat key gets `raw`, and the same
/// agent's `model`/`provider` pins are cleared so the qualified spec is
/// the single source of routing for that role there. Returns the status
/// line to show; `Err` means validation or I/O failed and nothing was
/// written.
pub fn save_role_model(
    workspace_root: &std::path::Path,
    kind: EngineAgentKind,
    raw: &str,
) -> Result<String, String> {
    let raw = raw.trim();
    let settings = Settings::load(workspace_root)?;
    let ids: Vec<String> = PROVIDERS
        .iter()
        .map(|p| p.id.to_string())
        .chain(std::iter::once(
            crate::config::LOCAL_PROVIDER.id.to_string(),
        ))
        .chain(settings.providers.keys().cloned())
        .collect();
    let is_provider = |id: &str| ids.iter().any(|p| p == id);
    let location = flat_key(kind);
    if let Some(issue) = check_spec(location, raw, &is_provider) {
        return Err(issue.line());
    }
    let project = crate::settings::project_settings_path(workspace_root);
    let project_only = Settings::load_from(std::slice::from_ref(&project))?;
    let project_wins = project_only
        .agent_engine_config
        .as_ref()
        .is_some_and(|e| e.model_for(kind).is_some());
    let path = if project_wins {
        project
    } else {
        crate::settings::user_settings_path()
            .ok_or_else(|| "cannot determine $HOME for user settings".to_string())?
    };
    let scope_only = Settings::load_from(std::slice::from_ref(&path))?;
    let mut engine = scope_only.agent_engine_config.unwrap_or_default();
    match kind {
        EngineAgentKind::Default => engine.default_model = Some(raw.to_string()),
        EngineAgentKind::Worker => engine.pipeline_worker_model = Some(raw.to_string()),
        EngineAgentKind::Judge => engine.pipeline_judge_model = Some(raw.to_string()),
        EngineAgentKind::Triage => engine.pipeline_triage_model = Some(raw.to_string()),
    }
    if let Some(agents) = engine.agents.as_mut()
        && let Some(agent) = agents.get_mut(kind).as_mut()
    {
        agent.model = None;
        agent.provider = None;
    }
    engine.save_to(&path)?;
    // A model you have no key for saves fine — it just won't serve yet.
    // Credential resolution is non-interactive here, like the launch check.
    let configured = crate::config::discover_configured_providers();
    let note = parse_model_spec(raw, &is_provider)
        .filter(|spec| !configured.iter().any(|c| c.config.id == spec.provider))
        .map(|spec| {
            format!(
                "\nnote: no credential currently resolves for provider `{}` — the \
                 setting is saved, but calls will fail until one is configured",
                spec.provider
            )
        })
        .unwrap_or_default();
    Ok(format!(
        "{location} = `{raw}` saved to {} — applies to runs started from now on{note}",
        path.display()
    ))
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

    #[test]
    fn save_role_model_targets_the_project_scope_that_defines_the_key() {
        let _env = crate::test_env::lock();
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join(".stella");
        std::fs::create_dir_all(&project).unwrap();
        // The broken state the recovery command exists for: an openrouter
        // pin over the TUI-qualified doubled spec, defined at project scope
        // — so a user-scope write would lose the merge.
        std::fs::write(
            project.join("settings.json"),
            r#"{"agent_engine_config": {
                "default_model": "openrouter/openrouter/auto",
                "agents": {"default": {"provider": "openrouter"}}
            }}"#,
        )
        .unwrap();
        let status = save_role_model(dir.path(), EngineAgentKind::Default, "zai/glm-5.2")
            .expect("a seed-known spec must save");
        assert!(status.contains("default_model"), "{status}");
        assert!(
            status.contains(".stella"),
            "the project scope must be the target: {status}"
        );
        let raw = std::fs::read_to_string(project.join("settings.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let engine = &json["agent_engine_config"];
        assert_eq!(engine["default_model"], "zai/glm-5.2");
        // The stale pin is cleared — the flat spec is the single source of
        // routing for the role in this scope after the write.
        assert!(
            engine["agents"]["default"].get("provider").is_none(),
            "{raw}"
        );
    }

    #[test]
    fn save_role_model_rejects_a_mis_shaped_spec_and_writes_nothing() {
        let _env = crate::test_env::lock();
        let dir = tempfile::tempdir().unwrap();
        let err = save_role_model(
            dir.path(),
            EngineAgentKind::Default,
            "openrouter/openrouter/openrouter/auto",
        )
        .expect_err("the over-qualified form must be rejected");
        assert!(err.contains("over-qualified"), "{err}");
        assert!(
            !dir.path().join(".stella").exists(),
            "a rejected spec must write nothing"
        );
    }

    #[test]
    fn save_role_model_falls_back_to_the_user_scope() {
        let _env = crate::test_env::lock();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let saved_home = std::env::var_os("HOME");
        // SAFETY: env lock held for the whole mutate-read-restore window.
        unsafe { std::env::set_var("HOME", &home) };
        let result = save_role_model(dir.path(), EngineAgentKind::Judge, "zai/glm-5.2");
        // Restore BEFORE asserting so a failure can't leak the fake HOME.
        // SAFETY: same lock window as above.
        unsafe {
            match &saved_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
        let status = result.expect("no project opinion → user-scope save");
        assert!(status.contains("pipeline_judge_model"), "{status}");
        let raw = std::fs::read_to_string(home.join(".config/stella/settings.json"))
            .expect("the user settings file is created");
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            json["agent_engine_config"]["pipeline_judge_model"],
            "zai/glm-5.2"
        );
    }
}
