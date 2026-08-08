//! What a workspace's own repository declares uninteresting.
//!
//! Two tree walks in this codebase need the same answer: the code-graph walk
//! ([`crate::walk`]), which must not index a checked-in build tree, and the
//! workspace probe in `stella-tools`, which must not attribute one as agent
//! work. They used to approximate it separately — the graph with a hardcoded
//! deny-list, the probe with nothing at all — and the two approximations
//! disagreed with each other and with `git`.
//!
//! This module is the one answer both consume. It lives here rather than in
//! `stella-tools` because `stella-tools` already depends on this crate and
//! not the reverse, so one implementation can serve both without a new crate
//! or a new dependency edge; it costs this crate nothing but `std`.
//!
//! **The resolution is `git`'s own, not a reimplementation.** Per-directory
//! ignore files, negations, `.git/info/exclude`, and the user's global
//! excludesfile all behave exactly as they do at the command line, because
//! the answer *is* git's. A reimplementation would be a second ignore engine
//! to keep correct forever, and the failure mode of a subtly-wrong one is
//! silent: a file quietly indexed, or quietly not.

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

    /// Ask the repository at `root` what it ignores.
    ///
    /// One `git ls-files` per call (~30ms on a 20k-entry tree), and only when
    /// `root` **itself** hosts the repository. That gate is correctness, not
    /// economy: without it a workspace that merely sits *under* someone
    /// else's repository — a scratch directory beneath a `$HOME` dotfiles
    /// repo whose `.gitignore` says `*` — would inherit rules nobody wrote
    /// for it, and the caller would go silently blind.
    ///
    /// Failures are absences: no `git` on the host, or a directory that only
    /// looks like a repository, yields [`Self::none`] and an unfiltered walk.
    /// Degrading to "ignore nothing" is the safe direction — it costs work,
    /// never correctness.
    #[must_use]
    pub fn resolve(root: &Path) -> Self {
        if !root.join(".git").exists() {
            return Self::none();
        }
        let Ok(output) = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
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

    #[test]
    fn none_excludes_nothing() {
        let ignore = WorkspaceIgnore::none();
        assert!(ignore.is_empty());
        assert!(!ignore.excludes("target/debug/app"));
        assert!(!ignore.excludes_dir("target"));
    }
}
