//! **Witness.** The code graph stays with the tree it describes. Everything
//! else under `.stella/private/` follows the state-root redirect.
//!
//! `STELLA_WORKSPACE_STATE_ROOT` (`stella_home::WORKSPACE_STATE_ROOT_ENV`)
//! exists for a turn running in a throwaway git worktree. It keeps what the
//! turn *learned* in the repository that outlives it. That is the reflection
//! log, the telemetry store, and the recalled context.
//!
//! `codegraph.db` is not learning. It is a parsed image of the files under the
//! workspace root. So a redirect pointed one tree's index at another tree's
//! state. A person searching their own checkout got answers about a tree they
//! were not looking at. Two work units at once wrote one database, which wrecks
//! a code graph for good.
//!
//! This is its own test binary because it sets an environment variable. The
//! resolver reads that variable from `std::env`. Setting it in a shared binary
//! would change what every test running beside it sees.

use std::path::Path;

/// Where the resolver puts `name` for the workspace at `root`.
fn resolved(root: &Path, name: &str) -> std::path::PathBuf {
    stella_store::workspace_private_sqlite_path(root, name).expect("the resolver answers")
}

/// One redirect, two answers. The index stays in the tree. The telemetry store
/// moves.
///
/// It fails on a base where the redirect moves both, and the worktree's index
/// is written over the one in the state root.
#[test]
fn the_code_graph_stays_with_the_tree_while_the_store_follows_the_redirect() {
    let worktree = tempfile::tempdir().expect("worktree");
    let state_root = tempfile::tempdir().expect("state root");
    // The resolver hands back real paths. On macOS a temp directory is reached
    // through a symlink (`/var` -> `/private/var`). Two spellings of one
    // directory would fail on the shape of the disk, not on the rule at hand.
    let worktree_real = worktree.path().canonicalize().expect("worktree resolves");
    let state_root_real = state_root
        .path()
        .canonicalize()
        .expect("state root resolves");

    // The control, in the same process. With no redirect both files land under
    // the worktree. So a pass below has to come from the redirect being read,
    // not from two answers agreeing by default.
    let plain_graph = resolved(&worktree_real, "codegraph.db");
    let plain_store = resolved(&worktree_real, "store.db");
    assert!(plain_graph.starts_with(&worktree_real), "{plain_graph:?}");
    assert!(plain_store.starts_with(&worktree_real), "{plain_store:?}");

    // SAFETY: this binary holds one test. Nothing else in the process reads or
    // writes the environment.
    unsafe {
        std::env::set_var(stella_home::WORKSPACE_STATE_ROOT_ENV, &state_root_real);
    }

    let graph = resolved(&worktree_real, "codegraph.db");
    let store = resolved(&worktree_real, "store.db");

    assert_eq!(
        graph,
        plain_graph,
        "the code graph describes the tree at {}, so the redirect must not move it",
        worktree_real.display()
    );
    assert!(
        store.starts_with(&state_root_real),
        "a session's telemetry must outlive the worktree, got {}",
        store.display()
    );
    assert!(
        stella_store::TREE_ANCHORED_STATE.contains(&"codegraph.db"),
        "the rule is declared where a caller can read it"
    );
}
