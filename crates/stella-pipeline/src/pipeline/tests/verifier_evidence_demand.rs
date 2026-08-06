//! #1295: what happens when a model verifier passes and *nothing deterministic*
//! stands behind it.
//!
//! The behaviour under test is deliberately conditional, and the condition is
//! the whole design. Asking the worker for corroboration is only affordable
//! where corroboration is reachable — which means where a tracked command
//! exists, because the two facts that would clear
//! [`crate::verify::LadderInputs::verifier_pass_stands_alone`] (a fail→pass flip,
//! or touched tests green) are both observations of that command. With none
//! resolved, the ask is unanswerable no matter how well the worker responds,
//! and the turn it costs is pure loss. These scenarios pin both directions.

use super::*;

/// Ports + config wiring shared by the scenarios below, so each test reads as
/// its scripted inputs and its assertion rather than as thirty lines of
/// boilerplate. `verifier_evidence_demand` is left at whatever `config` says —
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

/// The case the feature exists for: the verifier said "done", nothing backs it,
/// and a tracked command is sitting right there that could. The pipeline
/// spends one revision asking for the evidence — and when the worker produces
/// it, the run finishes on a DETERMINISTIC rung instead of being filed as an
/// unverified pass. This is the conversion the issue asks to be measured.
#[tokio::test]
async fn a_standalone_verifier_pass_buys_one_revision_when_a_command_can_answer() {
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
        "triage, worker, verifier, and the one demanded revision; a second verifier \
         call would mean the deterministic rung did not take"
    );
}

/// The measured failure mode from the first attempt (#1211 §1), pinned so it
/// cannot come back: with no tracked command, `verifier_pass_stands_alone` is
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
        "triage, worker, verifier — and nothing else: no revision turn"
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
        "an unanswered ask still records the verifier's pass — as unverified, \
         never as a failure"
    );
    assert_eq!(outcome.revisions, 1, "one ask, not one per verifier pass");
    assert_eq!(
        calls, 4,
        "triage, worker, verifier, and the single ask. The revised turn changed \
         nothing the verdict depends on, so the verifier's re-read is the CACHED \
         pass (#1431) — no fifth call"
    );
    assert!(
        verdict.summary.contains("verdict reused"),
        "the reused verdict says so, so the transcript stays honest: {}",
        verdict.summary
    );
}

/// The FAIL shape of the same reuse (#1431): a revision that changes neither
/// the diff nor the evidence re-asks the verifier the byte-identical question.
/// The cached opinion is the same opinion at zero cost — a second paid call
/// could differ only by sampling noise, and no one defends a verification
/// layer that flips on noise.
#[tokio::test]
async fn an_unchanged_revision_reuses_the_verdict_instead_of_rebuying_it() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("FAIL — the change does not handle the empty case"),
        text_result("I believe the change is correct as written"),
    ]);
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::TimeOut, TestScript::TimeOut],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        max_revisions: 1,
        ..PipelineConfig::default()
    };

    let (outcome, _events, calls) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(!verdict.passed, "the reused FAIL still fails the run");
    assert_eq!(
        calls, 4,
        "triage, worker, verifier, one revision — the second verdict round is \
         the cached FAIL, not a fifth call"
    );
    assert!(
        verdict.summary.contains("verdict reused"),
        "the reuse is stated in the verdict the run reports: {}",
        verdict.summary
    );
}

/// Switched off, the pipeline is byte-for-byte the behaviour that shipped:
/// relabel on the spot, no revision, no second verifier call. The flag is what
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
        verifier_evidence_demand: false,
        ..PipelineConfig::default()
    };

    let (outcome, _events, calls) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed && !verdict.deterministic);
    assert_eq!(outcome.revisions, 0, "off means no ask");
    assert_eq!(calls, 3, "triage, worker, verifier");
}

