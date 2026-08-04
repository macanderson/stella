//! Witness-isolation and failed-adoption regression tests.

use super::*;
use stella_core::hooks::{
    HookAction, HookExecError, HookExecResult, HookMatcher, HookRunner, Hooks,
};
use stella_protocol::ToolCall;

struct NeverTools;

#[async_trait]
impl ToolExecutor for NeverTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }

    async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
        panic!("the session ToolExecutor must not serve isolated candidates (got `{name}`)");
    }
}

fn tool_result(name: &str) -> CompletionResult {
    CompletionResult {
        text: String::new(),
        tool_calls: vec![ToolCall {
            call_id: format!("call-{name}"),
            name: name.into(),
            input: serde_json::json!({}),
        }],
        usage: CompletionUsage {
            reported: true,
            ..CompletionUsage::default()
        },
        model: "scripted".into(),
        cost_usd: 0.0001,
        finish_reason: None,
    }
}

#[derive(Default)]
struct RecordingHookRunner {
    cwd: std::sync::Mutex<Vec<String>>,
    payload_cwd: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl HookRunner for RecordingHookRunner {
    async fn run(
        &self,
        _action: &HookAction,
        payload_json: &str,
        cwd: &str,
    ) -> Result<HookExecResult, HookExecError> {
        let mut calls = self.cwd.lock().unwrap();
        std::fs::create_dir_all(cwd).unwrap();
        std::fs::write(
            std::path::Path::new(cwd).join(format!("hook-{}", calls.len())),
            b"candidate hook",
        )
        .unwrap();
        calls.push(cwd.to_string());
        let payload: serde_json::Value = serde_json::from_str(payload_json).unwrap();
        self.payload_cwd
            .lock()
            .unwrap()
            .push(payload["cwd"].as_str().unwrap().to_string());
        Ok(HookExecResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

#[tokio::test]
async fn witness_worker_and_revision_hooks_are_bound_to_the_candidate_root() {
    let candidate_root = tempfile::tempdir().unwrap();
    let session_root = tempfile::tempdir().unwrap();
    let baseline_root = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        tool_result("write_file"),
        text_result("worker done"),
        // Authoring runs after the worker, and its tool call must NOT fire a
        // hook: the authoring snapshot is not the user's tree.
        tool_result("write_file"),
        text_result(
            "TEST_COMMAND: cargo test --test authority_witness authority_witness -- --exact",
        ),
        tool_result("write_file"),
        text_result("revision done"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Candidate: post-execute observation fails, then the revision's passes.
    let candidate = FakeWorkspace::new(0, vec![false, true], Ok(vec![]), log.clone())
        .with_root(candidate_root.path().display().to_string())
        .with_repo_status(SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/authority_witness.rs", "sha256:test")],
        ]));
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone())
        .with_root(baseline_root.path().display().to_string())
        .with_repo_status(SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/authority_witness.rs", "sha256:test")],
        ]));
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log);
    let resolver = OneProvider(&provider);
    let session_runner = NeverRunner;
    let session_status = NeverRepoStatus;
    let tools = NeverTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let hooks = Hooks {
        pre_tool_use: Some(vec![HookMatcher {
            matcher: Some("*".into()),
            hooks: vec![HookAction::new("record cwd")],
        }]),
        post_tool_use: None,
        session_start: None,
    };
    let hook_runner = RecordingHookRunner::default();
    let (tx, _rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &session_status,
            touches: &NoFileTouches,
            diagnostics: &session_runner,
            tests: &session_runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: Some((&hooks, &hook_runner)),
            candidate_workspaces: Some(&port),
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            max_revisions: 1,
            engine: EngineConfig {
                cwd: session_root.path().display().to_string(),
                ..EngineConfig::default()
            },
            ..PipelineConfig::default()
        },
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("pipeline runs");
    assert_eq!(outcome.status, PipelineStatus::Completed);
    let cwd = hook_runner.cwd.lock().unwrap().clone();
    assert_eq!(
        cwd.len(),
        2,
        "witness authoring cannot run hooks; worker and revision each fire once"
    );
    assert!(
        cwd.iter()
            .all(|path| path == &candidate_root.path().display().to_string()),
        "{cwd:?}"
    );
    let payload_cwd = hook_runner.payload_cwd.lock().unwrap().clone();
    assert_eq!(payload_cwd, cwd, "hook payload and process cwd must agree");
    assert_eq!(std::fs::read_dir(candidate_root.path()).unwrap().count(), 2);
    assert_eq!(std::fs::read_dir(session_root.path()).unwrap().count(), 0);
}

