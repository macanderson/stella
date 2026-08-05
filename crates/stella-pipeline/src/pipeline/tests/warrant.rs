//! Proportionate verification: a change with nothing to prove completes with a
//! stated reason instead of buying a verifier call to confirm the absence of a
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
/// command — the shape that used to mean `ModelVerdict` — and completes anyway.
///
/// The scripted provider serves exactly TWO calls (triage, worker). A verifier
/// call would exhaust it and error the run, so the fixture size is the
/// assertion. Before the warrant, this run paid for a third call to be told
/// that prose has no runtime behavior.
#[tokio::test]
async fn a_docs_only_change_completes_without_buying_a_verifier_call() {
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
        "the run spent exactly triage + worker; a verifier call would be a third"
    );
    assert!(
        !stages(&drain(&mut rx)).contains(&StageKind::Verifier),
        "no verifier stage for a change with nothing to prove"
    );

    let verdict = outcome.verdict.expect("a reasoned verdict, not silence");
    assert!(verdict.passed);
    assert!(
        verdict.summary.contains("documentation only"),
        "the verdict must STATE why no test was written: {}",
        verdict.summary
    );
}

const SOURCE_DIFF: &str = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1,2 @@
 fn a() {}
+fn b() { run(); }
";

/// Triage's `VERIFIER: no` is a prompt-time guess, and this arm is only reached
/// when the ladder came back inconclusive — the state that falsifies the
/// waiver's own premise ("success is self-evident or a test already proves
/// it"). On a behavioral diff nothing proved, the waiver must not stand: the
/// third scripted call IS the verifier, and the run must spend it.
#[tokio::test]
async fn a_verifier_waiver_on_a_behavioral_diff_still_buys_the_verifier() {
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: single\nWITNESS: no\nVERIFIER: no"),
        text_result("done"),
        text_result("PASS — the change is sound"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], SOURCE_DIFF);
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
        PipelineConfig {
            witness_writer: false,
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Add the b() helper", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    assert!(
        provider.script.lock().await.is_empty(),
        "the verifier call must be spent: a waived verifier here would leave the third result unserved"
    );
    assert!(
        stages(&drain(&mut rx)).contains(&StageKind::Verifier),
        "a behavioral diff keeps its reviewer, whatever triage guessed"
    );
    let verdict = outcome.verdict.expect("a judged verdict");
    assert!(
        !verdict.summary.contains("waived"),
        "the verdict must be the verifier's, not the waiver's: {}",
        verdict.summary
    );
}

