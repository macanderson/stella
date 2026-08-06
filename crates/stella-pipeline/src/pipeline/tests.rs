//! Unit tests for [`super`] — split out of `pipeline.rs` to keep the
//! orchestrator a manageable size; a child module, so the
//! private surface (`CandidateSurface`, `Pipeline::gather_diff`, ...)
//! stays reachable via `super::*`.

mod management_accounting;
mod telemetry;

use super::*;
use crate::CmdKind;
use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::router::{CircuitBreaker, ProviderProfile, RoleTable};
use stella_core::{Clock, ToolExecutor};
use stella_protocol::event::BudgetMode;
use stella_protocol::{
    CompletionRequestRef, CompletionResult, CompletionUsage, FileChangeKind, MessageRole,
    ProviderError, ScopeProposal, ToolCall, ToolOutput, ToolSchema,
};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;

use super::test_doubles::{FakeWorkspace, FakeWorkspacePort, NeverRepoStatus, NeverRunner};
use crate::ports::{
    AdoptedChange, ArtifactIdentity, ArtifactKind, CmdOutcome, ContextRecallPort,
    DiagnosticInvocation, DiagnosticRunner, NoContextRecall, NoFileTouches, NoRepoStatus,
    NoRepoStructure, Recall, TestInvocation, TestRunner,
};
use stella_protocol::{ContextProviderUsage, ContextUsage};

// test doubles

/// An [`ApprovalGate`] that approves every proposal — the double most tests
/// use so an interactive scope review never blocks a scripted run. Test-only
/// on purpose: production has no auto-approving gate (the headless bypass
/// skips the gate outright rather than consulting one).
struct AutoApproveGate;

#[async_trait]
impl ApprovalGate for AutoApproveGate {
    async fn review(&self, _proposal: &ScopeProposal) -> ScopeDecision {
        ScopeDecision::Approve
    }
}

/// A [`RepoStatusPort`] returning a fixed untracked `path -> fingerprint`
/// map — the "after execute" snapshot `gather_diff` diffs against a
/// caller-supplied before-snapshot.
struct FakeRepoStatus {
    files: HashMap<String, String>,
}
impl FakeRepoStatus {
    fn new(files: Vec<(&str, &str)>) -> Self {
        Self {
            files: files
                .into_iter()
                .map(|(p, fp)| (p.to_string(), fp.to_string()))
                .collect(),
        }
    }
}
#[async_trait]
impl RepoStatusPort for FakeRepoStatus {
    async fn untracked_fingerprints(&self) -> HashMap<String, String> {
        self.files.clone()
    }
}

/// A [`FileTouchPort`] serving a scripted SEQUENCE of readings — one per
/// `mutations_recorded` call, holding the last once exhausted.
///
/// A sequence rather than a constant because the real counter is monotonic and
/// the pipeline reads it twice: once for the candidate's baseline, before any
/// work, and again when it folds each observation. A fixture that returned the
/// same number to both would report a delta of zero and silently reproduce the
/// bug under test.
struct SeqTouches {
    readings: std::sync::Mutex<VecDeque<u64>>,
    last: std::sync::atomic::AtomicU64,
}

