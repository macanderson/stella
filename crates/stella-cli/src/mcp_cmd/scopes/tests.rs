use std::collections::HashSet;
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

    let (servers, _) = planned_servers(&workspace);
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

/// The session plan for `workspace`: its servers and the names that start
/// granted. It fails the test when the plan connects nothing.
fn planned_servers(workspace: &Path) -> (Vec<McpServerConfig>, HashSet<String>) {
    let provider = crate::config::PROVIDERS
        .first()
        .expect("the provider table is never empty")
        .clone();
    let model_id = provider.default_model.to_string();
    let mut cfg = crate::config::Config::for_tests(provider, model_id);
    cfg.workspace_root = workspace.to_path_buf();
    match crate::agent::load_mcp_plan(&cfg) {
        crate::agent::McpPlan::Servers {
            servers,
            notices,
            granted,
        } => {
            assert!(notices.is_empty(), "{notices:?}");
            (servers, granted)
        }
        other => panic!("expected servers, got {other:?}"),
    }
}

/// A home and a repo for a test that builds the session plan. The plan also
/// reads plugin rosters, so the whole home is redirected, not only the
/// stella root. `trusted` sets `STELLA_TRUST_PROJECT`.
struct PlanFixture {
    _dir: tempfile::TempDir,
    home: std::path::PathBuf,
    workspace: std::path::PathBuf,
    _home: crate::paths::TestHomeGuard,
    _restore: crate::test_env::EnvRestore,
    _env: crate::test_env::EnvLock,
}

fn plan_fixture(trusted: bool) -> PlanFixture {
    let env = crate::test_env::lock();
    let restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let workspace = dir.path().join("repo");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let home_guard = crate::paths::test_user_home(home.clone());
    // SAFETY: we hold the env lock until the env is put back.
    unsafe {
        std::env::remove_var("STELLA_PROJECT_HOOKS");
        if trusted {
            std::env::set_var("STELLA_TRUST_PROJECT", "1");
        } else {
            std::env::remove_var("STELLA_TRUST_PROJECT");
        }
    }
    PlanFixture {
        _dir: dir,
        home,
        workspace,
        _home: home_guard,
        _restore: restore,
        _env: env,
    }
}

/// The grants come from the read the plan's servers came from. A "no" in
/// the user file holds the server back, and a repo entry takes its place.
/// An edit to a file after that read changes neither.
#[test]
fn the_session_grants_come_from_the_plans_own_read() {
    let fx = plan_fixture(true);
    write_user_file(
        &fx.home,
        "[servers.withheld]\ntransport = \"stdio\"\ncmd = \"a\"\ngranted = false\n\
         [servers.overridden]\ntransport = \"stdio\"\ncmd = \"b\"\ngranted = false\n",
    );
    write_workspace_file(
        &fx.workspace,
        "[servers.overridden]\ntransport = \"stdio\"\ncmd = \"c\"\n",
    );

    let (_, granted) = planned_servers(&fx.workspace);
    // The file loses its "no" after the plan was read.
    write_user_file(&fx.home, &stdio_server("withheld", "a"));

    assert!(!granted.contains("withheld"), "{granted:?}");
    assert!(
        granted.contains("overridden"),
        "the hand-written workspace entry replaced the withheld one: {granted:?}"
    );
}

/// In a repo you have not trusted, your servers start in `$STELLA_HOME`. A
/// runner such as `npx` would otherwise load a package the repo ships.
#[test]
fn an_untrusted_repo_starts_your_servers_in_stella_home() {
    let fx = plan_fixture(false);
    write_user_file(&fx.home, &stdio_server("everywhere", "npx"));

    let (servers, _) = planned_servers(&fx.workspace);

    assert_eq!(servers.len(), 1, "{servers:?}");
    assert_eq!(servers[0].cwd, Some(fx.home.join(".stella")));
}

/// In a trusted repo the repo may run code anyway, so your servers start
/// where Stella runs, as the repo's own servers do.
#[test]
fn a_trusted_repo_starts_your_servers_where_stella_runs() {
    let fx = plan_fixture(true);
    write_user_file(&fx.home, &stdio_server("everywhere", "npx"));

    let (servers, _) = planned_servers(&fx.workspace);

    assert_eq!(servers.len(), 1, "{servers:?}");
    assert_eq!(servers[0].cwd, None);
}