/// Pinning worker and verifier to one model costs the run its authored witness,
/// not the task: the pipeline warns once and proceeds down the unauthored
/// verify ladder. (The profile-level single-model case is covered by
/// `single_model_config_degrades_to_unauthored_witness_instead_of_aborting`;
/// this pins the roles explicitly, which is a distinct resolution path.)
#[tokio::test]
async fn authored_witness_degrades_when_verifier_is_worker() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS looks right"),
    ]);
    let resolver = OneProvider(&provider);
    // Session ports, not candidate ones: with no author to isolate from, the
    // run stays in the working tree and engages no snapshot machinery.
    let runner = ScriptedRunner::new(vec![], "@@ -1 +1 @@\n-a\n+b");
    let repo_status = SeqRepoStatus::new(vec![vec![]]);
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let same = ModelRef::new("scripted", "same-model");
    let mut roles = RoleTable::new();
    roles.pin(Role::Worker, same.clone());
    roles.pin(Role::Verifier, same);
    let router = Router::new(
        roles,
        vec![ProviderProfile::new(
            "scripted",
            ModelRef::new("scripted", "worker"),
            ModelRef::new("scripted", "triage"),
            ModelRef::new("scripted", "verifier"),
        )],
        CircuitBreaker::new(Box::new(ZeroClock)),
    );
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("independence failure is a degradation, not a failure");
    assert_eq!(
        outcome.status,
        PipelineStatus::Completed,
        "pinning verifier to the worker must not abort the task: {outcome:?}"
    );
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, retryable: true }
                if message.contains("no author independent of the worker")
        )),
        "the degradation is announced once: {events:?}"
    );
    assert!(!stages(&events).contains(&StageKind::Witness));
}

/// The candidate that executes (id 0) and the pristine snapshot the witness is
/// authored in (id 1), with distinct roots so a graft's source is provable.
fn witness_pair(
    log: &Arc<std::sync::Mutex<Vec<String>>>,
) -> (FakeWorkspace, FakeWorkspace, ScriptedProvider) {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"), // worker executes FIRST
        text_result(
            "TEST_COMMAND: cargo test --test authority_witness authority_witness -- --exact",
        ),
    ]);
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/authority_witness.rs", "sha256:test")],
        ]),
    );
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone())
        .with_root("/authoring/snapshot")
        .with_repo_status(SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/authority_witness.rs", "sha256:test")],
        ]));
    (candidate, baseline, provider)
}

/// `keep_witness` is the explicit promotion step: the same run, opted in,
/// adopts the witness alongside the work. Paired with the default-path
/// assertion in `authored_witness_uses_a_disposable_snapshot_and_grafts_one_file`,
/// this pins BOTH directions of the flag — a default-only test would still
/// pass if the flag were ignored entirely.
#[tokio::test]
async fn keep_witness_promotes_the_authored_witness_into_the_real_tree() {
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (candidate, baseline, provider) = witness_pair(&log);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());

    let (outcome, _, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig {
            candidates: Some(1),
            keep_witness: true,
            ..PipelineConfig::default()
        },
        "Fix the failing test",
    )
    .await;
    let outcome = outcome.expect("run succeeds inside the snapshot");
    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "create",
            "create",
            "graft:0:/authoring/snapshot:tests/authority_witness.rs",
            "remove:1",
            "seal:0",
            "adopt:0",
            "remove:0"
        ],
        "keep_witness withholds nothing — the witness is adopted with the work"
    );
}

/// One candidate workspace to execute in, one throwaway snapshot to author
/// blind in, and exactly one file crossing between them.
///
/// The authoring snapshot is created only because the warrant asked for a
/// witness, and it is removed the moment the artifact has been copied across —
/// it never seals, never adopts, and never outlives the stage.
#[tokio::test]
async fn authored_witness_uses_a_disposable_snapshot_and_grafts_one_file() {
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (candidate, baseline, provider) = witness_pair(&log);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());

    let (outcome, _, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig {
            candidates: Some(1),
            ..PipelineConfig::default()
        },
        "Fix the failing test",
    )
    .await;
    let outcome = outcome.expect("run succeeds inside the snapshot");
    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "create",
            "create",
            // Grafted INTO candidate 0, FROM the authoring snapshot's root:
            // the author worked in a tree that never saw the worker's edits.
            "graft:0:/authoring/snapshot:tests/authority_witness.rs",
            "remove:1",
            "seal:0",
            "adopt:0:withhold=tests/authority_witness.rs",
            "remove:0"
        ],
        "the witness is authored elsewhere, grafted in to be run, and withheld \
         from the real tree by default"
    );
}