impl SeqTouches {
    fn new(readings: Vec<u64>) -> Self {
        Self {
            readings: std::sync::Mutex::new(readings.into()),
            last: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl crate::ports::FileTouchPort for SeqTouches {
    fn mutations_recorded(&self) -> u64 {
        match self.readings.lock().unwrap().pop_front() {
            Some(next) => {
                self.last.store(next, std::sync::atomic::Ordering::Relaxed);
                next
            }
            None => self.last.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
}

/// A [`RepoStatusPort`] serving a scripted SEQUENCE of snapshots — one per
/// `untracked_fingerprints` call, holding the last once exhausted. Lets a
/// test make the working tree "change" between the witness stage, the
/// execute turn, and the tamper check.
pub(super) struct SeqRepoStatus {
    snapshots: std::sync::Mutex<VecDeque<HashMap<String, String>>>,
    last: std::sync::Mutex<HashMap<String, String>>,
    tracked_snapshots: std::sync::Mutex<VecDeque<HashMap<String, String>>>,
    tracked_last: std::sync::Mutex<HashMap<String, String>>,
    artifact_identity: Option<ArtifactIdentity>,
    artifact_identities: std::sync::Mutex<VecDeque<Option<ArtifactIdentity>>>,
}
impl SeqRepoStatus {
    pub(super) fn new(snapshots: Vec<Vec<(&str, &str)>>) -> Self {
        let mapped: VecDeque<HashMap<String, String>> = snapshots
            .into_iter()
            .map(|files| {
                files
                    .into_iter()
                    .map(|(p, fp)| (p.to_string(), fp.to_string()))
                    .collect()
            })
            .collect();
        Self {
            snapshots: std::sync::Mutex::new(mapped),
            last: std::sync::Mutex::new(HashMap::new()),
            tracked_snapshots: std::sync::Mutex::new(VecDeque::new()),
            tracked_last: std::sync::Mutex::new(HashMap::new()),
            artifact_identity: None,
            artifact_identities: std::sync::Mutex::new(VecDeque::new()),
        }
    }

    fn with_tracked(mut self, snapshots: Vec<Vec<(&str, &str)>>) -> Self {
        self.tracked_snapshots = std::sync::Mutex::new(
            snapshots
                .into_iter()
                .map(|files| {
                    files
                        .into_iter()
                        .map(|(p, fp)| (p.to_string(), fp.to_string()))
                        .collect()
                })
                .collect(),
        );
        self
    }

    fn with_artifact_identity(mut self, identity: ArtifactIdentity) -> Self {
        self.artifact_identity = Some(identity);
        self
    }

    fn with_artifact_identities(self, identities: Vec<Option<ArtifactIdentity>>) -> Self {
        *self.artifact_identities.lock().unwrap() = identities.into();
        self
    }
}
#[async_trait]
impl RepoStatusPort for SeqRepoStatus {
    async fn untracked_fingerprints(&self) -> HashMap<String, String> {
        let mut q = self.snapshots.lock().unwrap();
        match q.pop_front() {
            Some(next) => {
                *self.last.lock().unwrap() = next.clone();
                next
            }
            None => self.last.lock().unwrap().clone(),
        }
    }

    async fn tracked_fingerprints(&self) -> HashMap<String, String> {
        let mut q = self.tracked_snapshots.lock().unwrap();
        match q.pop_front() {
            Some(next) => {
                *self.tracked_last.lock().unwrap() = next.clone();
                next
            }
            None => self.tracked_last.lock().unwrap().clone(),
        }
    }

    async fn artifact_identity(&self, path: &str) -> Option<ArtifactIdentity> {
        if let Some(identity) = self.artifact_identities.lock().unwrap().pop_front() {
            return identity;
        }
        self.artifact_identity.clone().or_else(|| {
            self.last
                .lock()
                .unwrap()
                .get(path)
                .map(|fingerprint| ArtifactIdentity {
                    path: path.to_string(),
                    fingerprint: fingerprint.clone(),
                    kind: ArtifactKind::Regular,
                    mode: 0o100644,
                    link_count: 1,
                })
        })
    }
}

/// A scripted provider serving triage, plan, worker, and verifier calls from
/// one ordered queue (the resolver hands the same provider to every role).
struct ScriptedProvider {
    script: TokioMutex<VecDeque<CompletionResult>>,
    /// Every prompt this provider was asked to complete, in order — so a test
    /// can assert what actually reached the model, not just what came back.
    seen: std::sync::Mutex<Vec<String>>,
    /// The same requests with per-message roles preserved, so a test can
    /// assert the system/user split of a management call (#1434) — the joined
    /// text above cannot show which half a sentence landed in.
    shapes: std::sync::Mutex<Vec<Vec<(stella_protocol::MessageRole, String)>>>,
}
impl ScriptedProvider {
    fn new(results: Vec<CompletionResult>) -> Self {
        Self {
            script: TokioMutex::new(results.into_iter().collect()),
            seen: std::sync::Mutex::new(Vec::new()),
            shapes: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The message text of each request, one joined string per call.
    fn prompts(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// Each request's messages as `(role, content)` pairs, in call order.
    fn shapes(&self) -> Vec<Vec<(stella_protocol::MessageRole, String)>> {
        self.shapes.lock().unwrap().clone()
    }
}
#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.seen.lock().unwrap().push(
            req.messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.shapes.lock().unwrap().push(
            req.messages
                .iter()
                .map(|m| (m.role, m.content.clone()))
                .collect(),
        );
        let mut q = self.script.lock().await;
        q.pop_front()
            .ok_or_else(|| ProviderError::Terminal("scripted provider exhausted".into()))
    }
}

/// A [`TurnSteering`] that hands out its queue on the first drain and never
/// soft-stops — the witness that a queued steer reaches the execute engine.
#[derive(Default)]
struct SteeringOnce {
    queued: std::sync::Mutex<Vec<String>>,
    drains: std::sync::atomic::AtomicU32,
}
impl stella_core::ports::TurnSteering for SteeringOnce {
    fn drain_steering(&self) -> Vec<String> {
        self.drains
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::mem::take(&mut *self.queued.lock().unwrap())
    }
    fn soft_stop_requested(&self) -> bool {
        false
    }
}

/// A resolver that returns the one scripted provider for every model.
struct OneProvider<'p>(&'p ScriptedProvider);
impl ProviderResolver for OneProvider<'_> {
    fn provider_for(&self, _model: &ModelRef) -> Option<&dyn Provider> {
        Some(self.0)
    }
}

/// A command runner: the git diff command returns a fixed diff; `ls-files`
/// reports the scripted untracked set; a `--no-index --numstat` reports
/// that file's scripted added-line count; anything else pops the next
/// queued test result (`true` = pass) or defaults to pass.
/// One scripted `run_test` result: a real pass/fail, or an infra outcome
/// (#860) so tests can model a timed-out runner without faking exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestScript {
    Pass,
    Fail,
    TimeOut,
    /// The machine ran out of memory and the kernel killed the run (#1294) —
    /// modelled as the runner classifies it, so a test can drive the retry
    /// without faking a signal.
    OutOfMemory,
    /// The command never produced a real exit status because it could not be
    /// spawned — the missing-toolchain shape (`pytest` with no Python). Its own
    /// variant rather than a shade of [`Self::TimeOut`] because the pipeline
    /// treats the two differently: a timeout may be worth waiting out, an
    /// unspawnable runner will be unspawnable on every retry.
    Infra,
    /// A pass whose stdout carries scripted runner output — for the #867
    /// fingerprint scenarios, which read test names from the tail.
    PassWith(&'static str),
    /// A failure whose stdout carries scripted runner output.
    FailWith(&'static str),
}

pub(super) struct ScriptedRunner {
    test_results: std::sync::Mutex<VecDeque<TestScript>>,
    /// Total `run_test` invocations, whatever they returned — the
    /// degradation gate pins this as the suite-run spend of a scenario.
    test_runs: std::sync::atomic::AtomicU32,
    pub(super) diff: String,
    /// Untracked files this workspace reports, as `(path, added_lines)`.
    untracked: Vec<(String, u32)>,
    /// What a failing run prints. Configurable so a test can plant a
    /// distinctive token and assert on where it does — and does not — travel.
    failure_tail: String,
    /// Exit code the `GitDiff` probe reports. Non-zero models a tree the diff
    /// machinery could not read at all, which is not the same as a clean one.
    diff_exit_code: i32,
    /// What the `GitDiff` probe prints on stderr. Carries git's own "not a git
    /// repository" wording, which the pipeline reads to tell a permanently
    /// inapplicable probe from a transiently failed one.
    diff_stderr: String,
    /// #1539: which runner programs the availability probe reports usable.
    /// `None` (the default) keeps the port's own default — everything
    /// available — so pre-existing scripts never see the constraint.
    available_runners: Option<Vec<String>>,
}
impl ScriptedRunner {
    pub(super) fn new(test_results: Vec<bool>, diff: &str) -> Self {
        Self::scripted(
            test_results
                .into_iter()
                .map(|passed| {
                    if passed {
                        TestScript::Pass
                    } else {
                        TestScript::Fail
                    }
                })
                .collect(),
            diff,
        )
    }
    fn scripted(test_results: Vec<TestScript>, diff: &str) -> Self {
        Self {
            test_results: std::sync::Mutex::new(test_results.into_iter().collect()),
            test_runs: std::sync::atomic::AtomicU32::new(0),
            diff: diff.to_string(),
            untracked: Vec::new(),
            failure_tail: "test failed".to_string(),
            diff_exit_code: 0,
            diff_stderr: String::new(),
            available_runners: None,
        }
    }
    /// How many times `run_test` was invoked on this runner.
    fn test_runs(&self) -> u32 {
        self.test_runs.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// A diff probe that FAILS: no stdout and a non-zero exit, the shape a
    /// candidate whose worktree registration vanished actually produces.
    fn with_blind_diff(mut self) -> Self {
        self.diff = String::new();
        self.diff_exit_code = 128;
        self
    }
    /// A workspace that is not a git repository at all — every Terminal-Bench
    /// task image, and the reason `GitDiff` can never answer there.
    fn with_not_a_repository(mut self) -> Self {
        self = self.with_blind_diff();
        self.diff_stderr =
            "fatal: not a git repository (or any of the parent directories): .git".to_string();
        self
    }
    fn with_failure_tail(mut self, tail: &str) -> Self {
        self.failure_tail = tail.to_string();
        self
    }
    pub(super) fn with_untracked(mut self, untracked: Vec<(&str, u32)>) -> Self {
        self.untracked = untracked
            .into_iter()
            .map(|(p, n)| (p.to_string(), n))
            .collect();
        self
    }
    /// #1539: script the availability probe — only these programs report
    /// usable. An empty vec models a workspace with no toolchain at all.
    pub(super) fn with_available_runners(mut self, programs: Vec<&str>) -> Self {
        self.available_runners = Some(programs.into_iter().map(str::to_string).collect());
        self
    }
}
#[async_trait]
impl DiagnosticRunner for ScriptedRunner {
    async fn run_diagnostic(&self, invocation: &DiagnosticInvocation) -> CmdOutcome {
        if let DiagnosticInvocation::UntrackedNumstat { path } = invocation {
            let numstat = self
                .untracked
                .iter()
                .find(|(candidate, _)| candidate == path)
                .map(|(p, n)| format!("{n}\t0\t{p}"))
                .unwrap_or_default();
            return CmdOutcome {
                exit_code: if numstat.is_empty() { 0 } else { 1 },
                stdout_tail: numstat,
                stderr_tail: String::new(),
                kind: CmdKind::Completed,
            };
        }
        if matches!(invocation, DiagnosticInvocation::GitDiff) {
            return CmdOutcome {
                exit_code: self.diff_exit_code,
                stdout_tail: self.diff.clone(),
                stderr_tail: self.diff_stderr.clone(),
                kind: CmdKind::Completed,
            };
        }
        CmdOutcome {
            exit_code: 0,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            kind: CmdKind::Completed,
        }
    }
}

#[async_trait]
impl TestRunner for ScriptedRunner {
    async fn runner_available(&self, probe: &TestInvocation) -> bool {
        match &self.available_runners {
            Some(programs) => programs.iter().any(|p| p == &probe.program),
            None => true,
        }
    }

    async fn run_test(&self, _invocation: &TestInvocation) -> CmdOutcome {
        self.test_runs
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let script = self
            .test_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(TestScript::Pass);
        match script {
            TestScript::Pass => CmdOutcome {
                exit_code: 0,
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                kind: CmdKind::Completed,
            },
            TestScript::Fail => CmdOutcome {
                exit_code: 1,
                stdout_tail: String::new(),
                stderr_tail: self.failure_tail.clone(),
                kind: CmdKind::Completed,
            },
            TestScript::TimeOut => CmdOutcome {
                exit_code: -1,
                stdout_tail: String::new(),
                stderr_tail: "command timed out after 300s".to_string(),
                kind: CmdKind::TimedOut,
            },
            TestScript::OutOfMemory => CmdOutcome {
                exit_code: -1,
                stdout_tail: String::new(),
                stderr_tail: "Killed".to_string(),
                kind: CmdKind::OutOfMemory,
            },
            TestScript::Infra => CmdOutcome {
                exit_code: -1,
                stdout_tail: String::new(),
                stderr_tail: "No such file or directory (os error 2)".to_string(),
                kind: CmdKind::Infra,
            },
            TestScript::PassWith(output) => CmdOutcome {
                exit_code: 0,
                stdout_tail: output.to_string(),
                stderr_tail: String::new(),
                kind: CmdKind::Completed,
            },
            TestScript::FailWith(output) => CmdOutcome {
                exit_code: 1,
                stdout_tail: output.to_string(),
                stderr_tail: self.failure_tail.clone(),
                kind: CmdKind::Completed,
            },
        }
    }
}

pub(super) struct EmptyTools;
#[async_trait]
impl ToolExecutor for EmptyTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: String::new(),
        }
    }
}

#[derive(Default)]
struct NoopSleeper;
#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

struct ZeroClock;
impl Clock for ZeroClock {
    fn now_ms(&self) -> u64 {
        0
    }
}

/// A scope gate scripted with a fixed decision.
struct FixedGate(ScopeDecision);
#[async_trait]
impl ApprovalGate for FixedGate {
    async fn review(&self, _proposal: &ScopeProposal) -> ScopeDecision {
        self.0.clone()
    }
}

/// A completion that calls one mutating tool — a turn that *acted*, whatever
/// a probe can afterwards see of it.
///
/// The distinction [`text_result`] cannot express, and the one the ladder's
/// no-op rung turns on: a turn with no tool calls could not have changed
/// anything, so its dark channels mean "nothing happened"; a turn with this
/// one could have, so the same dark channels mean "nobody can tell".
fn writing_tool_result(text: &str) -> CompletionResult {
    CompletionResult {
        tool_calls: vec![ToolCall {
            call_id: "call-1".into(),
            name: WRITING_TOOL.into(),
            input: serde_json::json!({}),
        }],
        ..text_result(text)
    }
}

/// The one tool [`OneWritingTool`] advertises.
const WRITING_TOOL: &str = "write_file";

/// A registry with a single mutating tool, for fixtures that need a turn to
/// have dispatched something. Its `execute` reports success and touches
/// nothing real — which is exactly the state under test: the call happened,
/// and no evidence channel can show what it did.
struct OneWritingTool;
#[async_trait]
impl ToolExecutor for OneWritingTool {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: WRITING_TOOL.into(),
            description: "write a file".into(),
            input_schema: serde_json::json!({ "type": "object" }),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: "written".into(),
        }
    }
}

fn text_result(text: &str) -> CompletionResult {
    CompletionResult {
        text: text.into(),
        tool_calls: vec![],
        usage: CompletionUsage {
            reported: true,
            ..CompletionUsage::default()
        },
        model: "scripted".into(),
        cost_usd: 0.0001,
        finish_reason: None,
    }
}

fn router() -> Router {
    Router::new(
        RoleTable::new(),
        vec![ProviderProfile::new(
            "scripted",
            ModelRef::new("scripted", "worker"),
            ModelRef::new("scripted", "triage"),
            ModelRef::new("scripted", "verifier"),
        )],
        CircuitBreaker::new(Box::new(ZeroClock)),
    )
}

/// Every role pinned to one model — what `--model` alone, a single-provider
/// account, and the benchmark engine posture all produce.
fn single_model_router() -> Router {
    let only = ModelRef::new("scripted", "only");
    Router::new(
        RoleTable::new(),
        vec![ProviderProfile::new(
            "scripted",
            only.clone(),
            only.clone(),
            only,
        )],
        CircuitBreaker::new(Box::new(ZeroClock)),
    )
}

fn drain(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(e) = rx.try_recv() {
        out.push(e);
    }
    out
}

fn stages(events: &[AgentEvent]) -> Vec<StageKind> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Stage { name } => Some(*name),
            _ => None,
        })
        .collect()
}

