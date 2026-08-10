//! The deterministic feedback boundary at the pipeline seam: a completed test
//! failure reaches the worker as execution data and never as reviewer prose.

use super::*;

/// A completed failing command gives the worker the exact bounded result it
/// needs to attempt a different solution: command, exit code, stdout, stderr.
/// No model-written review or course correction may ride beside it.
#[tokio::test]
async fn a_completed_test_failure_is_the_workers_only_verification_feedback() {
    // A tail shaped like a real assertion failure: it names the test, and it
    // carries the runtime values the assertion compared.
    const SEALED_TAIL: &str = "\
thread 'witness_totals_reconcile' panicked at tests/witness_zz9.rs:14:5:
assertion `left == right` failed
  left: 8675309
 right: 0";

    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),       // worker
        text_result("first fix"),  // revision 1
        text_result("second fix"), // revision 2
    ]);
    let resolver = OneProvider(&provider);
    // baseline (fail), post-execute (fail) → revise; post-revision-1 (fail) →
    // revise; post-revision-2 (fail) → revisions exhausted.
    let runner = ScriptedRunner::new(vec![false, false, false, false], "@@ -1 +1 @@\n-a\n+b")
        .with_failure_tail(SEALED_TAIL);
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, _rx) = mpsc::unbounded_channel();

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

    // The operator is not the adversary: their verdict keeps the real output.
    let verdict = outcome.verdict.expect("verified");
    assert!(!verdict.passed);
    assert!(
        verdict.summary.contains("8675309"),
        "the operator's verdict must carry the real failure: {}",
        verdict.summary
    );

    // The test result is the worker's reason to try another solution.
    let worker_prompts: String = messages.iter().map(|m| m.content.as_str()).collect();
    for expected in [
        "command: cargo test -p x",
        "exit_code: 1",
        "stdout:",
        "stderr:",
        "8675309",
        "assertion `left == right`",
        "witness_totals_reconcile",
        "witness_zz9.rs",
    ] {
        assert!(
            worker_prompts.contains(expected),
            "the deterministic receipt omitted {expected:?}:\n{worker_prompts}"
        );
    }
    assert!(
        !worker_prompts.contains("Independent reviewer"),
        "model prose may not accompany a deterministic test result:\n{worker_prompts}"
    );
    assert_eq!(outcome.revisions, 2, "the revise loop still ran");
}
