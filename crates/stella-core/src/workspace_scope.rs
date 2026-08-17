// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a session may see, and what it may change.
//!
//! # Two rules, and one exception that covers both
//!
//! - **Reads are unrestricted.** An agent fixing a build needs the system
//!   headers, the toolchain, the crate source under `~/.cargo`, the file an
//!   error message cites. A read boundary tight enough to mean anything is
//!   tight enough to make ordinary work fail, and a read cannot damage the
//!   user's tree.
//! - **Writes are confined** to the session's workspace plus whatever
//!   directories an operator granted. Creating, editing or deleting outside
//!   them is what turns a bad turn into lost work, and no correct turn needs
//!   it.
//!
//! The exception is **the origin project's tree**, and it is the only thing
//! this module hides. It is not a security boundary — the shell can defeat any
//! of this — it is a *correctness* boundary, because a second checkout of the
//! same repository at another revision answers questions about the wrong tree,
//! and does it silently.
//!
//! # The whole rule, in two branches
//!
//! Every session has a **workspace** (where it runs) and an **origin** (the
//! main project it belongs to). For an ordinary session those are the same
//! path. For a session that entered a worktree, the origin is the project the
//! worktree was cut from — recoverable from the path itself, because every
//! worktree Stella creates lives at `<origin>/.stella/worktrees/<name>`.
//!
//! ```text
//! if the workspace IS a worktree:
//!     deny everything under <origin> that is not under the workspace
//! else:
//!     deny everything under <origin>/.stella/worktrees
//! ```
//!
//! That is the entire policy. Worked through:
//!
//! - A session at `~/Projects/stella` sees the whole machine and writes inside
//!   its own tree — but cannot see `~/Projects/stella/.stella/worktrees`, so
//!   no sibling session's checkout can leak into its answers.
//! - A session that entered `~/Projects/stella/.stella/worktrees/feature-x`
//!   still reads `/usr`, `/etc`, `~/Documents`, and `/Users/macanderson`. It
//!   cannot read `~/Projects/stella` at all — not the parent, not a sibling
//!   worktree, not the original checkout of the file it is editing — until it
//!   exits. Its own worktree is exempt, because that IS its workspace.
//!
//! The second branch subsumes the first for a worktree session: siblings live
//! under `<origin>` and outside the workspace, so they are already denied.
//!
//! # `--allow-dir` cannot re-admit the origin
//!
//! A granted directory is filtered through the same denial
//! ([`SessionScope::with_additional`]). Otherwise `--allow-dir ~/Projects/stella`
//! from inside a worktree would hand back exactly the tree the worktree exists
//! to stay out of, and it would look like configuration rather than a bug.
//!
//! # No I/O (invariant #2)
//!
//! Every function here is a pure predicate over already-resolved, absolute,
//! lexically-normalized paths — including [`origin_of`], which recovers the
//! origin by *looking at the path*, never by reading `.git`. Resolution
//! belongs to the caller (`stella_tools::rootfd` for the file tools). That
//! split is what makes this property-testable, and it is the honest boundary:
//! this decides policy and cannot by itself close a TOCTOU race.

use std::path::{Component, Path, PathBuf};

/// Where every git worktree Stella creates lives, relative to the project
/// root.
///
/// One fixed location rather than a pattern, because "is this directory a
/// worktree?" is not a question a pure predicate can answer — it needs to read
/// `.git` — and a heuristic that guesses wrong in the permissive direction
/// hides nothing. Writing every worktree to one known place turns the question
/// into a path comparison, and lets [`origin_of`] recover a worktree's origin
/// from its own path.
pub const WORKTREES_SUBPATH: &str = ".stella/worktrees";

/// What a scope decided about one path.
///
/// A named enum rather than a `bool` because the refusals want different
/// sentences: a caller outside the write scope should be told what the scope
/// *is*, while a caller reaching into the origin project should be told that
/// the tree is deliberately invisible and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeDecision {
    /// The operation is allowed.
    Allowed,
    /// The path is outside every writable root. Reads never produce this.
    OutsideScope,
    /// The path is inside the origin project but outside this session's
    /// workspace — invisible for both reading and writing.
    DeniedOrigin,
}