// tests

/// A single-task goal whose test command flips fail→pass submits fast:
/// deterministic verdict, model verifier SKIPPED.
#[tokio::test]
async fn single_task_with_a_flip_submits_fast_and_skips_the_verifier() {
    // triage → "single"; worker turn → final text (no tool calls).
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    assert_eq!(outcome.task_class, TaskClass::SingleTask);
    assert_eq!(outcome.status, PipelineStatus::Completed);
    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed);
    assert!(verdict.deterministic, "flip → deterministic verdict");

    let events = drain(&mut rx);
    // Verifier stage must NOT appear (submit-fast skips it).
    assert!(
        !stages(&events).contains(&StageKind::Verdict),
        "the verifier must be skipped on a deterministic pass"
    );
    // A deterministic Verdict event must be present.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Verdict {
            passed: true,
            evidence
        } if evidence.deterministic
    )));
}

/// A pause gate that counts its polls — proof the pipeline actually consults
/// the seam, from both kinds of call site.
struct CountingGate(std::sync::atomic::AtomicU32);

#[async_trait::async_trait]
impl stella_core::ports::TurnGate for CountingGate {
    async fn wait_if_paused(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `Pipeline::with_turn_gate` is the seam that lets a supervisor's pause
/// reach a pipeline-driven worker (the raw step-loop always had one; the
/// pipeline path silently ignored pause). The gate must be polled by BOTH
/// kinds of spend: the management chokepoint (triage here) and the engine
/// turns the pipeline builds — otherwise a paused worker keeps buying calls
/// on whichever side was missed.
#[tokio::test]
async fn the_turn_gate_is_polled_by_management_calls_and_engine_turns() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, _rx) = mpsc::unbounded_channel();
    let gate = CountingGate(std::sync::atomic::AtomicU32::new(0));

    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    )
    .with_turn_gate(&gate);

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let polls = gate.0.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        polls >= 2,
        "the gate must be consulted by the triage call AND the worker's engine \
         turn — saw {polls} poll(s)"
    );
}

/// A context-recall port that never answers — a wedged embedding call or an
/// unresponsive CGP host.
struct WedgedRecall;

#[async_trait::async_trait]
impl ContextRecallPort for WedgedRecall {
    async fn recall(&self, _goal: &str) -> Recall {
        // Far past any ceiling, so the timeout is what ends this — the test
        // costs only the ceiling itself, not this sleep.
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        unreachable!("the recall ceiling must fire first")
    }
}

/// #616 — recall is advisory (L-C6), so it must never be able to hang a turn.
///
/// Nothing bounded `ContextRecallPort::recall`, and it is joined with triage
/// *before* the first stage completes, so a wedged embedder or an
/// unresponsive CGP host stopped the whole pipeline with no event after
/// `Stage { ContextRecall }` to say why. Past the ceiling recall degrades to
/// no frames and the turn proceeds. Without the timeout this test hangs
/// rather than fails.
#[tokio::test]
async fn a_wedged_context_recall_degrades_instead_of_hanging_the_turn() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    let tools = EmptyTools;
    let recall = WedgedRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        recall_latency_ceiling: std::time::Duration::from_millis(50),
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("a wedged recall must not fail the run");

    assert_eq!(outcome.status, PipelineStatus::Completed);
    let events = drain(&mut rx);
    // The stage is still announced — the degrade happens after it, so the
    // event stream does not silently skip a stage that was entered.
    assert!(
        stages(&events).contains(&StageKind::ContextRecall),
        "the recall stage must still be entered before it degrades"
    );
}

/// A mid-turn steer reaches the EXECUTE engine: a message queued on the
/// steering tap is injected as the execute turn's next observation and so
/// rides into the returned trajectory. Triage runs as a raw completion (no
/// engine), so the tap is drained only by the execute engine's step loop.
#[tokio::test]
async fn a_queued_steer_is_injected_into_the_execute_turn() {
    // triage → "single"; worker turn → final text (no tool calls).
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let steering = SteeringOnce {
        queued: std::sync::Mutex::new(vec!["also update the changelog".into()]),
        drains: std::sync::atomic::AtomicU32::new(0),
    };
    let (tx, _rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: Some(&steering),
        },
        tx,
        config,
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let injected = messages
        .iter()
        .filter(|m| m.role == MessageRole::User && m.content == "also update the changelog")
        .count();
    assert_eq!(
        injected, 1,
        "the steer must be injected exactly once, into the execute turn"
    );
    assert!(
        steering.drains.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "the execute engine must have drained the steering tap"
    );
}

