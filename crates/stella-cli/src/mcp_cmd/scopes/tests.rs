use std::path::Path;

use stella_mcp::{McpServerConfig, McpTransport};

use super::*;
use crate::paths::{UserPaths, test_paths};

/// Write `body` to `<home>/.stella/mcp.toml`. That is the user file for
/// this home.
fn write_user_file(home: &Path, body: &str) {
    let dir = home.join(".stella");
    std::fs::create_dir_all(&dir).expect("user stella root");
    std::fs::write(dir.join("mcp.toml"), body).expect("user mcp.toml");
}

/// Write `body` to `<workspace>/.stella/mcp.toml`.
fn write_workspace_file(workspace: &Path, body: &str) {
    let dir = workspace.join(".stella");
    std::fs::create_dir_all(&dir).expect("workspace .stella");
    std::fs::write(dir.join("mcp.toml"), body).expect("workspace mcp.toml");
}

/// One stdio server named `name` that runs `cmd`.
fn stdio_server(name: &str, cmd: &str) -> String {
    format!("[servers.{name}]\ntransport = \"stdio\"\ncmd = \"{cmd}\"\n")
}

fn command_of(config: &McpConfig, name: &str) -> Option<String> {
    match config.get(name)? {
        McpTransport::Stdio { cmd, .. } => Some(cmd.clone()),
        McpTransport::Http { url, .. } => Some(url.clone()),
    }
}

/// A home and a repo in one temp dir. The user files point at the home, for
/// this thread alone.
struct Fixture {
    _dir: tempfile::TempDir,
    home: std::path::PathBuf,
    workspace: std::path::PathBuf,
    _paths: crate::paths::TestPathsGuard,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let workspace = dir.path().join("repo");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let paths = test_paths(UserPaths::rooted_at(&home));
    Fixture {
        _dir: dir,
        home,
        workspace,
        _paths: paths,
    }
}

#[test]
fn the_user_file_is_stella_home_mcp_toml() {
    let fx = fixture();
    assert_eq!(
        user_mcp_toml_path(),
        Some(fx.home.join(".stella").join("mcp.toml"))
    );
}

#[test]
fn a_server_in_each_file_loads_both() {
    let fx = fixture();
    write_user_file(&fx.home, &stdio_server("everywhere", "user-mcp"));
    write_workspace_file(&fx.workspace, &stdio_server("here", "repo-mcp"));

    let own = load_own_config(&fx.workspace, true);

    assert_eq!(own.config.names(), vec!["everywhere", "here"]);
    assert_eq!(
        command_of(&own.config, "everywhere").as_deref(),
        Some("user-mcp")
    );
    assert_eq!(command_of(&own.config, "here").as_deref(), Some("repo-mcp"));
    assert!(own.notices.is_empty(), "{:?}", own.notices);
}

#[test]
fn the_workspace_entry_wins_a_shared_name() {
    let fx = fixture();
    write_user_file(&fx.home, &stdio_server("github", "user-github"));
    write_workspace_file(&fx.workspace, &stdio_server("github", "repo-github"));

    let own = load_own_config(&fx.workspace, true);

    assert_eq!(own.config.names(), vec!["github"]);
    assert_eq!(
        command_of(&own.config, "github").as_deref(),
        Some("repo-github"),
        "the workspace file is laid over the user file"
    );
    assert!(own.notices.is_empty(), "{:?}", own.notices);
}

/// The repo entry takes the place of the user entry as a whole. A grant in
/// the user file must not reach the command the repo chose.
#[test]
fn a_workspace_entry_takes_none_of_the_user_entrys_fields() {
    let fx = fixture();
    write_user_file(
        &fx.home,
        "[servers.github]\ntransport = \"stdio\"\ncmd = \"user-github\"\n\
         granted = true\ncandidate_safe = true\n",
    );
    write_workspace_file(
        &fx.workspace,
        "[servers.github]\ntransport = \"stdio\"\ncmd = \"repo-github\"\ngranted = false\n",
    );

    let own = load_own_config(&fx.workspace, true);

    assert_eq!(own.config.grant_decision("github"), Some(Some(false)));
    let servers = own.config.into_servers();
    assert!(!servers[0].candidate_safe, "{servers:?}");
}

#[test]
fn a_missing_user_file_says_nothing() {
    let fx = fixture();
    write_workspace_file(&fx.workspace, &stdio_server("here", "repo-mcp"));

    let own = load_own_config(&fx.workspace, true);

    assert_eq!(own.config.names(), vec!["here"]);
    assert!(own.notices.is_empty(), "{:?}", own.notices);
}

#[test]
fn no_files_load_nothing_and_say_nothing() {
    let fx = fixture();
    let own = load_own_config(&fx.workspace, true);
    assert!(own.config.names().is_empty());
    assert!(own.notices.is_empty(), "{:?}", own.notices);
}

#[test]
fn an_invalid_user_file_is_named_and_the_workspace_file_still_loads() {
    let fx = fixture();
    write_user_file(&fx.home, "[servers.broken\n");
    write_workspace_file(&fx.workspace, &stdio_server("here", "repo-mcp"));

    let own = load_own_config(&fx.workspace, true);

    assert_eq!(own.config.names(), vec!["here"]);
    let user_path = fx.home.join(".stella").join("mcp.toml");
    assert_eq!(own.notices.len(), 1, "{:?}", own.notices);
    assert!(
        own.notices[0].contains(&user_path.display().to_string()),
        "the notice names the file: {:?}",
        own.notices
    );
    assert!(own.notices[0].contains("is invalid"), "{:?}", own.notices);
}

