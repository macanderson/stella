//! The `/model` slash command — set the persistent default model from the
//! prompt, at parity with the SETTINGS tab. Kept out of the already-large
//! `command_deck` dispatcher: the parser, the catalog validation, and the
//! settings write live here; `command_deck` only wires them to `say`.

use crate::config::Config;

/// `/model …` — the persistent default-model setter (singular; `/models`
/// is the plural catalog command). A valid model id is one whitespace-free
/// token (`zai/glm-5.2`, `openrouter/openai/gpt-5.5`), so anything else is
/// answered with usage rather than a wasted model call.
pub enum ModelCommand {
    /// `/model <id>` — one token to validate and persist as the default.
    Set(String),
    /// `/model <two or more tokens>` — not a model id.
    Usage,
}

/// Parse `trimmed` as a [`ModelCommand`]; `None` leaves it on the normal
/// path. A bare `/model` (no argument) never reaches here — it has no
/// whitespace to split on and is handled by the exact-match arm.
pub fn parse_model_command(trimmed: &str) -> Option<ModelCommand> {
    let (head, rest) = trimmed.split_once(char::is_whitespace)?;
    if head != "/model" {
        return None;
    }
    let mut words = rest.split_whitespace();
    match (words.next(), words.next()) {
        (Some(id), None) => Some(ModelCommand::Set(id.to_string())),
        // No token (all whitespace) or more than one — not a model id.
        _ => Some(ModelCommand::Usage),
    }
}

/// The bare-`/model` summary: the persisted default (what new sessions use),
/// this session's live model — which a `--model` flag may have diverged —
/// and the pickable model list.
pub fn current_summary(cfg: &Config) -> String {
    let persisted = crate::settings::Settings::load(&cfg.workspace_root)
        .ok()
        .and_then(|s| s.agent_engine_config)
        .and_then(|e| e.default_model)
        .unwrap_or_else(|| "(unset — provider default / --model decides)".to_string());
    format!(
        "default model (new sessions): {persisted}\n\
         this session is running:      {}/{}\n\n\
         set the default with `/model <provider/slug>` (e.g. `/model zai/glm-5.2`):\n\n{}",
        cfg.provider.id,
        cfg.model_id,
        Config::available_models_plain(None),
    )
}

/// The pins this session is configured to run for triage / worker / verifier,
/// for the statline's MODEL cell and the `/models` dialog's standing column.
///
/// The deck otherwise learns a role's pin only from that role's first
/// `AgentEvent::StepUsage`, so before any turn it can say nothing, and a role
/// that is never reached (a verifier on a run with no inconclusive evidence)
/// stays blank for the whole session. Sending this at startup separates "not
/// configured" from "not yet used" — the driver is the only side that can,
/// because settings precedence lives here.
///
/// Marked `served: false`: this is intent, not evidence. The deck reads it as
/// `configured` and replaces it with whatever actually served.
pub fn configured_role_pins(
    cfg: &Config,
) -> Vec<(stella_tui::deck::PipelineRole, stella_tui::deck::RolePin)> {
    use stella_tui::deck::{PipelineRole, RolePin};

    let engine = crate::settings::Settings::load(&cfg.workspace_root)
        .ok()
        .and_then(|s| s.agent_engine_config);

    // `PipelineRole::Worker` alone. Triage and Verifier were pinned here from
    // `pipeline_triage_model`/`pipeline_verifier_model` until #3908 retired
    // both: settings can no longer express a model for either, so publishing a
    // pin for them would be the deck reporting an intent nothing recorded.
    // They now render unconfigured, which is what they are — and what a
    // plugin's declared seat will replace in #3909.
    [PipelineRole::Worker]
        .into_iter()
        .map(|slot| {
            // `model_for` already applies the full precedence chain
            // (`agents.default.model` > `default_model`), so this agrees with what
            // the router will actually pick rather than re-deriving it and
            // drifting.
            let (provider, model) = match engine.as_ref().and_then(|e| e.model_for()) {
                // Split on the FIRST slash only: `openrouter/openai/gpt-5.5` is
                // one provider and a two-segment model, not three segments.
                Some(spec) => match spec.split_once('/') {
                    Some((p, m)) => (p.to_string(), m.to_string()),
                    // A bare slug names no provider, so it resolves through
                    // whichever one this session is on.
                    None => (cfg.provider.id.to_string(), spec.to_string()),
                },
                // No opinion configured — the role rides the session's own pin.
                None => (cfg.provider.id.to_string(), cfg.model_id.to_string()),
            };
            (
                slot,
                RolePin {
                    provider,
                    model,
                    served: false,
                },
            )
        })
        .collect()
}