/// The zero-diff guard: triage misclassifies a file-touching task as a
/// lookup, but the non-empty diff revokes the verifier-skip and the task is
/// verified via the model verifier — it still completes ("correct downgrade").
///
/// The worker genuinely dispatches a mutating call: since #1553 a diff with
/// ZERO dispatched tool calls is foreign motion (someone else's edit in a
/// shared tree) and deliberately does not revoke the skip — the guard fires
/// only for motion the run could have caused.
#[tokio::test]
async fn misclassified_lookup_that_touches_files_still_gets_verified() {
    // triage → "lookup"; worker → writes, then "done"; verifier → "PASS".
    let provider = ScriptedProvider::new(vec![
        text_result("lookup"),
        writing_tool_result("editing"),
        text_result("done"),
        text_result("PASS looks right"),
    ]);
    let resolver = OneProvider(&provider);
    // Non-empty diff → files were touched, by the call above. No test command.
    let runner = ScriptedRunner::new(vec![], "@@ -1 +1 @@\n-a\n+b");
    let tools = OneWritingTool;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        test_command: None,
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Explain the retry policy", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    assert_eq!(outcome.task_class, TaskClass::SimpleLookup);
    assert_eq!(outcome.status, PipelineStatus::Completed);
    let verdict = outcome
        .verdict
        .expect("zero-diff guard forced verification");
    assert!(verdict.passed);
    assert!(!verdict.deterministic, "verified via the model verifier");

    let events = drain(&mut rx);
    assert!(
        stages(&events).contains(&StageKind::Verdict),
        "the zero-diff guard must run the verifier on an unexpected mutation"
    );
}

/// A clean lookup that touches nothing skips planning, verification, and
/// the verifier entirely.
#[tokio::test]
async fn clean_lookup_skips_plan_verify_and_verifier() {
    let provider = ScriptedProvider::new(vec![text_result("lookup"), text_result("the answer")]);
    let resolver = OneProvider(&provider);
    // Empty diff → nothing touched.
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("What does the flip oracle do?", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    assert_eq!(outcome.task_class, TaskClass::SimpleLookup);
    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert!(
        outcome.verdict.is_none(),
        "no verification for a clean lookup"
    );
    assert_eq!(outcome.final_text, "the answer");

    let s = stages(&drain(&mut rx));
    assert!(!s.contains(&StageKind::Plan));
    assert!(!s.contains(&StageKind::Verify));
    assert!(!s.contains(&StageKind::Verdict));
}

/// A greeting takes the conversational fast path: **one** plain completion, no
/// triage call, no plan / witness / execute / verify / verdict. This is the fix
/// for "typing `hi` authored a witness test", and now also for "typing `hi`
/// paid for a classification that could not change the answer".
///
/// The scripted provider serves exactly ONE call. If triage still called the
/// model — or any work stage ran — the queue would be exhausted and the run
/// would error, so the fixture size is itself the assertion.
#[tokio::test]
async fn a_greeting_takes_the_conversational_path_and_skips_all_work() {
    let provider =
        ScriptedProvider::new(vec![text_result("Hi! How can I help with your codebase?")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("hi", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(outcome.final_text, "Hi! How can I help with your codebase?");
    assert!(outcome.verdict.is_none(), "no verification for a greeting");
    assert_eq!(outcome.revisions, 0);
    // Exactly one model call: the reply. A greeting resolves deterministically,
    // so paying a triage round-trip to be told so is pure latency.
    assert!(
        provider.script.lock().await.is_empty(),
        "the greeting spent exactly the one scripted call"
    );

    // The assistant turn is adopted into the trajectory for follow-up context.
    assert!(matches!(
        messages.last(),
        Some(m) if m.content == "Hi! How can I help with your codebase?"
    ));

    let events = drain(&mut rx);
    let s = stages(&events);
    // No work stage ran — only triage, recall, and the terminal complete.
    for forbidden in [
        StageKind::Plan,
        StageKind::ScopeReview,
        StageKind::Witness,
        StageKind::Execute,
        StageKind::Verify,
        StageKind::Verdict,
    ] {
        assert!(
            !s.contains(&forbidden),
            "{forbidden:?} must not run for chat"
        );
    }
    // The reply reached the user as streamed text.
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Text { delta } if delta == "Hi! How can I help with your codebase?"
        )),
        "the conversational reply is emitted as text"
    );
}

/// A multi-step plan above the scope-review thresholds, running headless
/// with no bypass, is a named error (never a silent auto-approve).
#[tokio::test]
async fn paid_headless_scope_review_error_retains_settled_cost() {
    // triage → "multi"; plan → a 6-step JSON array (default threshold 5).
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result(r#"["s1","s2","s3","s4","s5","s6"]"#),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = FixedGate(ScopeDecision::Approve);
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        headless: true,
        headless_bypass_scope_review: false,
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let err = pipeline
        .run(
            "Refactor across the codebase and then update all callers",
            &mut messages,
            &mut budget,
        )
        .await
        .expect_err("headless scope review must be a named error");
    assert_eq!(err.cause, PipelineError::ScopeReviewRequiredHeadless);
    assert!(
        (err.total_cost_usd - 0.0002).abs() < 1e-9,
        "triage and plan spend must survive the hard error: {err:?}"
    );
    // The error leaves through the `Result`, so without an explicit event the
    // stream simply stops mid-plan. A consumer reading only events (the bench
    // adapter, the deck) must still be told why the run ended.
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, .. } if message.contains("scope review")
        )),
        "the headless scope stop announces itself: {events:?}"
    );
}

/// The user aborting at the scope-review gate ends the run cleanly (not an
/// error), with an `Aborted` status.
#[tokio::test]
async fn user_abort_at_scope_review_is_a_clean_abort() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result(r#"["s1","s2","s3","s4","s5","s6"]"#),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = FixedGate(ScopeDecision::Abort);
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run(
            "Refactor across the codebase and then rename all callers",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("a user abort is a clean outcome, not an error");
    assert!(matches!(outcome.status, PipelineStatus::Aborted { .. }));
    // Execution never started.
    let s = stages(&drain(&mut rx));
    assert!(!s.contains(&StageKind::Execute));
}

#[test]
fn count_diff_lines_ignores_headers() {
    let diff = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n context";
    assert_eq!(count_diff_lines(diff), 2);
}

/// P1/P2 regression: a large NEW file must contribute its real added-line
/// count (not a flat 1, which slipped a 10k-line file under the diff
/// budget), and a file already untracked before the turn must not be
/// attributed to it.
#[tokio::test]
async fn gather_diff_counts_real_new_file_lines_and_excludes_pre_existing() {
    let provider = ScriptedProvider::new(vec![]);
    let resolver = OneProvider(&provider);
    // Empty tracked diff; the ScriptedRunner reports src/huge.rs's no-index
    // numstat as 5000 added lines. The repo status reports it as untracked
    // with fingerprint "v2".
    let runner = ScriptedRunner::new(vec![], "").with_untracked(vec![("src/huge.rs", 5000)]);
    let repo_status = FakeRepoStatus::new(vec![("src/huge.rs", "v2")]);
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, _rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );

    let surface = CandidateSurface {
        diagnostics: &runner,
        tests: &runner,
        lint: None,
        mutation: None,
        coverage: None,
        repo_status: &repo_status,
        cwd: None,
        hook_runner: None,
        workspace: None,
    };
    // No baseline → the file is this turn's; its real 5000 lines count.
    let probe = pipeline.gather_diff(surface, &HashMap::new()).await;
    assert_eq!(
        probe.lines, 5000,
        "a new file counts its real added lines, not 1"
    );
    assert!(
        probe.text.contains("src/huge.rs"),
        "diff text names the new file"
    );
    assert!(probe.available, "a readable tree reports itself readable");

    // Present before at the SAME fingerprint → untouched dirty state, not
    // attributed to the turn.
    let unchanged: HashMap<String, String> =
        std::iter::once(("src/huge.rs".to_string(), "v2".to_string())).collect();
    let probe2 = pipeline.gather_diff(surface, &unchanged).await;
    assert_eq!(
        probe2.lines, 0,
        "a pre-existing untracked file is not this turn's change"
    );
    assert!(
        probe2.available,
        "an empty result from a readable tree is still an OBSERVATION — the          distinction the ladder's abstain rung turns on"
    );

    // Present before but at a DIFFERENT fingerprint → the turn edited an
    // already-untracked file; it must be visible (the P1 regression).
    let modified: HashMap<String, String> =
        std::iter::once(("src/huge.rs".to_string(), "v1".to_string())).collect();
    let probe3 = pipeline.gather_diff(surface, &modified).await;
    assert_eq!(
        probe3.lines, 5000,
        "an edit to an already-untracked file is counted"
    );
    assert!(probe3.text.contains("src/huge.rs"));
}