#[tokio::test]
async fn sealed_witness_identity_survives_git_reclassification_out_of_untracked_files() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"), // worker
        text_result(
            "TEST_COMMAND: cargo test --test authority_witness authority_witness -- --exact",
        ),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let identity = ArtifactIdentity {
        path: "tests/authority_witness.rs".into(),
        fingerprint: "sha256:test".into(),
        kind: ArtifactKind::Regular,
        mode: 0o100644,
        link_count: 1,
    };
    let candidate_status = SeqRepoStatus::new(vec![
        vec![],
        vec![("tests/authority_witness.rs", "sha256:test")],
        // `git add -A && git commit` in `seal()` makes the grafted witness
        // tracked. Classification changes; filesystem identity does not.
        vec![],
    ])
    .with_artifact_identity(identity.clone());
    let baseline_status = SeqRepoStatus::new(vec![
        vec![],
        vec![("tests/authority_witness.rs", "sha256:test")],
    ])
    .with_artifact_identity(identity);
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone())
        .with_repo_status(candidate_status);
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone())
        .with_root("/authoring/snapshot")
        .with_repo_status(baseline_status);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());

    let (outcome, _, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig {
            candidates: Some(1),
            ..PipelineConfig::default()
        },
        "Fix the failing test",
    )
    .await;

    let outcome = outcome.expect("classification change is not witness tamper");
    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "create",
            "create",
            "graft:0:/authoring/snapshot:tests/authority_witness.rs",
            "remove:1",
            "seal:0",
            "adopt:0:withhold=tests/authority_witness.rs",
            "remove:0"
        ]
    );
}

#[tokio::test]
async fn authored_witness_isolation_failure_degrades_to_a_bare_run() {
    // Isolation is INFRASTRUCTURE, not security: if the authored-witness path
    // can't snapshot a workspace, the run degrades to a bare worker turn on
    // the working tree rather than ending having done nothing. (A poisoned
    // witness is different — that keeps its fail-closed abort; see the
    // symlink/language/production-edit tests below.)
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    // Working session ports — the bare fallback runs over these.
    let runner = ScriptedRunner::new(vec![], "@@ -1 +1 @@\n-a\n+b");
    let repo_status = SeqRepoStatus::new(vec![vec![]]);
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![Err(WorkspaceError::Snapshot {
            reason: "no isolated worktree".into(),
        })],
        log.clone(),
    );
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
        PipelineConfig::default(),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("isolation failure degrades, it does not error");

    assert!(
        !matches!(
            outcome.status,
            PipelineStatus::Aborted { ref reason } if reason.contains("isolation")
        ),
        "an isolation failure must not end the turn: {outcome:?}"
    );
    // Isolation WAS attempted (once, at n=1) before degrading.
    assert_eq!(*log.lock().unwrap(), vec!["create"]);
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: true }
                if message.contains("running a bare worker turn")
        )),
        "the degradation announces itself: {events:?}"
    );
}

/// Assert an aborted run stopped for the stated reason.
///
/// Every abort test below scripts a provider, and an exhausted `ScriptedProvider`
/// *also* aborts the run — with a passing `matches!(.., Aborted { .. })`. So
/// matching on the variant alone cannot tell "the integrity gate fired" from
/// "the fixture ran out of responses one turn earlier", and a reordering of the
/// pipeline silently converts these guards into tests of the fixture.
fn assert_aborted_because(status: &PipelineStatus, expected: &str) {
    let PipelineStatus::Aborted { reason } = status else {
        panic!("expected an aborted run, got {status:?}");
    };
    assert!(
        reason.contains(expected),
        "aborted for the wrong reason\n  expected to contain: {expected}\n  actual: {reason}"
    );
}

#[tokio::test]
async fn tracked_production_edit_by_witness_author_aborts_without_adoption() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("worker done"),
        text_result(
            "TEST_COMMAND: cargo test --test authority_witness authority_witness -- --exact",
        ),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let status = SeqRepoStatus::new(vec![
        vec![],
        vec![("tests/authority_witness.rs", "sha256:test")],
    ])
    .with_tracked(vec![vec![], vec![("src/lib.rs", "sha256:mutated")]]);
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone());
    // The witness test FAILS on the pre-execution code (vec![false]) so the
    // flow reaches the tracked-file mutation check — a fail-closed SECURITY
    // rejection — rather than the (degradable) repair path.
    let baseline =
        FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone()).with_repo_status(status);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());

    let (outcome, _, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the failing test",
    )
    .await;
    let outcome = outcome.expect("author mutation is an aborted candidate");
    assert_aborted_because(
        &outcome.status,
        "witness author modified tracked file(s): src/lib.rs",
    );
    let log = log.lock().unwrap().clone();
    assert!(!log.iter().any(|entry| entry.starts_with("adopt:")));
    assert!(log.contains(&"remove:0".to_string()));
    assert!(
        log.contains(&"remove:1".to_string()),
        "the authoring snapshot is torn down even when it is what failed: {log:?}"
    );
}