/// #1509's witness: the demand and a repair are not substitutes, so buying
/// the demand must not walk the candidate into its next refutation one
/// repair round short of the configured allowance.
///
/// The demand is bought (the verifier passed standing alone), the demanded
/// turn comes back RED — a real refutation — and the candidate must still
/// get the full `max_revisions` (2) worth of repair rounds after it. On
/// `main` the demand's revision counts as a repair attempt, so the second
/// repair round is refused and the run ends `VerificationFailed` one round
/// early, with the green flip sitting one turn away.
#[tokio::test]
async fn an_evidence_demand_does_not_spend_a_repair_round() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the change reads correct"),
        text_result("tried to produce the evidence"),
        text_result("first repair"),
        text_result("second repair"),
    ]);
    // Baseline red; candidate inconclusive (nothing deterministic, so the
    // verifier's pass stands alone and buys the demand); the demanded turn
    // observes RED; two repair rounds follow — red, then the green flip —
    // plus the pre-submit confirmation run.
    let runner = ScriptedRunner::scripted(
        vec![
            TestScript::Fail,
            TestScript::TimeOut,
            TestScript::Fail,
            TestScript::Fail,
            TestScript::Pass,
            TestScript::Pass,
        ],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        // Off so the model script stays a pure count of worker turns — the
        // second consecutive red would otherwise buy a guidance call, which
        // has its own witnesses.
        distress_guidance: false,
        // `max_revisions` stays at the default 2: the subject is that BOTH
        // configured rounds survive the demand, not a bigger allowance.
        ..PipelineConfig::default()
    };

    let (outcome, _events, calls) = run_scenario!(provider, runner, config);

    assert_eq!(
        outcome.status,
        PipelineStatus::Completed,
        "the full allowance reaches the flip: {:?}",
        outcome.verdict
    );
    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.passed && verdict.deterministic,
        "the second repair round's fail->pass flip is the verdict: {verdict:?}"
    );
    assert_eq!(
        outcome.revisions, 3,
        "one demand plus the FULL configured allowance of two repairs — on \
         main the demand eats a repair and this reads 2 with the run refused"
    );
    assert_eq!(
        calls, 6,
        "triage, worker, verifier, the demand, and two repair rounds"
    );
}

/// An uncorroborated verifier pass must be stamped `Unverifiable` on the wire,
/// not `ModelJudge`.
///
/// Every other channel on this path already says abstention: the score is
/// `Unverified`, the summary leads with UNVERIFIED, and `unverifiable()` puts
/// `VerificationUnavailable` on the rail. The snapshot's rung was the one field
/// left disagreeing — stamped when the verifier answered, before the branch
/// below discovered the answer stood alone.
///
/// It is not a labelling nicety. `reward::outcome_term` reads the rung and
/// nothing else: `ModelJudge` earns `+weights.judged`, while `Unverifiable`
/// returns `DiscardReason::Abstained`. Left wrong, every pass the ladder
/// declined to believe was fed back as a positive training signal — the exact
/// verdicts #871's asymmetric trust exists to refuse.
///
/// The scenario is the one that produced it in the wild: a warranted witness
/// whose runner is not installed. `TestScript::Infra` observes no assertion, so
/// there is no flip and no green test, and the verifier's "done" is all that is
/// left standing.
#[tokio::test]
async fn an_uncorroborated_verifier_pass_is_stamped_as_an_abstention_not_a_judgement() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the change reads correct"),
    ]);
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::Infra],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let config = PipelineConfig {
        test_command: Some("pytest -q".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        // The ask is a separate axis with its own tests above; switching it off
        // keeps this scenario on the relabelling branch it is about.
        verifier_evidence_demand: false,
        ..PipelineConfig::default()
    };

    let (outcome, _events, _calls) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.passed && !verdict.deterministic,
        "an unverified pass is still not a failure"
    );
    assert_eq!(
        outcome.score,
        Some(crate::candidate::CandidateScore::Unverified),
        "the score already knew this was not a real pass"
    );
    assert!(
        verdict.summary.starts_with("UNVERIFIED"),
        "and so did the summary: {}",
        verdict.summary
    );

    let rung = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot")
        .rung
        .expect("a rung is stamped");
    assert_eq!(
        rung,
        stella_protocol::LadderRung::Unverifiable,
        "the rung must agree with the abstention every other channel recorded — \
         `ModelJudge` here is what fed an uncorroborated pass into reward as a win"
    );
    assert!(
        matches!(
            crate::reward::outcome_term(rung, verdict.passed, &Default::default()),
            Err(crate::reward::DiscardReason::Abstained)
        ),
        "the consequence that makes the rung load-bearing: this trajectory must \
         be discarded, never credited"
    );
}
