//! FlipHalt arming (#1793) — **both** witnesses and the doubles they share.
//!
//! The mid-turn early stop used to be armed only from a configured
//! `--test-command` baseline, so on the authored-witness path — the default
//! for every run without a configured command — a revision kept running to
//! its step and loop caps after the witness had already flipped. The repair:
//! `witness_on_demand` arms the latch the moment the witness's failing
//! baseline is credited into the oracle, and the revision receives it
//! (unfired) through the same `run_engine_turn` seam the execute turn uses.
//!
//! The two paths are pinned by two witnesses that differ only in where the
//! tracked command comes from, so they live together with `PassingShell` and
//! `shell_call_result` — the doubles both need — rather than reaching for
//! them across a module boundary. They were split apart once already, and the
//! parent's next wholesale rewrite deleted the configured-command witness and
//! both doubles without failing a gate: the crate had stopped compiling for
//! the missing doubles first, so nothing was left to notice the missing test.
//! Keeping the cluster in one file is what makes that clobber a merge
//! conflict instead of a silent deletion.

use super::*;

/// A shell double whose every command "passes": the output carries the
/// trailing exit-0 marker [`crate::flip_halt::exit_status`] parses. What
/// [`EmptyTools`] can never express — a worker *observing* the tracked test
/// succeed through a tool result.
struct PassingShell;
#[async_trait]
impl ToolExecutor for PassingShell {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a shell command".into(),
            input_schema: serde_json::json!({ "type": "object" }),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: "1 passed\n[exit code: 0]".into(),
        }
    }
}

/// A completion that runs `command` through the shell — the observation the
/// flip halt correlates by `call_id` and scores against the tracked test.
fn shell_call_result(command: &str) -> CompletionResult {
    CompletionResult {
        tool_calls: vec![ToolCall {
            call_id: format!("call-shell-{command}"),
            name: "bash".into(),
            input: serde_json::json!({ "command": command }),
        }],
        ..text_result("")
    }
}

/// #1793 witness (configured-command side): a revision that observes the
/// tracked test go fail→pass halts at that step boundary instead of running
/// on. The provider is scripted with steps BEYOND the flip; consuming them
/// is exactly the waste `flip_halt` exists to stop, so the call count is the
/// assertion.
#[tokio::test]
async fn a_revision_halts_at_the_step_where_the_tracked_test_flips() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        // Execute turn: acts, but the suite still fails afterwards.
        text_result("done"),
        // Revision, step 1: re-run the tracked test — it now passes.
        shell_call_result("cargo test -p x"),
        // Steps the revision would burn WITHOUT the halt. They must never be
        // consumed: the goal was met at the step above.
        shell_call_result("cargo test -p x"),
        text_result("revision done"),
    ]);
    let resolver = OneProvider(&provider);
    // Baseline fails (arms the halt), post-execute fails (forces the
    // revision), post-revise passes (the flip), confirmation passes (#859).
    let runner = ScriptedRunner::scripted(
        vec![
            TestScript::Fail,
            TestScript::Fail,
            TestScript::Pass,
            TestScript::Pass,
        ],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let tools = PassingShell;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
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

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed, "the flip was confirmed: {verdict:?}");
    assert_eq!(
        provider.prompts().len(),
        3,
        "triage, execute, one revision step — the revision must halt at the \
         boundary where the tracked test flipped, not spend the scripted \
         steps beyond it"
    );
}