impl ScopeDecision {
    /// Whether the operation may proceed.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, ScopeDecision::Allowed)
    }
}

/// The origin project a workspace belongs to, recovered from its path.
///
/// `<origin>/.stella/worktrees/<name>` → `<origin>`. Anything else is its own
/// origin. Purely lexical, so it works on a path that does not exist yet and
/// costs no syscall.
///
/// Nested worktrees resolve to the **outermost** origin, which is the
/// conservative direction: a worktree cut from a worktree still must not see
/// the tree at the top.
#[must_use]
pub fn origin_of(workspace: &Path) -> PathBuf {
    let marker: Vec<Component> = Path::new(WORKTREES_SUBPATH).components().collect();
    let parts: Vec<Component> = workspace.components().collect();
    // Scan left to right so the OUTERMOST `.stella/worktrees` wins.
    for start in 0..parts.len() {
        if parts[start..].starts_with(&marker) {
            return parts[..start].iter().collect();
        }
    }
    workspace.to_path_buf()
}

/// Whether `workspace` is itself a worktree — i.e. its origin is some other
/// directory.
#[must_use]
pub fn is_worktree(workspace: &Path) -> bool {
    origin_of(workspace) != normalize(workspace)
}

/// One session's view of the filesystem: what it can see, and what it can
/// change.
///
/// Constructed once per session and never widened by a tool — that is the
/// point of handing it *to* the tools rather than letting them read a setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScope {
    /// Where this session runs. Always the first writable root.
    workspace: PathBuf,
    /// The project `workspace` belongs to — equal to `workspace` unless this
    /// session entered a worktree.
    origin: PathBuf,
    /// Writable roots, workspace first, then operator grants in order.
    roots: Vec<PathBuf>,
}