#[test]
fn assemble_user_message_puts_recall_before_the_task() {
    let frames = vec![RecalledFrame {
        citation_label: "driver.rs".into(),
        provider: "code-graph".into(),
        source: "code-graph".into(),
        kind: "symbol".into(),
        uri: None,
        method: None,
        content: "run_turn".into(),
        token_cost: 5,
        id: None,
        content_digest: None,
    }];
    let msg = assemble_user_message("do the thing", &frames, VerificationContract::None);
    let recall_idx = msg.find("Recalled context").unwrap();
    let task_idx = msg.find("do the thing").unwrap();
    assert!(recall_idx < task_idx, "recall rides before the goal");
}

#[test]
fn assemble_user_message_is_just_the_goal_when_no_recall() {
    assert_eq!(
        assemble_user_message("hello", &[], VerificationContract::None),
        "hello"
    );
}

/// The configured test command is the run's actual oracle, so the worker is
/// told it up front instead of discovering it from the first failure's
/// disclosure. Only the operator-configured command ever rides here — an
/// authored witness's command is airlocked, and does not exist at assembly
/// time anyway.
#[test]
fn assemble_user_message_states_the_configured_verification_contract() {
    let msg = assemble_user_message(
        "fix the parser",
        &[],
        VerificationContract::Oracle("cargo test -p parser"),
    );
    let task_idx = msg.find("fix the parser").unwrap();
    let contract_idx = msg.find("`cargo test -p parser`").unwrap();
    assert!(
        task_idx < contract_idx,
        "the task leads; the contract qualifies it"
    );
    assert!(msg.contains("failing before your change and passing after it"));
    assert!(msg.contains("Do not modify the tests it runs"));
}

/// With no oracle and no independent author, the worker is told up front that
/// its own failing test — written before the fix — is the run's only
/// deterministic evidence. This is the same-model degraded posture: the run
/// proceeds, but test-first is now the worker's job and the message says so.
#[test]
fn assemble_user_message_demands_test_first_when_nothing_else_verifies() {
    let msg = assemble_user_message("add retries", &[], VerificationContract::WorkerTestFirst);
    let task_idx = msg.find("add retries").unwrap();
    let contract_idx = msg.find("write the failing test").unwrap();
    assert!(
        task_idx < contract_idx,
        "the task leads; the contract qualifies it"
    );
    assert!(msg.contains("Before implementing"));
    assert!(msg.contains("only deterministic evidence"));
}

/// With no `--test-command`, the witness author arms the flip oracle: its
/// authored command is observed failing, the worker's change flips it, and
/// the run submits fast on deterministic evidence — verifier skipped.
#[tokio::test]
async fn witness_authored_command_arms_the_flip_oracle_and_submits_fast() {
    // triage → "single"; worker → done; THEN the witness author, because the
    // warrant only has a diff to read once the worker has produced one.
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("wrote the test.\nTEST_COMMAND: cargo test --test witness witness -- --exact"),
    ]);
    // The two halves of the flip now come from two trees. Candidate (id 0):
    // the post-execute observation passes. Baseline (id 1): the same command
    // fails on the pre-execution code, which is what makes it a flip rather
    // than one tree observed twice.
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
    );
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
    );
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log);
    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the retry bug",
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    assert_eq!(outcome.status, PipelineStatus::Completed);
    let verdict = outcome.verdict.expect("verified");
    assert!(verdict.passed);
    assert!(
        verdict.deterministic,
        "a witness flip is deterministic evidence: {}",
        verdict.summary
    );
    assert!(
        verdict
            .summary
            .contains("cargo test --test witness witness -- --exact"),
        "the evidence names the witness command: {}",
        verdict.summary
    );

    let s = stages(&events);
    assert!(s.contains(&StageKind::Witness), "witness stage emitted");
    assert!(
        !s.contains(&StageKind::Verdict),
        "verifier skipped on the flip"
    );
}

/// #1538: a candidate that wrote through its isolation into the real tree is
/// failed at verification, in the round that caused it — named as the
/// candidate's own defect. Before the post-seal check, this escape survived
/// verification and surfaced only when the winner's adoption collided with
/// the stray copy, attributed to nobody.
#[tokio::test]
async fn a_candidate_that_wrote_outside_its_workspace_is_failed_not_adopted() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("wrote the test.\nTEST_COMMAND: cargo test --test witness witness -- --exact"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Same authored-witness scripting as the flip test above — the escape
    // must be caught at *verification*, so the run has to get that far.
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone())
        .with_repo_status(SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/witness.rs", "w1")],
        ]))
        .with_escaped(vec!["app/vm.js"]);
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
    );
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());
    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the retry bug",
    )
    .await;
    let outcome = outcome.expect("run returns an outcome");

    match &outcome.status {
        PipelineStatus::Aborted { reason, .. } => {
            assert!(
                reason.contains("wrote outside its isolated workspace"),
                "the escape is named as the candidate's defect: {reason}"
            );
            assert!(
                reason.contains("`app/vm.js`"),
                "the path is named: {reason}"
            );
            assert!(
                !reason.contains("git apply"),
                "caught at verification, not reported as an adoption conflict: {reason}"
            );
        }
        other => panic!("an escaped candidate must abort the run, got {other:?}"),
    }
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, .. } if message.contains("wrote outside its isolated workspace")
        )),
        "the escape reaches the event stream: {events:?}"
    );
    let log = log.lock().unwrap();
    assert!(
        !log.iter().any(|entry| entry.starts_with("adopt:")),
        "escaped work must never be adopted: {log:?}"
    );
}

/// #1539: an author that names a runner the workspace cannot spawn is caught
/// by the availability constraint at parse time — degraded with the honest
/// reason and the available set named, never by spending the baseline run to
/// discover an unobservable command and blaming generic infra noise.
#[tokio::test]
async fn an_unavailable_runner_choice_degrades_naming_the_available_set() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("wrote the test.\nTEST_COMMAND: pytest tests/test_witness.py"),
        // The unauthored ladder's verifier, after the witness degrades.
        text_result("PASS looks right"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone());
    let baseline = FakeWorkspace::new(1, vec![], Ok(vec![]), log.clone())
        .with_available_runners(vec!["cargo"]);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());
    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the retry bug",
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    assert!(
        !matches!(outcome.status, PipelineStatus::Aborted { .. }),
        "an unavailable runner degrades the witness, never the run: {outcome:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, .. }
                if message.contains("witness author chose `pytest`")
                    && message.contains("available runners: cargo")
        )),
        "the degradation names the choice and the available set: {events:?}"
    );
}

