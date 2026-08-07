//! FlipHalt arming on the authored-witness path (#1793).
//!
//! The mid-turn early stop used to be armed only from a configured
//! `--test-command` baseline, so on the authored-witness path — the default
//! for every run without a configured command — a revision kept running to
//! its step and loop caps after the witness had already flipped. This pins
//! the repair: `witness_on_demand` arms the latch the moment the witness's
//! failing baseline is credited into the oracle, and the revision receives
//! it (unfired) through the same `run_engine_turn` seam the execute turn
//! uses.

use super::*;

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
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone())
        .with_repo_status(SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/authority_witness.rs", "sha256:test")],
        ]));
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
