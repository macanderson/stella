//! `stella auth` — manage BYOK provider keys stored in
//! `~/.stella/credentials.toml`. A small, flat store for a handful of
//! keys, not a config language (`CredentialsFile`'s own doc intent) —
//! `set`/`remove`/`list` is the whole surface. Every command here masks the
//! secret value in its own output; the only place a plaintext key is ever
//! written is the 0600 credentials file itself.
//!
//! Every subcommand splits into a pure core (operates on an already-loaded
//! `&mut CredentialsFile`, returns the lines to print — never touches the
//! real filesystem) and a thin `run_*` wrapper that loads/saves the real
//! `~/.stella/credentials.toml`. This is what lets the core be unit
//! tested against a temp-path `CredentialsFile` instead of ever touching a
//! developer's actual credentials file.
//!
//! "One key per provider" holds for every provider but Bedrock, which needs a
//! secret access key and a region beside its access key id. `set` stores those
//! companions too — see `config::aux` — because the alternative is
//! what shipped before: `stella auth set bedrock` reported success, and the
//! next run failed on a value the command had no way to accept.

use colored::Colorize as _;
use stella_model::credential::{ApiKey, CredentialsFile};
use zeroize::Zeroizing;

use crate::config::{AuxField, ProviderConfig, settable_aux_fields};
use crate::credential_status;
use crate::settings::Settings;

/// Entry point for `stella auth <cmd>`.
pub fn run(cmd: &crate::AuthCmd) -> Result<(), String> {
    match cmd {
        crate::AuthCmd::Set {
            provider,
            key,
            stdin,
            fields,
        } => run_set(provider, key.as_deref(), *stdin, fields),
        crate::AuthCmd::Remove { provider } => run_remove(provider),
        crate::AuthCmd::List => run_list(),
    }
}

fn credentials_path_display() -> String {
    CredentialsFile::default_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.stella/credentials.toml".to_string())
}

fn load_file() -> Result<CredentialsFile, String> {
    CredentialsFile::load_default()
        .map_err(|e| format!("could not read {}: {e}", credentials_path_display()))
}

/// Reject ids that would corrupt lookups elsewhere — `config::custom_provider`
/// enforces the same rule for settings.json-defined providers. Otherwise
/// permissive about WHICH id: a key can legitimately be stored ahead of the
/// provider being declared in settings.json, or under one of the built-ins.
fn validate_provider_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.contains('/') || id.chars().any(char::is_whitespace) {
        return Err(format!(
            "`{id}` is not a valid provider id (no slashes or whitespace)"
        ));
    }
    Ok(())
}

/// The key value for `set`, in priority order: `--key` (warned), `--stdin`
/// (one line, trimmed), or an interactive masked prompt (rpassword) —
/// mirroring `stella_model::credential`'s own interactive-prompt idiom and
/// `connect_cmd`'s `prompt_secret`.
fn read_key_value(provider: &str, key: Option<&str>, use_stdin: bool) -> Result<String, String> {
    if let Some(k) = key {
        eprintln!(
            "  {} --key exposes the credential in shell history and `ps` output — prefer \
             `stella auth set {provider} --stdin` or the interactive masked prompt (omit \
             both --key and --stdin)",
            "⚠".yellow()
        );
        if k.is_empty() {
            return Err("--key value is empty".to_string());
        }
        return Ok(k.to_string());
    }
    // Both remaining paths build a buffer the caller never sees — only the
    // trimmed value below escapes — so the original is wiped on drop rather
    // than left legible in freed heap for the life of the process. Same
    // treatment `stella_model::credential::prompt_for_key` gives its own.
    if use_stdin {
        let mut buf = Zeroizing::new(String::new());
        std::io::stdin()
            .read_line(&mut buf)
            .map_err(|e| format!("could not read key from stdin: {e}"))?;
        let trimmed = buf.trim().to_string();
        if trimmed.is_empty() {
            return Err("empty key read from stdin".to_string());
        }
        return Ok(trimmed);
    }
    let value = Zeroizing::new(
        rpassword::prompt_password(format!("  API key for `{provider}`: "))
            .map_err(|e| format!("cannot read secret from terminal: {e}"))?,
    );
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() {
        return Err("empty input — no credential entered".to_string());
    }
    Ok(trimmed)
}