/// A user file that is a symlink is not followed. Its servers are skipped,
/// and the notice names the file.
#[cfg(unix)]
#[test]
fn a_symlinked_user_file_is_refused() {
    let fx = fixture();
    let elsewhere = fx.home.join("elsewhere.toml");
    std::fs::write(&elsewhere, stdio_server("planted", "attacker")).expect("target");
    std::fs::create_dir_all(fx.home.join(".stella")).expect("user stella root");
    let user_path = fx.home.join(".stella").join("mcp.toml");
    std::os::unix::fs::symlink(&elsewhere, &user_path).expect("symlink");

    let own = load_own_config(&fx.workspace, true);

    assert!(own.config.names().is_empty(), "{:?}", own.config.names());
    assert_eq!(own.notices.len(), 1, "{:?}", own.notices);
    assert!(
        own.notices[0].contains(&user_path.display().to_string()),
        "{:?}",
        own.notices
    );
}

/// A user file in a directory other users can write to is refused. Anyone
/// who can write there could swap the file.
#[cfg(unix)]
#[test]
fn a_user_file_in_a_shared_directory_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = fixture();
    write_user_file(&fx.home, &stdio_server("everywhere", "user-mcp"));
    let root = fx.home.join(".stella");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).expect("chmod");

    let own = load_own_config(&fx.workspace, true);

    assert!(own.config.names().is_empty(), "{:?}", own.config.names());
    assert_eq!(own.notices.len(), 1, "{:?}", own.notices);
}

/// The `stella mcp` commands and the deck list your servers too, an OAuth
/// login finds a server that only your file names, and a change to that
/// server is written to your file.
#[test]
fn a_user_file_server_is_listed_logged_into_and_edited_in_the_user_file() {
    let fx = fixture();
    write_user_file(
        &fx.home,
        &format!(
            "{}[servers.remote]\ntransport = \"http\"\nurl = \"https://mcp.example.com/mcp\"\n",
            stdio_server("everywhere", "user-mcp")
        ),
    );
    write_workspace_file(&fx.workspace, &stdio_server("here", "repo-mcp"));
    let user_path = fx.home.join(".stella").join("mcp.toml");
    let user_file = || {
        read_mcp_file(&user_path)
            .expect("user file")
            .expect("there")
    };

    assert_eq!(
        load_listed_config(&fx.workspace).expect("listing").names(),
        vec!["everywhere", "here", "remote"]
    );
    assert_eq!(
        super::super::http_server_url(&fx.workspace, "remote").as_deref(),
        Ok("https://mcp.example.com/mcp")
    );

    assert!(super::super::set_granted(&fx.workspace, "everywhere", false).expect("grant"));
    assert_eq!(user_file().grant_decision("everywhere"), Some(Some(false)));
    assert_eq!(
        super::super::load_config(&fx.workspace)
            .expect("workspace file")
            .names(),
        vec!["here"],
        "the workspace file gained no entry"
    );

    assert!(super::super::remove(&fx.workspace, "everywhere").expect("remove"));
    assert_eq!(user_file().names(), vec!["remote"]);
}

/// When both files name a server, the workspace entry is the one that runs,
/// so a change is written there and the user entry is left alone.
#[test]
fn a_shared_name_is_edited_in_the_workspace_file() {
    let fx = fixture();
    write_user_file(&fx.home, &stdio_server("github", "user-github"));
    write_workspace_file(&fx.workspace, &stdio_server("github", "repo-github"));
    let user_path = fx.home.join(".stella").join("mcp.toml");

    assert!(super::super::set_granted(&fx.workspace, "github", false).expect("grant"));

    assert_eq!(
        super::super::load_config(&fx.workspace)
            .expect("workspace file")
            .grant_decision("github"),
        Some(Some(false))
    );
    assert_eq!(
        read_mcp_file(&user_path)
            .expect("user file")
            .expect("there")
            .grant_decision("github"),
        Some(None)
    );
}
