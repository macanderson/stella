//! How the overlay is read, how it fails, and how it merges with the other
//! files.
//!
//! No test here lets [`install`] succeed. It sets the overlay once for the
//! whole process. Then each other test in the binary would load settings
//! with it.

#[cfg(unix)]
use stella_core::hooks::HookEvent;

use super::*;
use crate::settings::ProjectTrust;
use crate::settings::toml_config::IssuesSection;

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

#[cfg(unix)]
fn trusted() -> ProjectTrust {
    ProjectTrust {
        hooks: true,
        credentials: true,
    }
}

/// The commands one event's hooks run, in the order they run.
#[cfg(unix)]
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

#[cfg(unix)]
#[test]
fn a_launch_overlay_reads_its_hooks_provider_and_server() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);

    let overlay = read(&path, None).expect("overlay reads");

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

    let error = read(&path, None).expect_err("a missing file");

    assert!(error.contains(FLAG), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
}

#[cfg(unix)]
#[test]
fn an_overlay_that_is_not_toml_is_an_error_that_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), "[providers.anthropic\nbase_url = 1\n");

    let error = read(&path, None).expect_err("broken TOML");

    assert!(error.contains(&path.display().to_string()), "{error}");
}

/// The file's own `[meta]` may say it is an overlay. It may say no other
/// scope.
#[cfg(unix)]
#[test]
fn an_overlay_cannot_claim_another_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), "[meta]\nscope = \"managed\"\n");

    let error = read(&path, None).expect_err("not managed");

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

    let error = read(&path, None).expect_err("a shared directory");

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

    let error = install(Some(&path), None).expect_err("the two asks contradict");

    assert!(error.contains("STELLA_NO_SETTINGS"), "{error}");
    assert!(active().is_none(), "a refused overlay installs nothing");
}

/// Off Unix, Stella can't check who may write the file or its folder.
/// Another user could swap in hooks that run in Stella. So the read refuses
/// even a file the user wrote, and the error says why.
#[cfg(not(unix))]
#[test]
fn an_overlay_is_refused_off_unix() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);

    let error = read(&path, None).expect_err("no owner check on this system");

    assert!(error.contains(FLAG), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
    assert!(error.contains("cannot check who may write"), "{error}");
}

// ---- child runs ---------------------------------------------------------

/// The read keeps the SHA-256 of the exact bytes it read. A child run checks
/// its own read against it.
#[cfg(unix)]
#[test]
fn the_read_keeps_the_sha256_of_the_files_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), "[tools]\ntask_list = \"on\"\n");

    let overlay = read(&path, None).unwrap();

    assert_eq!(
        overlay.sha256,
        "fdbaef748a26ede9a5a6bc0f9b6acb52c604df316cb1754cfc1d91e65e82bbfe"
    );
    assert_eq!(overlay.pin().sha256, overlay.sha256);
}

/// A child given its parent's hash loads the file while its bytes match.
#[cfg(unix)]
#[test]
fn a_child_with_the_parents_hash_loads_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);
    let parent = read(&path, None).unwrap();

    let child = read(&path, Some(&parent.sha256)).expect("the same bytes");

    assert_eq!(child.sha256, parent.sha256);
    assert_eq!(child.mcp.names(), vec!["oxagen"]);
}

/// The launcher rewrote the file between two turns. The next turn refuses
/// it, rather than run with another key or other hooks, and names the file.
#[cfg(unix)]
#[test]
fn a_child_refuses_a_file_that_changed_since_the_parent_read_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);
    let parent = read(&path, None).unwrap();
    std::fs::write(
        &path,
        LAUNCH_OVERLAY.replace("launch-run-token", "another-run-token"),
    )
    .unwrap();

    let error = read(&path, Some(&parent.sha256)).expect_err("other bytes");

    assert!(error.contains(&path.display().to_string()), "{error}");
    assert!(error.contains("changed"), "{error}");
    assert!(!error.contains("another-run-token"), "{error}");
}

/// The flags a child gets: the path, then the hash, each after its flag.
#[test]
fn a_pin_hands_a_child_the_path_and_the_hash() {
    let pin = Pin {
        path: PathBuf::from("/opt/launch/overlay.toml"),
        sha256: "ab".repeat(32),
    };

    assert_eq!(
        pin.child_args(),
        [
            OsString::from(FLAG),
            "/opt/launch/overlay.toml".into(),
            SHA256_FLAG.into(),
            "ab".repeat(32).into(),
        ]
    );
}

/// A hash with no file to check is a mistake, so the launch stops.
#[test]
fn a_hash_without_an_overlay_is_refused() {
    let error = install(None, Some("ab")).expect_err("nothing to check");

    assert!(error.starts_with(SHA256_FLAG), "{error}");
    assert!(error.contains(&format!("needs {FLAG}")), "{error}");
    assert!(active().is_none(), "a refused hash installs nothing");
}

/// The hash flag is the name clap parses, is hidden, and has no env var. A
/// project `.env` can set env vars for Stella, so an env var here would let
/// a repo vouch for a file the launcher never read.
#[test]
fn the_hash_flag_is_hidden_and_has_no_env_var() {
    use clap::{CommandFactory as _, Parser as _};

    let command = crate::cli::Cli::command();
    let arg = command
        .get_arguments()
        .find(|arg| arg.get_id().as_str() == "config_overlay_sha256")
        .expect("the flag exists");
    assert_eq!(
        arg.get_long().map(|long| format!("--{long}")),
        Some(SHA256_FLAG.to_owned())
    );
    assert!(arg.is_hide_set());
    assert!(arg.is_global_set());
    assert!(arg.get_env().is_none());

    let globals = crate::cli::Cli::try_parse_from([
        "stella",
        "self-driving",
        "state",
        FLAG,
        "/opt/launch/overlay.toml",
        SHA256_FLAG,
        "ab",
    ])
    .expect("these argv parse")
    .globals;
    assert_eq!(globals.config_overlay_sha256.as_deref(), Some("ab"));
}

