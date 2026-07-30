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
    let mut engine = crate::settings::Settings::load(&cfg.workspace_root)
        .ok()
        .and_then(|s| s.agent_engine_config)
        .unwrap_or_default();
    engine.default_model = Some(format!("{}/{}", spec.provider, spec.model));
    // A hand-edited `agents.default.model` outranks the flat `default_model`
    // (settings precedence), so clear it — exactly what the tab's save does.
    if let Some(ags) = engine.agents.as_mut()
        && let Some(a) = ags
            .get_mut(crate::settings::EngineAgentKind::Default)
            .as_mut()
    {
        a.model = None;
    }
    let path = crate::settings::user_config_path().ok_or_else(|| {
        "could not save the default model: the user settings path is unavailable \
         (is $HOME set?)"
            .to_string()
    })?;
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