/// The half that keeps the waiver useful: where the warrant AGREES nothing
/// needs a reviewer (a docs-only change), triage's `VERIFIER: no` is honored
/// exactly as before — two provider calls, no verifier stage, and a verdict that
/// says the review was deliberately waived rather than broken.
#[tokio::test]
async fn a_verifier_waiver_stands_where_the_warrant_agrees() {
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: single\nWITNESS: no\nVERIFIER: no"),
        text_result("done"),
    ]);
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
        "exactly triage + worker were spent"
    );
    assert!(
        !stages(&drain(&mut rx)).contains(&StageKind::Verifier),
        "the waiver stands where the warrant agrees there is nothing to review"
    );
    let verdict = outcome.verdict.expect("a reasoned verdict");
    assert!(
        verdict.summary.contains("waived"),
        "the verdict must state the review was deliberately waived: {}",
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
/// the ladder with nothing at all to reason over, and a verifier handed an empty
/// record does not produce a better answer — it produced `FAIL … the file
/// likely does not exist` about a file that was in the container (#973). So the
/// ladder abstains, and the fixture size is again the assertion: exactly three
/// provider calls, because a fourth would mean a verifier was bought to guess.
///
/// The worker must *dispatch a mutating tool call* for this to be the abstain
/// case at all. A turn that calls nothing has the same four dark channels but
/// a different truth — it could not have changed anything — and now resolves
/// as `NothingAttempted` instead (see the two no-op tests at the end of this
/// module). Handing this fixture `EmptyTools` would quietly retarget it at
/// that rung and stop it testing abstention.
#[tokio::test]
async fn a_blind_diff_probe_never_completes_as_nothing_changed() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        writing_tool_result("writing the fix"),
        text_result("done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], DOCS_DIFF).with_blind_diff();
    let tools = OneWritingTool;
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
        !stages(&events).contains(&StageKind::Verifier),
        "with every channel blind there is nothing for a verifier to read — asking \
         one anyway is what produced a confident false negative"
    );
    // The rail has to say it too. Without a proof step of its own the
    // abstention arrives as `Verdict { passed: true }` and renders as
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
/// the verifier is told.
///
/// This is the Terminal-Bench trial with one channel restored. The task
/// directory is still not a git repository, so the diff probe is still blind
/// and there is still no test command — but the registry recorded six mutating
/// touches, and six is what must appear in the evidence summary. It read
/// `file_change_events=0`, and the verifier answered that the file "likely does
/// not exist" while it sat in the container (#973).
///
/// Note what the six also buy: the ladder escalates rather than abstaining,
/// because a recorded touch is real evidence that the tree changed. Blindness
/// is the *absence* of every channel, not the absence of a readable diff.
#[tokio::test]
async fn the_verifier_is_told_what_the_recorder_counted_not_zero() {
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

    let verifier_prompt = provider
        .prompts()
        .into_iter()
        .find(|p| p.contains("file_change_events="))
        .expect("the verifier was asked, and its prompt carries the evidence summary");
    assert!(
        verifier_prompt.contains("file_change_events=6"),
        "the verifier must be told what the recorder counted: {verifier_prompt}"
    );
    assert!(
        stages(&drain(&mut rx)).contains(&StageKind::Verifier),
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

/// The `regex-log` Terminal-Bench 2.1 trial, end to end: the worker answers in
/// prose, calls no tool, and the workspace is a plain directory the diff probe
/// can never read. Harbor scored the real trial 0.0; Stella reported success.
///
/// [`crate::verify`]'s ladder tests pin the *decision*; this pins what the
/// pipeline does with it, which is where the defect was actually visible. The
/// ladder emitted its abstention correctly the whole time — it was the
/// `passed: true` beside it, and the `Complete` stage after it, that told
/// every reader downstream the task was done.
///
/// `max_revisions: 0` so the turn is terminal here rather than looping; the
/// revision behaviour is the next test.
#[tokio::test]
async fn a_turn_that_called_no_tool_does_not_report_success() {
    // triage → single; worker → a confident claim it never acted on. The
    // scripted provider serves exactly two calls: a verifier call would exhaust
    // it and error the run, so the fixture size asserts none is bought.
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: single\nWITNESS: no\nVERIFIER: yes"),
        text_result("I've written the regex to /app/regex.txt. The task is complete."),
    ]);
    let resolver = OneProvider(&provider);
    // No test results and no readable tree: on Terminal-Bench every channel
    // except the dispatch count is structurally dark.
    let runner = ScriptedRunner::new(vec![], "").with_not_a_repository();
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
        PipelineConfig {
            test_command: None,
            max_revisions: 0,
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run(
            "Write a regex to /app/regex.txt",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("run completes");

    let verdict = outcome
        .verdict
        .expect("a no-op turn still reports a verdict");
    assert!(
        !verdict.passed,
        "a turn that dispatched no tool must not report success: {}",
        verdict.summary
    );
    assert!(
        verdict.summary.contains("NO WORK ATTEMPTED"),
        "and must say plainly what happened: {}",
        verdict.summary
    );
    assert!(
        !verdict.summary.contains("UNVERIFIABLE"),
        "this is a determinate finding, not a missing one: {}",
        verdict.summary
    );

    let events = drain(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Verdict {
                passed: false,
                evidence
            } if evidence.summary.contains("NO WORK ATTEMPTED")
        )),
        "the emitted verdict — the field every downstream reader keys on — must be false"
    );
    assert!(
        !stages(&events).contains(&StageKind::Verifier),
        "nothing was attempted, so there is nothing for a verifier to weigh"
    );
}

/// The recovery half: given a revision to spend, a no-op turn is pushed to act
/// rather than ending the run. This is the behaviour that would have changed
/// the eleven trials' outcomes — each stopped at 2–3 steps with revisions
/// still on the table.
#[tokio::test]
async fn a_no_op_turn_is_sent_back_to_do_the_work() {
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: single\nWITNESS: no\nVERIFIER: yes"),
        text_result("I've written the regex to /app/regex.txt. The task is complete."),
        // The revision turn. Still no tools — the second no-op is terminal,
        // which keeps this test about the push-back and not about recovery
        // the fixture cannot actually stage.
        text_result("Confirmed, the file is written."),
    ]);
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
            test_command: None,
            max_revisions: 1,
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run(
            "Write a regex to /app/regex.txt",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("run completes");

    let revision = provider
        .prompts()
        .into_iter()
        .find(|p| p.contains("without calling a single tool"))
        .expect("the worker was sent back with the no-op told plainly");
    assert!(
        revision.contains("does not perform it"),
        "and told that describing the work is not doing it: {revision}"
    );
    assert_eq!(
        outcome.revisions, 1,
        "exactly the one revision the config allowed was spent"
    );
    assert!(
        !outcome.verdict.expect("a verdict").passed,
        "a second no-op exhausts the revisions and still must not report success"
    );
}
