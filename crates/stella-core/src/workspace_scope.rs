// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a session may **change** things — the pure decision half.
//!
//! # The asymmetry, stated first
//!
//! Reading and writing are governed differently here, on purpose:
//!
//! - **Reads are unrestricted.** An agent asked to fix a build needs the
//!   system headers, the toolchain, the crate source under `~/.cargo`, the
//!   error message's cited file. A read boundary drawn tightly enough to be
//!   meaningful is drawn tightly enough to make ordinary work fail, and a
//!   read cannot damage the user's tree.
//! - **Writes are confined.** Creating, editing, deleting or executing a
//!   script outside the session's own tree is what turns a bad turn into lost
//!   work, and it is never something a correct turn needs.
//!
//! There is exactly one exception to "reads are unrestricted", and it is
//! [`DENIED_SUBPATH`] below.
//!
//! # No I/O (invariant #2)
//!
//! Every function here is a pure predicate over already-resolved, absolute,
//! lexically-normalized paths. Resolution — canonicalizing, following or
//! refusing symlinks, opening a descriptor — belongs to the caller
//! (`stella_tools::rootfd` for the file tools). That split is what makes this
//! property-testable, and it is also the honest boundary: this module decides
//! *policy*, and cannot by itself close a TOCTOU race.
//!
//! # Why worktrees are denied for reading too
//!
//! A git worktree created inside the project is a second checkout of the same
//! repository. An agent that can read it sees a parallel copy of every file it
//! is working on, at a different revision — so a search returns two answers
//! for one question, a read can return the *other* branch's version of the
//! file being edited, and a fan-out worker can silently grade itself against a
//! sibling's tree. None of that is a security problem; all of it is a
//! correctness problem, and it is invisible when it happens.
//!
//! The fix is structural rather than heuristic: worktrees always live at one
//! known path ([`DENIED_SUBPATH`]), and that path is denied outright. A
//! session that IS a worktree passes its own root as the scope root, at which
//! point the worktree is simply the workspace and nothing about it is denied —
//! the denial is about *other* trees, never about the one you are in.

use std::path::{Component, Path, PathBuf};

/// Where every git worktree Stella creates must live, relative to the project
/// root, and the one path denied even to reads.
///
/// One fixed location rather than a pattern, because "is this directory a
/// worktree?" is not a question a pure predicate can answer — it requires
/// reading `.git` — and a heuristic that guesses wrong in the permissive
/// direction denies nothing. Writing every worktree to one known place turns
/// the question into a path comparison.
pub const DENIED_SUBPATH: &str = ".stella/worktrees";

/// What a scope decided about one path.
///
/// A named enum rather than a `bool` because the two refusals want different
/// sentences: a caller outside the scope should be told what the scope *is*,
/// while a caller reaching into a worktree should be told that the tree is
/// another session's and that entering it is the supported move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeDecision {
    /// The operation is inside the scope.
    Allowed,
    /// The path lies outside every allowed root.
    OutsideScope,
    /// The path is inside an allowed root but under [`DENIED_SUBPATH`] — a
    /// worktree belonging to another session.
    DeniedWorktree,
}

impl ScopeDecision {
    /// Whether the operation may proceed.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, ScopeDecision::Allowed)
    }
}

/// The directories one session may write to.
///
/// Constructed once per session and never mutated by a tool: widening it is an
/// operator action (`--allow-dir`, `stella.toml`, `/add-dir`), never something
/// the model can ask for. That is the whole reason this is a value handed
/// *to* the tools rather than a setting they read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteScope {
    /// Allowed roots, absolute and normalized. The session root is always
    /// first; operator-added directories follow in declaration order.
    roots: Vec<PathBuf>,
}

impl WriteScope {
    /// A scope over `session_root` alone — the default every session starts
    /// from, and what an agent gets with no configuration at all.
    #[must_use]
    pub fn new(session_root: impl Into<PathBuf>) -> Self {
        Self {
            roots: vec![normalize(&session_root.into())],
        }
    }