/// #1539: a workspace where NO vocabulary runner is usable degrades before
/// the author turn is even dispatched — zero model spend on a witness that
/// could never be observed, and the reason says so.
#[tokio::test]
async fn a_workspace_with_no_usable_runner_skips_the_author_turn() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        // The unauthored ladder's verifier — NOT a witness-author reply. If
        // the author turn had run anyway, it would have consumed this text
        // and degraded on "produced no TEST_COMMAND line" instead of the
        // no-runner reason asserted below.
        text_result("PASS looks right"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone());
    let baseline =
        FakeWorkspace::new(1, vec![], Ok(vec![]), log.clone()).with_available_runners(vec![]);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());
    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the retry bug",
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    assert!(
        !matches!(outcome.status, PipelineStatus::Aborted { .. }),
        "an absent toolchain degrades honestly, never aborts: {outcome:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, .. }
                if message.contains("no supported test runner is available")
        )),
        "the degradation states the toolchain fact: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, .. } if message.contains("produced no TEST_COMMAND")
        )),
        "the author turn must never have been dispatched: {events:?}"
    );
}

/// The point of the assessment: triage can route work onto a cheaper path
/// than the keyword floor would. This goal trips `deterministic_floor`'s
/// "across the codebase" marker — under the old `max(model, floor)` rule it
/// bought a plan, an authored witness, and a verifier no matter what triage said.
/// An independent verifier model IS available here, so a skipped witness proves
/// triage's call was honored rather than independence being unavailable.
///
/// The verifier is the one ceremony triage does NOT get to decline outright: its
/// `VERIFIER: no` was a prompt-time guess, this fixture's diff is behavioral, and
/// nothing proved it — so the waiver does not stand (`verifier_waiver_stands`)
/// and the reviewer runs. Cheaper-than-the-floor still holds: no plan turn,
/// no witness author, no baseline runs.
#[tokio::test]
async fn triage_can_route_work_onto_a_cheaper_path_than_the_keyword_floor() {
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: single\nWITNESS: no\nVERIFIER: no"),
        text_result("done"),
        text_result("PASS looks right"),
    ]);
    let (outcome, events, _) = run_unisolated_with_router(
        &provider,
        PipelineConfig::default(),
        "Rename the retry helper across the codebase",
        router(),
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(
        outcome.task_class,
        TaskClass::SingleTask,
        "triage read the goal; the floor only pattern-matched it"
    );
    let s = stages(&events);
    assert!(!s.contains(&StageKind::Plan), "single task plans nothing");
    assert!(
        !s.contains(&StageKind::Witness),
        "triage said no witness: {s:?}"
    );
    assert!(
        s.contains(&StageKind::Verdict),
        "a behavioral diff keeps its reviewer, whatever triage guessed: {s:?}"
    );
    // Three paid calls: triage, the worker, and the verifier the evidence
    // demanded. The plan and witness-author ceremony triage declined is
    // never bought.
    let calls = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::StepUsage { .. }))
        .count();
    assert_eq!(calls, 3, "no plan or witness-author call is bought: {s:?}");
}

/// The observed failure, end to end at the seam that actually decides it.
///
/// Triage answered `CLASS: chat` for a real request about a folder of
/// documents. The conversational route is tool-less by construction
/// (`run_conversational` calls `metered_raw_call`, which binds no tools), so
/// the worker replied "Let me first check what operating system you're on",
/// could not check anything, and the turn reported complete at 100%. Reaching
/// `Execute` is the whole assertion: that is the tool-bound turn.
#[tokio::test]
async fn a_chat_classification_on_a_files_request_still_reaches_execute() {
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: chat\nWITNESS: no\nVERIFIER: no"),
        text_result("done"),
        // The zero-diff guard revokes the lookup's verifier-skip and the diff is
        // behavioral, so the `VERIFIER: no` waiver does not stand.
        text_result("PASS looks right"),
    ]);
    let (outcome, events, _) = run_unisolated_with_router(
        &provider,
        PipelineConfig::default(),
        "i want you to organize my documents folder but I don't know whats \
         inside so can you explore and help me develop a convention for my \
         files and where they are saved and what i call them",
        router(),
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    let s = stages(&events);
    assert!(
        s.contains(&StageKind::Execute),
        "a request to explore and organize files must reach the tool-bound \
         execute turn, not the tool-less chat path: {s:?}"
    );
    assert_eq!(outcome.status, PipelineStatus::Completed);
}

/// A headless run never takes the chat route on the model's opinion: its goal
/// arrived from a script, a CI job, or a benchmark harness, the chat path is
/// terminal no-work, and a misroute there reports an untouched task as
/// complete. Only the model's say is withheld — the deterministic greeting arm
/// (exercised above) still routes.
#[tokio::test]
async fn headless_runs_ignore_a_model_chat_call_and_reach_execute() {
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: chat\nWITNESS: no\nVERIFIER: no"),
        text_result("done"),
        // Behavioral diff → the `VERIFIER: no` waiver does not stand.
        text_result("PASS looks right"),
    ]);
    let (outcome, events, _) = run_unisolated_with_router(
        &provider,
        PipelineConfig {
            headless: true,
            ..PipelineConfig::default()
        },
        // Short, and free of every deterministic vocabulary veto — an
        // interactive run WOULD route this to chat; headless must not.
        "how would you approach the thing we discussed",
        router(),
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    let s = stages(&events);
    assert!(
        s.contains(&StageKind::Execute),
        "a headless goal must reach the tool-bound execute turn even when \
         triage called it chat: {s:?}"
    );
    assert_eq!(outcome.status, PipelineStatus::Completed);
}

/// Losing the independent witness author must cost the run its authored
/// witness, never the whole task. With every role pinned to one model the
/// pipeline used to abort here after a single model call, having executed
/// nothing — which is what a benchmark or solo-provider account always hits.
/// It must instead warn once and fall through to the unauthored verify ladder.
#[tokio::test]
async fn single_model_config_degrades_to_unauthored_witness_instead_of_aborting() {
    // triage → "single"; worker → done; verifier → verdict. No witness-author
    // turn is scripted because no independent author can be resolved.
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS looks right"),
    ]);
    let (outcome, events, _) = run_unisolated_with_router(
        &provider,
        PipelineConfig::default(),
        "Fix the retry bug",
        single_model_router(),
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    assert_eq!(
        outcome.status,
        PipelineStatus::Completed,
        "a single-model config must still complete the task"
    );
    assert!(
        !stages(&events).contains(&StageKind::Witness),
        "witness authoring is skipped, not attempted without an author"
    );
    // Announced on BOTH channels, which is the contract: the warning carries
    // the transcript's prose account, and the proof step carries the rail's.
    // Reporting on only one is the failure mode this pairing exists to stop —
    // a warning scrolls away, and a rail with nothing to show falls back to
    // "not reported" when the reason was known all along.
    let warned = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::Error { message, retryable: true }
                if message.contains("no author independent of the worker")
        )
    });
    assert!(warned, "the degradation is announced: {events:?}");
    let on_the_rail = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::Proof {
                step: stella_protocol::ProofStep::WitnessUnavailable { reason }
            } if reason.contains("no author independent of the worker")
        )
    });
    assert!(
        on_the_rail,
        "the rail must state the reason, not fall back to `not reported`: {events:?}"
    );
}

