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
/// Scripts a third provider response so the escalation this must produce has a
/// judge to reach; before the fix the run stopped at two and the third was
/// never drawn.
#[tokio::test]
async fn a_blind_diff_probe_never_completes_as_nothing_changed() {
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
        stages(&drain(&mut rx)).contains(&StageKind::Judge),
        "an unreadable diff must escalate, not self-certify"
    );
}
