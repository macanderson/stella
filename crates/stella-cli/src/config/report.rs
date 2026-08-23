//! `Config`'s own status report: `stella config`'s printed summary and the
//! engine-role wiring line beneath it.
//!
//! Split out of `config.rs` (#3566, #4494) as its own `impl Config` block —
//! the same shape `reload.rs` already uses for the one mutation `Config`
//! supports after construction. This is the complementary "read it back"
//! half: everything here is `&self`, prints, and never changes what a
//! session will do. [`Config::print_models`] delegates to
//! [`Config::print_available_models`] rather than duplicating it, so
//! `/models` and `stella config`'s own table can never drift from
//! `stella models`.

use colored::Colorize as _;

use super::Config;

impl Config {
    /// Print the provider/model table for an interactive session. The listing
    /// depends only on `PROVIDERS` and the ambient environment, never on
    /// `self`, so it delegates to the static [`Config::print_available_models`]
    /// — one renderer backs both the `/models` REPL command and the top-level
    /// `stella models` subcommand, and they can never drift apart. The REPL
    /// call site has no startup `Loaded` record handy, so its source labels
    /// degrade to the generic `env:VAR` form rather than a specific dotenv
    /// filename — see `credential_status::label_for`.
    pub fn print_models(&self) {
        Self::print_available_models(None);
    }

    /// `loaded_env` is the startup dotenv-load record (main's
    /// `env_files::maybe_load` result) — pass `Some` so the API Key line can
    /// name the exact `.env*` file a key came from, and so the "Env files"
    /// line (always shown, unconditionally — unlike `STELLA_ENV_DEBUG`) can
    /// list which files/names were loaded.
    pub fn print_config(&self, loaded_env: Option<&crate::env_files::Loaded>) {
        println!(
            "{}\n",
            "Stella — Current Configuration".bright_cyan().bold()
        );
        println!(
            "  Provider:   {}",
            self.provider.display_name.bright_magenta()
        );
        println!(
            "  Model:      {}/{}",
            self.provider.id.bright_magenta(),
            self.model_id.bright_white()
        );
        let source = self
            .credential_source
            .map(|s| crate::credential_status::label_for(&self.provider, s, loaded_env))
            .unwrap_or_else(|| "n/a (local placeholder)".to_string());
        println!(
            "  API Key:    {} {}",
            self.api_key.redacted_preview().dimmed(),
            format!("({source})").dimmed()
        );
        println!("  Base URL:   {}", self.effective_base_url().dimmed());
        println!("  Workspace:  {}", self.workspace_root.display());
        println!("  Dialect:    {}", self.provider.dialect.label());
        if let Some(summary) = loaded_env.and_then(crate::credential_status::env_files_summary) {
            println!("  Env files:  {}", summary.dimmed());
        }
        self.print_role_wiring();
    }

    /// The four engine roles, what each will actually send, and which setting
    /// decided it.
    ///
    /// Printed unconditionally, including when no engine settings exist — "all
    /// four ride the session model" is an answer, and a block that appears
    /// only sometimes cannot be used to check anything.
    fn print_role_wiring(&self) {
        use crate::config_wiring::{render, resolve};
        let configured = super::discover_configured_providers();
        let is_provider = |id: &str| configured.iter().any(|c| c.config.id == id);
        let session = crate::engine_config::ModelSpec {
            provider: self.provider.id.to_string(),
            model: self.model_id.clone(),
        };
        let wiring = resolve(
            self.engine_settings.as_ref(),
            &session,
            self.model_pinned_by_flag,
            &is_provider,
        );
        println!("\n  {}", "Engine roles".bright_cyan().bold());
        for line in render(&wiring) {
            println!("    {line}");
        }
    }
}
