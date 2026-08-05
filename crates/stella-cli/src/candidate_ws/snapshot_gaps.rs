//! What a candidate snapshot leaves behind.
//!
//! The snapshot is a *git view* of the tree, not a copy of it: `git worktree
//! add` checks out HEAD, the uncommitted tracked delta is applied over that,
//! and untracked files ride in from `git ls-files --others --exclude-standard`
//! — which excludes ignored paths by construction. So everything matched by
//! `.gitignore` is absent from every candidate: `node_modules/`, `.venv/`, a
//! downloaded dataset, a compiled artifact the tests exec.
//!
//! For the tracked delta that is exactly right; ignored bytes are derived
//! state, and copying `target/` N times to run N candidates would cost more
//! than the fan-out. What is wrong is that the omission is *invisible from
//! inside*. The candidate sees a plausible checkout, runs the project's own
//! test command, and gets `Cannot find module` — a failure that reads as its
//! own mistake, in a tree where the fix (reinstall) is not obviously available
//! and the real cause is not observable at all.
//!
//! This module names the gap. It does not close it: closing it is a workspace
//! substrate that copies whole trees instead of asking git what belongs in
//! one, which is a different adapter behind the same port.

use std::path::Path;

/// How many entries a report may name. A diagnostic must not itself become a
/// context-flooding wall of text on a tree that ignores ten thousand log
/// files — but a cap that hides its own existence turns "here is what's
/// missing" into a quiet lie, so truncation is stated in the list it
/// truncates (see [`omitted_ignored_paths`]).
const MAX_REPORTED: usize = 20;

/// Ignored paths that exist in the real tree and are absent from every
/// candidate snapshot, workspace-root-relative, as a **display list**.
///
/// `--directory` is what makes this affordable to report: a wholly-ignored
/// directory collapses to one `dir/` entry instead of enumerating its
/// contents, so `node_modules` costs one line rather than forty thousand.
///
/// Scoped to the workspace root the same way the untracked overlay is — a
/// path above the session root is not in the candidate's tree under any
/// substrate, so naming it would describe a gap the candidate cannot act on.
///
/// Best-effort by construction. This is a diagnostic about a snapshot that has
/// already been built successfully; a tree we cannot describe is still a tree
/// that works, so every failure path yields an empty list rather than
/// propagating. Over [`MAX_REPORTED`] entries the list's final element says
/// how many were withheld.
pub(super) async fn omitted_ignored_paths(toplevel: &Path, root_rel: &Path) -> Vec<String> {
    let listing = match super::git(
        toplevel,
        &[
            "-c",
            "core.quotePath=false",
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
    )
    .await
    {
        Ok(listing) => listing,
        Err(_) => return Vec::new(),
    };

    let mut found: Vec<String> = Vec::new();
    let mut withheld = 0usize;
    for rel in listing.split('\0').filter(|p| !p.is_empty()) {
        let Ok(ws_rel) = Path::new(rel).strip_prefix(root_rel) else {
            continue;
        };
        let ws_rel = ws_rel.to_string_lossy();
        if ws_rel.is_empty() {
            continue;
        }
        if found.len() < MAX_REPORTED {
            found.push(ws_rel.into_owned());
        } else {
            withheld += 1;
        }
    }
    if withheld > 0 {
        found.push(format!("… and {withheld} more ignored path(s)"));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Build a real repo with one commit, a tracked file, and ignored state.
    async fn repo_with_ignored_state() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t.invalid"],
            vec!["config", "user.name", "t"],
        ] {
            super::super::git(root, &args).await.expect("git setup");
        }
        std::fs::write(root.join(".gitignore"), "node_modules/\n*.log\n").expect("write ignore");
        std::fs::write(root.join("src.rs"), "fn main() {}").expect("write src");
        std::fs::create_dir_all(root.join("node_modules/left-pad")).expect("mkdir");
        std::fs::write(root.join("node_modules/left-pad/index.js"), "x").expect("write dep");
        std::fs::write(root.join("build.log"), "noise").expect("write log");
        super::super::git(root, &["add", "-A"]).await.expect("add");
        super::super::git(root, &["commit", "-q", "-m", "init", "--no-gpg-sign"])
            .await
            .expect("commit");
        dir
    }

    /// The whole point: the dependency directory the tests need is reported,
    /// and reported as ONE entry rather than one per vendored file.
    #[tokio::test]
    async fn an_ignored_dependency_directory_is_named_once() {
        let repo = repo_with_ignored_state().await;
        let found = omitted_ignored_paths(repo.path(), Path::new("")).await;

        assert!(
            found.iter().any(|p| p.starts_with("node_modules")),
            "the dependency dir a candidate cannot build without: {found:?}"
        );
        assert_eq!(
            found
                .iter()
                .filter(|p| p.starts_with("node_modules"))
                .count(),
            1,
            "--directory must collapse it, not enumerate it: {found:?}"
        );
        assert!(
            found.iter().any(|p| p == "build.log"),
            "an ignored file is omitted too: {found:?}"
        );
    }

    /// Tracked files are in the snapshot, so naming them would be false.
    #[tokio::test]
    async fn tracked_files_are_not_reported_as_missing() {
        let repo = repo_with_ignored_state().await;
        let found = omitted_ignored_paths(repo.path(), Path::new("")).await;
        assert!(
            !found.iter().any(|p| p == "src.rs" || p == ".gitignore"),
            "tracked content is present in the snapshot: {found:?}"
        );
    }

    /// A diagnostic must never be able to fail a snapshot that is otherwise
    /// sound — the candidate can still run, it just runs uninformed.
    #[tokio::test]
    async fn a_non_repository_reports_nothing_instead_of_failing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            omitted_ignored_paths(dir.path(), Path::new(""))
                .await
                .is_empty()
        );
    }

    /// Paths above the session root describe a gap the candidate could not act
    /// on even if it knew, so they are not its business.
    #[tokio::test]
    async fn only_paths_under_the_session_root_are_reported() {
        let repo = repo_with_ignored_state().await;
        std::fs::create_dir_all(repo.path().join("sub")).expect("mkdir sub");
        std::fs::write(repo.path().join("sub/inner.log"), "x").expect("write");

        let found = omitted_ignored_paths(repo.path(), &PathBuf::from("sub")).await;
        assert!(
            found.iter().any(|p| p == "inner.log"),
            "reported relative to the session root: {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.contains("node_modules")),
            "a sibling of the session root is out of scope: {found:?}"
        );
    }
}