/// The `/model` argument-menu vocabulary
/// ([`stella_tui::Inbound::ModelCandidates`]): the **active provider's**
/// models as `provider/slug` specs. A configured non-empty `allowed_models`
/// list IS the vocabulary — scoped to the active provider, exactly the
/// narrowing the SETTINGS tab's picker applies (`views::engine`'s
/// `picker_candidates`); without one, the runtime catalog's rows for that
/// provider. Sent at startup and re-sent whenever the answer may have moved
/// (a `/model` set, a catalog refresh, an engine-config save).
pub fn model_candidates(cfg: &Config) -> Vec<String> {
    let provider = cfg.provider.id;
    let allowed: Vec<String> = crate::settings::Settings::load(&cfg.workspace_root)
        .ok()
        .and_then(|s| s.agent_engine_config)
        .and_then(|e| e.allowed_models)
        .unwrap_or_default();
    if !allowed.is_empty() {
        return allowed
            .into_iter()
            .filter(|spec| {
                spec.strip_prefix(provider)
                    .is_some_and(|rest| rest.starts_with('/'))
            })
            .collect();
    }
    stella_model::Catalog::current()
        .entries()
        .iter()
        .filter(|e| e.provider == provider)
        .map(|e| format!("{}/{}", e.provider, e.id))
        .collect()
}

/// [`model_candidates`] as the inbound the deck folds.
pub fn candidates_inbound(cfg: &Config) -> stella_tui::Inbound {
    stella_tui::Inbound::ModelCandidates(model_candidates(cfg))
}