/// An artifact whose language does not match its runner is rejected — and now
/// rejected *after* the worker has run, because that is when authoring happens.
/// The fail-closed decision is unchanged; what it costs changed, and this pins
/// both halves so the trade is visible rather than assumed.
#[tokio::test]
async fn witness_language_mismatch_aborts_after_worker_execution() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("worker done"),
        text_result(
            "TEST_COMMAND: cargo test --test authority_witness authority_witness -- --exact",
        ),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone());
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/test_authority.py", "sha256:test")],
        ]),
    );
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());

    let (outcome, _, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the failing test",
    )
    .await;
    let outcome = outcome.expect("mismatch is a truthful candidate abort");
    assert_aborted_because(
        &outcome.status,
        "witness artifact `tests/test_authority.py` does not match test runner `cargo`",
    );
    // Two snapshots, both torn down, nothing adopted: a rejected witness never
    // reaches the real tree and never leaks a workspace.
    assert_eq!(
        *log.lock().unwrap(),
        vec!["create", "create", "remove:1", "remove:0"]
    );
}

#[tokio::test]
async fn symlink_witness_artifact_aborts_after_worker_execution() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("worker done"),
        text_result(
            "TEST_COMMAND: cargo test --test authority_witness authority_witness -- --exact",
        ),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let status = SeqRepoStatus::new(vec![
        vec![],
        vec![("tests/authority_witness.rs", "sha256:symlink")],
    ])
    .with_artifact_identity(ArtifactIdentity {
        path: "tests/authority_witness.rs".into(),
        fingerprint: "sha256:symlink".into(),
        kind: ArtifactKind::Symlink,
        mode: 0o120777,
        link_count: 1,
    });
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone());
    let baseline =
        FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone()).with_repo_status(status);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());

    let (outcome, _, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the failing test",
    )
    .await;
    let outcome = outcome.expect("symlink is a truthful candidate abort");
    // Rejected in the tree that authored it, before any copy — a symlink must
    // never be resolved by the graft into the candidate.
    assert_aborted_because(
        &outcome.status,
        "witness artifact `tests/authority_witness.rs` has an unsafe or unstable filesystem identity",
    );
    let log = log.lock().unwrap().clone();
    assert!(
        !log.iter().any(|entry| entry.starts_with("graft:")),
        "a symlink artifact is rejected before it can be grafted: {log:?}"
    );
}

#[tokio::test]
async fn post_baseline_witness_tamper_is_hard_failure_even_if_verifier_would_pass() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("worker done"),
        text_result("TEST_COMMAND: cargo test --test witness witness -- --exact"),
        text_result("PASS override attempt"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Candidate: pinned at w1 straight after the graft, then found at w2 by
    // the tamper check — the worker rewrote the test it was being judged by.
    let candidate_status = SeqRepoStatus::new(vec![
        vec![],
        vec![("tests/witness.rs", "w1")],
        vec![("tests/witness.rs", "w2")],
    ])
    .with_artifact_identities(vec![
        Some(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w1".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        }),
        Some(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w2".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        }),
    ]);
    let baseline_status = SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]])
        .with_artifact_identity(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w1".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        });
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone())
        .with_repo_status(candidate_status);
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone())
        .with_repo_status(baseline_status);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());

    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig {
            max_revisions: 0,
            ..PipelineConfig::default()
        },
        "Fix the retry bug",
    )
    .await;
    let outcome = outcome.expect("tamper is a truthful candidate failure");
    assert_aborted_because(
        &outcome.status,
        "witness artifact changed after its accepted baseline: tests/witness.rs",
    );
    assert!(!stages(&events).contains(&StageKind::Verifier));
    let log = log.lock().unwrap().clone();
    assert!(
        !log.iter().any(|entry| entry.starts_with("adopt:")),
        "{log:?}"
    );
    assert!(log.contains(&"remove:0".to_string()));
}

#[tokio::test]
async fn failed_final_verification_never_adopts_and_removes_all_candidates() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("candidate zero"),
        text_result("candidate one"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![
            Ok(FakeWorkspace::new(
                0,
                vec![false, false],
                Ok(vec![]),
                log.clone(),
            )),
            Ok(FakeWorkspace::new(
                1,
                vec![false, false],
                Ok(vec![]),
                log.clone(),
            )),
        ],
        log.clone(),
    );

    let (outcome, _, _) =
        run_isolated(&provider, &port, isolated_config(2), "Fix the failing test").await;
    let outcome = outcome.expect("red verification is a terminal outcome");
    assert!(matches!(
        outcome.status,
        PipelineStatus::VerificationFailed { .. }
    ));
    let log = log.lock().unwrap().clone();
    assert!(
        !log.iter().any(|entry| entry.starts_with("adopt:")),
        "{log:?}"
    );
    assert!(log.contains(&"remove:0".to_string()));
    assert!(log.contains(&"remove:1".to_string()));
}

