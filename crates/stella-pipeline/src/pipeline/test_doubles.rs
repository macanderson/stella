//! Workspace-isolation test doubles for [`super::tests`] — moved out of
//! `tests.rs` when it reached its file-size ceiling (the `settlement.rs`
//! pattern: relieve the ratchet by extracting a coherent cluster, never by
//! raising it). The substrate doubles these compose (`ScriptedRunner`,
//! `SeqRepoStatus`, `EmptyTools`) still live beside their many direct users
//! in `tests.rs`; the deliberate cross-import below is test-only wiring.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use stella_core::ToolExecutor;

use crate::ports::{
    AdoptedChange, CandidateWorkspace, CandidateWorkspacePort, CmdOutcome, DiagnosticInvocation,
    DiagnosticRunner, RepoStatusPort, TestInvocation, TestRunner, WorkspaceError,
};

use super::tests::{EmptyTools, ScriptedRunner, SeqRepoStatus};

/// Session ports that fail the test if touched — the witness that
/// isolated candidates only ever reach their own workspace's surface,
/// never the real tree's.
pub(super) struct NeverRunner;
#[async_trait]
impl DiagnosticRunner for NeverRunner {
    async fn run_diagnostic(&self, invocation: &DiagnosticInvocation) -> CmdOutcome {
        panic!(
            "the session DiagnosticRunner must not serve isolated candidates (got {invocation:?})"
        );
    }
}
#[async_trait]
impl TestRunner for NeverRunner {
    async fn run_test(&self, invocation: &TestInvocation) -> CmdOutcome {
        panic!("the session TestRunner must not serve isolated candidates (got {invocation:?})");
    }
}
pub(super) struct NeverRepoStatus;
#[async_trait]
impl RepoStatusPort for NeverRepoStatus {
    async fn untracked_fingerprints(&self) -> HashMap<String, String> {
        panic!("the session RepoStatusPort must not serve isolated candidates");
    }
}

/// A scripted [`CandidateWorkspace`]: per-candidate command results, a
/// canned adoption outcome, and a shared log of lifecycle calls.
pub(super) struct FakeWorkspace {
    id: usize,
    root: String,
    tools: Box<dyn ToolExecutor>,
    diagnostics: ScriptedRunner,
    repo_status: SeqRepoStatus,
    adopt_result: Result<Vec<AdoptedChange>, WorkspaceError>,
    sealed_unchanged: bool,
    /// Canned outcome for the one artifact copy between workspaces. `Err`
    /// stands in for the real failures: the worker already wrote a file at
    /// that path, or the parent directory is absent.
    graft_result: Result<(), WorkspaceError>,
    /// Scripted escape report (#1538) — the paths the post-seal check should
    /// claim the real tree already holds this candidate's sealed bytes at.
    escaped: Vec<String>,
    log: Arc<std::sync::Mutex<Vec<String>>>,
}

impl FakeWorkspace {
    pub(super) fn new(
        id: usize,
        test_results: Vec<bool>,
        adopt_result: Result<Vec<AdoptedChange>, WorkspaceError>,
        log: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            id,
            root: "/candidate/workspace".into(),
            tools: Box::new(EmptyTools),
            diagnostics: ScriptedRunner::new(test_results, "@@ -1 +1 @@\n-a\n+b"),
            repo_status: SeqRepoStatus::new(vec![]),
            adopt_result,
            sealed_unchanged: true,
            graft_result: Ok(()),
            escaped: Vec::new(),
            log,
        }
    }

    pub(super) fn with_repo_status(mut self, repo_status: SeqRepoStatus) -> Self {
        self.repo_status = repo_status;
        self
    }

    /// Untracked files this workspace reports, as `(path, added_lines)` —
    /// what a diff gather bills to the turn unless the path was already known.
    pub(super) fn with_untracked(mut self, untracked: Vec<(&str, u32)>) -> Self {
        self.diagnostics =
            std::mem::replace(&mut self.diagnostics, ScriptedRunner::new(vec![], ""))
                .with_untracked(untracked);
        self
    }

    /// The diff this workspace reports after execution — what the warrant
    /// reads to decide whether a witness is worth buying.
    pub(super) fn with_diff(mut self, diff: &str) -> Self {
        self.diagnostics.diff = diff.to_string();
        self
    }

    pub(super) fn with_graft_failure(mut self, reason: &str) -> Self {
        self.graft_result = Err(WorkspaceError::Graft {
            reason: reason.into(),
            path: "tests/witness.rs".into(),
            workspace: "/candidate/workspace".into(),
        });
        self
    }

    pub(super) fn with_post_verification_drift(mut self) -> Self {
        self.sealed_unchanged = false;
        self
    }

    pub(super) fn with_root(mut self, root: impl Into<String>) -> Self {
        self.root = root.into();
        self
    }

    /// The tool executor this candidate's engine turns run against — for
    /// scenarios where a scripted worker must *observe* something through a
    /// tool result (e.g. the tracked test's exit status feeding the flip
    /// halt), which the default silent [`EmptyTools`] can never carry.
    pub(super) fn with_tools(mut self, tools: impl ToolExecutor + 'static) -> Self {
        self.tools = Box::new(tools);
        self
    }

    /// A candidate that wrote through its isolation (#1538): the post-seal
    /// escape check will report these paths as already holding the
    /// candidate's sealed bytes in the real tree.
    pub(super) fn with_escaped(mut self, paths: Vec<&str>) -> Self {
        self.escaped = paths.into_iter().map(str::to_string).collect();
        self
    }

    /// #1539: script which runner programs this workspace's availability
    /// probe reports usable. An empty vec models no toolchain at all.
    pub(super) fn with_available_runners(mut self, programs: Vec<&str>) -> Self {
        self.diagnostics =
            std::mem::replace(&mut self.diagnostics, ScriptedRunner::new(vec![], ""))
                .with_available_runners(programs);
        self
    }
}

