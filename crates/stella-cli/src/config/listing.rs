//! The provider/model listing renderers — `stella models`, `stella config`'s
//! model table, and the deck's `/models`.
//!
//! A sibling module rather than more lines in `config.rs`: that file is
//! resolution logic (which provider, which credential, which model) and had
//! reached the 1500-line ratchet (`scripts/check-file-size.sh`) with no room
//! left. Rendering a table of what is *available* is a separate concern from
//! resolving what this session will *use*, so it is the seam that splits
//! cleanly — the two halves share only `PROVIDERS`, `effective_builtin`, and
//! `custom_provider`.
//!
//! Both renderers below are deliberately two views of ONE listing: the ANSI
//! [`Config::print_available_models`] writes to stdout for the terminal
//! commands, and [`Config::available_models_plain`] returns a string because
//! the Command Deck runs an alternate screen that stdout printing would
//! corrupt. They must never drift — `/models` in the deck lists exactly what
//! `stella models` does.

use std::env;

use colored::Colorize;
use stella_model::credential::CredentialsFile;

use super::{Config, LOCAL_PROVIDER, PROVIDERS, custom_provider, effective_builtin};

impl Config {
    /// The credentials file to check provider status against, degrading to
    /// an empty in-memory one (rather than aborting the whole listing) on a
    /// read/parse failure — the listing commands must still show the
    /// built-ins even when `~/.stella/credentials.toml` is malformed.
    /// Returns the degradation warning line, if any, alongside the file.
    fn credentials_file_for_listing() -> (CredentialsFile, Option<String>) {
        match CredentialsFile::load_default() {
            Ok(f) => (f, None),
            Err(e) => (
                CredentialsFile::empty(),
                Some(format!("~/.stella/credentials.toml could not be read: {e}")),
            ),
        }
    }

    /// The provider/model table as plain text (no ANSI): the built-in
    /// providers with their key status, then any config-defined providers
    /// from `.stella/settings.json` — the same two sections the ANSI
    /// `print_available_models` renders, so `/models` in the deck lists
    /// exactly what `stella models` does. The Command Deck renders this into
    /// the transcript; stdout printing would corrupt the alternate screen, so
    /// the deck needs a string, not a print. `loaded_env`: see
    /// [`Config::print_config`]'s doc — `None` at call sites that don't have
    /// the startup dotenv record handy (the deck, the REPL fallback).
    pub fn available_models_plain(loaded_env: Option<&crate::env_files::Loaded>) -> String {
        // Surface a settings load/parse failure rather than silently reporting
        // built-in defaults (which would hide a malformed config and wrong key
        // status), then continue with defaults so the listing still renders.
        let (settings, load_error) = match env::current_dir()
            .map_err(|e| e.to_string())
            .and_then(|ws| crate::settings::Settings::load(&ws))
        {
            Ok(s) => (s, None),
            Err(e) => (crate::settings::Settings::default(), Some(e)),
        };
        let (credentials_file, credentials_error) = Self::credentials_file_for_listing();
        let mut lines = vec!["Available providers & models:".to_string()];
        if let Some(e) = &load_error {
            lines.push(format!("  ! settings could not be read: {e}"));
        }
        if let Some(e) = &credentials_error {
            lines.push(format!("  ! {e}"));
        }
        for p in PROVIDERS {
            let p = effective_builtin(p, &settings);
            let settings_key = crate::credential_status::settings_literal_key(&p, &settings);
            let status = crate::credential_status::status_for(
                &p,
                settings_key.as_deref(),
                &credentials_file,
                loaded_env,
            );
            lines.push(format!(
                "  {} {}/{}  {}{}",
                if status.configured { "✓" } else { "✗" },
                p.id,
                p.default_model,
                p.display_name,
                status
                    .source_label
                    .map(|s| format!("  [{s}]"))
                    .unwrap_or_default(),
            ));
        }
        // Config-defined (non-built-in) providers, mirroring the ANSI table.
        let mut printed_header = false;
        for (id, entry) in &settings.providers {
            if PROVIDERS.iter().any(|p| p.id == id.as_str()) || id == LOCAL_PROVIDER.id {
                continue;
            }
            // A malformed provider entry surfaces as a warning line rather than
            // silently vanishing — the ANSI renderer warns here too, and a
            // deck user needs to see that their settings.json is broken.
            let p = match custom_provider(id, entry) {
                Ok(p) => p,
                Err(e) => {
                    lines.push(format!("  ! provider `{id}` is misconfigured: {e}"));
                    continue;
                }
            };
            if !printed_header {
                lines.push("Config-defined providers (settings.json):".to_string());
                printed_header = true;
            }
            let settings_key = entry.api_key.clone();
            let status = crate::credential_status::status_for(
                &p,
                settings_key.as_deref(),
                &credentials_file,
                loaded_env,
            );
            lines.push(format!(
                "  {} {}/{}  {}{}",
                if status.configured { "✓" } else { "✗" },
                p.id,
                if p.default_model.is_empty() {
                    "<model>"
                } else {
                    p.default_model
                },
                p.display_name,
                status
                    .source_label
                    .map(|s| format!("  [{s}]"))
                    .unwrap_or_default(),
            ));
        }
        lines.push("Pin one with --model provider/model_id on the next launch.".to_string());
        lines.join("\n")
    }