/// A witness whose test passes on the pre-execution code proves nothing: one
/// bounded repair retry, and if it still passes the run finishes without one.
///
/// What changed with demand-driven authoring is the *cost* of that outcome.
/// While authoring ran first, a useless witness discarded the whole candidate
/// and `run` degraded to a fresh bare worker turn — the task was executed
/// twice because scaffolding failed. Now the work already exists when the
/// author is asked, so a useless witness costs only the authoring calls and
/// the candidate finishes on the unauthored ladder. The artifact never leaves
/// the authoring snapshot, so nothing the author wrote can reach adoption
/// either way.
#[tokio::test]
async fn a_witness_that_never_fails_finishes_the_run_without_re_executing_it() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"), // worker
        text_result("TEST_COMMAND: cargo test --test witness always_green -- --exact"),
        // The repair attempt also yields a command that passes -> useless.
        text_result("TEST_COMMAND: cargo test --test witness still_green -- --exact"),
        // The candidate then finishes on the unauthored ladder, so give the
        // fallback generous responses.
        text_result("PASS looks right"),
        text_result("done"),
        text_result("PASS looks right"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone());
    // Both the author's command and its repair PASS on the pre-execution
    // code, so neither proves anything.
    let baseline =
        FakeWorkspace::new(1, vec![true, true], Ok(vec![]), log.clone()).with_repo_status(
            SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
        );
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());
    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the retry bug",
    )
    .await;
    let outcome = outcome.expect("a useless witness degrades, it does not error");
    // A witness that proves nothing couldn't be AUTHORED — the task proceeds
    // unwitnessed rather than dying.
    assert!(
        !matches!(outcome.status, PipelineStatus::Aborted { .. }),
        "a useless witness must not end the turn: {outcome:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: true }
                if message.contains("continuing without an authored witness")
        )),
        "the degradation announces itself: {events:?}"
    );
    // The verdict is not deterministic: with no witness there was no flip, so
    // the ladder had to buy the verifier. Nothing is scored as proven.
    let verdict = outcome.verdict.expect("verified");
    assert!(
        !verdict.deterministic,
        "an unauthored run has no deterministic evidence: {}",
        verdict.summary
    );
    // One candidate workspace and one authoring snapshot — NOT a second
    // execution. The old behavior re-ran the whole task here.
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log.iter().filter(|entry| *entry == "create").count(),
        2,
        "one candidate + one authoring snapshot: {log:?}"
    );
    assert!(
        !log.iter().any(|entry| entry.starts_with("graft:")),
        "a rejected witness is never grafted into the candidate: {log:?}"
    );
}

/// Distress guidance: the FIRST deterministic failure revises on raw
/// evidence alone; the SECOND spends one verifier call whose course-correction
/// rides with the next revision prompt.
#[tokio::test]
async fn second_consecutive_red_verification_gets_verifier_guidance() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),      // worker
        text_result("first fix"), // revision 1 (no guidance)
        text_result("You are patching the symptom; fix the parser instead."), // guidance
        text_result("second fix"), // revision 2 (carries guidance)
    ]);
    let resolver = OneProvider(&provider);
    // baseline (fail), post-execute (fail) → revise; post-revision-1
    // (fail) → distress → guidance → revise; post-revision-2 (fail) →
    // revisions exhausted → deterministic failed verdict.
    let runner = ScriptedRunner::new(vec![false, false, false, false], "@@ -1 +1 @@\n-a\n+b");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        max_revisions: 2,
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let verdict = outcome.verdict.expect("verified");
    assert!(!verdict.passed);
    assert!(verdict.deterministic, "red tests are a deterministic fail");
    assert_eq!(outcome.revisions, 2);

    // The guidance text reached the worker's revision prompt.
    let carried = messages.iter().any(|m| {
        m.content.contains("Independent reviewer course-correction")
            && m.content.contains("fix the parser instead")
    });
    assert!(carried, "guidance rides with the second revision prompt");
    assert!(
        stages(&drain(&mut rx)).contains(&StageKind::Verdict),
        "the guidance call is an honest Verifier stage in the stream"
    );
}

// best-of-N candidate isolation

/// Build a pipeline over panicking session command/repo-status ports (so
/// any candidate I/O that escapes its workspace fails the test) and run
/// `goal` with the given port + config. Returns the outcome and events.
async fn run_isolated(
    provider: &ScriptedProvider,
    port: &FakeWorkspacePort,
    config: PipelineConfig,
    goal: &str,
) -> (
    Result<PipelineOutcome, PipelineRunError>,
    Vec<AgentEvent>,
    Vec<CompletionMessage>,
) {
    run_isolated_with_router(provider, port, config, goal, router()).await
}

/// Run over ordinary *session* ports with no candidate-workspace port, the
/// path an unauthored run takes. Isolation exists to protect the session tree
/// from a witness author; with no author there is nothing to protect it from,
/// so no snapshot machinery is engaged — which is what lets Stella work in a
/// plain directory that is not a git repository.
/// The "never choose nothing" backstop: when every candidate fails ISOLATION
/// setup before the worker runs, the pipeline must degrade to a bare
/// execution on the working tree — not end the turn having done nothing.
#[tokio::test]
async fn a_setup_failure_degrades_to_a_bare_execution_instead_of_aborting() {
    // triage → single; worker → done; verifier → verdict. No witness-author
    // turn: the failure is pure isolation setup, and the bare fallback runs
    // the worker once. The verifier runs despite `VERIFIER: no` — the diff is
    // behavioral, so the waiver does not stand (`verifier_waiver_stands`).
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: single\nWITNESS: no\nVERIFIER: no"),
        text_result("done"),
        text_result("PASS looks right"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "@@ -1 +1 @@\n-a\n+b");
    let repo_status = SeqRepoStatus::new(vec![vec![]]);
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    // A candidate port that fails to isolate every candidate. Best-of-N
    // (n=2) drives it, so both candidates are setup_aborted.
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![
            Err(WorkspaceError::Snapshot {
                reason: "not a git repo".into(),
            }),
            Err(WorkspaceError::Snapshot {
                reason: "not a git repo".into(),
            }),
        ],
        log,
    );
    let config = PipelineConfig {
        candidates: Some(2),
        ..PipelineConfig::default()
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: Some(&port),
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the retry bug", &mut messages, &mut budget)
        .await
        .expect("a setup failure is a degradation, not a run-ending error");

    let iso = "candidate isolation failed: workspace snapshot failed: not a git repo";
    assert!(
        !matches!(&outcome.status, PipelineStatus::Aborted { reason, .. } if reason == iso),
        "an isolation setup failure must not end the turn: {outcome:?}"
    );
    assert!(
        !matches!(outcome.status, PipelineStatus::Aborted { .. }),
        "the backstop must produce a real execution, not any abort: {outcome:?}"
    );
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: true }
                if message.contains("running a bare worker turn")
        )),
        "the degradation announces itself once: {events:?}"
    );
}
async fn run_unisolated_with_router(
    provider: &ScriptedProvider,
    config: PipelineConfig,
    goal: &str,
    router: Router,
) -> (
    Result<PipelineOutcome, PipelineRunError>,
    Vec<AgentEvent>,
    Vec<CompletionMessage>,
) {
    let resolver = OneProvider(provider);
    let runner = ScriptedRunner::new(vec![], "@@ -1 +1 @@\n-a\n+b");
    let repo_status = SeqRepoStatus::new(vec![vec![]]);
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline.run(goal, &mut messages, &mut budget).await;
    (outcome, drain(&mut rx), messages)
}

/// [`run_isolated`] over a caller-supplied router, so a test can pin the
/// roles to one model (the single-model configuration every benchmark and
/// solo-provider setup uses).
async fn run_isolated_with_router(
    provider: &ScriptedProvider,
    port: &FakeWorkspacePort,
    config: PipelineConfig,
    goal: &str,
    router: Router,
) -> (
    Result<PipelineOutcome, PipelineRunError>,
    Vec<AgentEvent>,
    Vec<CompletionMessage>,
) {
    run_isolated_full(provider, port, config, goal, router, None).await
}

/// [`run_isolated_with_router`] plus an optional mutation probe (#870).
async fn run_isolated_full(
    provider: &ScriptedProvider,
    port: &FakeWorkspacePort,
    config: PipelineConfig,
    goal: &str,
    router: Router,
    mutation: Option<&dyn MutationProbe>,
) -> (
    Result<PipelineOutcome, PipelineRunError>,
    Vec<AgentEvent>,
    Vec<CompletionMessage>,
) {
    let resolver = OneProvider(provider);
    let diagnostics = NeverRunner;
    let repo_status = NeverRepoStatus;
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &diagnostics,
            tests: &diagnostics,
            lint: None,
            mutation,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: Some(port),
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline.run(goal, &mut messages, &mut budget).await;
    (outcome, drain(&mut rx), messages)
}

fn isolated_config(n: u32) -> PipelineConfig {
    PipelineConfig {
        test_command: Some("cargo test".into()),
        // A red first candidate must fail immediately, not revise — keeps
        // the scripts to one worker turn per candidate.
        max_revisions: 0,
        candidates: Some(n),
        ..PipelineConfig::default()
    }
}

