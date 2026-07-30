//! The feedback airlock at the pipeline seam: the operator sees the runner's
//! real failure, the worker sees only what the disclosure grain allows.
//!
//! Design: [`docs/design/witness-protocol.md`](../../../../../docs/design/witness-protocol.md) §4.

use super::*;

/// The witness for the feedback airlock: a distinctive token printed by the
/// failing test runner must reach the OPERATOR's verdict and must never reach
/// the WORKER's revision prompt.
///
/// Before the airlock, `verify_candidate` fed `deterministic_fail_evidence`'s
/// summary — the raw stderr tail — straight into `revision_prompt`, so this
/// test fails on the old code for the right reason: the token is present in
/// the worker's messages.
#[tokio::test]
async fn the_runner_tail_reaches_the_operator_but_never_the_worker() {
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
        // Isolate the airlock: guidance is a separate model call with its own
        // scrub, and this witness is about the deterministic path.
        distress_guidance: false,
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

    // The worker never sees the detector's fingerprint.
    let worker_prompts: String = messages.iter().map(|m| m.content.as_str()).collect();
    for sealed in [
        "8675309",
        "assertion `left == right`",
        "witness_totals_reconcile",
        "witness_zz9.rs",
    ] {
        assert!(
            !worker_prompts.contains(sealed),
            "the revision prompt leaked sealed material {sealed:?}:\n{worker_prompts}"
        );
    }

    // ...but it is still told enough to converge.
    assert!(
        worker_prompts.contains("Verification did not pass"),
        "the worker must still learn that verification failed"
    );
    assert_eq!(outcome.revisions, 2, "the revise loop still ran");
}
