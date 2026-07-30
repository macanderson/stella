//! Proportionate verification: a change with nothing to prove completes with a
//! stated reason instead of buying a judge call to confirm the absence of a
//! test that was never warranted.
//!
//! Design: [`docs/design/witness-protocol.md`](../../../../../docs/design/witness-protocol.md) §7.

use super::*;

const DOCS_DIFF: &str = "\
--- a/README.md
+++ b/README.md
@@ -1 +1,2 @@
 # stella
+A clearer opening paragraph.
";

/// The witness: a docs-only change reaches the ladder with no flip and no test
/// command — the shape that used to mean `ModelJudge` — and completes anyway.
///
/// The scripted provider serves exactly TWO calls (triage, worker). A judge
/// call would exhaust it and error the run, so the fixture size is the
/// assertion. Before the warrant, this run paid for a third call to be told
/// that prose has no runtime behavior.
#[tokio::test]
async fn a_docs_only_change_completes_without_buying_a_judge_call() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], DOCS_DIFF);
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
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            // No test command and no witness writer: the ladder has nothing
            // deterministic to stand on, which is exactly the case that used
            // to escalate.
            witness_writer: false,
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Rewrite the README opening", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert!(
        provider.script.lock().await.is_empty(),
        "the run spent exactly triage + worker; a judge call would be a third"
    );
    assert!(
        !stages(&drain(&mut rx)).contains(&StageKind::Judge),
        "no judge stage for a change with nothing to prove"
    );

    let verdict = outcome.verdict.expect("a reasoned verdict, not silence");
    assert!(verdict.passed);
    assert!(
        verdict.summary.contains("documentation only"),
        "the verdict must STATE why no test was written: {}",
        verdict.summary
    );
}

/// The witness: a diff probe that FAILED must never complete as "nothing
/// changed".
///
/// `warrant` is documented fail-closed — anything the diff machinery could not
/// see buys the test. Its one guard against a blind probe is `file_changes`,
/// and that count is structurally zero inside an isolated candidate (the engine
/// emits no `FileChange`; the deck's tap wraps only the session tool stack). So
/// a candidate whose worktree registration had vanished — the observed failure,
/// `git add -A` → "fatal: not a git repository" — reported an empty diff, which
/// read as `NothingChanged`, which completed the run with a PASSING
/// deterministic verdict asserting no behavior had changed. No witness stage,
/// no warning, and a false green.
///
/// The answer is now stronger than the escalation this originally asserted.
/// With no test command, no flip and no recorded touch, a blind probe leaves
/// the ladder with nothing at all to reason over, and a judge handed an empty
/// record does not produce a better answer — it produced `FAIL … the file
/// likely does not exist` about a file that was in the container (#973). So the
/// ladder abstains, and the fixture size is again the assertion: exactly two
/// provider calls, because a third would mean a judge was bought to guess.
#[tokio::test]
async fn a_blind_diff_probe_never_completes_as_nothing_changed() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], DOCS_DIFF).with_blind_diff();
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
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            witness_writer: false,
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the progress bar reset", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let verdict = outcome.verdict.expect("a reasoned verdict, not silence");
    assert!(
        !verdict.summary.contains("no files changed"),
        "a probe that could not read the tree is not a report that the tree is clean: {}",
        verdict.summary
    );
    assert!(
        !verdict.deterministic,
        "nothing deterministic was observed — the diff is the thing that failed"
    );
    assert!(
        verdict.summary.starts_with("UNVERIFIABLE"),
        "the verdict must say plainly that nothing could be observed: {}",
        verdict.summary
    );

    let events = drain(&mut rx);
    assert!(
        !stages(&events).contains(&StageKind::Judge),
        "with every channel blind there is nothing for a judge to read — asking \
         one anyway is what produced a confident false negative"
    );
    // The rail has to say it too. Without a proof step of its own the
    // abstention arrives as `JudgeVerdict { passed: true }` and renders as
    // `✓ passed`, which is the same silence in the opposite direction.
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Proof {
                step: stella_protocol::ProofStep::VerificationUnavailable { .. }
            }
        )),
        "an abstention must reach the proof rail, not only the log"
    );
}

/// The end of the wire the bug broke: what the recorder counted has to be what
/// the judge is told.
///
/// This is the Terminal-Bench trial with one channel restored. The task
/// directory is still not a git repository, so the diff probe is still blind
/// and there is still no test command — but the registry recorded six mutating
/// touches, and six is what must appear in the evidence summary. It read
/// `file_change_events=0`, and the judge answered that the file "likely does
/// not exist" while it sat in the container (#973).
///
/// Note what the six also buy: the ladder escalates rather than abstaining,
/// because a recorded touch is real evidence that the tree changed. Blindness
/// is the *absence* of every channel, not the absence of a readable diff.
#[tokio::test]
async fn the_judge_is_told_what_the_recorder_counted_not_zero() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the change is sound"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], DOCS_DIFF).with_blind_diff();
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    // Nothing before the turn, six mutating touches by the time it is folded —
    // the shape of a real registry, which only ever counts up.
    let touches = SeqTouches::new(vec![0, 6]);
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
            touches: &touches,
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
        PipelineConfig {
            witness_writer: false,
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the progress bar reset", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let judge_prompt = provider
        .prompts()
        .into_iter()
        .find(|p| p.contains("file_change_events="))
        .expect("the judge was asked, and its prompt carries the evidence summary");
    assert!(
        judge_prompt.contains("file_change_events=6"),
        "the judge must be told what the recorder counted: {judge_prompt}"
    );
    assert!(
        stages(&drain(&mut rx)).contains(&StageKind::Judge),
        "six observed mutations are evidence, so this escalates rather than abstaining"
    );
}

/// The other half of the same claim: a blind probe that says WHY it is blind.
///
/// Terminal-Bench task images are plain directories, so `git diff` there does
/// not fail transiently — it can never answer. Naming that is the difference
/// between a reader deciding to retry and a reader deciding to use another
/// channel.
#[tokio::test]
async fn a_non_repository_says_so_rather_than_reporting_a_generic_failure() {
    let provider = ScriptedProvider::new(vec![]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "").with_not_a_repository();
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
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
        repo_status: &repo_status,
        cwd: None,
        hook_runner: None,
        workspace: None,
    };
    let probe = pipeline.gather_diff(surface, &HashMap::new()).await;

    assert!(
        !probe.available,
        "a tree that cannot be read is not a tree that was read and found clean"
    );
    assert_eq!(probe.lines, 0);
    assert!(
        probe.text.contains("not a git repository"),
        "the report must name the cause: {}",
        probe.text
    );
    assert!(
        probe.text.contains("NOT evidence that nothing changed"),
        "and must refuse the inversion outright: {}",
        probe.text
    );
}
