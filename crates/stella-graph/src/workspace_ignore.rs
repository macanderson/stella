//! What a workspace's own repository declares uninteresting.
//!
//! The code-graph walk (this crate's private `walk` module, and the watcher
//! beside it) must not index a checked-in build tree. A hardcoded deny-list
//! used to approximate that answer and disagreed with `git`; this module is
//! the one answer the walks consume instead, over `std` plus `tempfile`.
//!
//! **The resolution is `git`'s own, not a reimplementation.** Per-directory
//! ignore files, negations, `.git/info/exclude`, and the user's global
//! excludesfile all behave exactly as they do at the command line, because
//! the answer *is* git's. A reimplementation would be a second ignore engine
//! to keep correct forever, and the failure mode of a subtly-wrong one is
//! silent: a file quietly indexed, or quietly not.
//!
//! That holds for a workspace that is **not** a repository too. An extracted
//! tarball or a bench container's `/app` can carry a `.gitignore` and no
//! `.git`, and it means what it says — but `git ls-files` needs a repository
//! to answer at all. So the answer stays git's: an empty git dir in a
//! temporary directory, with `root` handed to it as the work tree. Every
//! file under `root` is then untracked, `--ignored --exclude-standard`
//! selects the ones the workspace's own rules exclude, and nothing above
//! `root` is in the work tree to be inherited. The user's global excludes
//! are switched off for that call for the same reason: they are not the
//! workspace's own declaration (#3204).

use std::collections::BTreeSet;
use std::path::Path;

/// The set of workspace-relative paths a repository's ignore rules exclude.
///
/// Resolved once per walk and then queried per path — never re-resolved
/// per entry, which would be one subprocess per file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceIgnore {
    /// Walk-relative paths, with a wholly-ignored directory collapsed to its
    /// topmost entry and a trailing slash (`target/`) — the shape
    /// `git ls-files --directory` emits, which is what lets a prune at
    /// descent be an exact match instead of a scan.
    ignored: BTreeSet<String>,
}

impl WorkspaceIgnore {
    /// Ignore nothing. The honest answer wherever the rules cannot or must
    /// not be consulted: a workspace that is not a repository, a caller that
    /// switched the filter off, or an executor forbidden from spawning a
    /// child process.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Ask what the workspace at `root` declares uninteresting.
    ///
    /// One `git ls-files` per call (~30ms on a 20k-entry tree, preceded by a
    /// `git init` into a temporary directory in the second shape below),
    /// reading only what `root` **itself** declares. That is correctness,
    /// not economy:
    /// a workspace that merely sits *under* someone else's repository — a
    /// scratch directory beneath a `$HOME` dotfiles repo whose `.gitignore`
    /// says `*` — must not inherit rules nobody wrote for it, or the caller
    /// goes silently blind.
    ///
    /// Two shapes qualify, and nothing else is asked at all:
    /// - `root` hosts the repository (`root/.git`) — the ordinary case.
    /// - `root` is no repository but carries its own `root/.gitignore` — an
    ///   extracted tarball, a bench container's `/app`. Answered through a
    ///   temporary empty git dir with `root` as the work tree, which is what
    ///   keeps the resolution git's own; see this module's header.
    ///
    /// Failures are absences: no `git` on the host, no writable temporary
    /// directory, or a directory that only looks like a repository, yields
    /// [`Self::none`] and an unfiltered walk. Degrading to "ignore nothing"
    /// is the safe direction — it costs work, never correctness.
    #[must_use]
    pub fn resolve(root: &Path) -> Self {
        if root.join(".git").exists() {
            return Self::ask_git(root, None);
        }
        if !root.join(".gitignore").exists() {
            return Self::none();
        }
        // Kept alive to the end of the call: dropping the handle deletes the
        // git dir, and `ask_git` still has to run against it.
        let Ok(scratch) = tempfile::TempDir::new() else {
            return Self::none();
        };
        let git_dir = scratch.path().join("detached.git");
        // `--template=` empties the init template, so a user's
        // `init.templateDir` cannot install an `info/exclude` that would
        // then read as the workspace's own declaration.
        let Ok(init) = std::process::Command::new("git")
            .args(["init", "--quiet", "--bare", "--template="])
            .arg(&git_dir)
            .output()
        else {
            return Self::none();
        };
        if !init.status.success() {
            return Self::none();
        }
        Self::ask_git(root, Some(&git_dir))
    }