    /// The scope widened by `extra` operator-supplied directories.
    ///
    /// Order is preserved and duplicates are dropped, so the same directory
    /// named by both `stella.toml` and `--allow-dir` is one root rather than
    /// two. A directory that is already inside an existing root is dropped
    /// entirely: it adds no permission, and keeping it would make the
    /// rendered scope misleading about how many places a session can write.
    #[must_use]
    pub fn with_additional(mut self, extra: impl IntoIterator<Item = PathBuf>) -> Self {
        for candidate in extra {
            let candidate = normalize(&candidate);
            if self
                .roots
                .iter()
                .any(|root| is_within(&candidate, root) || root == &candidate)
            {
                continue;
            }
            self.roots.push(candidate);
        }
        self
    }

    /// The session root — the first entry, always present.
    #[must_use]
    pub fn session_root(&self) -> &Path {
        // `roots` is non-empty by construction: `new` seeds it and nothing
        // removes entries.
        self.roots.first().map_or(Path::new(""), PathBuf::as_path)
    }

    /// Every allowed root, session root first.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Whether `path` may be created, modified, deleted, or executed as a
    /// script.
    ///
    /// `path` must be absolute and already resolved by the caller; a relative
    /// path is [`ScopeDecision::OutsideScope`] rather than being joined
    /// against a guessed root, because guessing which root a relative path
    /// belongs to is how a confinement check silently permits the wrong tree.
    #[must_use]
    pub fn may_write(&self, path: &Path) -> ScopeDecision {
        let path = normalize(path);
        if !path.is_absolute() {
            return ScopeDecision::OutsideScope;
        }
        // Denial is checked FIRST and wins: a worktree lives inside an allowed
        // root by construction, so an allow-then-deny order would let every
        // one of them through.
        if self.denies(&path) {
            return ScopeDecision::DeniedWorktree;
        }
        if self
            .roots
            .iter()
            .any(|root| &path == root || is_within(&path, root))
        {
            return ScopeDecision::Allowed;
        }
        ScopeDecision::OutsideScope
    }

    /// Whether `path` may be read.
    ///
    /// Everything is readable except another session's worktrees — see the
    /// module header for why reads are otherwise unconfined and why that one
    /// exception is a correctness rule rather than a security one.
    #[must_use]
    pub fn may_read(&self, path: &Path) -> ScopeDecision {
        let path = normalize(path);
        if self.denies(&path) {
            return ScopeDecision::DeniedWorktree;
        }
        ScopeDecision::Allowed
    }

    /// Whether `path` sits under any allowed root's [`DENIED_SUBPATH`].
    ///
    /// A root that is ITSELF inside a worktree directory is not denied: that
    /// is the "entered the worktree" case, where the worktree is the
    /// workspace. Checked by comparing against each root's own denied
    /// directory, so entering `<project>/.stella/worktrees/x` as the session
    /// root denies only `<project>/.stella/worktrees/x/.stella/worktrees`.
    fn denies(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| {
            let denied = denied_dir(root);
            path == denied || is_within(path, &denied)
        })
    }
}

/// The directory worktrees live in, for a given project root.
#[must_use]
pub fn denied_dir(root: &Path) -> PathBuf {
    let mut denied = root.to_path_buf();
    for part in DENIED_SUBPATH.split('/') {
        denied.push(part);
    }
    denied
}

