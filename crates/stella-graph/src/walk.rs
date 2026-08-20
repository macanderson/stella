//! Directory walk that yields indexable source files.
//!
//! L-S5 makes ripgrep semantics (gitignore-aware, hidden-excluded) the
//! contract for search tooling. This module implements a
//! **deny-list approximation** rather than pulling ripgrep's `ignore` crate:
//! it skips hidden directories (leading `.`), the usual build/vendor/generated
//! caches (`target`, `node_modules`, `dist`, `build`, `out`, `.next`,
//! `dist-standalone`, `vendor`, `__pycache__`, `venv`, …), and never follows
//! symlinked directories (cycle safety), then keeps only files whose
//! extension [`Language::from_path`] recognizes.
//!
//! This is the cheap, structural half of generated/minified exclusion
//! (issue #272) — a whole directory of build output never even gets opened.
//! The other half — `.gitattributes` `linguist-generated=true`, the
//! `*.min.*` filename convention, and minified-content sniffing for files
//! that live outside any denied directory — runs per-file once content is
//! already in hand, in [`crate::generated`] via `crate::store::index_one`.
//!
//! **The ignore-discipline gap is closed** (#2360). Custom `.gitignore`
//! patterns — per-directory ignore files, negations, glob rules,
//! `.git/info/exclude`, the global excludesfile — are now honored, because
//! [`crate::workspace_ignore`] asks `git` rather than reimplementing it. The
//! deny-list below survives *beside* that answer rather than under it: it is
//! what keeps a workspace that is **not** a repository (or one whose build
//! output is checked in on purpose) from indexing a vendored bundle, which
//! no ignore file would have said anything about. Two filters, two jobs — the
//! rules say what this repository does not track, the deny-list says what is
//! never worth parsing.

use std::path::{Path, PathBuf};

use crate::lang::Language;
use crate::workspace_ignore::WorkspaceIgnore;

/// Directory names that never contain first-party source worth indexing.
/// `dist`/`build`/`out`/`node_modules`/`vendor` are classic build/vendor
/// caches; `.next` and `dist-standalone` are generated-bundle directories
/// specifically called out by issue #272 (a checked-in minified
/// `dist-standalone/*.mjs` bundle was polluting recall with zero-signal
/// frames).
const DENY_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "dist-standalone",
    "build",
    "out",
    ".next",
    "__pycache__",
    "venv",
    ".venv",
    "coverage",
    "vendor",
];

/// Whether a directory should be skipped: hidden (leading `.`) or on the
/// build/vendor deny-list.
fn is_denied_dir(name: &str) -> bool {
    name.starts_with('.') || DENY_DIRS.contains(&name)
}

/// Whether `dir` is a checkout of its own — a nested repository, a submodule,
/// or a linked git worktree (where `.git` is a *file* pointing at the real
/// gitdir, which is why this tests existence rather than directory-ness).
///
/// Such a directory is never walked. It belongs to a different workspace: its
/// ignore rules are its own and this walk resolved somebody else's
/// ([`WorkspaceIgnore`] is resolved once, at `root`), and `stella` launched
/// inside it indexes it into its *own* `.stella/private/codegraph.db`. Walking
/// it here would copy every one of its files into the parent's index — a
/// second, stale, unowned copy of a tree that already has an index, and on a
/// machine with several worktrees checked out under one directory, one copy
/// per worktree.
///
/// The hidden-directory rule above already covers the common layout
/// (`.claude/worktrees/…`, `.git/…`); this covers a worktree parked at a
/// visible path, which is the layout `git worktree add ../feature-x` produces
/// when someone puts it inside the tree instead of beside it.
fn is_other_checkout(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Whether a path *below `root`* is one the index should not hold — excluded
/// by the repository's own rules (`ignore`), or on the deny-list the walk
/// applies. Used to filter live watcher events. Only the portion under `root`
/// is examined, so a workspace that itself lives inside a hidden or vendor
/// directory (e.g. `~/.cache/proj`) is not wrongly excluded. A path outside
/// `root` is treated as ignored.
///
/// `ignore` is passed in rather than resolved here because this runs once per
/// watcher event: resolving would be one `git` invocation per saved file.
pub(crate) fn rel_is_ignored(root: &Path, path: &Path, ignore: &WorkspaceIgnore) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true;
    };
    if !ignore.is_empty()
        && let Some(rel) = rel.to_str()
        && ignore.excludes(rel)
    {
        return true;
    }
    rel.components().any(|component| match component {
        std::path::Component::Normal(name) => {
            let name = name.to_string_lossy();
            name.starts_with('.') || DENY_DIRS.contains(&name.as_ref())
        }
        _ => false,
    })
}