// ---- merge --------------------------------------------------------------

/// Hooks add to the user's own, and the overlay's run after them.
#[cfg(unix)]
#[test]
fn overlay_hooks_run_after_the_users_own() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);
    let overlay = read(&path, None).unwrap();
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
#[cfg(unix)]
#[test]
fn overlay_scalars_replace_the_users_and_leave_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);
    let overlay = read(&path, None).unwrap();
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
#[cfg(unix)]
#[test]
fn an_untrusted_project_cannot_remove_the_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);
    let overlay = read(&path, None).unwrap();
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
#[cfg(unix)]
#[test]
fn a_trusted_project_sits_below_the_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LAUNCH_OVERLAY);
    let overlay = read(&path, None).unwrap();
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
#[cfg(unix)]
#[test]
fn a_managed_denial_caps_the_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), "[tools]\ntask_list = \"on\"\n");
    let overlay = read(&path, None).unwrap();
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

// ---- self-driving and issues ---------------------------------------------

/// An overlay that picks the loop's worker and its tracker.
#[cfg(unix)]
const LOOP_OVERLAY: &str = r#"
[self_driving.worker]
kind = "claude"

[issues]
provider = "linear"
"#;

/// A workspace's `[self_driving]` table, as its file wrote it.
const WORKSPACE_LOOP: &str = r#"
[worker]
kind = "claude"
command = "/opt/bin/claude"

[triage]
priorities = ["P0", "P1"]
defect_kinds = ["bug"]
"#;

/// An overlay's `[self_driving]` table: one worker key and one list.
const OVERLAY_LOOP: &str = r#"
[worker]
model = "opus"

[triage]
priorities = ["urgent"]
"#;

/// The two tables laid one over the other, key by key.
const LAID_LOOP: &str = r#"
[worker]
kind = "claude"
command = "/opt/bin/claude"
model = "opus"

[triage]
priorities = ["urgent"]
defect_kinds = ["bug"]
"#;

fn table(text: &str) -> toml::Table {
    toml::from_str(text).expect("the fixture is TOML")
}

/// The read keeps the overlay's `[self_driving]` and `[issues]` tables. The
/// settings merge drops both, so without this the loop would never see
/// them.
#[cfg(unix)]
#[test]
fn the_read_keeps_the_self_driving_and_issues_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_overlay(dir.path(), LOOP_OVERLAY);

    let overlay = read(&path, None).unwrap();

    assert_eq!(overlay.loop_sections, LoopSections::of(LOOP_OVERLAY));
    let issues = overlay.loop_sections.issues.expect("[issues] is kept");
    assert_eq!(
        issues.get("provider").and_then(toml::Value::as_str),
        Some("linear")
    );
}

/// Each key the overlay sets takes its value. A nested table merges the same
/// way, and a list is one value that the overlay's list replaces.
#[test]
fn an_overlay_table_lays_over_the_workspace_key_by_key() {
    let mut base = table(WORKSPACE_LOOP);

    lay_table(&mut base, &table(OVERLAY_LOOP));

    assert_eq!(base, table(LAID_LOOP));
}

/// With no overlay table, the section the workspace loader read comes back
/// as it was.
#[test]
fn with_no_overlay_table_the_workspace_section_is_kept() {
    let read_by_the_loader = IssuesSection {
        provider: "linear".to_owned(),
        manifest: Some("tracker/linear.toml".to_owned()),
    };

    let laid = lay_section(
        "issues",
        read_by_the_loader.clone(),
        Some("[issues]\nprovider = \"linear\"\n"),
        None,
    );

    assert_eq!(laid, read_by_the_loader);
}

/// The overlay's key wins, and a key it leaves out keeps the workspace's
/// value.
#[test]
fn an_overlay_section_sets_its_keys_and_keeps_the_workspaces_others() {
    let written = "[issues]\nprovider = \"linear\"\nmanifest = \"tracker/linear.toml\"\n";
    let overlay = LoopSections::of("[issues]\nprovider = \"github\"\n");

    let laid: IssuesSection = lay_section(
        "issues",
        IssuesSection::default(),
        Some(written),
        overlay.issues.as_ref(),
    );

    assert_eq!(laid.provider, "github");
    assert_eq!(laid.manifest.as_deref(), Some("tracker/linear.toml"));
}

/// A workspace file that is missing or does not parse adds nothing. The
/// overlay lies over the built-in defaults.
#[test]
fn an_overlay_section_lies_over_the_defaults_when_the_workspace_has_none() {
    let overlay = LoopSections::of("[issues]\nmanifest = \"launch/github.toml\"\n");

    let laid: IssuesSection = lay_section(
        "issues",
        IssuesSection::default(),
        None,
        overlay.issues.as_ref(),
    );

    assert_eq!(laid.provider, IssuesSection::default().provider);
    assert_eq!(laid.manifest.as_deref(), Some("launch/github.toml"));
}
