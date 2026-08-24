//! Session-scoped overrides: the `/model` picker's model switch and the
//! `/agent` picker's assume, applied to THIS session only — settings files
//! are never written here, so every future session keeps the configured
//! default. Kept out of `command_deck.rs` (a god file closed to growth): the
//! driver loop's arms delegate to [`apply_model_override`] /
//! [`assume_agent`] and stay a few lines each.
//!
//! A switch is atomic: everything is resolved and the new adapter is built
//! against a **candidate** [`Config`] clone, and the session's `Config` is
//! replaced only once the whole chain succeeded — a refused or failing
//! switch leaves the running wiring untouched.
//!
//! The system prompt is rewritten between turns, exactly as
//! `WorkspaceInput::AgentAssume` always did (the prompt prefix is
//! byte-stable *across* a turn — AGENTS.md's rule #7 — so `messages[0]`
//! may move only at a turn boundary). The
//! SessionStart hook context is NOT re-run for the rewrite: the caller hands
//! in the suffix captured at startup, so a hook's side effects happen once
//! per session no matter how often the model changes.

use tokio::sync::mpsc::UnboundedSender;

use stella_model::provider::Provider;
use stella_protocol::CompletionMessage;
use stella_tui::Inbound;
use stella_tui::deck::{PipelineRole, RolePin};
use stella_tui::envelope::AgentScope;

use crate::config::Config;

/// A validated, built model switch: the adapter to install and the line to
/// show. Produced by [`switch_session_model`] after `cfg` was updated.
pub(super) struct SessionModelSwitch {
    pub(super) provider: Box<dyn Provider>,
    pub(super) notice: String,
}

/// The prompt plane the driver loop owns — handed in as one bundle because a
/// switch must move all three together or not at all.
pub(super) struct PromptPlane<'a> {
    /// The persona-free prompt an assumed agent's block is appended to.
    pub(super) base_system_prompt: &'a mut String,
    /// The full prompt (base + any assumed persona).
    pub(super) system_prompt: &'a mut String,
    /// The live conversation; `messages[0]` is the seeded system message.
    pub(super) messages: &'a mut Vec<CompletionMessage>,
}

/// Resolve `id` (`provider/slug`, or a bare catalog slug on the session's
/// provider) into a session-only model switch: validate it, enforce the
/// `[models].allowed` list when one is configured, resolve the target
/// provider's credential non-interactively, build the adapter, and — only
/// then — commit the change onto `cfg`. No settings file is touched.
pub(super) fn switch_session_model(
    cfg: &mut Config,
    id: &str,
) -> Result<SessionModelSwitch, String> {
    if cfg.engine_settings_trusted {
        return Err(
            "managed engine settings pin this session's routing — the session model \
             override is disabled under a trusted launcher"
                .to_string(),
        );
    }
    let configured = crate::config::discover_configured_providers();
    // Any known provider id parses — so an uncredentialed built-in is refused
    // below by name ("no credential resolves for…"), never as a typo.
    let is_provider = |candidate: &str| {
        candidate == cfg.provider.id
            || configured.iter().any(|p| p.config.id == candidate)
            || crate::config::PROVIDERS.iter().any(|p| p.id == candidate)
    };
    let Some(spec) = crate::engine_config::parse_model_spec(id, &is_provider) else {
        return Err(format!(
            "model `{id}` not recognized — use `provider/slug` (e.g. `/model zai/glm-5.2`); \
             `/model` alone lists what your configured providers offer"
        ));
    };
    // The `[models].allowed` list scopes the picker's vocabulary, and the
    // typed form must answer to the same list or the restriction is only as
    // strong as the user's memory of it. Entries match as the full
    // `provider/slug` spec or as the raw string the setting carries.
    let allowed = crate::settings::Settings::load(&cfg.workspace_root)
        .ok()
        .and_then(|s| s.agent_engine_config)
        .map(|e| e.allowed_models().to_vec())
        .unwrap_or_default();
    let full_spec = format!("{}/{}", spec.provider, spec.model);
    if !allowed.is_empty() && !allowed.iter().any(|a| a == &full_spec || a == id) {
        return Err(format!(
            "`{full_spec}` is not in this workspace's allowed model list \
             (`[models].allowed` / `agent_engine_config.allowed_models`) — \
             allowed: {}",
            allowed.join(", ")
        ));
    }
    // Validated at switch time, same posture as `/model default` (#895): the
    // catalog check proves the provider will serve the wire slug, so the
    // refusal happens here instead of as the next turn's 400. Built-ins
    // only — a settings-defined endpoint is its own authority.
    if let Some(provider_config) = crate::config::PROVIDERS
        .iter()
        .find(|p| p.id == spec.provider)
        && let Some(issue) =
            crate::settings_check::check_resolved_model(provider_config, &spec.model)
    {
        return Err(format!("`{id}` was not applied — {}", issue.message));
    }

    // Build the whole switch on a candidate, then commit. A failure below
    // must leave the running session exactly as it was.
    let mut candidate = cfg.clone();
    if spec.provider != cfg.provider.id {
        if cfg.base_url_override.is_some() {
            return Err(format!(
                "--base-url pins this session to {} — a cross-provider switch would \
                 silently drop the endpoint override; restart to change providers",
                cfg.provider.id
            ));
        }
        let target = configured
            .into_iter()
            .find(|p| p.config.id == spec.provider)
            .ok_or_else(|| {
                format!(
                    "no credential resolves for `{}` — configure it (env var or \
                     ~/.stella/credentials.toml) and pick again",
                    spec.provider
                )
            })?;
        candidate.provider = target.config;
        candidate.api_key = target.api_key;
        candidate.aux_credentials = target.aux;
        // The startup label described the OLD provider's credential; a stale
        // one is a lie, an absent one degrades to the generic form.
        candidate.credential_source = None;
    }
    candidate.model_id = spec.model.clone();
    // The override is the session's explicit pin from here on — engine
    // wiring must not re-route the worker back to the settings default.
    candidate.model_pinned_by_flag = true;
    let provider = crate::agent::build_provider(&candidate)?;
    *cfg = candidate;
    Ok(SessionModelSwitch {
        provider,
        notice: format!(
            "session model → {full_spec} — this session only; new sessions keep the \
             settings default"
        ),
    })
}