/// Lexical `.`/`..` collapse, with no filesystem access.
///
/// A caller that has already canonicalized loses nothing here. A caller that
/// has not gets `..` resolved *textually*, which is the conservative reading
/// for a confinement check: `<root>/../etc` normalizes out of the root and is
/// refused, rather than passing a naive `starts_with`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Popping past the prefix/root is not possible: `pop` returns
                // false there, and the `..` is simply dropped, which keeps the
                // result inside the filesystem root rather than manufacturing
                // a path above it.
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `path` is strictly inside `root` — a component-wise prefix test.
///
/// Component-wise, never `str::starts_with`: `/work/project-evil` textually
/// starts with `/work/project` and is a completely different directory.
fn is_within(path: &Path, root: &Path) -> bool {
    let mut path_parts = path.components();
    for root_part in root.components() {
        match path_parts.next() {
            Some(part) if part == root_part => {}
            _ => return false,
        }
    }
    // Something must remain, or `path` IS `root` rather than inside it.
    path_parts.next().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/work/project")
    }

    #[test]
    fn the_session_root_and_its_children_are_writable() {
        let scope = WriteScope::new(root());
        assert_eq!(scope.may_write(&root()), ScopeDecision::Allowed);
        assert_eq!(
            scope.may_write(Path::new("/work/project/src/main.rs")),
            ScopeDecision::Allowed
        );
        assert_eq!(
            scope.may_write(Path::new("/work/project/a/b/c/deep.txt")),
            ScopeDecision::Allowed
        );
    }

    #[test]
    fn anything_outside_the_root_is_refused_for_writing() {
        let scope = WriteScope::new(root());
        for outside in [
            "/etc/passwd",
            "/work/other",
            "/work",
            "/work/project-evil/src/main.rs",
        ] {
            assert_eq!(
                scope.may_write(Path::new(outside)),
                ScopeDecision::OutsideScope,
                "{outside} must be outside the scope"
            );
        }
    }

    /// The sibling-prefix trap a `str::starts_with` check falls into.
    #[test]
    fn a_sibling_sharing_a_name_prefix_is_not_inside() {
        let scope = WriteScope::new(root());
        assert_eq!(
            scope.may_write(Path::new("/work/projectile/src/main.rs")),
            ScopeDecision::OutsideScope
        );
    }

    /// `..` is collapsed lexically before the prefix test, so a path that
    /// merely *starts* inside the root but climbs out of it is refused.
    #[test]
    fn a_dotdot_escape_is_collapsed_before_the_check() {
        let scope = WriteScope::new(root());
        assert_eq!(
            scope.may_write(Path::new("/work/project/../other/file.txt")),
            ScopeDecision::OutsideScope
        );
        assert_eq!(
            scope.may_write(Path::new("/work/project/src/../src/main.rs")),
            ScopeDecision::Allowed,
            "a `..` that stays inside is still inside"
        );
    }

    #[test]
    fn a_relative_path_is_refused_rather_than_guessed_at() {
        let scope = WriteScope::new(root());
        assert_eq!(
            scope.may_write(Path::new("src/main.rs")),
            ScopeDecision::OutsideScope
        );
    }

    #[test]
    fn an_operator_added_directory_becomes_writable() {
        let scope = WriteScope::new(root()).with_additional([PathBuf::from("/work/shared-lib")]);
        assert_eq!(
            scope.may_write(Path::new("/work/shared-lib/src/lib.rs")),
            ScopeDecision::Allowed
        );
        assert_eq!(
            scope.may_write(Path::new("/work/elsewhere/x")),
            ScopeDecision::OutsideScope
        );
    }

    /// A directory already covered by an existing root adds no permission, so
    /// it is dropped rather than listed — otherwise the rendered scope claims
    /// more places than it governs.
    #[test]
    fn a_redundant_addition_is_not_recorded_twice() {
        let scope = WriteScope::new(root()).with_additional([
            PathBuf::from("/work/project/src"),
            PathBuf::from("/work/shared"),
            PathBuf::from("/work/shared"),
        ]);
        assert_eq!(scope.roots().len(), 2, "{:?}", scope.roots());
        assert_eq!(scope.session_root(), root());
    }

    /// The worktree rule, in both directions.
    #[test]
    fn worktrees_are_denied_for_writing_and_for_reading() {
        let scope = WriteScope::new(root());
        let inside_a_worktree = Path::new("/work/project/.stella/worktrees/task-1/src/main.rs");
        assert_eq!(
            scope.may_write(inside_a_worktree),
            ScopeDecision::DeniedWorktree
        );
        assert_eq!(
            scope.may_read(inside_a_worktree),
            ScopeDecision::DeniedWorktree,
            "reading another session's worktree is the correctness hazard the \
             denial exists for"
        );
        assert_eq!(
            scope.may_write(Path::new("/work/project/.stella/worktrees")),
            ScopeDecision::DeniedWorktree,
            "the directory itself is denied, not just its contents"
        );
    }

    /// Denial beats permission. A worktree is inside the session root by
    /// construction, so an allow-first order would let every one through.
    #[test]
    fn the_worktree_denial_wins_over_the_root_permission() {
        let scope = WriteScope::new(root());
        assert!(
            !scope
                .may_write(Path::new("/work/project/.stella/worktrees/x/f"))
                .is_allowed()
        );
    }

    /// The rest of `.stella` is ordinary workspace state and stays writable —
    /// the denial is about worktrees, not about Stella's directory.
    #[test]
    fn the_rest_of_the_stella_directory_is_not_denied() {
        let scope = WriteScope::new(root());
        assert_eq!(
            scope.may_write(Path::new("/work/project/.stella/settings.json")),
            ScopeDecision::Allowed
        );
    }

    /// "Entering the worktree": a session rooted AT a worktree treats it as
    /// the workspace, and nothing about it is denied.
    #[test]
    fn a_session_rooted_at_a_worktree_may_write_its_whole_tree() {
        let scope = WriteScope::new(PathBuf::from("/work/project/.stella/worktrees/task-1"));
        assert_eq!(
            scope.may_write(Path::new(
                "/work/project/.stella/worktrees/task-1/src/main.rs"
            )),
            ScopeDecision::Allowed
        );
        assert_eq!(
            scope.may_read(Path::new(
                "/work/project/.stella/worktrees/task-1/README.md"
            )),
            ScopeDecision::Allowed
        );
        // Its own nested worktree directory is still denied.
        assert_eq!(
            scope.may_write(Path::new(
                "/work/project/.stella/worktrees/task-1/.stella/worktrees/inner/f"
            )),
            ScopeDecision::DeniedWorktree
        );
    }

    /// Reads are otherwise unrestricted — the asymmetry the module opens with.
    #[test]
    fn reads_outside_the_scope_are_allowed() {
        let scope = WriteScope::new(root());
        for readable in ["/etc/hosts", "/usr/include/stdio.h", "/work/other/src.rs"] {
            assert_eq!(
                scope.may_read(Path::new(readable)),
                ScopeDecision::Allowed,
                "{readable} must stay readable"
            );
        }
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// Path segments that cannot themselves introduce separators or
        /// traversal — the traversal cases are covered by the explicit tests
        /// above, and mixing them in here would only re-test `normalize`.
        fn segment() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9_-]{0,7}".prop_map(|s| s)
        }

        fn segments(max: usize) -> impl Strategy<Value = Vec<String>> {
            proptest::collection::vec(segment(), 1..=max)
        }

        proptest! {
            /// Anything built by extending the session root is writable, at
            /// any depth. The property the file tools depend on: an agent
            /// working inside its own tree is never refused.
            #[test]
            fn any_descendant_of_the_root_is_writable(tail in segments(6)) {
                let scope = WriteScope::new(root());
                let mut path = root();
                for part in &tail {
                    path.push(part);
                }
                prop_assert_eq!(scope.may_write(&path), ScopeDecision::Allowed);
            }

            /// The complement: a path built under a DIFFERENT absolute root is
            /// never writable, however deep it goes.
            #[test]
            fn no_descendant_of_a_foreign_root_is_writable(tail in segments(6)) {
                let scope = WriteScope::new(root());
                let mut path = PathBuf::from("/elsewhere");
                for part in &tail {
                    path.push(part);
                }
                prop_assert_eq!(scope.may_write(&path), ScopeDecision::OutsideScope);
            }

            /// The worktree denial holds at every depth and beats the root
            /// permission every time — the ordering bug this module could most
            /// plausibly regress into.
            #[test]
            fn nothing_under_the_worktree_directory_is_ever_writable(tail in segments(6)) {
                let scope = WriteScope::new(root());
                let mut path = denied_dir(&root());
                for part in &tail {
                    path.push(part);
                }
                prop_assert_eq!(scope.may_write(&path), ScopeDecision::DeniedWorktree);
                prop_assert_eq!(scope.may_read(&path), ScopeDecision::DeniedWorktree);
            }

            /// Widening a scope only ever adds permission — it can never take
            /// one away. A regression here would make `--allow-dir` able to
            /// lock an agent out of its own workspace.
            #[test]
            fn widening_never_removes_a_permission(tail in segments(4), extra in segments(3)) {
                let narrow = WriteScope::new(root());
                let mut added = PathBuf::from("/other");
                for part in &extra {
                    added.push(part);
                }
                let wide = WriteScope::new(root()).with_additional([added]);

                let mut path = root();
                for part in &tail {
                    path.push(part);
                }
                if narrow.may_write(&path).is_allowed() {
                    prop_assert!(wide.may_write(&path).is_allowed());
                }
            }
        }
    }
}
