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
        (outcome, drain(&mut rx), provider.prompts())
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

    let (outcome, _events, prompts) = run_scenario!(provider, runner, config);

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
        prompts.len(),
        3,
        "triage, worker, and the one demanded revision — the ask is the only thing \
         the verification side spends, and it spends no model of its own"
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
        text_result("nothing further is needed"),
    ]);
    let runner = ScriptedRunner::scripted(Vec::new(), "@@ -1 +1 @@\n-old\n+new");
    let config = PipelineConfig {
        // No `test_command`, and no authored witness to supply one — the
        // Terminal-Bench shape the original measurement ran into.
        test_command: None,
        roster: Roster::default().with_enabled(ModelCallRole::WitnessAuthor, false),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };

    let (outcome, _events, prompts) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        !verdict.deterministic,
        "nothing deterministic could have been observed here"
    );
    // The one revision this run spends is the unproven handback (#2908), and
    // the distinction is the subject of this test: an ask names a command and
    // demands its output, and no such command exists here. Asserted by
    // identity rather than by count, because the count can no longer tell the
    // two apart.
    let asked = prompts
        .iter()
        .any(|prompt| prompt.contains("NOTHING deterministic backs up this change"));
    assert!(
        !asked,
        "an unanswerable ask must not be bought — this is the whole reason the \
         feature was switched off the first time: {:?}",
        prompts
    );
    assert_eq!(
        outcome.revisions, 1,
        "and the turn that WAS spent is the handback, not the ask"
    );
    assert_eq!(
        prompts.len(),
        3,
        "triage, worker, and the handback the worker declines"
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
        text_result("there is no test surface for this"),
        text_result("nothing further is needed"),
    ]);
    // Never observable: every run is inconclusive, so the evidence the ask
    // wants cannot appear however many times it is asked for — including on
    // the handback round (#2908), where an exhausted queue would hand this
    // scenario the green suite it is written to be denied.
    let runner = ScriptedRunner::scripted(
        vec![
            TestScript::Fail,
            TestScript::TimeOut,
            TestScript::TimeOut,
            TestScript::TimeOut,
        ],
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

    let (outcome, _events, prompts) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.passed && !verdict.deterministic,
        "an unanswered ask still ends as unverified — never as a failure"
    );
    assert_eq!(
        outcome.revisions, 2,
        "one ask, not one per round — the second revision is the unproven \
         handback (#2908), which is a different thing and spends its turn once"
    );
    assert_eq!(
        prompts.len(),
        4,
        "triage, worker, the single ask, and the handback. The re-observed tree \
         lands back on the same abstention with the ask already spent, and \
         nothing re-reads it"
    );
    assert!(
        verdict.summary.starts_with("UNVERIFIED"),
        "the ask went unanswered, so the turn ends on the abstention and says so in \
         its first word: {}",
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
        text_result("nothing further is needed"),
    ]);
    // Inconclusive on both rounds: an exhausted queue would pass the second
    // one and turn this scenario into a deterministic flip.
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::TimeOut, TestScript::TimeOut],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        verifier_evidence_demand: false,
        ..PipelineConfig::default()
    };

    let (outcome, _events, prompts) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed && !verdict.deterministic);
    let asked = prompts
        .iter()
        .any(|prompt| prompt.contains("NOTHING deterministic backs up this change"));
    assert!(!asked, "off means no ask: {prompts:?}");
    assert_eq!(
        outcome.revisions, 1,
        "the flag switches off the ask, not the unproven handback (#2908) — \
         those are different turns with different texts"
    );
    assert_eq!(
        prompts.len(),
        3,
        "triage, worker, and the declined handback"
    );
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
        // second deterministic red would otherwise buy a guidance call, which
        // has its own witnesses.
        roster: Roster::default().with_enabled(ModelCallRole::DistressGuidance, false),
        // `max_revisions` stays at the default 2: the subject is that BOTH
        // configured rounds survive the demand, not a bigger allowance.
        ..PipelineConfig::default()
    };

    let (outcome, _events, prompts) = run_scenario!(provider, runner, config);

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
        prompts.len(),
        5,
        "triage, worker, the demand, and two repair rounds"
    );
}

/// The wild scenario that produced #871's asymmetric trust, re-pinned for a
/// ladder with nothing to be asymmetric about: a warranted witness whose runner
/// is not installed.
///
/// `TestScript::Infra` observes no assertion, so there is no flip and no green
/// test. Under the old design a verifier's "done" was all that was left
/// standing, and the run had to be talked back down from it — relabelled
/// UNVERIFIED, its rung restamped, so reward extraction would not train on a
/// verdict the ladder had declined to believe.
///
/// Nothing has to be talked down now. The pass was never claimed: the ladder
/// reaches the same state and reports it directly. The assertions are
/// deliberately the same ones, because the *observable* contract they pin is
/// what mattered and it has not moved — a missing toolchain is not a failing
/// change, and it is not a passing one either.
#[tokio::test]
async fn a_missing_runner_is_unproven_rather_than_passed_or_failed() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::Infra],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let config = PipelineConfig {
        test_command: Some("pytest -q".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        // The ask is a separate axis with its own tests above; switching it off
        // keeps this scenario on the branch it is about.
        verifier_evidence_demand: false,
        ..PipelineConfig::default()
    };

    let (outcome, _events, _prompts) = run_scenario!(provider, runner, config);

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.passed && !verdict.deterministic,
        "an unproven turn is still not a failure"
    );
    assert_eq!(
        outcome.score,
        Some(crate::candidate::CandidateScore::Unverified),
        "the score says plainly that nothing proved this"
    );
    assert!(
        verdict.summary.starts_with("UNVERIFIED"),
        "and so does the summary: {}",
        verdict.summary
    );
    assert_eq!(
        verdict
            .ladder
            .as_deref()
            .expect("the verdict carries its snapshot")
            .rung,
        Some(stella_protocol::LadderRung::Unverified),
        "the rung must agree with the summary — reward extraction reads the rung \
         and nothing else, so a disagreement here trains on the wrong label"
    );
}