/// Parse the repeated `--field NAME=VALUE` flags against the set this
/// provider actually reads.
///
/// A name the chain never looks up is rejected rather than stored: a value
/// written under `AWS_SECRET_KEY` would sit in the file looking configured
/// while every resolution step misses it, which is a worse outcome than the
/// typo being named at the point of the typo. The error lists the accepted
/// names, since for a provider with none the whole flag is a mistake.
fn parse_field_flags(
    provider: &str,
    accepted: &[AuxField],
    fields: &[String],
) -> Result<Vec<(&'static str, String)>, String> {
    let mut parsed: Vec<(&'static str, String)> = Vec::new();
    for raw in fields {
        let (name, value) = raw
            .split_once('=')
            .ok_or_else(|| format!("--field must be NAME=VALUE, got `{raw}`"))?;
        let name = name.trim();
        let Some(field) = accepted.iter().find(|f| f.name == name) else {
            return Err(if accepted.is_empty() {
                format!("provider `{provider}` has no companion values to store")
            } else {
                format!(
                    "`{name}` is not a companion value for `{provider}` — accepted: {}",
                    accepted
                        .iter()
                        .map(|f| f.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
        };
        if value.is_empty() {
            return Err(format!("--field {name} has an empty value"));
        }
        if parsed.iter().any(|(stored, _)| *stored == field.name) {
            return Err(format!("--field {name} was given more than once"));
        }
        parsed.push((field.name, value.to_string()));
    }
    Ok(parsed)
}

/// Read the companion values not supplied by `--field`, via a per-value
/// prompt. Secrets are masked; the region is not (it is routing, and a masked
/// prompt would imply a secrecy it does not have). Blank input means "leave
/// whatever is already stored", so re-running `set` to rotate only the key
/// does not silently clear a region.
fn prompt_for_fields(
    file: &CredentialsFile,
    provider: &str,
    accepted: &[AuxField],
    already_given: &[(&'static str, String)],
) -> Result<Vec<(&'static str, String)>, String> {
    let mut collected = Vec::new();
    for field in fields_still_unanswered(accepted, already_given) {
        let stored = file.field(provider, field.name);
        let suffix = if stored.is_some() {
            " [stored — blank keeps it]"
        } else {
            ""
        };
        let label = format!("  {}{suffix}: ", field.prompt);
        let entered = if field.secret {
            Zeroizing::new(
                rpassword::prompt_password(&label)
                    .map_err(|e| format!("cannot read secret from terminal: {e}"))?,
            )
        } else {
            use std::io::Write as _;
            print!("{label}");
            std::io::stdout()
                .flush()
                .map_err(|e| format!("cannot write the prompt: {e}"))?;
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|e| format!("cannot read from stdin: {e}"))?;
            Zeroizing::new(line)
        };
        let trimmed = entered.trim();
        if !trimmed.is_empty() {
            collected.push((field.name, trimmed.to_string()));
        }
    }
    Ok(collected)
}

/// Which companion values still need asking about — the pure half of
/// [`prompt_for_fields`], split out because the rest of it needs a terminal
/// and this is the part that can be wrong.
fn fields_still_unanswered<'a>(
    accepted: &'a [AuxField],
    already_given: &'a [(&'static str, String)],
) -> impl Iterator<Item = &'a AuxField> {
    accepted
        .iter()
        .filter(move |field| !already_given.iter().any(|(name, _)| *name == field.name))
}

/// Pure core of `stella auth set`: validates the id, resolves the key value
/// and any companion values, and stores them into `file` (caller saves).
/// Returns the masked confirmation lines — never a raw value.
///
/// `prompt` is the seam that keeps this testable: the real command passes
/// [`prompt_for_fields`], and a test passes a closure, so the companion-value
/// logic is exercised without a terminal. Non-interactive callers (`--key`,
/// `--stdin`) never prompt at all — a script that pipes a key must not block
/// on a question it cannot answer, so it supplies companions with `--field`
/// or leaves what is already stored in place.
fn set_in(
    file: &mut CredentialsFile,
    path_display: &str,
    provider: &str,
    key: Option<&str>,
    use_stdin: bool,
    fields: &[String],
    prompt: impl FnOnce(
        &CredentialsFile,
        &str,
        &[AuxField],
        &[(&'static str, String)],
    ) -> Result<Vec<(&'static str, String)>, String>,
) -> Result<Vec<String>, String> {
    validate_provider_id(provider)?;
    let accepted = settable_aux_fields(&provider_config(provider));
    let mut companions = parse_field_flags(provider, accepted, fields)?;

    let value = read_key_value(provider, key, use_stdin)?;
    let api_key = ApiKey::new(value);

    let interactive = key.is_none() && !use_stdin;
    if interactive && !accepted.is_empty() {
        companions.extend(prompt(file, provider, accepted, &companions)?);
    }

    file.set(provider, api_key.reveal());
    let mut lines = vec![format!(
        "stored key for `{provider}` in {path_display} ({})",
        api_key.redacted_preview()
    )];
    for (name, value) in &companions {
        file.set_field(provider, name, value.clone());
        lines.push(format!(
            "stored {name} for `{provider}` ({})",
            ApiKey::new(value.clone()).redacted_preview()
        ));
    }

    // Say so rather than leaving the run to fail later: a Bedrock row with an
    // access key id and no secret access key resolves at `stella models` and
    // then dies at the first request with a SigV4 error naming a variable the
    // user believed they had just configured.
    for field in accepted {
        if file.field(provider, field.name).is_none() && field.name == "AWS_SECRET_ACCESS_KEY" {
            lines.push(format!(
                "no {} stored — `{provider}` cannot sign a request until one is set \
                 (rerun without --key/--stdin, or pass --field {}=…)",
                field.name, field.name
            ));
        }
    }
    Ok(lines)
}

/// The provider row to look companion values up against. A settings.json
/// provider cannot declare the Bedrock dialect, and an id stored ahead of any
/// declaration synthesizes an OpenAI-compatible row — both correctly yield no
/// companion values.
fn provider_config(id: &str) -> ProviderConfig {
    let settings = std::env::current_dir()
        .ok()
        .and_then(|ws| Settings::load(&ws).ok())
        .unwrap_or_default();
    credential_status::provider_config_for(id, &settings)
}

fn run_set(
    provider: &str,
    key: Option<&str>,
    use_stdin: bool,
    fields: &[String],
) -> Result<(), String> {
    let mut file = load_file()?;
    let path_display = credentials_path_display();
    let messages = set_in(
        &mut file,
        &path_display,
        provider,
        key,
        use_stdin,
        fields,
        prompt_for_fields,
    )?;
    file.save()
        .map_err(|e| format!("failed to save {path_display}: {e}"))?;
    for message in messages {
        println!("  {} {}", "◆".yellow(), message);
    }
    Ok(())
}

/// Pure core of `stella auth remove`: removes `provider` from `file` if
/// present. Returns `(message, removed)` — the caller only re-saves the
/// file when `removed` is true.
fn remove_from(file: &mut CredentialsFile, path_display: &str, provider: &str) -> (String, bool) {
    if file.remove(provider) {
        (
            format!("removed key for `{provider}` from {path_display}"),
            true,
        )
    } else {
        (
            format!("no stored key for `{provider}` in {path_display} — nothing to remove"),
            false,
        )
    }
}

fn run_remove(provider: &str) -> Result<(), String> {
    let mut file = load_file()?;
    let path_display = credentials_path_display();
    let (message, removed) = remove_from(&mut file, &path_display, provider);
    if removed {
        file.save()
            .map_err(|e| format!("failed to save {path_display}: {e}"))?;
        println!("  {} {}", "◆".yellow(), message);
    } else {
        println!("  {} {}", "·".green(), message);
    }
    Ok(())
}

/// Pure core of `stella auth list`: one display line per stored provider —
/// redacted preview, the `credentials.toml` label, and (when something else
/// currently outranks the stored key in the real resolution chain — an env
/// var, a settings.json literal) a "shadowed by …" note, so this list stays
/// consistent with `stella models`/`stella config` rather than a static
/// echo of the file's own contents. Never includes a raw secret value.
fn list_lines(file: &CredentialsFile, settings: &Settings) -> Vec<String> {
    file.provider_ids()
        .filter_map(|id| {
            let value = file.get(id)?;
            let preview = ApiKey::new(value).redacted_preview();
            let provider = credential_status::provider_config_for(id, settings);
            let settings_key = credential_status::settings_literal_key(&provider, settings);
            let status =
                credential_status::status_for(&provider, settings_key.as_deref(), file, None);
            let note = match status.source_label {
                Some(label) if label != "credentials.toml" => {
                    format!("  (shadowed by {label})")
                }
                _ => String::new(),
            };
            // Companion values by NAME only — the same rule as the key itself,
            // and the reason to show them at all is that a Bedrock row without
            // AWS_SECRET_ACCESS_KEY looks identical to a working one otherwise.
            let companions: Vec<&str> = file.field_names(id).collect();
            let companion_note = if companions.is_empty() {
                String::new()
            } else {
                format!("  + {}", companions.join(", "))
            };
            Some(format!(
                "{id}  {preview}  credentials.toml{companion_note}{note}"
            ))
        })
        .collect()
}

fn run_list() -> Result<(), String> {
    let file = load_file()?;
    crate::tui::section_header("Stored credentials");
    let lines = {
        // Best-effort: a malformed settings.json costs the provider-config
        // lookup its overrides, never the listing itself.
        let settings = std::env::current_dir()
            .ok()
            .and_then(|ws| Settings::load(&ws).ok())
            .unwrap_or_default();
        list_lines(&file, &settings)
    };
    if lines.is_empty() {
        println!(
            "  {}",
            "none — run `stella auth set <provider>` to store one.".dimmed()
        );
        return Ok(());
    }
    for line in lines {
        println!("  {} {line}", "◆".yellow());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The prompt seam for tests that must never reach a terminal. A
    /// non-interactive `set` (`--key`/`--stdin`) is documented never to
    /// prompt; this turns "documented" into "asserted".
    fn never_prompt(
        _file: &CredentialsFile,
        _provider: &str,
        _accepted: &[AuxField],
        _given: &[(&'static str, String)],
    ) -> Result<Vec<(&'static str, String)>, String> {
        panic!("set_in must not prompt on a non-interactive path");
    }

    fn temp_credentials_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "stella-test-auth-cmd-{name}-{}.toml",
            std::process::id()
        ))
    }

    #[test]
    fn validate_provider_id_rejects_empty_slash_and_whitespace() {
        assert!(validate_provider_id("zai").is_ok());
        assert!(validate_provider_id("my-gateway").is_ok());
        assert!(validate_provider_id("").is_err());
        assert!(validate_provider_id("zai/glm").is_err());
        assert!(validate_provider_id("has space").is_err());
    }

    // set: witness for masked stdout + 0600 perms on disk

    #[test]
    fn set_in_stores_the_key_but_never_echoes_it_in_the_confirmation_message() {
        let path = temp_credentials_path("set-masked");
        let _ = std::fs::remove_file(&path);
        let mut file = CredentialsFile::load(&path).unwrap();

        let messages = set_in(
            &mut file,
            "credentials.toml",
            "zai",
            Some("sk-super-secret-value-1234"),
            false,
            &[],
            never_prompt,
        )
        .unwrap();
        let message = messages.join("\n");

        assert!(
            !message.contains("sk-super-secret-value-1234"),
            "confirmation message must never contain the raw secret: {message}"
        );
        assert!(message.contains("zai"));
        assert_eq!(file.get("zai"), Some("sk-super-secret-value-1234"));

        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn set_then_save_writes_the_real_file_0600() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_credentials_path("set-perms");
        let _ = std::fs::remove_file(&path);
        let mut file = CredentialsFile::load(&path).unwrap();

        set_in(
            &mut file,
            "credentials.toml",
            "zai",
            Some("sk-value"),
            false,
            &[],
            never_prompt,
        )
        .unwrap();
        file.save().unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "stella auth set must leave a 0600 file"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn set_in_rejects_an_invalid_provider_id_before_touching_the_file() {
        let mut file = CredentialsFile::load(temp_credentials_path("set-invalid")).unwrap();
        let err = set_in(
            &mut file,
            "credentials.toml",
            "zai/glm",
            Some("sk-value"),
            false,
            &[],
            never_prompt,
        )
        .unwrap_err();
        assert!(err.contains("not a valid provider id"));
        assert_eq!(file.get("zai/glm"), None);
    }

    // set: Bedrock's companion values (#1301)

    #[test]
    fn set_in_stores_bedrock_companion_values_and_never_echoes_them() {
        let mut file = CredentialsFile::load(temp_credentials_path("set-bedrock")).unwrap();

        let messages = set_in(
            &mut file,
            "credentials.toml",
            "bedrock",
            Some("AKIAEXAMPLEACCESSKEY"),
            false,
            &[
                "AWS_SECRET_ACCESS_KEY=the-secret-access-key".to_string(),
                "AWS_REGION=ap-southeast-2".to_string(),
            ],
            never_prompt,
        )
        .unwrap();

        assert_eq!(file.get("bedrock"), Some("AKIAEXAMPLEACCESSKEY"));
        assert_eq!(
            file.field("bedrock", "AWS_SECRET_ACCESS_KEY"),
            Some("the-secret-access-key")
        );
        assert_eq!(file.field("bedrock", "AWS_REGION"), Some("ap-southeast-2"));

        let printed = messages.join("\n");
        assert!(!printed.contains("the-secret-access-key"), "{printed}");
        assert!(printed.contains("AWS_SECRET_ACCESS_KEY"), "{printed}");
    }

    #[test]
    fn set_in_says_so_when_bedrock_is_left_unable_to_sign() {
        // The exact shape of the old failure: `stella auth set bedrock`
        // reported success and the next run died on AWS_SECRET_ACCESS_KEY.
        let mut file = CredentialsFile::load(temp_credentials_path("set-bedrock-bare")).unwrap();

        let messages = set_in(
            &mut file,
            "credentials.toml",
            "bedrock",
            Some("AKIAEXAMPLEACCESSKEY"),
            false,
            &[],
            never_prompt,
        )
        .unwrap();

        let printed = messages.join("\n");
        assert!(
            printed.contains("cannot sign a request"),
            "storing only an access key id must say so: {printed}"
        );
    }

    #[test]
    fn set_in_rejects_a_field_name_the_chain_would_never_read() {
        let mut file = CredentialsFile::load(temp_credentials_path("set-bad-field")).unwrap();
        let err = set_in(
            &mut file,
            "credentials.toml",
            "bedrock",
            Some("AKIAEXAMPLEACCESSKEY"),
            false,
            &["AWS_SECRET_KEY=oops".to_string()],
            never_prompt,
        )
        .unwrap_err();

        assert!(err.contains("AWS_SECRET_ACCESS_KEY"), "{err}");
        assert_eq!(file.field("bedrock", "AWS_SECRET_KEY"), None);
        // Rejected before the key is read, so nothing is half-written.
        assert_eq!(file.get("bedrock"), None);
    }

    #[test]
    fn set_in_rejects_companion_values_for_a_one_key_provider() {
        let mut file = CredentialsFile::load(temp_credentials_path("set-field-zai")).unwrap();
        let err = set_in(
            &mut file,
            "credentials.toml",
            "zai",
            Some("sk-value"),
            false,
            &["AWS_REGION=us-east-1".to_string()],
            never_prompt,
        )
        .unwrap_err();
        assert!(err.contains("no companion values"), "{err}");
    }

    #[test]
    fn the_prompt_skips_every_companion_value_a_flag_already_answered() {
        // Asking again for something just passed on the command line is how a
        // scripted `--field` invocation ends up blocking on a terminal.
        let accepted = settable_aux_fields(&provider_config("bedrock"));
        let given = vec![("AWS_SECRET_ACCESS_KEY", "from-the-flag".to_string())];

        let remaining: Vec<&str> = fields_still_unanswered(accepted, &given)
            .map(|f| f.name)
            .collect();
        assert_eq!(remaining, vec!["AWS_SESSION_TOKEN", "AWS_REGION"]);

        let nothing_given: Vec<(&'static str, String)> = Vec::new();
        assert_eq!(
            fields_still_unanswered(accepted, &nothing_given).count(),
            accepted.len()
        );
    }

    // remove

    #[test]
    fn remove_from_reports_success_and_actually_drops_the_entry() {
        let mut file = CredentialsFile::load(temp_credentials_path("remove-hit")).unwrap();
        file.set("zai", "sk-value");

        let (message, removed) = remove_from(&mut file, "credentials.toml", "zai");
        assert!(removed);
        assert!(message.contains("removed"));
        assert_eq!(file.get("zai"), None);
    }

    #[test]
    fn remove_from_reports_absence_without_erroring() {
        let mut file = CredentialsFile::load(temp_credentials_path("remove-miss")).unwrap();
        let (message, removed) = remove_from(&mut file, "credentials.toml", "nonexistent");
        assert!(!removed, "removing an absent entry must not claim success");
        assert!(message.contains("nothing to remove"));
    }

    // list: masked, and consistent with the #249 source labels

    #[test]
    fn list_lines_shows_a_redacted_preview_never_the_raw_value() {
        let mut file = CredentialsFile::load(temp_credentials_path("list-masked")).unwrap();
        file.set("zai", "sk-super-secret-value-1234");
        let settings = Settings::default();

        let lines = list_lines(&file, &settings);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains("sk-super-secret-value-1234"));
        assert!(lines[0].contains("zai"));
        assert!(lines[0].contains("credentials.toml"));
    }

    #[test]
    fn list_lines_notes_when_an_env_var_shadows_the_stored_key() {
        let _env = crate::test_env::lock();
        // `provider_config_for` derives `<ID>_API_KEY` (uppercased,
        // punctuation folded to `_`) for an id that matches no built-in or
        // settings.json entry — this id's derived var is
        // `STELLA_TEST_AUTH_LIST_SHADOW_API_KEY` (see
        // `config::derived_env_var`'s own test for the exact rule).
        let provider_id = "stella-test-auth-list-shadow";
        let env_var = "STELLA_TEST_AUTH_LIST_SHADOW_API_KEY";
        // SAFETY: guarded by test_env's lock; a unique var name unused
        // elsewhere.
        unsafe {
            std::env::set_var(env_var, "sk-from-env");
        }
        let mut file = CredentialsFile::load(temp_credentials_path("list-shadow")).unwrap();
        file.set(provider_id, "sk-from-credentials-file");
        let settings = Settings::default();

        let lines = list_lines(&file, &settings);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains(&format!("shadowed by env:{env_var}")),
            "expected a shadow note naming the winning env var, got: {}",
            lines[0]
        );

        unsafe {
            std::env::remove_var(env_var);
        }
    }

    #[test]
    fn list_lines_is_empty_when_nothing_stored() {
        let file = CredentialsFile::load(temp_credentials_path("list-empty")).unwrap();
        assert!(list_lines(&file, &Settings::default()).is_empty());
    }
}