#[tokio::test]
async fn post_verification_candidate_drift_is_rejected_before_adoption() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workspace = FakeWorkspace::new(0, vec![false, true], Ok(vec![]), log.clone())
        .with_post_verification_drift();
    let port = FakeWorkspacePort::new(
        vec![
            Ok(workspace),
            Err(WorkspaceError::Snapshot {
                reason: "second candidate unavailable".into(),
            }),
        ],
        log.clone(),
    );

    let (outcome, _, _) =
        run_isolated(&provider, &port, isolated_config(2), "Fix the failing test").await;
    let outcome = outcome.expect("drift is a truthful candidate failure");
    assert!(matches!(outcome.status, PipelineStatus::Aborted { .. }));
    let log = log.lock().unwrap().clone();
    assert!(log.contains(&"seal:0".to_string()), "{log:?}");
    assert!(
        !log.iter().any(|entry| entry.starts_with("adopt:")),
        "{log:?}"
    );
    assert!(log.contains(&"remove:0".to_string()));
}

/// The saving this change exists for: a change with nothing to prove buys no
/// witness at all.
///
/// The worker edits documentation only. Because authoring now runs *after*
/// execution, `witness::warrant` reads that diff before anything is bought and
/// answers `DocsOnly` — so no authoring snapshot is created and no author model
/// call is dispatched. Under the previous ordering this exact run paid for a
/// witness author turn against a tree that had not been touched yet, and only
/// discovered afterwards that the change was prose.
///
/// Scripting exactly THREE provider responses is what makes this a real test: a
/// surviving witness-author call would exhaust the queue and abort the run.
#[tokio::test]
async fn a_docs_only_change_authors_no_witness_and_creates_no_second_snapshot() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("updated the docs"), // worker
        text_result("PASS reads well"),  // verifier: TestsOnly/PureRemoval keep it
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![], Ok(vec![]), log.clone())
        .with_diff("--- a/docs/retry.md\n+++ b/docs/retry.md\n@@ -1 +1 @@\n-old prose\n+new prose");
    // ONE workspace queued. A second `create` would fail the snapshot and warn,
    // so the `create` count assertion below cannot be satisfied by accident.
    let port = FakeWorkspacePort::new(vec![Ok(candidate)], log.clone());
    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Document the retry helper",
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    assert_eq!(outcome.status, PipelineStatus::Completed);
    let verdict = outcome.verdict.expect("verified");
    assert!(verdict.passed);
    assert!(
        verdict.summary.contains("documentation only"),
        "the run states why no test was written: {}",
        verdict.summary
    );
    assert!(
        !stages(&events).contains(&StageKind::Witness),
        "no witness stage runs when nothing warrants one: {:?}",
        stages(&events)
    );
    assert!(
        !usage::usage_roles(&events)
            .iter()
            .any(|role| role == "witness_author"),
        "a docs-only change dispatches no author call: {:?}",
        usage::usage_roles(&events)
    );
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log.iter().filter(|entry| *entry == "create").count(),
        1,
        "no authoring snapshot is taken for a change with nothing to prove: {log:?}"
    );
}

/// A graft that cannot land degrades; it does not discard the work.
///
/// The realistic collision: the worker wrote its own test at the path the
/// author chose. Overwriting would destroy real work to make room for
/// scaffolding, so the graft fails — and because execution already happened,
/// the candidate finishes on the unauthored ladder instead of being thrown
/// away. Fail-closed still applies to artifacts that cannot be *trusted*; this
/// is one that cannot be *placed*.
#[tokio::test]
async fn a_graft_collision_keeps_the_work_and_drops_only_the_witness() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"), // worker
        text_result("TEST_COMMAND: cargo test --test witness witness -- --exact"),
        text_result("PASS looks right"), // verifier, since there is no flip
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone())
        .with_graft_failure("exclusive file creation failed: File exists (os error 17)");
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
    let outcome = outcome.expect("a failed graft is not a failed run");

    assert!(
        !matches!(outcome.status, PipelineStatus::Aborted { .. }),
        "the worker's change survives a failed graft: {outcome:?}"
    );
    let verdict = outcome.verdict.expect("verified");
    assert!(
        !verdict.deterministic,
        "with no witness in the tree there is no flip to be deterministic about: {}",
        verdict.summary
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error { message, retryable: true }
                if message.contains("continuing without an authored witness")
        )),
        "the degradation announces itself: {events:?}"
    );
    // Nothing is withheld, because nothing was grafted — the withhold list and
    // what is actually in the tree can never disagree.
    let log = log.lock().unwrap().clone();
    assert!(
        log.contains(&"adopt:0".to_string()),
        "the work is adopted with no witness to withhold: {log:?}"
    );
}