#[test]
fn an_invalid_workspace_file_is_named_and_the_user_file_still_loads() {
    let fx = fixture();
    write_user_file(&fx.home, &stdio_server("everywhere", "user-mcp"));
    write_workspace_file(&fx.workspace, "not = [toml");

    let own = load_own_config(&fx.workspace, true);

    assert_eq!(own.config.names(), vec!["everywhere"]);
    let workspace_path = super::super::mcp_toml_path(&fx.workspace);
    assert_eq!(own.notices.len(), 1, "{:?}", own.notices);
    assert!(
        own.notices[0].contains(&workspace_path.display().to_string()),
        "the notice names the file: {:?}",
        own.notices
    );
}

/// When the repo is not trusted, its file is not read. So it can't hide a
/// server of yours. The reason is said once.
#[test]
fn an_untrusted_workspace_file_is_refused_and_the_user_file_still_loads() {
    let fx = fixture();
    write_user_file(&fx.home, &stdio_server("github", "user-github"));
    write_workspace_file(&fx.workspace, &stdio_server("github", "hostile"));

    let own = load_own_config(&fx.workspace, false);

    assert_eq!(
        command_of(&own.config, "github").as_deref(),
        Some("user-github")
    );
    assert_eq!(own.notices.len(), 1, "{:?}", own.notices);
    assert!(
        own.notices[0].contains("STELLA_TRUST_PROJECT=1"),
        "{:?}",
        own.notices
    );
}

#[test]
fn an_untrusted_workspace_without_a_file_says_nothing() {
    let fx = fixture();
    write_user_file(&fx.home, &stdio_server("everywhere", "user-mcp"));

    let own = load_own_config(&fx.workspace, false);

    assert_eq!(own.config.names(), vec!["everywhere"]);
    assert!(own.notices.is_empty(), "{:?}", own.notices);
}

/// A test that sets no home reads no user file. So the file on your own
/// machine can't change a result.
#[test]
fn a_test_with_no_home_reads_no_user_file() {
    assert_eq!(user_mcp_toml_path(), None);
}

/// The plan is what the session starts, so ask it too. A server in each file
/// both start. When a name is in both, the repo entry starts.
#[test]
fn the_session_plan_carries_the_user_servers_and_the_workspace_override() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let workspace = dir.path().join("repo");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let _home = crate::paths::test_user_home(home.clone());
    // SAFETY: we hold the env lock until the env is put back.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    write_user_file(
        &home,
        &format!(
            "{}{}",
            stdio_server("everywhere", "user-mcp"),
            stdio_server("github", "user-github")
        ),
    );
    write_workspace_file(
        &workspace,
        &format!(
            "{}{}",
            stdio_server("here", "repo-mcp"),
            stdio_server("github", "repo-github")
        ),
    );

    let provider = crate::config::PROVIDERS
        .first()
        .expect("the provider table is never empty")
        .clone();
    let model_id = provider.default_model.to_string();
    let mut cfg = crate::config::Config::for_tests(provider, model_id);
    cfg.workspace_root = workspace.clone();

    let servers: Vec<McpServerConfig> = match crate::agent::load_mcp_plan(&cfg) {
        crate::agent::McpPlan::Servers(servers, notices) => {
            assert!(notices.is_empty(), "{notices:?}");
            servers
        }
        other => panic!("expected servers from both files, got {other:?}"),
    };
    let named = |name: &str| {
        servers
            .iter()
            .find(|server| server.name == name)
            .map(|server| server.transport.clone())
    };
    assert_eq!(
        servers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["everywhere", "github", "here"]
    );
    assert!(
        matches!(named("everywhere"), Some(McpTransport::Stdio { cmd, .. }) if cmd == "user-mcp")
    );
    assert!(matches!(named("here"), Some(McpTransport::Stdio { cmd, .. }) if cmd == "repo-mcp"));
    assert!(
        matches!(named("github"), Some(McpTransport::Stdio { cmd, .. }) if cmd == "repo-github"),
        "the workspace entry is the one that starts: {servers:?}"
    );

    // A plugin server with a name from the user file is dropped. The same
    // goes for a name from the repo file.
    assert_eq!(
        crate::agent::own_mcp_server_names(&workspace),
        vec!["everywhere", "github", "here"]
    );
}

/// The grants read both files, the same way the plan does. A "no" in the
/// user file holds the server back. A repo entry takes its place.
#[test]
fn the_session_grants_read_the_user_file_too() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let fx = fixture();
    // SAFETY: we hold the env lock until the env is put back.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };
    write_user_file(
        &fx.home,
        "[servers.withheld]\ntransport = \"stdio\"\ncmd = \"a\"\ngranted = false\n\
         [servers.overridden]\ntransport = \"stdio\"\ncmd = \"b\"\ngranted = false\n",
    );
    write_workspace_file(
        &fx.workspace,
        "[servers.overridden]\ntransport = \"stdio\"\ncmd = \"c\"\n",
    );
    let plan: Vec<McpServerConfig> = ["withheld", "overridden"]
        .into_iter()
        .map(|name| McpServerConfig {
            name: name.to_string(),
            transport: McpTransport::Stdio {
                cmd: "x".into(),
                args: vec![],
                env: Default::default(),
            },
            candidate_safe: false,
        })
        .collect();

    let grants = super::super::initial_grants(&fx.workspace, &plan);
    let granted = grants.lock().expect("grants");

    assert!(!granted.contains("withheld"), "{granted:?}");
    assert!(
        granted.contains("overridden"),
        "the hand-written workspace entry replaced the withheld one: {granted:?}"
    );
}
