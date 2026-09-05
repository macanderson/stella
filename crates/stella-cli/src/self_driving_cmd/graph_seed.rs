//! Starting a work unit with the code index the repository already has.
//!
//! A unit of work runs in a git worktree cut from the base ref. The worktree
//! starts with no index. So the first thing a turn did was parse every file in
//! the repository. On the run that measured this, that was 1 650 files and `0
//! unchanged`, three times over for three issues. It came out of the spend
//! meant for the work.
//!
//! The index is a pure function of the tree, and the two trees are one tree. So
//! the repository's index is the answer the worktree was about to compute.
//! Handing it over is a file copy.
//!
//! # Why the worktree keeps its own index
//!
//! The other way is two work units writing one database while a person searches
//! it. That wrecks a code graph for good. `codegraph.db` is the one
//! workspace-private file the state-root redirect leaves in the tree
//! (`stella_store::TREE_ANCHORED_STATE`). That is what gives each worktree its
//! own index. The rest of what a turn writes under `.stella/private/` still
//! follows the redirect into the repository. A session's learning is meant to
//! outlive the worktree. Its index is not.

use std::path::Path;

/// The index this module copies. One spelling for both paths below.
const CODE_GRAPH: &str = "codegraph.db";

/// Copy `parent`'s code graph into `worktree`, so the turn starts warm.
///
/// `Ok(false)` means there was nothing to do. Either the repository has no
/// index, or the worktree already has one. Neither is a failure. A turn with a
/// cold index is slower, never wrong.
pub(super) fn seed_from_parent(parent: &Path, worktree: &Path) -> Result<bool, String> {
    let Some(source) = stella_store::existing_workspace_private_sqlite_path(parent, CODE_GRAPH)
        .map_err(|error| format!("cannot resolve the repository's code graph: {error}"))?
    else {
        return Ok(false);
    };
    let dest = stella_store::workspace_private_sqlite_path(worktree, CODE_GRAPH)
        .map_err(|error| format!("cannot resolve the worktree's code graph: {error}"))?;
    stella_graph::seed_index(&source, &dest).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seed is a **copy**. A write to one index must not reach the other.
    ///
    /// A hard link would pass every check on content, and would still be one
    /// index under two names. So the two are compared as files, not as bytes.
    #[test]
    fn the_worktree_gets_its_own_file_not_the_repositorys() {
        let parent = tempfile::tempdir().expect("repository");
        std::fs::write(parent.path().join("lib.rs"), "pub fn shared() {}\n").expect("source");
        let parent_db = stella_store::workspace_private_sqlite_path(parent.path(), CODE_GRAPH)
            .expect("resolve the repository index");
        stella_graph::CodeGraph::open(parent.path(), &parent_db)
            .expect("open")
            .index_all()
            .expect("index");

        let worktree = tempfile::tempdir().expect("worktree");
        std::fs::write(worktree.path().join("lib.rs"), "pub fn shared() {}\n").expect("source");
        assert!(seed_from_parent(parent.path(), worktree.path()).expect("seed"));

        let seeded = stella_store::workspace_private_sqlite_path(worktree.path(), CODE_GRAPH)
            .expect("resolve the worktree index");
        assert_ne!(seeded, parent_db, "the worktree writes its own file");
        assert!(
            stella_graph::CodeGraph::open(worktree.path(), &seeded)
                .expect("open the seed")
                .indexes_file(&worktree.path().join("lib.rs"))
                .expect("ask"),
            "the seeded index must already hold the repository's rows"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_ne!(
                std::fs::metadata(&seeded).expect("stat").ino(),
                std::fs::metadata(&parent_db).expect("stat").ino(),
                "a hard link is the shared writable index this exists to avoid"
            );
        }
    }

    /// A repository nobody has indexed hands over nothing. It says so rather
    /// than failing the unit of work.
    #[test]
    fn an_unindexed_repository_seeds_nothing() {
        let parent = tempfile::tempdir().expect("repository");
        let worktree = tempfile::tempdir().expect("worktree");
        assert!(!seed_from_parent(parent.path(), worktree.path()).expect("seed"));
    }
}
