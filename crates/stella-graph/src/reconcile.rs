// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Git-aware reconciliation: what changed since this index last saw the
//! repository, and how much of the tree a pass therefore has to touch.
//!
//! # Why this is a reconciliation and not a notification
//!
//! The obvious way to re-index on a merge is a `post-merge` hook installed
//! into `.git/hooks`. That was rejected, and the reason is the whole design:
//! a hook is an **install step**, and an install step is a thing a fresh
//! clone does not have, CI never runs, `git clone --depth` cannot recover,
//! and a user can forget exactly once. A trigger that can be missing is a
//! trigger whose absence is invisible — the index is simply, quietly, wrong.
//!
//! So the trigger here is not an event anyone has to deliver. It is a
//! **comparison against observed state**: the store records the commit it
//! last reconciled against ([`read_head`]), and every writer open asks git
//! what HEAD is now. A merge, a rebase, a `checkout`, a `pull`, a bisect
//! step, or a clone someone handed you all present identically — the recorded
//! sha does not match the current one — and none of them had to tell us.
//! Missing the moment costs latency and never costs correctness, which is
//! precisely the property a hook does not have.
//!
//! # The load-bearing caveat: this is an accelerator, not an oracle
//!
//! A git diff describes **committed** history. It says nothing about the
//! uncommitted working tree, which is where a coding agent spends its entire
//! life. Two consequences, and getting either wrong would corrupt the index:
//!
//! - [`Plan::Unchanged`] does **not** authorize skipping a pass. HEAD sitting
//!   still is the normal state of a session that is editing files. The
//!   content-hash skip in the indexer remains the truth about what needs
//!   re-parsing; this module only ever says *where to look first*.
//! - [`Plan::Scoped`] is a **superset request, not a filter**. The paths it
//!   names are known-dirty and worth doing eagerly; files it does not name may
//!   still be dirty from an uncommitted edit, and are still caught by the
//!   ordinary walk.
//!
//! Everything here degrades to [`Plan::FullWalk`], which is the behaviour the
//! index had before this module existed. A repository with no git, a shallow
//! clone whose old sha was never fetched, a history rewritten out from under
//! us, or a `git` that is not on PATH all land there. That is why the oracle
//! returns [`Option`] rather than an error at every question: there is no
//! failure here that is not simply "scope less confidently".

use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::GraphError;

/// The questions reconciliation needs answered about the repository, as a
/// port (invariant 1) rather than a direct `git` dependency.
///
/// Two reasons this is a trait and not three free functions calling
/// `Command`. It keeps [`plan`] a pure decision over injected answers, so the
/// branch table below is unit-testable without building a git repository per
/// case — including the cases that are awkward to construct for real, like a
/// sha that resolves on one machine and not another. And it leaves room for a
/// non-subprocess implementation (libgit2, or reading `.git/HEAD` directly)
/// without touching a single caller.
///
/// Every method returns [`Option`], never [`Result`]. A repository question
/// this crate cannot answer is never an error — it is a reason to do the
/// safe, dumb, complete thing instead.
pub trait RepoOracle {
    /// The commit HEAD currently resolves to, or `None` when there is no
    /// repository, no commits yet, or no way to ask.
    fn head(&self) -> Option<String>;

    /// Workspace-relative paths that differ between two commits, or `None`
    /// when the range cannot be resolved — an unreachable `from` after a
    /// shallow fetch, a garbage-collected sha, a rewritten history.
    ///
    /// `None` and `Some(vec![])` are **not** the same answer and callers must
    /// not conflate them: the first means "I cannot tell you what changed"
    /// and demands a full walk; the second means "nothing changed" and is a
    /// real, cheap, trustworthy result.
    fn changed_between(&self, from: &str, to: &str) -> Option<Vec<String>>;
}

/// Why a pass could not be narrowed to a path set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullWalkReason {
    /// No commit was ever recorded for this store — a first index, or one
    /// written before this module existed. There is no `from` to diff against.
    NeverReconciled,
    /// A commit is recorded and HEAD has moved, but the range between them
    /// does not resolve: a shallow clone that never fetched the old commit, a
    /// rebase or amend that orphaned it, or a `gc` that collected it.
    HistoryUnavailable,
}