/// The statline's worker pin after an override: the session's own wiring,
/// never the settings default `configured_role_pins` would re-derive.
pub(super) fn override_role_pins(cfg: &Config) -> Vec<(PipelineRole, RolePin)> {
    vec![(
        PipelineRole::Worker,
        RolePin {
            provider: cfg.provider.id.to_string(),
            model: cfg.model_id.clone(),
            served: false,
        },
    )]
}

/// Rebuild the prompt plane from the current `cfg`: the base prompt (with the
/// startup hook suffix re-appended, never re-run), the full prompt (persona
/// re-appended when one is assumed), and the seeded system message.
pub(super) fn refresh_prompts(
    cfg: &Config,
    prompts: &mut PromptPlane<'_>,
    hook_suffix: &str,
    persona: Option<&str>,
    pipeline_persona: bool,
    active_rules: &crate::rules::ResolvedRules,
) {
    let mut base = if pipeline_persona {
        // Same shape as the session-start build: no model line rather than a
        // possibly-false one — the pipeline's worker may be re-routed.
        crate::agent::build_pipeline_system_prompt(cfg, &cfg.workspace_root, active_rules, None)
    } else {
        crate::agent::build_system_prompt(cfg, &cfg.workspace_root, active_rules)
    };
    base.push_str(hook_suffix);
    *prompts.system_prompt = match persona {
        Some(persona) => format!("{base}\n\n{persona}"),
        None => base.clone(),
    };
    *prompts.base_system_prompt = base;
    if let Some(first) = prompts.messages.first_mut()
        && first.role == stella_protocol::MessageRole::System
    {
        first.content = prompts.system_prompt.clone();
    }
}

/// Apply `/model <id>` (typed or picked) to the running session. On success
/// the provider handle, the prompt plane, the deck's header meta and the
/// statline pin all move together; on refusal one chrome note says why and
/// nothing moves.
#[allow(clippy::too_many_arguments)] // the driver loop's owned state, threaded — a struct of nine
// borrows would outline the same list with lifetimes on top
pub(super) fn apply_model_override(
    id: &str,
    cfg: &mut Config,
    provider: &mut Box<dyn Provider>,
    mut prompts: PromptPlane<'_>,
    hook_suffix: &str,
    persona: Option<&str>,
    pipeline_persona: bool,
    active_rules: &crate::rules::ResolvedRules,
    lead_meta: &mut stella_tui::AgentMeta,
    in_tx: &UnboundedSender<Inbound>,
) {
    match switch_session_model(cfg, id) {
        Ok(switch) => {
            *provider = switch.provider;
            refresh_prompts(
                cfg,
                &mut prompts,
                hook_suffix,
                persona,
                pipeline_persona,
                active_rules,
            );
            announce_switch(cfg, lead_meta, in_tx, switch.notice);
        }
        Err(error) => {
            let _ = in_tx.send(super::chrome_note(error));
        }
    }
}

