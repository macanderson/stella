//! **Witness (#4394).** The index build and every graph reader must resolve
//! `codegraph.db` to the same file under a redirected workspace state root.
//!
//! `STELLA_WORKSPACE_STATE_ROOT` (`stella_home::WORKSPACE_STATE_ROOT_ENV`) is
//! what lets a `stella self-driving` turn run in a throwaway worktree while its
//! private state — the reflection log, the telemetry store, the code graph —
//! outlives it in the repository. The build resolved through the store and
//! honoured the redirect; `search::codegraph::graph_db_path` joined the path by
//! hand and did not. So a redirected session indexed one database and mounted
//! its watcher, its CGP host and its Command Deck on an empty one at the
//! literal path, and the agent's own edits were never indexed where the queries
//! looked.
//!
//! Its own test binary because it mutates the process environment: the variable
//! under test is read from `std::env` deep inside the store's resolver, and a
//! test that set it in a shared binary would be mutating state every
//! concurrently-running test in that binary can observe.

use std::path::Path;

/// The path a reader resolves, or a panic naming why it could not.
fn reader_sees(root: &Path) -> Option<std::path::PathBuf> {
    stella_tools::search::codegraph::graph_db_path(root).expect("the reader resolves")
}

/// The two resolvers agree — under a redirect, and without one — and neither
/// asking creates a workspace that was not there.
///
/// Fails on the base, where `graph_db_path` answers
/// `<worktree>/.stella/private/codegraph.db` while the build writes
/// `<state root>/.stella/private/codegraph.db`.
#[test]
fn the_watcher_mounts_the_database_the_build_writes() {
    let worktree = tempfile::tempdir().expect("worktree");
    let state_root = tempfile::tempdir().expect("state root");
    // Canonical, because the store's resolver returns canonical paths and on
    // macOS a temporary directory is reached through a symlink (`/var` ->
    // `/private/var`) — comparing the two spellings would fail on the
    // filesystem's shape rather than on the behaviour under test.
    let worktree_real = worktree
        .path()
        .canonicalize()
        .expect("worktree canonicalizes");
    let state_root_real = state_root
        .path()
        .canonicalize()
        .expect("state root canonicalizes");

    // Read-only, before anything else: a workspace nobody has indexed answers
    // "no index" and is left exactly as it was found. A reader that resolved
    // through the *writable* path would satisfy every other assertion here and
    // still create `.stella/` in whatever directory a Command Deck search was
    // typed in.
    assert_eq!(reader_sees(&worktree_real), None);
    assert!(
        !worktree_real.join(".stella").exists(),
        "resolving a read must never create the workspace store"
    );

    // The control, in the same process: with no redirect the two resolvers
    // agree trivially, so a pass below has to come from the redirect being
    // honoured rather than from both answers being the same by default.
    let plain_build = stella_store::workspace_private_sqlite_path(&worktree_real, "codegraph.db")
        .expect("the build resolves a path with no redirect");
    std::fs::write(&plain_build, b"").expect("the build leaves an index behind");
    assert_eq!(reader_sees(&worktree_real), Some(plain_build.clone()));
    assert!(
        plain_build.starts_with(&worktree_real),
        "with no redirect the index belongs under the workspace, got {}",
        plain_build.display()
    );

    // SAFETY: this test binary holds exactly one test, so nothing else in the
    // process is reading or writing the environment.
    unsafe {
        std::env::set_var(stella_home::WORKSPACE_STATE_ROOT_ENV, &state_root_real);
    }

    let build = stella_store::workspace_private_sqlite_path(&worktree_real, "codegraph.db")
        .expect("the build resolves a path under the redirect");
    std::fs::write(&build, b"").expect("the redirected build leaves an index behind");

    assert_eq!(
        reader_sees(&worktree_real),
        Some(build.clone()),
        "a redirected session must build and read one database, not two"
    );
    assert!(
        build.starts_with(&state_root_real),
        "the redirect must be what decides the location, got {}",
        build.display()
    );
    assert_ne!(
        build, plain_build,
        "the literal path under the worktree — which still exists, and still holds the \
         unredirected index — is the answer this witness exists to reject"
    );
}