/// Tamper exclusion: the worker modified the witness test file after it
/// was authored, so the candidate hard-fails without verifier override.
#[tokio::test]
async fn a_tampered_witness_file_hard_fails_before_verifier_evaluation() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"), // worker
        text_result("TEST_COMMAND: cargo test --test witness witness -- --exact"),
        text_result("FAIL the test was edited"), // must remain unused
    ]);
    // The candidate's own repo status. `artifact_identity` is asked twice: once
    // right after the graft, to pin the baseline against the bytes that will
    // actually run (w1), and once at the tamper check, where the worker has
    // since rewritten the file (w2) — TAMPERED.
    let candidate_status = SeqRepoStatus::new(vec![
        vec![],
        vec![("tests/witness.rs", "w1")],
        vec![("tests/witness.rs", "w2")],
    ])
    .with_artifact_identities(vec![
        Some(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w1".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        }),
        Some(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w2".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        }),
    ]);
    // The authoring snapshot: the artifact appears (w1) and is accepted there.
    let baseline_status = SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]])
        .with_artifact_identity(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w1".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        });
    let config = PipelineConfig {
        max_revisions: 0, // verifier FAIL ends the run — keeps the script short
        ..PipelineConfig::default()
    };
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Candidate: the post-execute observation PASSES, so the oracle flips and
    // the ladder would otherwise submit fast. Tamper must override that.
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone())
        .with_repo_status(candidate_status);
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone())
        .with_repo_status(baseline_status);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log);
    let (outcome, events, _) = run_isolated(&provider, &port, config, "Fix the retry bug").await;
    let outcome = outcome.expect("run succeeds");

    let PipelineStatus::Aborted { reason } = &outcome.status else {
        panic!("tamper must abort the candidate, got {:?}", outcome.status);
    };
    assert!(
        reason.contains("witness artifact changed after its accepted baseline: tests/witness.rs"),
        "aborted for the wrong reason: {reason}"
    );
    assert!(
        !stages(&events).contains(&StageKind::Verifier),
        "tamper is not eligible for verifier override"
    );
}

/// Tamper exclusion: the worker RENAMED the witness test file after it was
/// authored. A rename keeps every content facet — bytes, mode, link count —
/// and an aliased lookup (a case-folding filesystem, a symlinked parent
/// directory) can still resolve the pinned path to those bytes, so the
/// observation arrives identical in everything but the location it was
/// actually made at. The location is part of the pinned identity: the
/// candidate hard-fails without verifier override, exactly like an edit.
#[tokio::test]
async fn a_renamed_witness_file_hard_fails_before_verifier_evaluation() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"), // worker
        text_result("TEST_COMMAND: cargo test --test witness witness -- --exact"),
        text_result("PASS override attempt"), // must remain unused
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    // The candidate's own repo status. `artifact_identity` is asked twice:
    // once right after the graft, where the artifact sits at its accepted
    // path (w1 at tests/witness.rs), and once at the tamper check — where the
    // worker has since moved the file (the untracked delta shows the move):
    // the very same bytes, observed at tests/renamed_witness.rs.
    let candidate_status = SeqRepoStatus::new(vec![
        vec![],
        vec![("tests/witness.rs", "w1")],
        vec![("tests/renamed_witness.rs", "w1")],
    ])
    .with_artifact_identities(vec![
        Some(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w1".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        }),
        Some(ArtifactIdentity {
            path: "tests/renamed_witness.rs".into(),
            fingerprint: "w1".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        }),
    ]);
    // The authoring snapshot: the artifact appears (w1) and is accepted there.
    let baseline_status = SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]])
        .with_artifact_identity(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w1".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        });
    // The post-execute observation PASSES — the renamed file still collects
    // under the flip command on this run — so the ladder would submit fast.
    // The rename must override that.
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone())
        .with_repo_status(candidate_status);
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone())
        .with_repo_status(baseline_status);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());
    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig {
            max_revisions: 0,
            ..PipelineConfig::default()
        },
        "Fix the retry bug",
    )
    .await;
    let outcome = outcome.expect("a renamed witness is a truthful candidate failure");

    assert_aborted_because(
        &outcome.status,
        "witness artifact changed after its accepted baseline: tests/witness.rs",
    );
    assert!(
        !stages(&events).contains(&StageKind::Verifier),
        "a renamed witness is not eligible for verifier override"
    );
    let log = log.lock().unwrap().clone();
    assert!(
        !log.iter().any(|entry| entry.starts_with("adopt:")),
        "a moved witness must never reach adoption: {log:?}"
    );
}