/// What one reconciliation decided a pass should do.
///
/// Read every variant as advice about **ordering and eagerness**, never as
/// permission to skip work — see this module's caveat. The index's
/// correctness rests on the indexer's content hash in all four.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Not a git repository (or git cannot be reached at all). There is no
    /// commit to record and nothing to scope from; the pass proceeds exactly
    /// as it did before this module existed.
    NoRepo,
    /// HEAD is where the store last saw it. Committed history contributes no
    /// dirty paths — which is emphatically not the same as "nothing is
    /// dirty", because the working tree moves without HEAD.
    Unchanged {
        /// The commit both the store and the repository agree on.
        head: String,
    },
    /// HEAD moved and the range resolved. `paths` is the committed change
    /// set: worth indexing and re-embedding first, and not a filter on what
    /// else the pass may touch.
    Scoped {
        /// The commit the store last reconciled against.
        from: String,
        /// The commit it should record once the pass completes.
        to: String,
        /// Workspace-relative paths that differ between the two, including
        /// paths that were deleted (which the caller prunes) and both sides
        /// of a rename.
        paths: Vec<PathBuf>,
    },
    /// The pass must walk the whole tree, because history could not narrow
    /// it. `to` is still recorded afterwards, so the *next* pass can scope.
    FullWalk {
        /// The commit to record once the pass completes, when there is one.
        to: Option<String>,
        /// Why narrowing failed.
        reason: FullWalkReason,
    },
}

impl Plan {
    /// The commit a completed pass should record, if any.
    ///
    /// [`Plan::Unchanged`] returns `None` deliberately: re-stamping a sha
    /// that is already stored is a write for no information, and the write
    /// path is the one place this module can contend with a live session.
    pub fn commit_to_record(&self) -> Option<&str> {
        match self {
            Plan::Scoped { to, .. } => Some(to),
            Plan::FullWalk { to, .. } => to.as_deref(),
            Plan::NoRepo | Plan::Unchanged { .. } => None,
        }
    }

    /// The committed paths to do first, empty when there are none to name.
    pub fn priority_paths(&self) -> &[PathBuf] {
        match self {
            Plan::Scoped { paths, .. } => paths,
            _ => &[],
        }
    }
}

/// Decide what a pass should do, given what the store last recorded and what
/// the repository says now.
///
/// Pure: every repository fact arrives through `oracle`, so this function has
/// no I/O, no clock, and no environment, and the whole branch table is
/// testable against a fake (invariant 2's discipline, applied one crate out
/// from the engine that mandates it).
pub fn plan(stored_head: Option<&str>, oracle: &dyn RepoOracle) -> Plan {
    let Some(current) = oracle.head() else {
        return Plan::NoRepo;
    };
    let Some(stored) = stored_head else {
        return Plan::FullWalk {
            to: Some(current),
            reason: FullWalkReason::NeverReconciled,
        };
    };
    if stored == current {
        return Plan::Unchanged { head: current };
    }
    match oracle.changed_between(stored, &current) {
        Some(paths) => Plan::Scoped {
            from: stored.to_string(),
            to: current,
            paths: paths.into_iter().map(PathBuf::from).collect(),
        },
        None => Plan::FullWalk {
            to: Some(current),
            reason: FullWalkReason::HistoryUnavailable,
        },
    }
}

/// A [`RepoOracle`] backed by the `git` binary on PATH.
///
/// Subprocess rather than a linked library because this crate is otherwise
/// free of a git dependency and the two questions asked here are stable
/// plumbing — `rev-parse` and `diff --name-only` predate every porcelain
/// change git has made — where the cost of being wrong is a [`None`] that
/// falls back to a full walk.
#[derive(Debug, Clone)]
pub struct GitCli {
    root: PathBuf,
}

impl GitCli {
    /// An oracle rooted at a workspace directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Run a git plumbing command, yielding its stdout only on a clean exit.
    ///
    /// A non-zero status is the *expected* signal for "that sha means nothing
    /// here", so it is deflected to [`None`] rather than being reported: a
    /// shallow clone answering "unknown revision" on stderr is an ordinary
    /// day, not a fault worth printing over a user's session.
    fn run(&self, args: &[&str]) -> Option<Vec<u8>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .ok()?;
        output.status.success().then_some(output.stdout)
    }
}

