//! Who wrote the file — the discriminator the post-seal drift check was
//! missing (#2877).
//!
//! `sealed_unchanged_inner` runs *after* the flip oracle has executed the test
//! command inside this very worktree, and it reads any difference from the
//! seal as the worker tampering with a verified tree. On a gcov-instrumented
//! task the binary rewrites `*.gcda` on every run, so the check answered
//! `false` about the oracle's own side effects and the pipeline discarded the
//! whole candidate — a 194,561-line change a grader independently scored 2 of
//! 3 passing. #2876 repaired the `__pycache__`-shaped instance of this by
//! subtracting a fixed list of scratch directories, and said in as many words
//! that the list was not the answer: the next task brings the next artifact
//! type, and `.gcda` was already it.
//!
//! # Why an observation and not a longer list
//!
//! An extension blocklist answers "does this path *look* like something a test
//! run produces". The question that actually decides the case is "did the
//! verification run write this path", and it is answerable exactly, for any
//! artifact type that exists or ever will, by looking while the run happens.
//!
//! [`ObservedTestRunner`] brackets every `run_test` with a `git status` of the
//! candidate worktree and records the paths that appear between the two reads.
//! Those, and only those, are the verification run's own leavings; anything
//! else that moved after the seal is unattributed and stays fatal, which is
//! what keeps the check a check. Cost is two `git status` invocations per
//! oracle run — unmeasurable beside running a test suite.
//!
//! The attribution is deliberately conservative in the one place it is not
//! exact. A path that had *already* drifted before the run and was rewritten
//! by it appears in both reads, so it is not attributed and the candidate
//! still fails. Being wrong toward refusing an adoption is the direction that
//! loses work rather than trust, and it is the direction the seal check exists
//! to be wrong in.
//!
//! # What this does not claim
//!
//! It does not claim the oracle's writes are harmless. A test command that
//! rewrites a source file is attributed here exactly like a `.gcda`, and the
//! candidate proceeds. What makes that survivable is a fact about the
//! deliverable rather than about this module: adoption applies
//! `git diff <anchor> <sealed>`, computed from two immutable commit objects,
//! so the bytes delivered are the bytes that were verified no matter what the
//! worktree does afterwards. The drift check is a statement about the
//! *claim* — whether the flip was measured against the tree that is being
//! delivered — which is why #2881's rule applies: a post-completion check may
//! downgrade a claim, never discard a deliverable. Reporting an attributed
//! source rewrite as a claim degradation, rather than silently, is #2990.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use stella_pipeline::ports::{CmdOutcome, TestInvocation, TestRunner};

use crate::agent::TypedTestRunner;

/// The paths this candidate's own verification runs wrote, accumulated across
/// every oracle run since the last seal.
///
/// Cleared by [`Self::forget`] on every seal: the seal commits the tree, so a
/// path attributed under the previous seal is part of the new one and can only
/// serve to excuse a *later* mutation at the same path.
#[derive(Default)]
pub(super) struct OracleWrites(Mutex<BTreeSet<String>>);

impl OracleWrites {
    /// Whether the verification run wrote this repository-relative path.
    pub(super) fn wrote(&self, path: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(path)
    }

    /// Drop every attribution — called from the seal, whose commit makes them
    /// all moot.
    pub(super) fn forget(&self) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    fn record(&self, paths: impl IntoIterator<Item = String>) {
        let mut set = self.0.lock().unwrap_or_else(|p| p.into_inner());
        set.extend(paths);
    }
}

/// The candidate's [`TestRunner`], bracketed so the worktree paths a test run
/// moves are attributed to the run rather than to the worker.
pub(super) struct ObservedTestRunner {
    inner: TypedTestRunner,
    /// The shadow worktree's toplevel — the tree the status reads describe,
    /// which is not necessarily the runner's root (the session workspace may
    /// be a subdirectory) and must not be, since adoption is repo-wide.
    toplevel: PathBuf,
    written: std::sync::Arc<OracleWrites>,
}

impl ObservedTestRunner {
    pub(super) fn new(
        inner: TypedTestRunner,
        toplevel: PathBuf,
        written: std::sync::Arc<OracleWrites>,
    ) -> Self {
        Self {
            inner,
            toplevel,
            written,
        }
    }
}

#[async_trait]
impl TestRunner for ObservedTestRunner {
    async fn run_test(&self, invocation: &TestInvocation) -> CmdOutcome {
        let before = moved_paths(&self.toplevel).await;
        let outcome = self.inner.run_test(invocation).await;
        let after = moved_paths(&self.toplevel).await;
        self.written.record(after.difference(&before).cloned());
        outcome
    }

    async fn runner_available(&self, probe: &TestInvocation) -> bool {
        // A `--version` probe is not an oracle run and writes nothing worth
        // attributing; bracketing it would buy two `git status` calls per
        // availability question and excuse nothing.
        self.inner.runner_available(probe).await
    }
}

/// Every path `git status` currently reports as moved in `toplevel`, or an
/// empty set when the status cannot be read.
///
/// An unreadable status degrades toward attributing *nothing*, which leaves
/// the drift check exactly as strict as it was before this module existed —
/// the safe direction for a probe whose whole job is to excuse a difference.
async fn moved_paths(toplevel: &Path) -> BTreeSet<String> {
    let status = super::git(
        toplevel,
        &[
            "status",
            "--porcelain",
            "--no-renames",
            "-z",
            "--untracked-files=all",
        ],
    )
    .await;
    status
        .map(|raw| super::adopt::drifted_paths(&raw).into_iter().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seal_forgets_every_attribution() {
        let writes = OracleWrites::default();
        writes.record(["build/x.gcda".to_string()]);
        assert!(writes.wrote("build/x.gcda"));
        writes.forget();
        assert!(
            !writes.wrote("build/x.gcda"),
            "the new seal contains it; excusing a later mutation there would be a hole"
        );
    }

    #[test]
    fn only_recorded_paths_are_attributed() {
        let writes = OracleWrites::default();
        writes.record(["build/x.gcda".to_string()]);
        assert!(!writes.wrote("src/lib.rs"));
    }
}