/// Push the deck-facing consequences of a committed switch: the header meta,
/// the statline's worker pin, a fresh SETTINGS snapshot, and the notice.
fn announce_switch(
    cfg: &Config,
    lead_meta: &mut stella_tui::AgentMeta,
    in_tx: &UnboundedSender<Inbound>,
    notice: String,
) {
    lead_meta.model = Some(format!("{}/{}", cfg.provider.id, cfg.model_id));
    let _ = in_tx.send(Inbound::Register(lead_meta.clone()));
    // The reset envelope, not `ConfiguredRoles`: the worker pin must replace
    // the served evidence of the model that no longer serves.
    let _ = in_tx.send(Inbound::RolePinsReset(override_role_pins(cfg)));
    let _ = in_tx.send(super::engine_config_inbound(cfg, None));
    // Empty when the caller folds its own line (an assumed agent's declared
    // model rides the assume note instead of a second one).
    if !notice.is_empty() {
        let _ = in_tx.send(super::chrome_note(notice));
    }
}

/// The session tool policy while an agent definition is assumed: the
/// session's base policy narrowed to the definition's `tools:` grant. A
/// definition with no grant restricts nothing. Always derived from the base
/// policy — never from the previous agent's — so re-assuming can only ever
/// swap one scope for another, not compound them.
pub(super) fn agent_scope_policy(
    base: &stella_tools::policy::ToolPolicy,
    tools: Option<&[String]>,
) -> stella_tools::policy::ToolPolicy {
    let mut policy = base.clone();
    if let Some(tools) = tools {
        policy.narrow_with(&stella_tools::skill_grant::grant_policy(tools));
    }
    policy
}

