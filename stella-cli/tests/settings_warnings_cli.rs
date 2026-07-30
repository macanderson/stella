//! End-to-end witness for the settings.json unrecognized-key warning.
//!
//! Every settings type in `stella-cli` is deliberately tolerant of unknown
//! fields — `deny_unknown_fields` would turn a key written by a newer stella
//! into a hard launch failure on an older one. The price of that tolerance is
//! that serde cannot tell a typo from a future key, so `"provider"` for
//! `"providers"` parsed cleanly, configured absolutely nothing, and printed
//! nothing at all.
//!
//! Only a process can prove the whole path: the file is read from the real
//! scope chain, the warning reaches stderr, stdout stays clean for whoever is
//! piping it, and the command still succeeds (advisory, never a gate).

use std::process::{Command, Output};

/// A workspace with the given `.stella/settings.json`, plus an empty HOME so
/// the developer's own user scope cannot contribute keys to the assertions.
fn workspace(settings: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("workspace");
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("dot dir");
    std::fs::write(dir.path().join(".stella/settings.json"), settings).expect("settings");
    (dir, home)
}

/// `stella models` loads the full scope chain and needs no provider or key —
/// the cheapest command that exercises the merge.
fn models(dir: &tempfile::TempDir, home: &tempfile::TempDir) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stella"))
        .arg("models")
        .current_dir(dir.path())
        .env("HOME", home.path())
        .env("NO_COLOR", "1")
        .env("STELLA_NO_ENV_FILE", "1")
        .env("STELLA_CATALOG_AUTO_REFRESH", "0")
        // Point the org-managed scope at a path that does not exist, so a real
        // /etc/stella/settings.json on the build host cannot affect this.
        .env("STELLA_MANAGED_SETTINGS", home.path().join("no-managed.json"))
        .env_remove("STELLA_MODEL")
        .output()
        .expect("run stella models")
}

#[test]
fn unrecognized_settings_keys_are_named_on_stderr_without_failing_the_command() {
    let (dir, home) = workspace(
        r#"{
             "provider": { "zai": { "api_key": "x" } },
             "enable_recapp": "on",
             "hooks": { "preToolUse": [] },
             "agent_engine_config": { "default_modle": "zai/glm-5.2" }
           }"#,
    );
    let output = models(&dir, &home);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(0),
        "the warning is advisory and must never gate a command: {stderr}"
    );
    assert!(
        stderr.contains("unrecognized key"),
        "the finding must be stated, not merely implied: {stderr}"
    );
    for typo in [
        "provider",
        "enable_recapp",
        "hooks.preToolUse",
        "agent_engine_config.default_modle",
    ] {
        assert!(
            stderr.contains(typo),
            "`{typo}` must be named so it can be fixed: {stderr}"
        );
    }
    assert!(
        stderr.contains(".stella/settings.json"),
        "the warning must say WHICH file: {stderr}"
    );
    // A machine reading the provider table must not have to filter diagnostics
    // out of it.
    assert!(
        !stdout.contains("unrecognized key"),
        "the warning belongs on stderr: {stdout}"
    );
}

#[test]
fn a_correctly_spelled_settings_file_warns_about_nothing() {
    let (dir, home) = workspace(
        r#"{
             "providers": { "zai": { "api_key_env": "SOME_VAR" } },
             "tools": { "bash": "off", "some-server__any_tool": "off" },
             "enable_recap": "on",
             "ui": { "theme": "stella-dark" },
             "agent_engine_config": {
               "default_model": "zai/glm-5.2",
               "agents": { "judge": { "params": { "temperature": 0.2 } } }
             }
           }"#,
    );
    let output = models(&dir, &home);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        !stderr.contains("unrecognized key"),
        "open maps (tools, providers) and every real key must stay silent: {stderr}"
    );
}

/// The chain is loaded several times over in one launch (`Config::load`, the
/// catalog bootstrap, `validate_at_launch`, every `/models` render). One
/// accurate notice repeated four times reads as a fault rather than a notice,
/// so the announcement is latched — exactly like the project-trust notices it
/// sits beside.
#[test]
fn the_warning_is_printed_once_per_process_not_once_per_settings_load() {
    let (dir, home) = workspace(r#"{ "enable_recapp": "on" }"#);
    let output = models(&dir, &home);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("unrecognized key").count(),
        1,
        "expected exactly one notice: {stderr}"
    );
}