/// Walk `root` and return every indexable source file (absolute paths).
/// Iterative (explicit stack) so deeply-nested trees cannot overflow the
/// call stack. Unreadable directories are skipped, never fatal.
///
/// The repository's ignore rules are resolved **once** for the whole walk and
/// threaded down the stack as a walk-relative path, which is what keeps the
/// cost at one subprocess per index pass rather than one per directory.
pub(crate) fn walk_indexable(root: &Path) -> Vec<PathBuf> {
    walk_indexable_with(root, &WorkspaceIgnore::resolve(root))
}

/// [`walk_indexable`] against an already-resolved rule set — the seam the
/// tests use to walk a fixture under explicit rules without a repository.
fn walk_indexable_with(root: &Path, ignore: &WorkspaceIgnore) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];

    while let Some((dir, rel)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let rel_child = if rel.is_empty() {
                name.to_string()
            } else {
                format!("{rel}/{name}")
            };
            // Symlinks report as neither is_dir nor is_file here, so they are
            // ignored — no symlink-cycle risk, no double-indexing.
            if file_type.is_dir() {
                let child = entry.path();
                if !is_denied_dir(&name)
                    && !ignore.excludes_dir(&rel_child)
                    && !is_other_checkout(&child)
                {
                    stack.push((child, rel_child));
                }
            } else if file_type.is_file() {
                if ignore.excludes(&rel_child) {
                    continue;
                }
                let path = entry.path();
                if Language::from_path(&path).is_some()
                    || crate::storage::indexes_without_language(&path)
                {
                    out.push(path);
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Issue #272: a checked-in minified bundle under `dist-standalone/`
    /// (found nested inside a monorepo app directory, not at the workspace
    /// root) must never reach the indexable set.
    #[test]
    fn dist_standalone_and_next_directories_are_never_walked() {
        let ws = TempDir::new().unwrap();
        let root = ws.path();
        fs::create_dir_all(root.join("apps/cli/dist-standalone")).unwrap();
        fs::write(
            root.join("apps/cli/dist-standalone/oxagen.mjs"),
            "function refresh() {}\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("apps/web/.next/static")).unwrap();
        fs::write(
            root.join("apps/web/.next/static/chunk.js"),
            "function chunk() {}\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("apps/cli/src")).unwrap();
        fs::write(root.join("apps/cli/src/main.rs"), "fn main() {}\n").unwrap();

        let files = walk_indexable(root);
        assert_eq!(files.len(), 1, "only real source is walked: {files:?}");
        assert!(files[0].ends_with("apps/cli/src/main.rs"));
    }

    /// **The worktree witness.** A checkout parked at a visible path inside
    /// the workspace — a linked git worktree (`.git` is a *file* there), a
    /// nested clone, a submodule — is not walked into the parent's index.
    ///
    /// Without this, launching `stella` in a repository that keeps its
    /// worktrees inside itself indexes every worktree's copy of every file
    /// into the one index, and a search then ranks the same function five
    /// times over, in five trees, four of which the user is not in.
    #[test]
    fn a_nested_checkout_or_worktree_is_never_walked() {
        let ws = TempDir::new().unwrap();
        let root = ws.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

        // A linked worktree: `.git` is a file pointing at the real gitdir.
        fs::create_dir_all(root.join("worktrees/feature/src")).unwrap();
        fs::write(
            root.join("worktrees/feature/.git"),
            "gitdir: /elsewhere/.git/worktrees/feature\n",
        )
        .unwrap();
        fs::write(root.join("worktrees/feature/src/main.rs"), "fn main() {}\n").unwrap();

        // A nested clone: `.git` is a directory.
        fs::create_dir_all(root.join("nested/.git")).unwrap();
        fs::create_dir_all(root.join("nested/src")).unwrap();
        fs::write(root.join("nested/src/lib.rs"), "pub fn nested() {}\n").unwrap();

        let files = walk_indexable_with(root, &WorkspaceIgnore::none());
        assert_eq!(
            files.len(),
            1,
            "only this checkout's own source is walked: {files:?}"
        );
        assert!(files[0].ends_with("src/main.rs"));
    }

    /// The gap this module documented for two releases: a `.gitignore` rule
    /// naming something the deny-list has never heard of (a generated
    /// directory with a project-specific name, a checked-out sibling
    /// checkout) was indexed anyway. It is honored now — and, because the
    /// answer is `git`'s own, so are the patterns a hand-rolled matcher
    /// usually gets wrong: a glob, and a negation re-admitting one file.
    #[test]
    fn a_repositorys_own_ignore_rules_are_honored_including_globs_and_negations() {
        let ws = TempDir::new().unwrap();
        let root = ws.path();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "--quiet"])
            .status()
            .expect("git must be runnable in the test environment");
        assert!(status.success(), "git init failed");
        fs::write(
            root.join(".gitignore"),
            // None of these three names is on DENY_DIRS.
            "generated-sdk/\n*.pb.rs\n!keep.pb.rs\n",
        )
        .unwrap();

        fs::create_dir_all(root.join("generated-sdk")).unwrap();
        fs::write(root.join("generated-sdk/client.rs"), "fn c() {}\n").unwrap();
        fs::write(root.join("schema.pb.rs"), "fn s() {}\n").unwrap();
        fs::write(root.join("keep.pb.rs"), "fn k() {}\n").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

        let mut files: Vec<String> = walk_indexable(root)
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        files.sort();
        assert_eq!(
            files,
            vec!["keep.pb.rs".to_string(), "src/main.rs".to_string()],
            "the ignored directory and the glob are pruned; the negation is re-admitted"
        );
    }

    /// The deny-list is not superseded by the rules — it is the answer where
    /// there are no rules to consult. A workspace that is not a repository
    /// must still skip a vendored bundle.
    #[test]
    fn the_deny_list_still_applies_when_there_are_no_rules_to_consult() {
        let ws = TempDir::new().unwrap();
        let root = ws.path();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "var x = 1;\n").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

        let files = walk_indexable_with(root, &WorkspaceIgnore::none());
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].ends_with("src/main.rs"));
    }

    /// A watcher event for a gitignored path must not re-index it — the same
    /// question the walk answers, asked one path at a time.
    #[test]
    fn a_watcher_event_for_an_ignored_path_is_not_relevant() {
        let ws = TempDir::new().unwrap();
        let root = ws.path();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "--quiet"])
            .status()
            .expect("git must be runnable in the test environment");
        assert!(status.success(), "git init failed");
        fs::write(root.join(".gitignore"), "generated-sdk/\n").unwrap();
        fs::create_dir_all(root.join("generated-sdk")).unwrap();
        fs::write(root.join("generated-sdk/client.rs"), "fn c() {}\n").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

        let ignore = WorkspaceIgnore::resolve(root);
        assert!(rel_is_ignored(
            root,
            &root.join("generated-sdk/client.rs"),
            &ignore
        ));
        assert!(!rel_is_ignored(root, &root.join("src/main.rs"), &ignore));
    }

    #[test]
    fn is_denied_dir_covers_generated_bundle_directories() {
        for name in [
            "dist",
            "dist-standalone",
            "build",
            "out",
            "vendor",
            "node_modules",
        ] {
            assert!(is_denied_dir(name), "{name} should be denied");
        }
        assert!(is_denied_dir(".next"), ".next is denied (hidden + listed)");
        assert!(!is_denied_dir("src"));
    }
}