/// Best-of-N candidate isolation tests — see the module doc there for why
/// the shared infra (`run_isolated`, `isolated_config`, ...) stays here.
#[cfg(test)]
mod verification_honesty {
    use super::super::verify_probes::verification_honest_diff;

    /// The archetypal lie: the turn emitted file-change events, but the diff
    /// came back empty (committed work, a baseline miss, an uncaptured file).
    /// A bare empty string reads to a verifier as "no changes were made" — the
    /// signal that once drove an agent to reinitialize git. The guard must
    /// turn it into an honest "couldn't capture", never "verified nothing".
    #[test]
    fn a_blind_empty_diff_with_file_changes_is_reported_as_uncaptured_not_absent() {
        let out = verification_honest_diff(String::new(), 3);
        assert!(
            !out.trim().is_empty(),
            "an empty diff with file changes must not stay empty"
        );
        assert!(out.contains("could not be captured"), "{out}");
        assert!(
            out.contains("NOT evidence that nothing changed"),
            "the marker must foreclose the 'no changes' reading: {out}"
        );
        assert!(out.contains('3'), "it names the file-change count: {out}");
    }

    /// A genuinely empty diff — no file-change events — is the truth and stays
    /// empty. The guard must not invent changes that did not happen.
    #[test]
    fn a_truly_empty_diff_with_no_file_changes_stays_empty() {
        assert_eq!(verification_honest_diff(String::new(), 0), "");
        assert_eq!(verification_honest_diff("   \n".to_string(), 0), "   \n");
    }

    /// A real diff is passed through untouched, regardless of the count.
    #[test]
    fn a_real_diff_passes_through_unchanged() {
        let diff = "@@ -1 +1 @@\n-a\n+b".to_string();
        assert_eq!(verification_honest_diff(diff.clone(), 0), diff);
        assert_eq!(verification_honest_diff(diff.clone(), 5), diff);
    }
}

/// The feedback airlock (`witness::airlock`) observed end to end: what a
/// verification failure tells the operator versus what it tells the worker.
/// A child module, so it reaches the scripted ports above via `super::*`.
mod airlock;
mod best_of_n;
mod chaos;
mod degradation_gate;
/// Golden-trajectory recordings of this pipeline's real event stream — a
/// child module so it reaches the scripted ports above via `super::*`.
mod golden;
/// The orchestrator MCP pre-fetch hook (issue #248) — split out for
/// the same file-size reason `tests.rs` itself was split from
/// `pipeline.rs`; a child module, so it reaches the fakes above via
/// `super::*`.
mod mcp_prefetch;
mod scope_gate_interactive;
mod shared_worktree;
mod terminal_outcomes;
mod usage;
/// Bounded repair after a refuted success claim (#1479).
mod verdict_repair;
mod verification_hardening;
/// Asking for corroboration when only a model verifier approved the work
/// (#1295), and — the part that decides whether the ask is affordable —
/// declining to ask where no tracked command could ever answer. A child
/// module, so it reaches the scripted ports above via `super::*`.
mod verifier_evidence_demand;
/// Proportionate verification: changes with nothing to prove complete with a
/// stated reason rather than escalating. A child module, so it reaches the
/// scripted ports above via `super::*`.
mod warrant;
mod witness_isolation;

/// A recall port whose plane reports what the fan-out spent — the shape
/// `stella-cli`'s `SessionMemory` produces from a real CGP host.
struct MeteredRecall;

#[async_trait::async_trait]
impl ContextRecallPort for MeteredRecall {
    async fn recall(&self, _goal: &str) -> Recall {
        Recall {
            frames: vec![RecalledFrame {
                citation_label: "driver.rs".into(),
                provider: "code-graph".into(),
                source: "code-graph".into(),
                kind: "symbol".into(),
                uri: None,
                method: None,
                content: "run_turn".into(),
                token_cost: 120,
                id: None,
                content_digest: None,
            }],
            usage: Some(ContextUsage {
                budget_requested: 1200,
                budget_consumed: 210,
                as_of: "2026-07-24T00:00:00Z".into(),
                providers: vec![
                    ContextProviderUsage {
                        provider_id: "code-graph".into(),
                        frames_served: 1,
                        frames_rejected: 0,
                        token_cost: 120,
                    },
                    // Served frames that lost fusion — invisible in the frame
                    // mix, and the whole reason the report is taken pre-fusion.
                    ContextProviderUsage {
                        provider_id: "workspace-memory".into(),
                        frames_served: 2,
                        frames_rejected: 1,
                        token_cost: 90,
                    },
                ],
            }),
            latency_ms: 7,
            used_ann_index: Some(false),
        }
    }
}

/// WITNESS (#452): the CGP usage report reaches telemetry. Before this, the
/// `ContextRecall` event carried only the *selected* frames' token sum and a
/// per-provider frame count — so a provider that served expensive frames which
/// lost fusion, or whose frames the host rejected outright, cost real tokens
/// that no event ever recorded. Context cost was visible, not meterable.
#[tokio::test]
async fn the_context_recall_event_carries_the_cgp_usage_report() {
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: single\nWITNESS: no\nVERIFIER: no"),
        text_result("done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "@@ -1 +1 @@\n-a\n+b");
    let repo_status = SeqRepoStatus::new(vec![vec![]]);
    let tools = EmptyTools;
    let recall = MeteredRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let _ = pipeline
        .run("Fix the retry bug", &mut messages, &mut budget)
        .await;

    let events = drain(&mut rx);
    let usage = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ContextRecall { usage, .. } => usage.clone(),
            _ => None,
        })
        .expect("the ContextRecall event must carry the CGP usage report");

    assert!(
        usage.is_consistent(),
        "a metering pipeline must be able to re-sum the total: {usage:?}"
    );
    assert_eq!(usage.budget_requested, 1200);
    assert_eq!(usage.budget_consumed, 210);
    assert_eq!(usage.total_frames_served(), 3);

    let memory = usage
        .providers
        .iter()
        .find(|p| p.provider_id == "workspace-memory")
        .expect("every provider the query reached is itemized");
    assert_eq!(
        memory.frames_served, 2,
        "frames served must be counted even when they lose fusion and never reach the prompt"
    );
    assert_eq!(
        memory.frames_rejected, 1,
        "a host-rejected frame is accounted, not silently forgotten"
    );
    assert_eq!(memory.token_cost, 90);
}

/// #616: recall is clamped at the source — per-frame truncation with an
/// in-content marker, and a dropped tail summarized in a marker frame — so a
/// mis-tuned recall port cannot silently inflate every subsequent turn.
#[test]
fn recalled_frames_are_bounded_with_visible_markers() {
    let frame = |label: &str, content: String| crate::ports::RecalledFrame {
        citation_label: label.into(),
        provider: "test".into(),
        source: "test".into(),
        kind: "memory".into(),
        uri: None,
        method: None,
        content,
        token_cost: 0,
        id: None,
        content_digest: None,
    };

    // One oversized frame is truncated, visibly.
    let bounded = super::bound_recalled_frames(vec![frame("big", "x".repeat(10_000))]);
    assert_eq!(bounded.len(), 1);
    assert!(bounded[0].content.len() < 5_000);
    assert!(
        bounded[0]
            .content
            .contains("truncated during recall budgeting")
    );

    // A pile of frames past the total budget is cut, with the tail counted.
    let many: Vec<_> = (0..20)
        .map(|i| frame(&format!("f{i}"), "y".repeat(3_000)))
        .collect();
    let bounded = super::bound_recalled_frames(many);
    let last = bounded.last().expect("frames survive");
    assert_eq!(last.citation_label, "recall-budget");
    assert!(last.content.contains("dropped"), "{}", last.content);
    let kept_content: usize = bounded.iter().map(|f| f.content.len()).sum();
    assert!(
        kept_content < super::RECALL_PROMPT_BUDGET_CHARS + 4_000 + 200,
        "cumulative content stays near the budget, got {kept_content}"
    );

    // Under budget: untouched, no marker.
    let small = super::bound_recalled_frames(vec![frame("s", "ok".into())]);
    assert_eq!(small.len(), 1);
    assert_eq!(small[0].content, "ok");
}
