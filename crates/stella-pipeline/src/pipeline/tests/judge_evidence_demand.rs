//! #1295: what happens when a model judge passes and *nothing deterministic*
//! stands behind it.
//!
//! The behaviour under test is deliberately conditional, and the condition is
//! the whole design. Asking the worker for corroboration is only affordable
//! where corroboration is reachable — which means where a tracked command
//! exists, because the two facts that would clear
//! [`crate::verify::LadderInputs::judge_pass_stands_alone`] (a fail→pass flip,
//! or touched tests green) are both observations of that command. With none
//! resolved, the ask is unanswerable no matter how well the worker responds,
//! and the turn it costs is pure loss. These scenarios pin both directions.

use super::*;

/// Ports + config wiring shared by the scenarios below, so each test reads as
/// its scripted inputs and its assertion rather than as thirty lines of
/// boilerplate. `judge_evidence_demand` is left at whatever `config` says —
/// that flag is the subject here, never a default worth hiding.
macro_rules! run_scenario {
    ($provider:expr, $runner:expr, $config:expr) => {{
        let provider = $provider;
        let resolver = OneProvider(&provider);
        let runner = $runner;
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
                // No coverage tooling (#1291): these scenarios turn on whether
                // a tracked command can answer the ask at all, and an
                // unmeasurable overlap is a separate axis with its own tests.
                coverage: None,
                approvals: &approvals,
                sleeper: &sleeper,
                hooks: None,
                candidate_workspaces: None,
                mcp_prefetch: None,
                steering: None,
            },
            tx,
            $config,
        );
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let outcome = pipeline
            .run("Make the thing work", &mut messages, &mut budget)
            .await
            .expect("run succeeds");
        (outcome, drain(&mut rx), provider.prompts().len())
    }};
}

/// The case the feature exists for: the judge said "done", nothing backs it,
/// and a tracked command is sitting right there that could. The pipeline
/// spends one revision asking for the evidence — and when the worker produces
/// it, the run finishes on a DETERMINISTIC rung instead of being filed as an
/// unverified pass. This is the conversion the issue asks to be measured.
#[tokio::test]
async fn a_standalone_judge_pass_buys_one_revision_when_a_command_can_answer() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the change reads correct"),
        text_result("added a test that covers it"),
    ]);
    // Baseline red (the oracle arms), candidate inconclusive (no assertion
    // observed — so no flip and no green test), then green after the ask,
    // plus the pre-submit confirmation run.
    let runner = ScriptedRunner::scripted(
        vec![
            TestScript::Fail,
            TestScript::TimeOut,
            TestScript::Pass,
            TestScript::Pass,
        ],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };

    let (outcome, _events, calls) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.passed && verdict.deterministic,
        "the demanded evidence arrived, so the run must finish on a \
         deterministic rung — got passed={} deterministic={}",
        verdict.passed,
        verdict.deterministic
    );
    assert_eq!(
        outcome.revisions, 1,
        "exactly one revision — the ask — should have been spent"
    );
    assert_eq!(
        calls, 4,
        "triage, worker, judge, and the one demanded revision; a second judge \
         call would mean the deterministic rung did not take"
    );
}

/// The measured failure mode from the first attempt (#1211 §1), pinned so it
/// cannot come back: with no tracked command, `judge_pass_stands_alone` is
/// true *by construction* — `touched_tests_passed` can only ever be `None`
/// and the flip oracle never observes a candidate run — so an ask could not
/// be satisfied by any worker on any turn. The pipeline must not buy one.
#[tokio::test]
async fn no_tracked_command_means_no_ask_at_all() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the change reads correct"),
    ]);
    let runner = ScriptedRunner::scripted(Vec::new(), "@@ -1 +1 @@\n-old\n+new");
    let config = PipelineConfig {
        // No `test_command`, and no authored witness to supply one — the
        // Terminal-Bench shape the original measurement ran into.
        test_command: None,
        witness_writer: false,
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };

    let (outcome, _events, calls) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        !verdict.deterministic,
        "nothing deterministic could have been observed here"
    );
    assert_eq!(
        outcome.revisions, 0,
        "an unanswerable ask must not be bought — this is the whole reason \
         the feature was switched off the first time"
    );
    assert_eq!(
        calls, 3,
        "triage, worker, judge — and nothing else: no revision turn"
    );
}

/// The ask is spent once per candidate. A worker that comes back without the
/// evidence is not asked again — it already answered, and paying for the same
/// answer twice is exactly the runaway cost this feature was reverted for.
#[tokio::test]
async fn the_ask_is_spent_once_even_when_the_evidence_never_arrives() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the change reads correct"),
        text_result("there is no test surface for this"),
        text_result("PASS — still reads correct"),
    ]);
    // Never observable: every run is inconclusive, so the evidence the ask
    // wants cannot appear however many times it is asked for.
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::TimeOut, TestScript::TimeOut],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        // `max_revisions` is deliberately generous: the cap under test is the
        // one-ask cap, not the revision budget running out.
        max_revisions: 3,
        ..PipelineConfig::default()
    };

    let (outcome, _events, calls) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.passed && !verdict.deterministic,
        "an unanswered ask still records the judge's pass — as unverified, \
         never as a failure"
    );
    assert_eq!(outcome.revisions, 1, "one ask, not one per judge pass");
    assert_eq!(
        calls, 5,
        "triage, worker, judge, the single ask, and the judge re-reading the \
         revised turn — then it stops"
    );
}

/// Switched off, the pipeline is byte-for-byte the behaviour that shipped:
/// relabel on the spot, no revision, no second judge call. The flag is what
/// makes the measurement in #1295 a two-arm comparison rather than a rebuild.
#[tokio::test]
async fn the_demand_can_be_switched_off() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the change reads correct"),
    ]);
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::TimeOut],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        judge_evidence_demand: false,
        ..PipelineConfig::default()
    };

    let (outcome, _events, calls) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed && !verdict.deterministic);
    assert_eq!(outcome.revisions, 0, "off means no ask");
    assert_eq!(calls, 3, "triage, worker, judge");
}
