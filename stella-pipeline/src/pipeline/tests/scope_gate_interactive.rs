//! The interactive scope-review seam: a run that is NOT headless and carries a
//! real approval gate must raise the card and, on approve, execute — the
//! posture the command deck adopted once it grew a native approval surface.
//! Its headless twin (`ScopeReviewRequiredHeadless`) lives in the parent
//! module; this is the branch that had no coverage.

use super::*;

/// The deck's configuration: NOT headless, with a real approval gate. The
/// same 6-step plan that returns `ScopeReviewRequiredHeadless` for a headless
/// run must instead raise a `ScopeReview` card and, on approve, carry on into
/// execution. This is the seam the command deck relies on — before it was
/// wired, an interactive session dead-ended on any plan over five steps and
/// pointed the user at `headless_scope_bypass`, which that path never reads.
#[tokio::test]
async fn interactive_scope_review_approve_proceeds_past_the_gate() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result(r#"["s1","s2","s3","s4","s5","s6"]"#),
        text_result("working"),
        text_result("done"),
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

    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
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
        // `headless: false` is the default — the deck's exact posture.
        PipelineConfig::default(),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let result = pipeline
        .run(
            "Refactor across the codebase and then update all callers",
            &mut messages,
            &mut budget,
        )
        .await;

    // Whatever else the scripted run does downstream, the scope gate must not
    // be what stopped it.
    if let Err(err) = &result {
        assert_ne!(
            err.cause,
            PipelineError::ScopeReviewRequiredHeadless,
            "an interactive run has someone to ask: {err:?}"
        );
    }
    let events = drain(&mut rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ScopeReview { .. })),
        "the approval card must be raised so the deck can render it"
    );
    assert!(
        stages(&events).contains(&StageKind::Execute),
        "an approved plan proceeds into execution"
    );
}