    /// The one `ls-files` invocation, against either the repository at
    /// `root` (`git_dir` absent) or a detached git dir holding `root` as its
    /// work tree.
    fn ask_git(root: &Path, git_dir: Option<&Path>) -> Self {
        // `ls-files` prints paths relative to the CURRENT DIRECTORY, not to
        // the work tree, so this is what makes the output walk-relative
        // whatever directory the process was launched from. Run from
        // `root/generated` without it, a workspace ignoring `generated/`
        // answers `./` — an entry that matches nothing the walk asks about.
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(root);
        if let Some(git_dir) = git_dir {
            command
                .arg("--git-dir")
                .arg(git_dir)
                .arg("--work-tree")
                .arg(root)
                // The user's global excludes are the user's, not this
                // workspace's. A repository gets them because git gives them
                // to it; a directory that is not one has no relationship to
                // them at all. An empty value disables the lookup outright,
                // including its XDG default.
                .args(["-c", "core.excludesFile="]);
        }
        let Ok(output) = command
            .args([
                "ls-files",
                "-z",
                "--others",
                "--ignored",
                "--directory",
                "--exclude-standard",
            ])
            .output()
        else {
            return Self::none();
        };
        if !output.status.success() {
            return Self::none();
        }
        Self {
            ignored: String::from_utf8_lossy(&output.stdout)
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    /// Whether these rules exclude the workspace-relative path `rel` —
    /// listed outright, or covered by a collapsed `dir/` entry above it.
    #[must_use]
    pub fn excludes(&self, rel: &str) -> bool {
        if self.ignored.is_empty() {
            return false;
        }
        if self.ignored.contains(rel) {
            return true;
        }
        let mut end = rel.len();
        while let Some(pos) = rel[..end].rfind('/') {
            if self.ignored.contains(&rel[..=pos]) {
                return true;
            }
            end = pos;
        }
        false
    }

    /// Whether these rules exclude the directory whose walk-relative path is
    /// `rel`. Separate from [`Self::excludes`] because a wholly-ignored
    /// directory is listed with its trailing slash, and a pruning walk holds
    /// the name without one.
    #[must_use]
    pub fn excludes_dir(&self, rel: &str) -> bool {
        !self.ignored.is_empty()
            && (self.excludes(rel) || self.ignored.contains(&format!("{rel}/")))
    }

    /// Whether anything is ignored at all. `true` for a non-repository, for a
    /// repository that ignores nothing, and wherever consultation was
    /// declined — all of which walk unfiltered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ignored.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_init(root: &Path) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "--quiet"])
            .status()
            .expect("git must be runnable in the test environment");
        assert!(status.success(), "git init failed");
    }

    #[test]
    fn a_repositorys_own_rules_are_resolved_including_a_collapsed_directory() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        std::fs::write(dir.path().join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        std::fs::write(dir.path().join("target/debug/app"), "bin\n").unwrap();
        std::fs::write(dir.path().join("run.log"), "noise\n").unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

        let ignore = WorkspaceIgnore::resolve(dir.path());
        assert!(
            ignore.excludes_dir("target"),
            "the tree is pruned at descent"
        );
        assert!(
            ignore.excludes("target/debug/app"),
            "and anything under it is covered by the collapsed entry"
        );
        assert!(ignore.excludes("run.log"));
        assert!(!ignore.excludes("main.rs"), "source is not excluded");
    }

    /// The gate that keeps an ancestor's rules from blinding a workspace that
    /// is not itself a repository — a scratch dir under a `$HOME` dotfiles
    /// repo ignoring `*` must still be walked in full.
    #[test]
    fn a_non_repository_ignores_nothing_even_inside_a_repository() {
        let outer = tempfile::tempdir().unwrap();
        git_init(outer.path());
        std::fs::write(outer.path().join(".gitignore"), "*\n").unwrap();
        let inner = outer.path().join("scratch");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("main.rs"), "fn main() {}\n").unwrap();

        let ignore = WorkspaceIgnore::resolve(&inner);
        assert!(ignore.is_empty(), "an ancestor's rules are not inherited");
        assert!(!ignore.excludes("main.rs"));
    }

    /// The gap #3204 closed: a workspace that is not a repository but says
    /// what it does not want indexed. An extracted tarball or a bench
    /// container's `/app` used to be walked with only the hardcoded
    /// deny-list, so a `generated/` tree it had declared uninteresting was
    /// indexed in full.
    #[test]
    fn a_non_repositorys_own_rules_are_honoured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "generated/\n*.log\n").unwrap();
        std::fs::create_dir_all(dir.path().join("generated/deep")).unwrap();
        std::fs::write(dir.path().join("generated/deep/gen.py"), "x = 1\n").unwrap();
        std::fs::write(dir.path().join("run.log"), "noise\n").unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();

        let ignore = WorkspaceIgnore::resolve(dir.path());
        assert!(
            ignore.excludes_dir("generated"),
            "the declared tree is pruned at descent"
        );
        assert!(ignore.excludes("generated/deep/gen.py"));
        assert!(ignore.excludes("run.log"));
        assert!(!ignore.excludes("main.rs"), "source is not excluded");
    }

    /// The other half, and why the gate above it still exists: a
    /// non-repository with its own rules is still only asked about itself.
    /// The ancestor ignoring `*` does not reach inside — that case, with
    /// the inner directory declaring nothing at all, is pinned by
    /// [`a_non_repository_ignores_nothing_even_inside_a_repository`].
    #[test]
    fn a_non_repositorys_own_rules_do_not_pull_in_an_ancestors() {
        let outer = tempfile::tempdir().unwrap();
        git_init(outer.path());
        std::fs::write(outer.path().join(".gitignore"), "*\n").unwrap();
        let inner = outer.path().join("scratch");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join(".gitignore"), "generated/\n").unwrap();
        std::fs::create_dir_all(inner.join("generated")).unwrap();
        std::fs::write(inner.join("generated/gen.py"), "x = 1\n").unwrap();
        std::fs::write(inner.join("main.rs"), "fn main() {}\n").unwrap();

        let ignore = WorkspaceIgnore::resolve(&inner);
        assert!(ignore.excludes_dir("generated"), "its own rule applies");
        assert!(
            !ignore.excludes("main.rs"),
            "the ancestor's `*` must not reach inside: it is not this \
             workspace's declaration"
        );
    }

    #[test]
    fn none_excludes_nothing() {
        let ignore = WorkspaceIgnore::none();
        assert!(ignore.is_empty());
        assert!(!ignore.excludes("target/debug/app"));
        assert!(!ignore.excludes_dir("target"));
    }
}