/// Apply `/agent` (or the AGENTS pane's `a`): the lead assumes `name` for
/// the rest of the session. The persona joins the system prompt (as before),
/// and the definition's declared scopes are now enforced rather than prose:
/// its `tools:` grant narrows the session tool policy, and its `model:` (if
/// declared) runs the same session-only switch `/model` does. A model that
/// cannot be applied degrades softly — the persona and toolbelt still land,
/// and the note says what did not.
#[allow(clippy::too_many_arguments)] // same driver-loop state bundle as `apply_model_override`
pub(super) fn assume_agent(
    name: &str,
    scope: AgentScope,
    cfg: &mut Config,
    provider: &mut Box<dyn Provider>,
    mut prompts: PromptPlane<'_>,
    hook_suffix: &str,
    assumed_persona: &mut Option<String>,
    pipeline_persona: bool,
    active_rules: &crate::rules::ResolvedRules,
    base_tool_policy: &stella_tools::policy::ToolPolicy,
    lead_meta: &mut stella_tui::AgentMeta,
    in_tx: &UnboundedSender<Inbound>,
) {
    let agent = match super::authoring::assumed_agent(&cfg.workspace_root, name, scope) {
        Ok(agent) => agent,
        Err(error) => {
            let _ = in_tx.send(Inbound::AgentAssumed { name: None });
            let _ = in_tx.send(super::chrome_note(format!("cannot assume {name}: {error}")));
            return;
        }
    };
    cfg.tool_policy = agent_scope_policy(base_tool_policy, agent.tools.as_deref());
    let mut notes = vec![format!("the lead is now {name} — from the next turn on")];
    if let Some(tools) = &agent.tools {
        notes.push(format!("toolbelt scoped to: {}", tools.join(", ")));
    }
    if let Some(model) = &agent.model {
        match switch_session_model(cfg, model) {
            Ok(switch) => {
                *provider = switch.provider;
                announce_switch(cfg, lead_meta, in_tx, String::new());
                notes.push(format!("model → {}/{}", cfg.provider.id, cfg.model_id));
            }
            Err(error) => notes.push(format!("declared model `{model}` not applied — {error}")),
        }
    }
    *assumed_persona = Some(agent.persona);
    refresh_prompts(
        cfg,
        &mut prompts,
        hook_suffix,
        assumed_persona.as_deref(),
        pipeline_persona,
        active_rules,
    );
    let _ = in_tx.send(Inbound::AgentAssumed {
        name: Some(name.to_string()),
    });
    let _ = in_tx.send(super::chrome_note(
        notes
            .into_iter()
            .filter(|n| !n.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
            minimal_prompt: false,
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
            plan_review: crate::settings::PlanReviewPolicy::default(),
            authority: crate::settings::AuthorityPolicy::default(),
            credential_source: None,
            credential_advisories: Vec::new(),
            aux_credentials: Default::default(),
            cache_ttl: None,
        }
    }

    /// **The witness for the session override.** Switching within the
    /// session's own provider changes `cfg` and builds an adapter — and
    /// writes NOTHING to disk: the user settings file must not exist
    /// afterwards, which is exactly what separates this from
    /// `/model default`.
    #[test]
    fn a_same_provider_switch_is_session_only() {
        let (td, _guard) = scratch();
        let mut cfg = test_config(td.path().join("repo"));
        let before = cfg.model_id.clone();

        let switch = switch_session_model(&mut cfg, "anthropic/claude-sonnet-5")
            .expect("a catalog model on the session's own provider must apply");
        assert_eq!(cfg.model_id, "claude-sonnet-5");
        assert_ne!(cfg.model_id, before);
        assert!(
            cfg.model_pinned_by_flag,
            "the override is the session's pin"
        );
        assert!(switch.notice.contains("this session only"));
        assert!(
            !td.path().join("home/.stella/settings.json").exists(),
            "a session override must never write settings"
        );
    }

    /// Every provider env var, for the sandbox below: a machine with a real
    /// `ZAI_API_KEY` exported must not turn the no-credential refusal into a
    /// successful switch.
    fn provider_env_names() -> Vec<&'static str> {
        let mut names: Vec<&'static str> = Vec::new();
        for p in crate::config::PROVIDERS {
            names.push(p.env_var);
            names.extend_from_slice(p.env_var_aliases);
        }
        names
    }

    /// A provider with no resolvable credential is refused with the way
    /// forward, and the refusal leaves `cfg` untouched.
    #[test]
    fn a_cross_provider_switch_needs_a_credential() {
        let _env = crate::test_env::lock();
        let (td, _guard) = scratch();
        let home = td.path().join("home");
        let _restore = crate::test_env::EnvRestore::capture(&crate::test_env::home_env_names(
            &provider_env_names(),
        ));
        // SAFETY: serialized behind the binary-wide env lock; `_restore`
        // outlives every read below.
        unsafe {
            crate::test_env::point_home_at(&home);
            for name in provider_env_names() {
                std::env::remove_var(name);
            }
        }
        let mut cfg = test_config(td.path().join("repo"));
        let before_model = cfg.model_id.clone();

        // `zai` resolves no key in this sandbox, so discovery cannot offer it
        // and the switch must refuse rather than build a dead adapter.
        let Err(error) = switch_session_model(&mut cfg, "zai/glm-5.2") else {
            panic!("an uncredentialed provider must refuse");
        };
        assert!(
            error.contains("no credential resolves for `zai`"),
            "{error}"
        );
        assert_eq!(cfg.provider.id, "anthropic", "a refusal must not move cfg");
        assert_eq!(cfg.model_id, before_model);
    }

    /// The `[models].allowed` restriction binds the typed form exactly as it
    /// scopes the picker: an off-list spec is refused by name, an on-list
    /// spec passes.
    #[test]
    fn the_allowed_model_list_binds_the_override() {
        let (td, _guard) = scratch();
        let workspace = td.path().join("repo");
        std::fs::create_dir_all(workspace.join(".stella")).unwrap();
        std::fs::write(
            workspace.join(".stella/settings.json"),
            r#"{"agent_engine_config": {"allowed_models": ["anthropic/claude-opus-5"]}}"#,
        )
        .unwrap();
        let mut cfg = test_config(workspace);

        let Err(error) = switch_session_model(&mut cfg, "anthropic/claude-sonnet-5") else {
            panic!("an off-list model must be refused");
        };
        assert!(error.contains("allowed model list"), "{error}");
        assert!(error.contains("anthropic/claude-opus-5"), "{error}");

        switch_session_model(&mut cfg, "anthropic/claude-opus-5")
            .expect("the allowed spec must pass");
        assert_eq!(cfg.model_id, "claude-opus-5");
    }

    /// Managed (trusted-launcher) engine settings own the routing; the
    /// override refuses rather than quietly out-ranking them.
    #[test]
    fn a_trusted_launcher_pins_the_session() {
        let (td, _guard) = scratch();
        let mut cfg = test_config(td.path().join("repo"));
        cfg.engine_settings_trusted = true;
        let Err(error) = switch_session_model(&mut cfg, "anthropic/claude-opus-5") else {
            panic!("trusted settings must refuse the override");
        };
        assert!(error.contains("trusted launcher"), "{error}");
    }

    /// The toolbelt scope always derives from the session's base policy:
    /// re-assuming swaps scopes instead of compounding them, and a
    /// grant-free definition restricts nothing.
    #[test]
    fn agent_tool_scopes_swap_and_never_compound() {
        let base = stella_tools::policy::ToolPolicy::allow_all();

        let scoped = agent_scope_policy(&base, Some(&["read_file".to_string()]));
        assert!(scoped.allows("read_file"));
        assert!(!scoped.allows("bash"), "outside the grant is withheld");

        // Second assume, different grant, derived from BASE — `bash` comes
        // back even though the previous scope denied it.
        let swapped = agent_scope_policy(&base, Some(&["bash".to_string()]));
        assert!(swapped.allows("bash"));
        assert!(!swapped.allows("read_file"));

        let unrestricted = agent_scope_policy(&base, None);
        assert!(unrestricted.allows("bash") && unrestricted.allows("read_file"));
    }
}