/// Validate `id` against the catalog (exactly as the settings tab's default
/// resolves, via [`crate::engine_config::parse_model_spec`]) and persist it
/// as `default_model` in user-scope settings through the same `save_to` the
/// tab calls. `Ok` = saved, carrying the confirmation to show; `Err` = a
/// message to show without saving.
pub fn set_default_model(cfg: &Config, id: &str) -> Result<String, String> {
    // Recognize any built-in or currently-configured provider as a valid
    // `provider/` prefix (a built-in needs no key yet — the credential is
    // prompted at next launch, same as the tab). Bare slugs resolve through
    // the seed catalog.
    let configured: Vec<String> = crate::config::discover_configured_providers()
        .into_iter()
        .map(|p| p.config.id.to_string())
        .collect();
    let is_provider = |candidate: &str| {
        candidate == cfg.provider.id
            || configured.iter().any(|p| p == candidate)
            || crate::config::PROVIDERS.iter().any(|p| p.id == candidate)
    };
    let Some(spec) = crate::engine_config::parse_model_spec(id, &is_provider) else {
        return Err(format!(
            "model `{id}` not recognized — use `provider/slug` (e.g. `/model zai/glm-5.2`), \
             or run `/models` to list what your configured providers offer"
        ));
    };
    // Validated at SET time, not at first use (#895). Parsing only proves the
    // string has a shape; this proves the provider will serve the WIRE slug —
    // catching `openrouter/auto`, which parses cleanly and then reaches the
    // gateway as the bare `auto` it does not serve. Refused before the write,
    // so a bad default never becomes every future session's 400. Only
    // built-in providers are checked: a settings-defined endpoint is its own
    // authority, the same posture `validate_model_slug` holds. The catalog is
    // already installed — `main` bootstraps it before any path that can reach
    // the deck.
    if let Some(provider_config) = crate::config::PROVIDERS
        .iter()
        .find(|p| p.id == spec.provider)
        && let Some(issue) =
            crate::settings_check::check_resolved_model(provider_config, &spec.model)
    {
        return Err(format!(
            "`{id}` was not saved — {}\n{}",
            issue.message,
            crate::settings_check::model_fix_hints(Some(&spec.provider))
                .iter()
                .map(|hint| format!("  → {hint}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    let path = crate::settings::user_config_path().ok_or_else(|| {
        "could not save the default model: the user settings path is unavailable \
         (is $HOME set?)"
            .to_string()
    })?;
    // Read the file this is about to rewrite, NOT `Settings::load`'s merged
    // view of user + managed + project. `save_to` replaces the whole
    // `agent_engine_config` block, so loading the merge and saving it here
    // would copy the project's and the org's opinions into the user file —
    // turning one repo's verifier pin, or an org-managed effort ceiling, into a
    // machine-wide default that outlives the repo it came from. The same rule
    // `tool_switches::save_switches` already follows.
    let mut engine = crate::settings::user_engine_config_at(&path)
        .map_err(|e| format!("could not load the current engine configuration: {e}"))?;
    engine.default_model = Some(format!("{}/{}", spec.provider, spec.model));
    // A hand-edited `agents.default.model` outranks the flat `default_model`
    // (settings precedence), so clear it — exactly what the tab's save does.
    if let Some(ags) = engine.agents.as_mut()
        && let Some(a) = ags.default.as_mut()
    {
        a.model = None;
    }
    engine
        .save_to(&path)
        .map_err(|e| format!("could not save the default model: {e}"))?;
    Ok(format!(
        "default model set to {}/{} — applies to sessions started from now on \
         (this session keeps {}/{}).",
        spec.provider, spec.model, cfg.provider.id, cfg.model_id
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A workspace with the user home pointed inside it, so the user scope is
    /// isolated from the machine's real `~/.stella`.
    ///
    /// This used to bundle the binary-wide environment lock with a `HOME`
    /// restore guard, in that drop order, so no other test could observe an
    /// unrestored `HOME`. The redirect is per-thread now (#1139): nothing to
    /// serialize, nothing to restore, and the returned guard only has to
    /// outlive the caller's test.
    fn scratch() -> (tempfile::TempDir, crate::paths::TestPathsGuard) {
        let td = tempfile::tempdir().unwrap();
        let home = td.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let guard = crate::paths::test_user_home(home);
        (td, guard)
    }

    fn test_config(workspace_root: PathBuf) -> Config {
        let provider = crate::config::PROVIDERS
            .iter()
            .find(|p| p.id == "anthropic")
            .unwrap()
            .clone();
        Config {
            model_id: provider.default_model.to_string(),
            provider,
            turn_timeout: None,
            max_output_tokens: None,
            plan_mode: false,
            model_pinned_by_flag: false,
            durability: Default::default(),
            output_ceilings: Default::default(),
            create_worktrees: Default::default(),
            allowed_write_dirs: Vec::new(),
            api_key: stella_model::ApiKey::new("dummy-key-unused-offline"),
            workspace_root,
            base_url_override: None,
            hooks: None,
            engine_settings: None,
            engine_settings_trusted: false,
            tool_policy: Default::default(),
            ignore_gitignore: true,
            reward_policy: crate::reward::RewardPolicy::default(),
            authority: crate::settings::AuthorityPolicy::default(),
            credential_source: None,
            credential_advisories: Vec::new(),
            aux_credentials: Default::default(),
            cache_ttl: None,
        }
    }

    /// **The witness for the `/model` argument menu's vocabulary.** Without
    /// an `allowed_models` list the candidates are the catalog's rows for
    /// the ACTIVE provider alone; with one configured, the list itself is
    /// the vocabulary, still scoped to the active provider — the same
    /// narrowing the SETTINGS tab's picker applies.
    #[test]
    fn model_candidates_scope_to_the_provider_and_honor_the_allowlist() {
        let (td, _guard) = scratch();
        let workspace = td.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();
        let cfg = test_config(workspace.clone());

        let open = model_candidates(&cfg);
        assert!(!open.is_empty(), "the seed catalog has anthropic rows");
        assert!(
            open.iter().all(|spec| spec.starts_with("anthropic/")),
            "only the active provider's models are offered: {open:?}"
        );

        std::fs::create_dir_all(workspace.join(".stella")).unwrap();
        std::fs::write(
            workspace.join(".stella/settings.json"),
            r#"{"agent_engine_config": {"allowed_models":
                ["anthropic/claude-opus-5", "zai/glm-5.2"]}}"#,
        )
        .unwrap();
        assert_eq!(
            model_candidates(&cfg),
            vec!["anthropic/claude-opus-5".to_string()],
            "a configured allowlist IS the vocabulary, scoped to the provider"
        );
    }

    #[test]
    fn setting_the_default_model_does_not_copy_project_settings_into_user_scope() {
        // `save_to` replaces the whole `agent_engine_config` block, so reading
        // `Settings::load`'s MERGED view here would write this repo's verifier pin
        // into `~/.stella` — promoting one project's choice to a machine-wide
        // default that outlives the repo it came from.
        let (td, _guard) = scratch();
        let workspace = td.path().join("repo");
        std::fs::create_dir_all(workspace.join(".stella")).unwrap();
        std::fs::write(
            workspace.join(".stella/settings.json"),
            r#"{"agent_engine_config": {"pipeline_verifier_model": "openai/gpt-5.5"}}"#,
        )
        .unwrap();

        let cfg = test_config(workspace);
        set_default_model(&cfg, "zai/glm-5.2").expect("the default model should save");

        let user_file = td.path().join("home/.stella/settings.json");
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&user_file).unwrap()).unwrap();
        let engine = raw
            .get("agent_engine_config")
            .expect("the block was written");
        assert_eq!(
            engine.get("default_model").and_then(|v| v.as_str()),
            Some("zai/glm-5.2"),
            "the edit itself must land"
        );
        assert!(
            engine.get("pipeline_verifier_model").is_none(),
            "the project's verifier pin leaked into user scope: {engine}"
        );
    }

    #[test]
    fn setting_the_default_model_preserves_the_rest_of_user_scope() {
        // Reading the target file is not licence to replace it: everything the
        // user file already said must survive the edit.
        let (td, _guard) = scratch();
        let user_dir = td.path().join("home/.stella");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(
            user_dir.join("settings.json"),
            r#"{"enable_recap": "on", "agent_engine_config": {
                 "compaction_budget_tokens": 90000,
                 "pipeline_triage_model": "deepseek/deepseek-chat",
                 "agents": {"default": {"prompt": "You are a strict reviewer."}}
               }}"#,
        )
        .unwrap();

        let cfg = test_config(td.path().join("repo"));
        set_default_model(&cfg, "zai/glm-5.2").expect("the default model should save");

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(user_dir.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(
            raw.get("enable_recap").and_then(|v| v.as_str()),
            Some("on"),
            "a sibling key outside the engine block must survive"
        );
        let engine = raw.get("agent_engine_config").unwrap();
        assert_eq!(
            engine
                .get("compaction_budget_tokens")
                .and_then(|v| v.as_u64()),
            Some(90000),
            "a live engine key the edit never mentioned must survive"
        );
        assert_eq!(
            engine
                .pointer("/agents/default/prompt")
                .and_then(|v| v.as_str()),
            Some("You are a strict reviewer.")
        );
        // A retired key is the one thing the save does NOT carry forward, and
        // that is deliberate: `save_to` re-serializes the typed block, which no
        // longer has a field to hold it. The operator was told by name that it
        // reads nothing (`settings::unknown`); writing it back would keep the
        // file asserting a capability the engine does not have.
        assert!(
            engine.get("pipeline_triage_model").is_none(),
            "a retired key must not be re-written on save: {engine}"
        );
        assert_eq!(
            engine.get("default_model").and_then(|v| v.as_str()),
            Some("zai/glm-5.2")
        );
    }

    /// #895: validated at SET time, not at first use. `openrouter/auto`
    /// parses cleanly — the prefix names a real provider — and then reaches
    /// the gateway as the bare `auto`, which it does not serve. Saving it
    /// would break every session started from then on, so the write must not
    /// happen at all, and the refusal must carry the way out.
    #[test]
    fn a_slug_the_catalog_rejects_is_refused_before_it_is_written() {
        let (td, _guard) = scratch();
        let cfg = test_config(td.path().join("repo"));

        let error = set_default_model(&cfg, "openrouter/auto")
            .expect_err("a wire slug the provider cannot serve must not be persisted");
        assert!(
            error.contains("vendor namespace"),
            "the refusal says what is wrong with the slug: {error}"
        );
        for remedy in [
            "stella models list --provider openrouter",
            "stella models refresh",
            "SETTINGS tab",
        ] {
            assert!(
                error.contains(remedy),
                "`{remedy}` must be offered — a refusal with no way forward is \
                 worse than the 400 it replaces: {error}"
            );
        }
        assert!(
            !td.path().join("home/.stella/settings.json").exists(),
            "a refused set must leave the user's settings untouched"
        );
    }

    #[test]
    fn a_valid_slug_still_saves() {
        // The guard above must not have made the ordinary path unreachable:
        // `openrouter/openai/gpt-5.5` keeps its vendor namespace on the wire
        // and is exactly the qualified form users are told to type.
        let (td, _guard) = scratch();
        let cfg = test_config(td.path().join("repo"));
        set_default_model(&cfg, "openrouter/openai/gpt-5.5").expect("a well-formed slug saves");
        let raw: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(td.path().join("home/.stella/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            raw.pointer("/agent_engine_config/default_model")
                .and_then(|v| v.as_str()),
            Some("openrouter/openai/gpt-5.5")
        );
    }

    #[test]
    fn parse_model_command_takes_one_id_and_separates_from_models() {
        // A single whitespace-free id (including nested OpenRouter slugs and
        // bare catalog slugs) is a Set; the value is validated downstream.
        assert!(matches!(
            parse_model_command("/model zai/glm-5.2"),
            Some(ModelCommand::Set(id)) if id == "zai/glm-5.2"
        ));
        assert!(matches!(
            parse_model_command("/model openrouter/openai/gpt-5.5"),
            Some(ModelCommand::Set(id)) if id == "openrouter/openai/gpt-5.5"
        ));
        assert!(matches!(
            parse_model_command("/model glm-5.2"),
            Some(ModelCommand::Set(id)) if id == "glm-5.2"
        ));
        // More than one token is not a model id → usage, never a model call.
        assert!(matches!(
            parse_model_command("/model what should I use"),
            Some(ModelCommand::Usage)
        ));
        // Bare `/model` has no whitespace to split — the exact-match arm owns it.
        assert!(parse_model_command("/model").is_none());
        // `/model` must not swallow the plural `/models`, nor the removed
        // `/model-<role>` heads.
        assert!(parse_model_command("/models refresh").is_none());
        assert!(parse_model_command("/model-default zai/glm-5.2").is_none());
    }
}
