//! The `/reload` slash command's engine — re-applying the settings scope
//! chain to a live [`Config`] without restarting the session.
//!
//! Its own module (not more lines in `config.rs`, which sits at the 1500-line
//! ceiling) because it is the one mutation `Config` supports after
//! construction: everything else in `config.rs` is the one-shot startup
//! resolution.

use super::Config;

impl Config {
    /// Re-read the settings scope chain (user + project, managed ceiling
    /// folded in) and re-apply everything [`Config::load_with_settings`]
    /// derives from it *except* provider/model/credential resolution — those
    /// need the full startup chain (interactive prompt included) and a
    /// mid-session change of provider is a much larger step than a config
    /// refresh. This is the `/reload` slash command's engine: same fields,
    /// same precedence (trusted-launcher engine override still wins), just
    /// re-derived from whatever is on disk *now* instead of what it was at
    /// session start.
    ///
    /// Settings unreadable (malformed JSON/TOML) is reported rather than
    /// silently keeping the stale in-memory values — the whole point of a
    /// manual reload is that it either worked or you were told why not.
    ///
    /// **All-or-nothing.** A failure leaves `self` byte-for-byte as it was, so
    /// the reported error and the session's actual posture agree. That is not
    /// a nicety here: the callers
    /// (`command_deck::settings_io::{reload_command, apply_pending_reload}`)
    /// both tell the user a failed reload kept the previous values and a
    /// restart will pick the files up, and a live agent turn reads these
    /// fields — a half-applied reload would run the *next* turn under a tool
    /// policy from disk and an authority from session start, a combination no
    /// scope chain ever produced.
    pub fn reload_from_disk(&mut self) -> Result<(), String> {
        // Phase 1 — derive everything, touching no field of `self`. Every
        // fallible call belongs above the commit block; a new one added below
        // it reintroduces the partial-update bug this split exists to prevent.
        let mut settings = crate::settings::Settings::load(&self.workspace_root)?;
        let trusted_engine = super::trusted_engine_config_override()?;
        let engine_is_trusted = trusted_engine.is_some();
        if let Some(engine) = trusted_engine {
            settings.agent_engine_config = Some(engine);
        }
        let engine_settings = if engine_is_trusted {
            settings.agent_engine_config.clone()
        } else {
            crate::engine_config::engine_with_provider_baseline(
                self.provider.id,
                &settings.agent_engine_config,
            )
        };
        let tool_policy = settings.tool_policy();
        let enable_recap = settings.recap_enabled();
        let trace_capture = settings.trace_capture_enabled();
        let reward_policy = settings.reward_policy()?;
        let create_worktrees = settings.create_worktrees();
        let hooks = if crate::enterprise_telemetry::process_free_authority_active() {
            None
        } else {
            settings.hooks.clone()
        };

        // Phase 2 — commit. Infallible from here down.
        self.engine_settings = engine_settings;
        self.engine_settings_trusted = engine_is_trusted;
        self.authority = settings.authority_policy;
        self.tool_policy = tool_policy;
        self.enable_recap = enable_recap;
        self.trace_capture = trace_capture;
        self.reward_policy = reward_policy;
        self.create_worktrees = create_worktrees;
        self.hooks = hooks;
        Ok(())
    }
}