/// The grafted witness is scaffolding, not the worker's work, so it must never
/// be attributed to the turn's diff.
///
/// Authoring-before-execute got this exclusion for free: the artifact existed
/// before `untracked_before` was snapshotted, so every later gather saw it as
/// pre-existing dirty state. Grafting after execution writes the file *after*
/// that snapshot, so the next gather — the one a revision performs — would read
/// a new untracked file and bill it to the worker, inflating `diff_lines`
/// against the SubmitFast budget and showing a verifier a change nobody made.
/// `witness_on_demand` enrolls the grafted path at its pinned fingerprint to
/// restore the exclusion.
#[tokio::test]
async fn a_grafted_witness_is_not_billed_to_the_workers_diff() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"), // worker
        text_result(
            "TEST_COMMAND: cargo test --test authority_witness authority_witness -- --exact",
        ),
        text_result("revision done"),
        // No verifier response. An escalation past SubmitFast aborts the run
        // rather than quietly passing on a heuristic.
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let identity = ArtifactIdentity {
        path: "tests/authority_witness.rs".into(),
        fingerprint: "sha256:test".into(),
        kind: ArtifactKind::Regular,
        mode: 0o100644,
        link_count: 1,
    };
    // The witness only appears in the candidate's untracked set once it has
    // been grafted — i.e. from the revision's diff gather onward, which is
    // exactly the gather that would misattribute it.
    let candidate = FakeWorkspace::new(0, vec![false, true], Ok(vec![]), log.clone())
        .with_untracked(vec![("tests/authority_witness.rs", 40)])
        .with_repo_status(
            SeqRepoStatus::new(vec![
                vec![],
                vec![],
                vec![("tests/authority_witness.rs", "sha256:test")],
            ])
            .with_artifact_identity(identity.clone()),
        );
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone())
        .with_root("/authoring/snapshot")
        .with_repo_status(
            SeqRepoStatus::new(vec![
                vec![],
                vec![("tests/authority_witness.rs", "sha256:test")],
            ])
            .with_artifact_identity(identity),
        );
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());

    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig {
            candidates: Some(1),
            max_revisions: 1,
            // Deliberately tiny. The worker's own diff fits; the witness's 40
            // added lines would not, so SubmitFast survives only if the
            // grafted artifact is excluded from the count.
            diff_budget_lines: 5,
            ..PipelineConfig::default()
        },
        "Fix the failing test",
    )
    .await;
    let outcome = outcome.expect("run succeeds");
    assert_eq!(outcome.status, PipelineStatus::Completed);

    let verdict = outcome.verdict.expect("verified");
    assert!(
        verdict.deterministic,
        "the flip submits fast; the witness never entered the diff budget: {}",
        verdict.summary
    );
    assert!(
        !stages(&events).contains(&StageKind::Verifier),
        "no verifier is bought: {:?}",
        stages(&events)
    );
}

/// ROADMAP §6's residual, pinned: the witness seal holds BETWEEN the revise
/// turns of a single candidate. The tamper check runs every verify-loop
/// iteration, so a revise turn that edits the witness file it is being
/// judged by is caught at the next iteration's check and hard-fails the
/// candidate — no verifier can be asked to bless it, and nothing is adopted.
#[tokio::test]
async fn a_revise_turn_that_edits_the_witness_hard_fails_at_the_next_check() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("worker done"),
        text_result("TEST_COMMAND: cargo test --test witness witness -- --exact"),
        // The revise turn — where the tampering happens.
        text_result("adjusted the implementation"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Iteration 1 sees the witness at its pinned identity (w1) and a red
    // test → revise. Iteration 2 finds the file at w2: the revise turn
    // rewrote the test it was being judged by.
    //
    // `artifact_identity` is consumed three times on the candidate: once by
    // the graft re-pin (w1, accepted), once by iteration 1's tamper check
    // (w1, still matches → the red test drives the revise turn), and once by
    // iteration 2's tamper check (w2, mismatch → abort AFTER the revise).
    let candidate_status = SeqRepoStatus::new(vec![
        vec![],
        vec![("tests/witness.rs", "w1")],
        vec![("tests/witness.rs", "w2")],
    ])
    .with_artifact_identities(vec![
        Some(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w1".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        }),
        Some(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w1".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        }),
        Some(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w2".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        }),
    ]);
    let baseline_status = SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]])
        .with_artifact_identity(ArtifactIdentity {
            path: "tests/witness.rs".into(),
            fingerprint: "w1".into(),
            kind: ArtifactKind::Regular,
            mode: 0o100644,
            link_count: 1,
        });
    // Red on both observations: the first drives the revise, the second is
    // never reached — the tamper check aborts first.
    let candidate = FakeWorkspace::new(0, vec![false, false], Ok(vec![]), log.clone())
        .with_repo_status(candidate_status);
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone())
        .with_repo_status(baseline_status);
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log.clone());

    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig {
            max_revisions: 1,
            ..PipelineConfig::default()
        },
        "Fix the retry bug",
    )
    .await;
    let outcome = outcome.expect("mid-loop tamper is a truthful candidate failure");
    assert_aborted_because(
        &outcome.status,
        "witness artifact changed after its accepted baseline: tests/witness.rs",
    );
    assert!(!stages(&events).contains(&StageKind::Verifier));
    let log = log.lock().unwrap().clone();
    assert!(
        !log.iter().any(|entry| entry.starts_with("adopt:")),
        "{log:?}"
    );
}

