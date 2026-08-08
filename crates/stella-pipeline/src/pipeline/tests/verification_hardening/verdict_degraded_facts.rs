//! The #1787 half of the verification hardening series: a verifier whose
//! replies carry no verdict token records one structured
//! [`ProofStep::VerdictDegraded`] fact per candidate, not per round.
//! Split out of the parent module to keep it under the 1500-line ratchet.

use super::*;

/// #1787 witness, fan-out half: TWO isolated candidates escalate to a
/// verifier whose replies never carry a verdict token, and the stream records
/// one structured [`ProofStep::VerdictDegraded`] fact per candidate — the
/// once-per-run prose warning cannot say which candidates the heuristic
/// judged, and before the fact existed nothing did.
#[tokio::test]
async fn a_two_candidate_fanout_records_which_candidates_degraded() {
    // triage, then per candidate one worker turn and one verifier escalation.
    // Every post-triage reply is deliberately tokenless: candidates run
    // concurrently, so whichever call pops which reply, no verifier can parse
    // a verdict out of it — the degradation is scripted independent of
    // completion order.
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("worked on it"),
        text_result("Here is my assessment of the change."),
        text_result("worked on it"),
        text_result("Here is my assessment of the change."),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![
            Ok(FakeWorkspace::new(0, vec![], Ok(vec![]), log.clone())),
            Ok(FakeWorkspace::new(1, vec![], Ok(vec![]), log.clone())),
        ],
        log,
    );
    // No test command and no witness author resolvable, so each candidate's
    // ladder is inconclusive over its diff and escalates to the verifier.
    let config = PipelineConfig {
        candidates: Some(2),
        max_revisions: 0,
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        distress_guidance: false,
        ..PipelineConfig::default()
    };

    let (outcome, events, _messages) = run_isolated(&provider, &port, config, "Fix the bug").await;
    outcome.expect("the run proceeds to a (failed) verdict");

    let mut degraded: Vec<u32> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Proof {
                step: ProofStep::VerdictDegraded { candidate, .. },
            } => Some(*candidate),
            _ => None,
        })
        .collect();
    degraded.sort_unstable();
    assert_eq!(
        degraded,
        vec![1, 2],
        "each candidate's degradation is recorded, keyed by ordinal"
    );
    let warnings = events
        .iter()
        .filter(|event| {
            matches!(event, AgentEvent::Error { message, .. }
                if message.contains("falls back to a deterministic heuristic"))
        })
        .count();
    assert_eq!(
        warnings, 1,
        "the transcript warning stays once per run; the per-candidate record is the proof step"
    );
}

/// #1787 witness, dedup half: ONE candidate whose escalation hits the same
/// non-compliant verifier on the first round AND on the revision records its
/// degradation fact exactly once — per candidate, not per round (the ladder
/// rung already counts rounds).
#[tokio::test]
async fn a_candidate_degrading_on_every_round_records_one_fact() {
    // triage; worker; tokenless verdict; revision turn; tokenless verdict.
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("Here is my assessment of the change."),
        text_result("revised"),
        text_result("Here is my assessment of the change."),
    ]);
    let resolver = OneProvider(&provider);
    // Red baseline, then a timed-out observation on every round: no flip, no
    // touched-test result, a diff — the ladder escalates each time.
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::TimeOut, TestScript::TimeOut],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = SeqRepoStatus::new(vec![vec![], vec![]]);
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        max_revisions: 1,
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
        .expect("the run proceeds to a (failed) verdict");
    assert!(
        matches!(outcome.status, PipelineStatus::VerificationFailed { .. }),
        "both rounds degraded to the failing heuristic: {:?}",
        outcome.status
    );

    // Both scripted verdict calls were really made — without this, a run that
    // never reached the second escalation would pass the one-fact assertion
    // below for the wrong reason.
    assert_eq!(
        provider.prompts().len(),
        5,
        "triage, worker, verdict, revision, verdict"
    );

    let events = drain(&mut rx);
    let facts: Vec<u32> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Proof {
                step: ProofStep::VerdictDegraded { candidate, .. },
            } => Some(*candidate),
            _ => None,
        })
        .collect();
    assert_eq!(
        facts,
        vec![1],
        "two degraded rounds, one candidate, one fact"
    );
}