impl SessionScope {
    /// The scope for a session rooted at `workspace`, with no operator grants.
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace = normalize(&workspace.into());
        let origin = origin_of(&workspace);
        Self {
            roots: vec![workspace.clone()],
            workspace,
            origin,
        }
    }

    /// The scope widened by operator-granted directories.
    ///
    /// Three filters, each closing a way the grant could be a lie:
    /// a directory already covered by an existing root adds nothing and is
    /// dropped (so the rendered scope never claims more places than it
    /// governs); a duplicate is dropped; and **a directory the origin denial
    /// hides is refused outright**, because granting write access to a tree
    /// the session cannot even see would be incoherent.
    #[must_use]
    pub fn with_additional(mut self, extra: impl IntoIterator<Item = PathBuf>) -> Self {
        for candidate in extra {
            let candidate = normalize(&candidate);
            if self.denies(&candidate) {
                continue;
            }
            if self
                .roots
                .iter()
                .any(|root| &candidate == root || is_within(&candidate, root))
            {
                continue;
            }
            self.roots.push(candidate);
        }
        self
    }

    /// Where this session runs.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The project this session's workspace belongs to.
    #[must_use]
    pub fn origin(&self) -> &Path {
        &self.origin
    }

    /// Every writable root, workspace first.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Whether this session is running inside a worktree.
    #[must_use]
    pub fn in_worktree(&self) -> bool {
        self.origin != self.workspace
    }

    /// Whether `path` may be read.
    ///
    /// Everything on the machine, except what the origin denial hides. `path`
    /// must be absolute and already resolved by the caller.
    #[must_use]
    pub fn may_read(&self, path: &Path) -> ScopeDecision {
        if self.denies(&normalize(path)) {
            return ScopeDecision::DeniedOrigin;
        }
        ScopeDecision::Allowed
    }

    /// Whether `path` may be created, modified, deleted, or executed as a
    /// script.
    ///
    /// A relative path is [`ScopeDecision::OutsideScope`] rather than being
    /// joined against a guessed root: guessing which root a relative path
    /// belongs to is how a confinement check silently permits the wrong tree.
    #[must_use]
    pub fn may_write(&self, path: &Path) -> ScopeDecision {
        let path = normalize(path);
        if !path.is_absolute() {
            return ScopeDecision::OutsideScope;
        }
        // Denial is checked first and wins: the denied tree can sit inside a
        // writable root (an ordinary session's own worktrees directory), so an
        // allow-first order would let every one of them through.
        if self.denies(&path) {
            return ScopeDecision::DeniedOrigin;
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

    /// The one predicate the whole module is about — see the header.
    ///
    /// A worktree session hides its entire origin project except its own
    /// workspace; every other session hides only the worktrees directory.
    fn denies(&self, path: &Path) -> bool {
        // Branch one: a worktree session hides its whole origin project,
        // exempting only its own workspace — which sits inside that origin,
        // so the exemption has to be checked first or the session would hide
        // itself from itself.
        if self.in_worktree() {
            if path == self.workspace || is_within(path, &self.workspace) {
                return self.hides_own_worktrees(path);
            }
            return path == self.origin || is_within(path, &self.origin);
        }
        // Branch two: every other session hides only the worktrees directory.
        self.hides_own_worktrees(path)
    }

    /// Whether `path` is inside this workspace's own worktrees directory.
    ///
    /// Applied to every session, including one already inside a worktree: a
    /// worktree that spawns its own children must not see them either, for
    /// exactly the reason the top-level session must not — they are parallel
    /// checkouts, and reading one answers about the wrong tree. A session that
    /// *enters* one of those children gets its own scope and its own exemption.
    fn hides_own_worktrees(&self, path: &Path) -> bool {
        let worktrees = worktrees_dir(&self.workspace);
        path == worktrees || is_within(path, &worktrees)
    }
}

/// The directory worktrees live in, for a given project root.
#[must_use]
pub fn worktrees_dir(origin: &Path) -> PathBuf {
    let mut dir = origin.to_path_buf();
    for part in WORKTREES_SUBPATH.split('/') {
        dir.push(part);
    }
    dir
}

/// Lexical `.`/`..` collapse, with no filesystem access.
///
/// A caller that has already canonicalized loses nothing. A caller that has
/// not gets `..` resolved *textually*, the conservative reading for a
/// confinement check: `<root>/../etc` normalizes out of the root and is
/// refused, rather than passing a naive `starts_with`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            // Popping past the root is impossible: `pop` returns false there
            // and the `..` is dropped, which keeps the result inside the
            // filesystem root rather than manufacturing a path above it.
            Component::ParentDir => {
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
    path_parts.next().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = "/Users/mac/Projects/stella";
    const WORKTREE: &str = "/Users/mac/Projects/stella/.stella/worktrees/feature-x";

    fn project() -> SessionScope {
        SessionScope::new(PROJECT)
    }

    fn worktree() -> SessionScope {
        SessionScope::new(WORKTREE)
    }

    #[test]
    fn an_ordinary_workspace_is_its_own_origin() {
        assert_eq!(origin_of(Path::new(PROJECT)), PathBuf::from(PROJECT));
        assert!(!is_worktree(Path::new(PROJECT)));
        assert!(!project().in_worktree());
    }

    #[test]
    fn a_worktrees_origin_is_recovered_from_its_path() {
        assert_eq!(origin_of(Path::new(WORKTREE)), PathBuf::from(PROJECT));
        assert!(is_worktree(Path::new(WORKTREE)));
        assert!(worktree().in_worktree());
        assert_eq!(worktree().origin(), Path::new(PROJECT));
    }

    /// A worktree cut from a worktree resolves to the OUTERMOST project — the
    /// conservative direction, since the top tree must stay hidden too.
    #[test]
    fn a_nested_worktree_resolves_to_the_outermost_origin() {
        let nested = format!("{WORKTREE}/.stella/worktrees/inner");
        assert_eq!(origin_of(Path::new(&nested)), PathBuf::from(PROJECT));
    }

    // ---- an ordinary session -------------------------------------------

    #[test]
    fn an_ordinary_session_reads_the_whole_machine() {
        let scope = project();
        for readable in [
            "/etc/hosts",
            "/usr/include/stdio.h",
            "/Users/mac",
            "/Users/mac/Documents/notes.md",
            "/Users/mac/Projects/other-project/src/lib.rs",
            "/Users/mac/Projects/stella/src/main.rs",
        ] {
            assert_eq!(
                scope.may_read(Path::new(readable)),
                ScopeDecision::Allowed,
                "{readable} must be readable"
            );
        }
    }

    #[test]
    fn an_ordinary_session_cannot_see_its_own_worktrees() {
        let scope = project();
        for hidden in [
            "/Users/mac/Projects/stella/.stella/worktrees",
            "/Users/mac/Projects/stella/.stella/worktrees/feature-x",
            "/Users/mac/Projects/stella/.stella/worktrees/feature-x/src/main.rs",
        ] {
            assert_eq!(
                scope.may_read(Path::new(hidden)),
                ScopeDecision::DeniedOrigin,
                "{hidden} must be hidden"
            );
            assert_eq!(
                scope.may_write(Path::new(hidden)),
                ScopeDecision::DeniedOrigin
            );
        }
    }

    #[test]
    fn an_ordinary_session_writes_its_own_tree_and_nothing_else() {
        let scope = project();
        assert_eq!(
            scope.may_write(Path::new("/Users/mac/Projects/stella/src/main.rs")),
            ScopeDecision::Allowed
        );
        assert_eq!(
            scope.may_write(Path::new("/Users/mac/Projects/other/src/main.rs")),
            ScopeDecision::OutsideScope
        );
        assert_eq!(
            scope.may_write(Path::new("/etc/passwd")),
            ScopeDecision::OutsideScope
        );
    }

    /// The rest of `.stella` is ordinary workspace state and stays usable —
    /// the denial is about worktrees, not about Stella's directory.
    #[test]
    fn the_rest_of_the_stella_directory_is_not_denied() {
        let scope = project();
        let settings = "/Users/mac/Projects/stella/.stella/settings.json";
        assert_eq!(scope.may_read(Path::new(settings)), ScopeDecision::Allowed);
        assert_eq!(scope.may_write(Path::new(settings)), ScopeDecision::Allowed);
    }

    // ---- a session that entered a worktree -----------------------------

    /// The user's worked example: from inside a worktree, `/Users/mac` is
    /// readable and `/Users/mac/Projects/stella` is not.
    #[test]
    fn a_worktree_session_reads_the_machine_but_never_its_origin() {
        let scope = worktree();
        for readable in [
            "/etc/hosts",
            "/usr/include/stdio.h",
            "/Users/mac",
            "/Users/mac/Documents/notes.md",
            "/Users/mac/Projects/other-project/src/lib.rs",
        ] {
            assert_eq!(
                scope.may_read(Path::new(readable)),
                ScopeDecision::Allowed,
                "{readable} must stay readable from inside a worktree"
            );
        }
        for hidden in [
            "/Users/mac/Projects/stella",
            "/Users/mac/Projects/stella/src/main.rs",
            "/Users/mac/Projects/stella/Cargo.toml",
            "/Users/mac/Projects/stella/.stella/settings.json",
            "/Users/mac/Projects/stella/.stella/worktrees/other-task/src/main.rs",
        ] {
            assert_eq!(
                scope.may_read(Path::new(hidden)),
                ScopeDecision::DeniedOrigin,
                "{hidden} must be invisible from inside a worktree"
            );
        }
    }

    #[test]
    fn a_worktree_session_sees_and_writes_its_own_tree() {
        let scope = worktree();
        for mine in [
            WORKTREE,
            "/Users/mac/Projects/stella/.stella/worktrees/feature-x/src/main.rs",
            "/Users/mac/Projects/stella/.stella/worktrees/feature-x/.stella/settings.json",
        ] {
            assert_eq!(scope.may_read(Path::new(mine)), ScopeDecision::Allowed);
            assert_eq!(scope.may_write(Path::new(mine)), ScopeDecision::Allowed);
        }
    }

    /// A worktree that spawns children hides them from itself, exactly as the
    /// top-level session hides its own — they are parallel checkouts either
    /// way. A session that *enters* one gets its own scope and its own
    /// exemption.
    #[test]
    fn a_worktree_session_still_hides_worktrees_it_creates() {
        let scope = worktree();
        let nested = format!("{WORKTREE}/.stella/worktrees/inner/src/main.rs");
        assert_eq!(
            scope.may_read(Path::new(&nested)),
            ScopeDecision::DeniedOrigin
        );

        let entered = SessionScope::new(format!("{WORKTREE}/.stella/worktrees/inner"));
        assert_eq!(
            entered.may_read(Path::new(&nested)),
            ScopeDecision::Allowed,
            "entering it makes it the workspace"
        );
    }

    // ---- operator grants ------------------------------------------------

    #[test]
    fn an_operator_granted_directory_becomes_writable() {
        let scope = project().with_additional([PathBuf::from("/Users/mac/shared-lib")]);
        assert_eq!(
            scope.may_write(Path::new("/Users/mac/shared-lib/src/lib.rs")),
            ScopeDecision::Allowed
        );
        assert_eq!(
            scope.may_write(Path::new("/Users/mac/elsewhere/x")),
            ScopeDecision::OutsideScope
        );
    }

    /// The rule the user named explicitly: `--allow-dir` can never hand a
    /// worktree session its origin project back.
    #[test]
    fn a_grant_cannot_re_admit_the_origin_from_inside_a_worktree() {
        let scope = worktree().with_additional([
            PathBuf::from(PROJECT),
            PathBuf::from("/Users/mac/Projects/stella/src"),
            PathBuf::from("/Users/mac/legitimately-shared"),
        ]);
        assert_eq!(
            scope.may_write(Path::new("/Users/mac/Projects/stella/src/main.rs")),
            ScopeDecision::DeniedOrigin,
            "granting the origin must not re-admit it"
        );
        assert_eq!(
            scope.may_read(Path::new("/Users/mac/Projects/stella/src/main.rs")),
            ScopeDecision::DeniedOrigin,
            "and it must not become readable either"
        );
        assert_eq!(
            scope.may_write(Path::new("/Users/mac/legitimately-shared/x.rs")),
            ScopeDecision::Allowed,
            "an unrelated grant still works"
        );
    }

    /// The same rule from an ordinary session: granting the worktrees
    /// directory does not un-hide it.
    #[test]
    fn a_grant_cannot_re_admit_the_worktrees_directory() {
        let scope = project().with_additional([PathBuf::from(
            "/Users/mac/Projects/stella/.stella/worktrees",
        )]);
        assert_eq!(
            scope.may_write(Path::new(
                "/Users/mac/Projects/stella/.stella/worktrees/x/f.rs"
            )),
            ScopeDecision::DeniedOrigin
        );
    }

    #[test]
    fn a_redundant_grant_is_not_recorded_twice() {
        let scope = project().with_additional([
            PathBuf::from("/Users/mac/Projects/stella/src"),
            PathBuf::from("/Users/mac/shared"),
            PathBuf::from("/Users/mac/shared"),
        ]);
        assert_eq!(scope.roots().len(), 2, "{:?}", scope.roots());
        assert_eq!(scope.workspace(), Path::new(PROJECT));
    }

    // ---- path handling ---------------------------------------------------

    #[test]
    fn a_sibling_sharing_a_name_prefix_is_not_inside() {
        let scope = project();
        assert_eq!(
            scope.may_write(Path::new("/Users/mac/Projects/stella-evil/src/main.rs")),
            ScopeDecision::OutsideScope
        );
    }

    #[test]
    fn a_dotdot_escape_is_collapsed_before_the_check() {
        let scope = project();
        assert_eq!(
            scope.may_write(Path::new("/Users/mac/Projects/stella/../other/f.txt")),
            ScopeDecision::OutsideScope
        );
        assert_eq!(
            scope.may_write(Path::new("/Users/mac/Projects/stella/src/../src/main.rs")),
            ScopeDecision::Allowed
        );
    }

    /// A `..` that climbs out of a worktree lands in the origin, and must be
    /// denied rather than merely "outside scope" — the escape a worktree
    /// session would most plausibly attempt.
    #[test]
    fn a_dotdot_climb_out_of_a_worktree_lands_in_the_denied_origin() {
        let scope = worktree();
        let climb = format!("{WORKTREE}/../../../src/main.rs");
        assert_eq!(
            scope.may_read(Path::new(&climb)),
            ScopeDecision::DeniedOrigin
        );
        assert_eq!(
            scope.may_write(Path::new(&climb)),
            ScopeDecision::DeniedOrigin
        );
    }

    #[test]
    fn a_relative_path_is_refused_rather_than_guessed_at() {
        assert_eq!(
            project().may_write(Path::new("src/main.rs")),
            ScopeDecision::OutsideScope
        );
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        fn segment() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9_-]{0,7}".prop_map(|s| s)
        }

        fn segments(max: usize) -> impl Strategy<Value = Vec<String>> {
            proptest::collection::vec(segment(), 1..=max)
        }

        fn under(base: &str, tail: &[String]) -> PathBuf {
            let mut path = PathBuf::from(base);
            for part in tail {
                path.push(part);
            }
            path
        }

        proptest! {
            /// An ordinary session writes anywhere in its own tree, at any
            /// depth — the property the file tools depend on.
            #[test]
            fn any_descendant_of_the_workspace_is_writable(tail in segments(6)) {
                prop_assert_eq!(
                    project().may_write(&under(PROJECT, &tail)),
                    ScopeDecision::Allowed
                );
            }

            /// …and reads anywhere on the machine outside its origin.
            #[test]
            fn any_path_outside_the_origin_is_readable(tail in segments(6)) {
                prop_assert_eq!(
                    project().may_read(&under("/elsewhere", &tail)),
                    ScopeDecision::Allowed
                );
                prop_assert_eq!(
                    worktree().may_read(&under("/elsewhere", &tail)),
                    ScopeDecision::Allowed
                );
            }

            /// Nothing under a worktree session's origin is ever visible,
            /// at any depth, for reading or writing.
            #[test]
            fn nothing_under_the_origin_is_visible_from_a_worktree(tail in segments(6)) {
                // `.stella/worktrees/feature-x` would be the workspace itself,
                // which is legitimately exempt; every other path under the
                // origin is denied.
                prop_assume!(tail.first().map(String::as_str) != Some(".stella"));
                let path = under(PROJECT, &tail);
                prop_assert_eq!(worktree().may_read(&path), ScopeDecision::DeniedOrigin);
                prop_assert_eq!(worktree().may_write(&path), ScopeDecision::DeniedOrigin);
            }

            /// A worktree session's own tree is always fully usable.
            #[test]
            fn a_worktrees_own_tree_is_always_usable(tail in segments(6)) {
                let path = under(WORKTREE, &tail);
                prop_assert_eq!(worktree().may_read(&path), ScopeDecision::Allowed);
                prop_assert_eq!(worktree().may_write(&path), ScopeDecision::Allowed);
            }

            /// Granting directories only ever adds write permission, and never
            /// changes what is visible.
            #[test]
            fn a_grant_never_removes_a_permission(tail in segments(4), extra in segments(3)) {
                let narrow = project();
                let wide = project().with_additional([under("/other", &extra)]);
                let path = under(PROJECT, &tail);
                if narrow.may_write(&path).is_allowed() {
                    prop_assert!(wide.may_write(&path).is_allowed());
                }
                prop_assert_eq!(narrow.may_read(&path), wide.may_read(&path));
            }
        }
    }
}