/// `require_independent_witness` turns the degradation above into a refusal —
/// and does it BEFORE the run spends anything (#1147).
///
/// The soft path is right for a session: a missing author costs the run its
/// authored witness, never the task. It is wrong for a host that has already
/// published the claim that an independent author exists. A benchmark arm
/// hashes its posture, names a second model in it, and reports numbers against
/// that hash; if the author silently goes missing the run still finishes, still
/// scores, and the number is now described by a configuration the run did not
/// have. Refusing is the only outcome that keeps the manifest true.
///
/// The provider is scripted with NO responses at all: reaching a single model
/// call would panic, which is how "before it spends anything" is proven rather
/// than asserted.
#[tokio::test]
async fn requiring_an_independent_witness_refuses_before_spending_anything() {
    let provider = ScriptedProvider::new(vec![]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let repo_status = SeqRepoStatus::new(vec![vec![]]);
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let same = ModelRef::new("scripted", "same-model");
    let mut roles = RoleTable::new();
    roles.pin(Role::Worker, same.clone());
    roles.pin(Role::Verifier, same);
    let router = Router::new(
        roles,
        vec![ProviderProfile::new(
            "scripted",
            ModelRef::new("scripted", "worker"),
            ModelRef::new("scripted", "triage"),
            ModelRef::new("scripted", "verifier"),
        )],
        CircuitBreaker::new(Box::new(ZeroClock)),
    );
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
        PipelineConfig {
            require_independent_witness: true,
            ..PipelineConfig::default()
        },
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let error = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect_err("a published independence claim the wiring cannot honor must refuse");

    match &error.cause {
        PipelineError::WitnessAuthorUnavailable(reason) => assert!(
            reason.contains("no author independent of the worker"),
            "the refusal must carry the reason, not just the verdict: {reason}"
        ),
        other => panic!("expected WitnessAuthorUnavailable, got {other:?}"),
    }
    assert_eq!(
        error.total_cost_usd, 0.0,
        "refusing after paying for a trajectory nobody may use defeats the point"
    );
    assert!(
        stages(&drain(&mut rx)).is_empty(),
        "no stage may open before the refusal"
    );
}

/// The same wiring with the flag OFF — the default every ordinary session
/// runs on — must still degrade rather than refuse. The refusal is opt-in for
/// hosts that made a claim outside the run; changing the default would turn a
/// single-provider setup into a broken CLI.
#[tokio::test]
async fn the_independence_requirement_is_opt_in() {
    assert!(
        !PipelineConfig::default().require_independent_witness,
        "a single-key setup must keep working: the verifier legitimately rides \
         the worker there, and `authored_witness_degrades_when_verifier_is_worker` \
         pins that path"
    );
}

/// A verifier distinct from the worker satisfies the requirement, so the flag is
/// inert on the arm it exists to protect: the treatment arm must RUN, not
/// merely fail loudly. Without this the previous test would pass just as well
/// against a guard that refuses unconditionally.
#[tokio::test]
async fn requiring_an_independent_witness_lets_a_distinct_verifier_through() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
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
    let mut roles = RoleTable::new();
    roles.pin(Role::Worker, ModelRef::new("scripted", "worker-model"));
    roles.pin(Role::Verifier, ModelRef::new("scripted", "author-model"));
    let router = Router::new(
        roles,
        vec![ProviderProfile::new(
            "scripted",
            ModelRef::new("scripted", "worker-model"),
            ModelRef::new("scripted", "triage"),
            ModelRef::new("scripted", "author-model"),
        )],
        CircuitBreaker::new(Box::new(ZeroClock)),
    );
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
        PipelineConfig {
            require_independent_witness: true,
            // Witness authoring itself needs candidate workspaces, which this
            // fixture has none of; the user's own test command keeps the run
            // on the single-shot path. The claim under test is the GUARD, and
            // the guard reads the router, not the ladder.
            test_command: Some("cargo test".to_string()),
            ..PipelineConfig::default()
        },
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await;
    assert!(
        !matches!(
            outcome.as_ref().map_err(|e| &e.cause),
            Err(PipelineError::WitnessAuthorUnavailable(_))
        ),
        "an independent verifier must satisfy the requirement: {outcome:?}"
    );
}
