//! How the overlay is read, how it fails, and how it merges with the other
//! files.
//!
//! No test here calls [`install`]. It sets the overlay once for the whole
//! process. Then each other test in the binary would load settings with it.

use stella_core::hooks::HookEvent;

use super::*;
use crate::settings::ProjectTrust;

/// What a tool writes for one launch it records: hooks, the Anthropic
/// endpoint and key, and one MCP server.
const LAUNCH_OVERLAY: &str = r#"
[meta]
scope = "overlay"

[providers.anthropic]
base_url = "http://127.0.0.1:47700/launch/one/stella/anthropic"
api_key = "launch-run-token"

[[hooks.UserPromptSubmit]]
[[hooks.UserPromptSubmit.hooks]]
type = "command"
command = "launch-hook UserPromptSubmit"
timeoutMs = 30000

[[hooks.PreToolUse]]
[[hooks.PreToolUse.hooks]]
type = "command"
command = "launch-hook PreToolUse"

[mcp.servers.oxagen]
transport = "stdio"
cmd = "/opt/launch/shim"
args = ["mcp-stdio", "--launch", "one"]
env = { LAUNCH_ID = "one" }
"#;

fn write_overlay(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("overlay.toml");
    std::fs::write(&path, body).expect("write overlay");
    path
}

fn parsed(json: &str) -> Settings {
    serde_json::from_str(json).expect("scope parses")
}

fn untrusted() -> ProjectTrust {
    ProjectTrust {
        hooks: false,
        credentials: false,
    }
}

fn trusted() -> ProjectTrust {
    ProjectTrust {
        hooks: true,
        credentials: true,
    }
}

/// The commands one event's hooks run, in the order they run.
fn commands(settings: &Settings, event: HookEvent) -> Vec<String> {
    settings
        .hooks
        .as_ref()
        .map(|hooks| {
            hooks
                .matchers_for(event)
                .iter()
                .flat_map(|matcher| matcher.hooks.iter().map(|hook| hook.command.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn anthropic(settings: &Settings) -> &crate::settings::ProviderSettings {
    settings
        .providers
        .get("anthropic")
        .expect("an anthropic entry")
}

// ---- read ---------------------------------------------------------------

#[test]
fn a_launch_overlay_reads_its_hooks_provider_and_server() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);

    let overlay = read(&path).expect("overlay reads");

    assert_eq!(overlay.path, path);
    assert_eq!(
        anthropic(&overlay.settings).base_url.as_deref(),
        Some("http://127.0.0.1:47700/launch/one/stella/anthropic")
    );
    assert_eq!(
        anthropic(&overlay.settings).api_key.as_deref(),
        Some("launch-run-token")
    );
    assert_eq!(
        commands(&overlay.settings, HookEvent::UserPromptSubmit),
        vec!["launch-hook UserPromptSubmit"]
    );
    assert_eq!(overlay.mcp.names(), vec!["oxagen"]);
}

/// A missing file stops the launch. The error names the file. Going on
/// without it would start the session as if no overlay had been given.
#[test]
fn a_missing_overlay_is_an_error_that_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gone.toml");

    let error = read(&path).expect_err("a missing file");

    assert!(error.contains(FLAG), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
}

#[test]
fn an_overlay_that_is_not_toml_is_an_error_that_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), "[providers.anthropic\nbase_url = 1\n");

    let error = read(&path).expect_err("broken TOML");

    assert!(error.contains(&path.display().to_string()), "{error}");
}

/// The file's own `[meta]` may say it is an overlay. It may say no other
/// scope.
#[test]
fn an_overlay_cannot_claim_another_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), "[meta]\nscope = \"managed\"\n");

    let error = read(&path).expect_err("not managed");

    assert!(error.contains("overlay"), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
}

/// The file can hold hooks and a key, so no other user may swap it. The
/// read refuses a folder that others can write to.
#[cfg(unix)]
#[test]
fn an_overlay_in_a_directory_others_can_write_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    let open_to_all = std::fs::Permissions::from_mode(0o777);
    std::fs::set_permissions(&shared, open_to_all).unwrap();
    let path = write_overlay(&shared, LAUNCH_OVERLAY);

    let error = read(&path).expect_err("a shared directory");

    assert!(error.contains(&path.display().to_string()), "{error}");
}

/// `STELLA_NO_SETTINGS=1` reads no config file. So an overlay in the same
/// launch is refused, and neither ask is dropped with no word.
///
/// The path names no file. If the refusal broke, the read would fail too.
/// So no test binary is left to run with an overlay set.
#[test]
fn an_overlay_is_refused_when_settings_are_switched_off() {
    let _isolated = crate::paths::test_filesystem_isolation(true);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("never-read.toml");

    let error = install(Some(&path)).expect_err("the two asks contradict");

    assert!(error.contains("STELLA_NO_SETTINGS"), "{error}");
    assert!(active().is_none(), "a refused overlay installs nothing");
}