/// #1793 witness (authored side): after `witness_on_demand` seeds a failing
/// witness, a revision that observes the witness command pass halts at that
/// step boundary. As in the configured-command twin, the provider is
/// scripted with steps beyond the flip and the call count is the assertion.
#[tokio::test]
async fn an_authored_witness_arms_the_revision_flip_halt() {
    let witness_command = "cargo test --test authority_witness authority_witness -- --exact";
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        // Worker execute turn: one mutating call, then done — with the suite
        // (the authored witness) still failing afterwards.
        writing_tool_result("editing"),
        text_result("worker done"),
        // Witness author, in the pristine baseline snapshot.
        writing_tool_result("authoring"),
        text_result(&format!("TEST_COMMAND: {witness_command}")),
        // Revision, step 1: re-run the witness — it now passes.
        shell_call_result(witness_command),
        // Steps the revision would burn WITHOUT the halt; never consumed.
        writing_tool_result("more edits"),
        text_result("revision done"),
    ]);
    let resolver = OneProvider(&provider);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Candidate: the post-execute observation fails, the post-revise one
    // passes. Its engine turns run against a shell that reports exit 0 —
    // the observation that must now latch the halt.
    let candidate = FakeWorkspace::new(0, vec![false, true], Ok(vec![]), log.clone())
        .with_tools(PassingShell)
        .with_repo_status(SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/authority_witness.rs", "sha256:test")],
        ]));
    // Authoring baseline: the witness fails there, which is what arms it.
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/authority_witness.rs", "sha256:test")],
        ]),
    );
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log);
    let session_runner = NeverRunner;
    let session_status = NeverRepoStatus;
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
            repo_status: &session_status,
            touches: &NoFileTouches,
            diagnostics: &session_runner,
            tests: &session_runner,
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
        PipelineConfig {
            max_revisions: 1,
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
    assert_eq!(
        provider.prompts().len(),
        6,
        "triage, two worker steps, two authoring steps, ONE revision step — \
         the revision must halt at the boundary where the authored witness \
         flipped, not spend the scripted steps beyond it"
    );
}
/// A shell double whose every command "passes": the output carries the
/// trailing exit-0 marker [`crate::flip_halt::exit_status`] parses. What
/// [`EmptyTools`] can never express — a worker *observing* the tracked test
/// succeed through a tool result.
struct PassingShell;
#[async_trait]
impl ToolExecutor for PassingShell {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a shell command".into(),
            input_schema: serde_json::json!({ "type": "object" }),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: "1 passed\n[exit code: 0]".into(),
        }
    }
}

/// A completion that runs `command` through the shell — the observation the
/// flip halt correlates by `call_id` and scores against the tracked test.
fn shell_call_result(command: &str) -> CompletionResult {
    CompletionResult {
        tool_calls: vec![ToolCall {
            call_id: format!("call-shell-{command}"),
            name: "bash".into(),
            input: serde_json::json!({ "command": command }),
        }],
        ..text_result("")
    }
}

/// #1793 witness (configured-command side): a revision that observes the
/// tracked test go fail→pass halts at that step boundary instead of running
/// on. The provider is scripted with steps BEYOND the flip; consuming them
/// is exactly the waste `flip_halt` exists to stop, so the call count is the
/// assertion.
#[tokio::test]
async fn a_revision_halts_at_the_step_where_the_tracked_test_flips() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        // Execute turn: acts, but the suite still fails afterwards.
        text_result("done"),
        // Revision, step 1: re-run the tracked test — it now passes.
        shell_call_result("cargo test -p x"),
        // Steps the revision would burn WITHOUT the halt. They must never be
        // consumed: the goal was met at the step above.
        shell_call_result("cargo test -p x"),
        text_result("revision done"),
    ]);
    let resolver = OneProvider(&provider);
    // Baseline fails (arms the halt), post-execute fails (forces the
    // revision), post-revise passes (the flip), confirmation passes (#859).
    let runner = ScriptedRunner::scripted(
        vec![
            TestScript::Fail,
            TestScript::Fail,
            TestScript::Pass,
            TestScript::Pass,
        ],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let tools = PassingShell;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
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

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed, "the flip was confirmed: {verdict:?}");
    assert_eq!(
        provider.prompts().len(),
        3,
        "triage, execute, one revision step — the revision must halt at the \
         boundary where the tracked test flipped, not spend the scripted \
         steps beyond it"
    );
}