    /// Print all available providers/models without needing a resolved
    /// config: the built-in table (with any settings.json overrides
    /// applied), then the config-defined providers. A malformed settings or
    /// credentials file degrades to a warning here — a listing command
    /// should still list the built-ins. `loaded_env`: see
    /// [`Config::print_config`]'s doc.
    pub fn print_available_models(loaded_env: Option<&crate::env_files::Loaded>) {
        let settings = match env::current_dir()
            .map_err(|e| e.to_string())
            .and_then(|ws| crate::settings::Settings::load(&ws))
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  {} {e}", "warning:".yellow());
                crate::settings::Settings::default()
            }
        };
        let (credentials_file, credentials_error) = Self::credentials_file_for_listing();
        if let Some(e) = &credentials_error {
            eprintln!("  {} {e}", "warning:".yellow());
        }
        println!(
            "{}\n",
            "Stella — Available Providers & Models".bright_cyan().bold()
        );
        let key_status = |status: &crate::credential_status::CredentialStatus| {
            if status.configured {
                "✓ configured".green()
            } else {
                "✗ no key".dimmed()
            }
        };
        let source_suffix = |status: &crate::credential_status::CredentialStatus| {
            status
                .source_label
                .as_ref()
                .map(|s| format!(" ({s})").dimmed().to_string())
                .unwrap_or_default()
        };
        for p in PROVIDERS {
            let p = effective_builtin(p, &settings);
            let settings_key = crate::credential_status::settings_literal_key(&p, &settings);
            let status = crate::credential_status::status_for(
                &p,
                settings_key.as_deref(),
                &credentials_file,
                loaded_env,
            );
            println!(
                "  {} {}/{}  {}  [{}]{}",
                key_status(&status),
                p.id.bright_magenta(),
                p.default_model.bright_white(),
                p.display_name,
                p.base_url.dimmed(),
                source_suffix(&status),
            );
        }
        let mut printed_header = false;
        for (id, entry) in &settings.providers {
            if PROVIDERS.iter().any(|p| p.id == id.as_str()) || id == LOCAL_PROVIDER.id {
                continue;
            }
            let p = match custom_provider(id, entry) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("  {} {e}", "warning:".yellow());
                    continue;
                }
            };
            if !printed_header {
                println!("\n  {}", "Config-defined providers (settings.json):".bold());
                printed_header = true;
            }
            let settings_key = entry.api_key.clone();
            let status = crate::credential_status::status_for(
                &p,
                settings_key.as_deref(),
                &credentials_file,
                loaded_env,
            );
            println!(
                "  {} {}/{}  {}  [{}] ({}){}",
                key_status(&status),
                p.id.bright_magenta(),
                if p.default_model.is_empty() {
                    "<model>"
                } else {
                    p.default_model
                }
                .bright_white(),
                p.display_name,
                p.base_url.dimmed(),
                p.dialect.label().dimmed(),
                source_suffix(&status),
            );
        }
        println!("\n  Use --model provider/model_id to pin a specific model.");
        println!("  Example: stella --model zai/glm-5.2 run 'fix the failing test'");
        println!(
            "  Local endpoints (Ollama, vLLM, LM Studio): stella --model local/<model> \
             --base-url http://localhost:11434/v1"
        );
    }
}
