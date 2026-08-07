//! #1479: a refuted success claim is a *finding*, not the end of the run.
//!
//! Verification catching the model lying about success is the expensive half
//! of the work, and it was being thrown away: the revision allowance is a
//! bare count, so once it was spent the run reported `VerificationFailed`
//! however much budget and wall clock were still sitting unspent. A caught
//! failure and an uncaught one scored identically.
//!
//! These scenarios pin both directions of the gate — a measured axis with
//! room grants bounded repair attempts past the allowance, and an
//! unmeasured one grants nothing, so a caller that never declared a ceiling
//! keeps exactly the behaviour it always had.

use super::*;

/// The three verification rounds every scenario here runs: a red baseline
/// (arming the flip oracle), two red candidate rounds, then a green one and
/// its pre-submit confirmation re-run.
fn three_round_runner() -> ScriptedRunner {
    ScriptedRunner::new(
        vec![false, false, false, true, true],
        "@@ -1 +1 @@\n-old\n+new",
    )
}

/// Triage, the worker, and one turn per repair attempt. Two spare entries so
/// the run that stops early is stopped by the gate under test rather than by
/// an exhausted script.
fn repair_provider() -> ScriptedProvider {
    ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("second attempt"),
        text_result("third attempt"),
        text_result("unused"),
    ])
}

/// Every port is scripted the same way in every scenario; only the budget
/// guard, the config, and the assertion differ, which is exactly the
/// variable under test. The three-argument form carries the shared config:
/// one revision by configuration — the second red round is the one the
/// allowance can no longer pay for, which is where the repair gate has to
/// answer — and `distress_guidance` off so the model script stays a pure
/// count of worker turns (the guidance call has its own witnesses).
macro_rules! repair_scenario {
    ($provider:expr, $runner:expr, $budget:expr) => {
        repair_scenario!(
            $provider,
            $runner,
            $budget,
            PipelineConfig {
                test_command: Some("cargo test -p x".into()),
                max_revisions: 1,
                distress_guidance: false,
                ..PipelineConfig::default()
            }
        )
    };
    ($provider:expr, $runner:expr, $budget:expr, $config:expr) => {{
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
        let mut budget = $budget;
        let outcome = pipeline
            .run(
                "Make input.csv match expected.csv",
                &mut messages,
                &mut budget,
            )
            .await
            .expect("a refuted verdict is a typed outcome, never a hard error");
        (outcome, drain(&mut rx))
    }};
}

/// The witness. Two red rounds spend the configured allowance; a declared
/// budget ceiling with room left in it buys a third round, and that one goes
/// green. Before #1479 the run stopped on the second refutation and reported
/// `VerificationFailed` with 99.9% of the declared ceiling unspent.
#[tokio::test]
async fn a_refuted_verdict_with_headroom_earns_another_repair_round() {
    let (outcome, events) = repair_scenario!(
        repair_provider(),
        three_round_runner(),
        BudgetGuard::new(BudgetMode::Observed, None, Some(5.0))
    );

    assert_eq!(
        outcome.status,
        PipelineStatus::Completed,
        "a repair round the declared budget could afford must be taken, not \
         thrown away with the ceiling almost untouched: {:?}",
        outcome.verdict
    );
    let verdict = outcome
        .verdict
        .expect("the passing round produced a verdict");
    assert!(
        verdict.passed,
        "the third round's fail->pass flip is the verdict: {verdict:?}"
    );
    assert_eq!(
        outcome.revisions, 2,
        "one revision from the configured allowance plus one earned from the \
         measured headroom"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Complete { .. })),
        "a repaired run ends on the success terminal event: {events:?}"
    );
}

/// The other direction, and the reason the gate reads a *measured* axis
/// rather than granting attempts on optimism: a guard that declares no
/// ceiling at all cannot say whether another round is affordable, so the
/// configured allowance stays the whole of the bound. Identical script to the
/// witness above — only the budget guard changes.
#[tokio::test]
async fn an_unmeasured_budget_grants_no_repair_past_the_allowance() {
    let (outcome, events) = repair_scenario!(
        repair_provider(),
        three_round_runner(),
        BudgetGuard::new(BudgetMode::Off, None, None)
    );

    assert!(
        matches!(outcome.status, PipelineStatus::VerificationFailed { .. }),
        "with nothing measuring the run, the allowance is the only bound: {:?}",
        outcome.status
    );
    assert_eq!(
        outcome.revisions, 1,
        "exactly the configured allowance — no round is granted on optimism"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Complete { .. })),
        "a refuted run must never emit the success terminal event: {events:?}"
    );
}