impl RepoOracle for GitCli {
    fn head(&self) -> Option<String> {
        let stdout = self.run(&["rev-parse", "HEAD"])?;
        let head = String::from_utf8(stdout).ok()?.trim().to_string();
        (!head.is_empty()).then_some(head)
    }

    fn changed_between(&self, from: &str, to: &str) -> Option<Vec<String>> {
        // `-z` because git otherwise C-quotes any path with a space, a quote,
        // or a non-ASCII byte in it, and a quoted path does not match the
        // index's stored path — the files most likely to be mangled would be
        // exactly the ones silently dropped from the change set.
        //
        // `--no-renames` because a detected rename reports only the new path,
        // and the OLD path is the row that has to be pruned. Rename detection
        // would leave a deleted file's symbols in the index indefinitely.
        //
        // `--diff-filter=d` excludes nothing on purpose: deletions must be in
        // the set, and the caller distinguishes them by whether the path
        // exists on disk. It is spelled out here only because the temptation
        // to filter to "files that exist" is real and would be a bug.
        let stdout = self.run(&[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            &format!("{from}..{to}"),
        ])?;
        let text = String::from_utf8(stdout).ok()?;
        Some(
            text.split('\0')
                .filter(|entry| !entry.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }
}

/// Absolute paths for a plan's priority set, filtered to those inside `root`.
///
/// Git reports paths relative to the repository root, which is not
/// necessarily the workspace root this index was built against. A path that
/// would escape the workspace is dropped rather than indexed: a file outside
/// the tree is not one this store has any business holding a row for.
///
/// The containment test is done on the **relative** path, before the join,
/// and that is not incidental. [`Path::starts_with`] is a lexical component
/// comparison that does not resolve `..`, so `/ws/../outside.rs` "starts
/// with" `/ws` and a post-join filter would wave it straight through. Testing
/// the components git gave us — no `..`, no root, no drive prefix — is the
/// check that actually holds.
pub fn absolute_priority_paths(root: &Path, plan: &Plan) -> Vec<PathBuf> {
    plan.priority_paths()
        .iter()
        .filter(|rel| is_contained(rel))
        .map(|rel| root.join(rel))
        .collect()
}

/// Is this a plain relative path that stays inside the tree it is joined to?
fn is_contained(rel: &Path) -> bool {
    use std::path::Component;
    rel.components()
        .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

/// The commit this store last reconciled against, or `None` for a store that
/// has never recorded one.
///
/// Absent state is not an error and must not be reported as one: every store
/// written before this table existed answers `None`, and the correct response
/// to that is [`FullWalkReason::NeverReconciled`] — a full walk, which is what
/// those stores were already doing.
pub fn read_head(conn: &Connection) -> Result<Option<String>, GraphError> {
    let stored = conn
        .query_row(
            "SELECT head_sha FROM code_graph_index_state WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(stored)
}

/// Record the commit a completed pass reconciled against.
///
/// Call this **after** the pass commits, never before. Stamping first and
/// crashing mid-pass would leave the store claiming a commit whose changes it
/// never indexed, and the next reconciliation would see an unmoved HEAD and
/// scope nothing — the one way this design can lose a file permanently rather
/// than merely late.
pub fn record_head(conn: &Connection, head: &str) -> Result<(), GraphError> {
    conn.execute(
        "INSERT INTO code_graph_index_state (id, head_sha, reconciled_at) \
         VALUES (1, ?1, ?2) \
         ON CONFLICT(id) DO UPDATE SET \
           head_sha = excluded.head_sha, \
           reconciled_at = excluded.reconciled_at",
        params![head, crate::store::now_unix()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`RepoOracle`] whose every answer is dictated by the test.
    struct FakeRepo {
        head: Option<String>,
        diff: Option<Vec<String>>,
    }

    impl RepoOracle for FakeRepo {
        fn head(&self) -> Option<String> {
            self.head.clone()
        }
        fn changed_between(&self, _from: &str, _to: &str) -> Option<Vec<String>> {
            self.diff.clone()
        }
    }

    fn oracle(head: Option<&str>, diff: Option<Vec<&str>>) -> FakeRepo {
        FakeRepo {
            head: head.map(str::to_string),
            diff: diff.map(|paths| paths.into_iter().map(str::to_string).collect()),
        }
    }

    #[test]
    fn no_head_means_no_repo() {
        assert_eq!(plan(Some("abc"), &oracle(None, None)), Plan::NoRepo);
    }

    #[test]
    fn a_store_that_never_recorded_a_commit_walks_the_tree() {
        assert_eq!(
            plan(None, &oracle(Some("abc"), None)),
            Plan::FullWalk {
                to: Some("abc".to_string()),
                reason: FullWalkReason::NeverReconciled,
            }
        );
    }

    #[test]
    fn an_unmoved_head_is_unchanged_and_records_nothing() {
        let decided = plan(Some("abc"), &oracle(Some("abc"), None));
        assert_eq!(
            decided,
            Plan::Unchanged {
                head: "abc".to_string()
            }
        );
        assert_eq!(decided.commit_to_record(), None);
        assert!(decided.priority_paths().is_empty());
    }

    /// The witness for the whole module: a merge moves HEAD, and the pass is
    /// handed exactly the paths that merge touched.
    #[test]
    fn a_moved_head_scopes_to_the_committed_change_set() {
        let decided = plan(
            Some("old"),
            &oracle(Some("new"), Some(vec!["src/z.rs", "src/a.rs"])),
        );
        assert_eq!(
            decided,
            Plan::Scoped {
                from: "old".to_string(),
                to: "new".to_string(),
                paths: vec![PathBuf::from("src/z.rs"), PathBuf::from("src/a.rs")],
            }
        );
        assert_eq!(decided.commit_to_record(), Some("new"));
        assert_eq!(decided.priority_paths().len(), 2);
    }

    /// `None` from the oracle and an empty diff are different answers, and
    /// conflating them is the bug this asserts against: an unresolvable range
    /// must widen to a full walk, never quietly report "nothing changed".
    #[test]
    fn an_unresolvable_range_widens_rather_than_reporting_no_changes() {
        assert_eq!(
            plan(Some("gone"), &oracle(Some("new"), None)),
            Plan::FullWalk {
                to: Some("new".to_string()),
                reason: FullWalkReason::HistoryUnavailable,
            }
        );
        assert_eq!(
            plan(Some("old"), &oracle(Some("new"), Some(vec![]))),
            Plan::Scoped {
                from: "old".to_string(),
                to: "new".to_string(),
                paths: Vec::new(),
            }
        );
    }

    /// A moved HEAD still records its commit even when the range failed, so
    /// one unresolvable step does not strand every later pass on a full walk.
    #[test]
    fn a_full_walk_still_records_its_commit() {
        let decided = plan(Some("gone"), &oracle(Some("new"), None));
        assert_eq!(decided.commit_to_record(), Some("new"));
    }

    /// Everything above tests the decision against a fake. This tests the
    /// part a fake cannot: that the flags handed to the real `git` binary
    /// actually produce the change set claimed for them. The `-z` framing and
    /// `--no-renames` are reasoning about git's behaviour, and reasoning about
    /// a subprocess's output format is exactly the kind of claim that has to
    /// be run rather than argued.
    mod real_git {
        use super::*;

        fn git(root: &Path, args: &[&str]) {
            let status = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .status()
                .expect("git must be runnable in the test environment");
            assert!(status.success(), "git {args:?} failed");
        }

        fn commit(root: &Path, message: &str) -> String {
            git(root, &["add", "-A"]);
            git(root, &["commit", "--quiet", "-m", message]);
            GitCli::new(root).head().expect("head after commit")
        }

        fn repo() -> tempfile::TempDir {
            let dir = tempfile::tempdir().expect("tempdir");
            git(dir.path(), &["init", "--quiet"]);
            git(dir.path(), &["config", "user.email", "test@example.com"]);
            git(dir.path(), &["config", "user.name", "Test"]);
            dir
        }

        #[test]
        fn a_real_range_reports_added_modified_and_deleted_paths() {
            let dir = repo();
            let root = dir.path();
            std::fs::create_dir_all(root.join("src")).expect("mkdir");
            std::fs::write(root.join("src/keep.rs"), "fn keep() {}\n").expect("write");
            std::fs::write(root.join("src/drop.rs"), "fn drop_me() {}\n").expect("write");
            let first = commit(root, "one");

            std::fs::write(root.join("src/keep.rs"), "fn keep() { () }\n").expect("write");
            std::fs::remove_file(root.join("src/drop.rs")).expect("remove");
            std::fs::write(root.join("src/added.rs"), "fn added() {}\n").expect("write");
            let second = commit(root, "two");

            let mut changed = GitCli::new(root)
                .changed_between(&first, &second)
                .expect("range resolves");
            changed.sort();
            assert_eq!(
                changed,
                vec![
                    "src/added.rs".to_string(),
                    "src/drop.rs".to_string(),
                    "src/keep.rs".to_string(),
                ],
                "a deleted path must be in the set — it is the row to prune"
            );
        }

        /// `--no-renames` is load-bearing: with rename detection on, git
        /// reports only the destination, and the source path's rows would sit
        /// in the index forever pointing at a file that no longer exists.
        #[test]
        fn a_rename_reports_both_sides_so_the_old_row_can_be_pruned() {
            let dir = repo();
            let root = dir.path();
            std::fs::create_dir_all(root.join("src")).expect("mkdir");
            std::fs::write(root.join("src/before.rs"), "fn subject() {}\n").expect("write");
            let first = commit(root, "one");

            git(root, &["mv", "src/before.rs", "src/after.rs"]);
            let second = commit(root, "two");

            let mut changed = GitCli::new(root)
                .changed_between(&first, &second)
                .expect("range resolves");
            changed.sort();
            assert_eq!(
                changed,
                vec!["src/after.rs".to_string(), "src/before.rs".to_string()]
            );
        }

        /// A path git would C-quote in its default output must come back
        /// verbatim, because a quoted path matches no row in the index and the
        /// file would be silently dropped from every change set it appears in.
        #[test]
        fn an_unusual_path_survives_the_framing_unquoted() {
            let dir = repo();
            let root = dir.path();
            let awkward = "src/a file—with spaces.rs";
            std::fs::create_dir_all(root.join("src")).expect("mkdir");
            std::fs::write(root.join(awkward), "fn odd() {}\n").expect("write");
            let first = commit(root, "one");
            std::fs::write(root.join(awkward), "fn odd() { () }\n").expect("write");
            let second = commit(root, "two");

            assert_eq!(
                GitCli::new(root)
                    .changed_between(&first, &second)
                    .expect("range resolves"),
                vec![awkward.to_string()]
            );
        }

        /// An unresolvable range is `None` — the answer that widens to a full
        /// walk — and emphatically not an empty change set.
        #[test]
        fn an_unknown_commit_yields_none_rather_than_an_empty_set() {
            let dir = repo();
            let root = dir.path();
            std::fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
            let head = commit(root, "one");
            assert_eq!(
                GitCli::new(root).changed_between("0".repeat(40).as_str(), &head),
                None
            );
        }

        /// Outside a repository every question answers `None`, which is what
        /// makes [`Plan::NoRepo`] reachable rather than theoretical.
        #[test]
        fn a_directory_that_is_not_a_repository_has_no_head() {
            let dir = tempfile::tempdir().expect("tempdir");
            assert_eq!(GitCli::new(dir.path()).head(), None);
        }
    }

    #[test]
    fn priority_paths_outside_the_workspace_are_dropped() {
        let root = Path::new("/ws");
        let decided = Plan::Scoped {
            from: "a".to_string(),
            to: "b".to_string(),
            paths: vec![
                PathBuf::from("src/a.rs"),
                // Lexically "starts with" /ws once joined — the case a
                // post-join `starts_with` filter lets through.
                PathBuf::from("../outside.rs"),
                PathBuf::from("nested/../../escape.rs"),
                PathBuf::from("/etc/passwd"),
            ],
        };
        assert_eq!(
            absolute_priority_paths(root, &decided),
            vec![PathBuf::from("/ws/src/a.rs")]
        );
    }
}