#[async_trait]
impl CandidateWorkspace for FakeWorkspace {
    fn root(&self) -> &str {
        &self.root
    }
    fn tools(&self) -> &dyn ToolExecutor {
        self.tools.as_ref()
    }
    fn witness_tools(&self) -> &dyn ToolExecutor {
        self.tools.as_ref()
    }
    fn diagnostics(&self) -> &dyn DiagnosticRunner {
        &self.diagnostics
    }
    fn tests(&self) -> &dyn TestRunner {
        &self.diagnostics
    }
    fn repo_status(&self) -> &dyn RepoStatusPort {
        &self.repo_status
    }
    async fn seal(&self) -> Result<(), WorkspaceError> {
        self.log.lock().unwrap().push(format!("seal:{}", self.id));
        Ok(())
    }
    async fn sealed_is_unchanged(&self) -> Result<bool, WorkspaceError> {
        Ok(self.sealed_unchanged)
    }
    async fn escaped_paths(&self) -> Vec<String> {
        self.escaped.clone()
    }
    async fn adopt(&self, withhold: &[String]) -> Result<Vec<AdoptedChange>, WorkspaceError> {
        // The withheld set is logged so tests can assert that an authored
        // witness never reaches the real tree, without reaching for real git.
        // Unchanged shape when nothing is withheld, so every pre-existing
        // `adopt:<id>` assertion still reads the same.
        self.log.lock().unwrap().push(if withhold.is_empty() {
            format!("adopt:{}", self.id)
        } else {
            format!("adopt:{}:withhold={}", self.id, withhold.join(","))
        });
        self.adopt_result.clone()
    }
    async fn graft_witness(&self, source_root: &str, path: &str) -> Result<(), WorkspaceError> {
        // Logged with the source so a test can prove the artifact came from a
        // *different* workspace than the one it lands in — the whole point of
        // authoring blind in a pristine snapshot.
        self.log
            .lock()
            .unwrap()
            .push(format!("graft:{}:{source_root}:{path}", self.id));
        self.graft_result.clone()
    }
    async fn remove(&self) {
        self.log.lock().unwrap().push(format!("remove:{}", self.id));
    }
}

/// A scripted [`CandidateWorkspacePort`]: each `create` pops the next
/// canned workspace (or failure). `panic_on_create` proves a path never
/// touches isolation at all.
pub(super) struct FakeWorkspacePort {
    script: std::sync::Mutex<VecDeque<Result<FakeWorkspace, WorkspaceError>>>,
    log: Arc<std::sync::Mutex<Vec<String>>>,
    panic_on_create: bool,
}

impl FakeWorkspacePort {
    pub(super) fn new(
        script: Vec<Result<FakeWorkspace, WorkspaceError>>,
        log: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            script: std::sync::Mutex::new(script.into_iter().collect()),
            log,
            panic_on_create: false,
        }
    }

    pub(super) fn untouchable() -> Self {
        Self {
            script: std::sync::Mutex::new(VecDeque::new()),
            log: Arc::new(std::sync::Mutex::new(Vec::new())),
            panic_on_create: true,
        }
    }
}

#[async_trait]
impl CandidateWorkspacePort for FakeWorkspacePort {
    async fn create(&self) -> Result<Box<dyn CandidateWorkspace>, WorkspaceError> {
        assert!(
            !self.panic_on_create,
            "the candidate workspace port must not be touched on this path"
        );
        self.log.lock().unwrap().push("create".into());
        match self.script.lock().unwrap().pop_front() {
            Some(Ok(ws)) => Ok(Box::new(ws)),
            Some(Err(e)) => Err(e),
            None => Err(WorkspaceError::Snapshot {
                reason: "workspace script exhausted".into(),
            }),
        }
    }
}
