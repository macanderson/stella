//! Token-drift calibration reaching the engines the pipeline builds (#1595).
//!
//! Its own module because the subject is a *wiring* fact, not a decision: the
//! arithmetic is `stella-core`'s and is tested there. What is tested here is
//! that the default `stella run` path hands the engine a calibration at all —
//! five of `stella-cli`'s seven assembly sites did and the two behind the
//! pipeline could not, because a pipeline had no way to accept one.

use super::*;

use stella_core::CalibrationMap;

/// Samples with a constant additive overhead, spanning enough sizes for the
/// fit to separate the overhead from the proportional error. Deliberately the
/// shape `stella-core`'s own witness uses: an ~8k per-request schema block on
/// top of an otherwise accurate estimate.
fn warmed() -> CalibrationMap {
    let calibration = CalibrationMap::new();
    calibration.seed(
        "worker",
        &[(4_000, 12_000), (40_000, 48_000), (100_000, 108_000)],
    );
    calibration
}

/// The factor each ENGINE step reports having sized its budget with.
///
/// `call_seq == 0` is the engine's own worker call; the pipeline's management
/// roles ride the same step at 1, 2, … and are excluded deliberately. A raw
/// management call has no transcript to compact and so no compaction budget —
/// its manifest carries the identity factor whatever the engines were lent,
/// and counting it would make this assert something untrue of the seam.
fn factors(events: &[AgentEvent]) -> Vec<f64> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::StepManifest {
                call_seq: 0,
                calibration_factor,
                ..
            } => Some(*calibration_factor),
            _ => None,
        })
        .collect()
}

/// Drive one scripted single-task run, optionally lending it a calibration,
/// and return the events it emitted.
async fn run_with(calibration: Option<&CalibrationMap>) -> Vec<AgentEvent> {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };
    let mut pipeline = Pipeline::new(
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
    if let Some(calibration) = calibration {
        pipeline = pipeline.with_calibration(calibration);
    }

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");
    drain(&mut rx)
}

/// **Witness (#1595).** The engines behind the default `stella run` size their
/// compaction budget against measured drift.
///
/// `stella-cli` assembles a session at seven call sites. Five seeded a
/// `CalibrationMap` from the store and handed it to the engine; the two behind
/// the pipeline — `run_pipeline_one_shot`, which *is* `stella run`, and fleet
/// workers — could not, because `Pipeline` had no way to accept one. So the
/// most-used path in the product was the only one estimating uncorrected.
///
/// Note what the gap cost beyond the seed: with no map at all the correction
/// is inert for the whole run, so the drift those turns *measured* was never
/// applied to them either. This asserts the correction is live, not that any
/// particular number came out of it — the arithmetic is `stella-core`'s and is
/// witnessed there.
#[tokio::test]
async fn the_pipeline_path_sizes_its_budget_with_the_callers_calibration() {
    let calibration = warmed();
    let corrected = factors(&run_with(Some(&calibration)).await);
    assert!(
        !corrected.is_empty(),
        "the run must report at least one step manifest to read a factor from"
    );
    assert!(
        corrected.iter().all(|f| (f - 1.0).abs() > f64::EPSILON),
        "every step must size its budget with the lent calibration, got {corrected:?}"
    );
}

/// The other half, and the reason the assertion above is meaningful: a
/// pipeline lent nothing corrects nothing. `1.0` is the identity factor —
/// what every pipeline-driven turn reported before this wiring existed.
#[tokio::test]
async fn a_pipeline_lent_no_calibration_reports_the_identity_factor() {
    let uncorrected = factors(&run_with(None).await);
    assert!(!uncorrected.is_empty());
    assert!(
        uncorrected.iter().all(|f| (f - 1.0).abs() < f64::EPSILON),
        "an unlent pipeline must not invent a correction, got {uncorrected:?}"
    );
}