/// #1507's witness. `EngineConfig::turn_budget` is a per-engine-turn
/// contract, and the gate's clock axis used to read it as the RUN's
/// deadline — so a tiny turn budget (a caller declining length
/// continuations aggressively) refused every repair round, including the
/// ones inside the configured allowance. The axis must not read it at all:
/// with no run deadline declared, the clock abstains and the allowance is
/// spent exactly as if no turn budget existed.
#[tokio::test]
async fn an_engine_turn_budget_no_longer_meters_the_repair_clock() {
    let (outcome, _events) = repair_scenario!(
        repair_provider(),
        three_round_runner(),
        BudgetGuard::new(BudgetMode::Observed, None, Some(5.0)),
        PipelineConfig {
            test_command: Some("cargo test -p x".into()),
            max_revisions: 1,
            distress_guidance: false,
            engine: EngineConfig {
                // Any elapsed time exceeds this, so a gate that reads it as
                // the run deadline refuses the FIRST repair. On `main` this
                // scenario ends `VerificationFailed` with zero revisions.
                turn_budget: Some(Duration::from_nanos(1)),
                ..EngineConfig::default()
            },
            ..PipelineConfig::default()
        }
    );

    assert_eq!(
        outcome.status,
        PipelineStatus::Completed,
        "a per-turn allowance must not be read as the run's deadline: {:?}",
        outcome.verdict
    );
    assert_eq!(
        outcome.revisions, 2,
        "the configured allowance plus the round the measured budget affords \
         — neither withheld by a clock nobody set on the run"
    );
}

/// A declared run deadline that has already elapsed refuses the repair — but
/// says so, and the verdict is reported either way (degradation warns, it
/// never disables).
#[tokio::test]
async fn an_elapsed_run_deadline_refuses_repair_and_still_reports_the_verdict() {
    let (outcome, events) = repair_scenario!(
        repair_provider(),
        three_round_runner(),
        BudgetGuard::new(BudgetMode::Off, None, None),
        PipelineConfig {
            test_command: Some("cargo test -p x".into()),
            max_revisions: 1,
            distress_guidance: false,
            run_budget: Some(Duration::ZERO),
            ..PipelineConfig::default()
        }
    );

    assert!(
        matches!(outcome.status, PipelineStatus::VerificationFailed { .. }),
        "no repair fits in an elapsed deadline: {:?}",
        outcome.status
    );
    assert_eq!(
        outcome.revisions, 0,
        "a repair that cannot finish must not start, even inside the allowance"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, retryable: true }
                if message.contains("no repair round was started")
        )),
        "the refusal is stated, never silent: {events:?}"
    );
}

/// A run deadline with room left grants past the allowance on the clock axis
/// alone — the run-scoped denominator the axis was always meant to have. On
/// `main` this measurement could only come from the per-turn budget, which a
/// multi-turn run had always already outlived.
#[tokio::test]
async fn a_run_deadline_with_room_grants_repair_past_the_allowance() {
    let (outcome, _events) = repair_scenario!(
        repair_provider(),
        three_round_runner(),
        BudgetGuard::new(BudgetMode::Off, None, None),
        PipelineConfig {
            test_command: Some("cargo test -p x".into()),
            max_revisions: 1,
            distress_guidance: false,
            run_budget: Some(Duration::from_secs(3600)),
            ..PipelineConfig::default()
        }
    );

    assert_eq!(
        outcome.status,
        PipelineStatus::Completed,
        "a measured clock axis with room buys the round the allowance cannot: {:?}",
        outcome.verdict
    );
    assert_eq!(
        outcome.revisions, 2,
        "one from the allowance, one from the measured run deadline"
    );
}