// ---- merge --------------------------------------------------------------

/// Hooks add to the user's own, and the overlay's run after them.
#[test]
fn overlay_hooks_run_after_the_users_own() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);
    let overlay = read(&path).unwrap();
    let user =
        parsed(r#"{"hooks": {"UserPromptSubmit": [{"hooks": [{"command": "my-own-hook"}]}]}}"#);

    let merged = Settings::merge_captured_scopes(
        &user,
        &Settings::default(),
        &Settings::default(),
        Some(&overlay.settings),
        untrusted(),
    );

    assert_eq!(
        commands(&merged, HookEvent::UserPromptSubmit),
        vec!["my-own-hook", "launch-hook UserPromptSubmit"]
    );
}

/// A plain value the overlay sets takes the place of the user's. One it
/// leaves out keeps the user's value.
#[test]
fn overlay_scalars_replace_the_users_and_leave_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);
    let overlay = read(&path).unwrap();
    let user = parsed(
        r#"{"providers": {"anthropic": {
          "base_url": "https://api.anthropic.com",
          "api_key": "sk-mine",
          "default_model": "claude-fable-5"
        }}}"#,
    );

    let merged = Settings::merge_captured_scopes(
        &user,
        &Settings::default(),
        &Settings::default(),
        Some(&overlay.settings),
        untrusted(),
    );

    let entry = anthropic(&merged);
    assert_eq!(
        entry.base_url.as_deref(),
        Some("http://127.0.0.1:47700/launch/one/stella/anthropic")
    );
    assert_eq!(entry.api_key.as_deref(), Some("launch-run-token"));
    assert_eq!(entry.default_model.as_deref(), Some("claude-fable-5"));
}

/// A repo file we don't trust loses its hooks and its endpoint. The
/// overlay's hooks and endpoint stay.
#[test]
fn an_untrusted_project_cannot_remove_the_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);
    let overlay = read(&path).unwrap();
    let project = parsed(
        r#"{
          "providers": {"anthropic": {"base_url": "https://repo.invalid"}},
          "hooks": {"UserPromptSubmit": [{"hooks": [{"command": "repo-hook"}]}]}
        }"#,
    );

    let merged = Settings::merge_captured_scopes(
        &Settings::default(),
        &Settings::default(),
        &project,
        Some(&overlay.settings),
        untrusted(),
    );

    assert_eq!(
        commands(&merged, HookEvent::UserPromptSubmit),
        vec!["launch-hook UserPromptSubmit"]
    );
    assert_eq!(
        anthropic(&merged).base_url.as_deref(),
        Some("http://127.0.0.1:47700/launch/one/stella/anthropic")
    );
}

/// A trusted repo still sits below the overlay. So its endpoint does not
/// beat the one the launch set.
#[test]
fn a_trusted_project_sits_below_the_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);
    let overlay = read(&path).unwrap();
    let project = parsed(r#"{"providers": {"anthropic": {"base_url": "https://repo.invalid"}}}"#);

    let merged = Settings::merge_captured_scopes(
        &Settings::default(),
        &Settings::default(),
        &project,
        Some(&overlay.settings),
        trusted(),
    );

    assert_eq!(
        anthropic(&merged).base_url.as_deref(),
        Some("http://127.0.0.1:47700/launch/one/stella/anthropic")
    );
}

/// The org's limit caps the overlay as it caps each other file.
#[test]
fn a_managed_denial_caps_the_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), "[tools]\ntask_list = \"on\"\n");
    let overlay = read(&path).unwrap();
    let managed = parsed(r#"{"tools": {"task_list": "off"}}"#);

    let merged = Settings::merge_captured_scopes(
        &Settings::default(),
        &managed,
        &Settings::default(),
        Some(&overlay.settings),
        trusted(),
    );

    assert!(!merged.tool_policy().allows("task_list"));
}

/// With no overlay, the merge gives the same result as it did before.
#[test]
fn no_overlay_changes_nothing() {
    let user = parsed(r#"{"providers": {"anthropic": {"api_key": "sk-mine"}}}"#);

    let merged = Settings::merge_captured_scopes(
        &user,
        &Settings::default(),
        &Settings::default(),
        None,
        untrusted(),
    );

    assert_eq!(anthropic(&merged).api_key.as_deref(), Some("sk-mine"));
    assert!(merged.hooks.is_none());
}
