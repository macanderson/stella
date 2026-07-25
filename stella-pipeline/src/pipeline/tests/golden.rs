//! Golden trajectories recorded from **this pipeline**, on fixed tasks.
//!
//! These are real recordings: each test drives the actual [`Pipeline`] over
//! scripted ports and records the `AgentEvent` stream it really emits — nothing
//! here is hand-authored the way the synthetic fixtures under
//! `tests/fixtures/` are. Scripted model/test doubles are what make them
//! deterministic and runnable in CI with no API key, which is the only reason a
//! recording can be asserted on every `cargo test` rather than refreshed by
//! hand and hoped over.
//!
//! # What they do and do not prove
//!
//! Both sides of the comparison are the same code, so these are a **drift
//! baseline**, not independent evidence: they catch a stage that stopped being
//! emitted, a tool that changed name, an event that moved or disappeared —
//! silent protocol regressions that no single-flow assertion notices. They do
//! *not* establish that the Rust stack agrees with a reference implementation.
//! That needs a [`RecordingSource::Reference`] recording, which is blocked on
//! an adapter — see `tests/reference_conformance.rs` and
//! `docs/replay-golden-trajectories.md`.
//!
//! # Refreshing
//!
//! `make record-golden` (or `STELLA_REFRESH_GOLDEN=1 cargo test -p
//! stella-pipeline golden`) rewrites the fixtures from the current code. Review
//! the diff: a change here is a change to the observable event contract, and
//! that is exactly the thing that should be hard to do by accident.

use std::path::PathBuf;

use super::*;
use crate::replay::golden::{GoldenManifest, GoldenTrajectory, RecordingSource};

/// Set to rewrite the fixtures instead of asserting against them.
const REFRESH_ENV: &str = "STELLA_REFRESH_GOLDEN";

fn golden_dir() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", "golden"]
        .iter()
        .collect()
}

/// Gate a freshly-recorded stream, then either rewrite the fixture (refresh
/// mode) or assert the committed golden is structurally equivalent to it.
fn check_golden(task_id: &str, description: &str, events: Vec<AgentEvent>) {
    let source = RecordingSource::RustStack {
        recorded_by: format!("stella-pipeline::pipeline::tests::golden::{task_id}"),
    };
    let manifest = GoldenManifest::for_recording(task_id, description, source, &events);
    // The recording must itself be a well-formed trajectory. If the pipeline
    // ever emits a stream that violates its own invariants, that is a defect
    // worth failing on here rather than quietly enshrining as the new golden.
    let recorded = GoldenTrajectory::new(manifest, events)
        .unwrap_or_else(|e| panic!("the recorded run is not a well-formed trajectory: {e}"));

    let dir = golden_dir();
    if std::env::var_os(REFRESH_ENV).is_some() {
        recorded
            .write(&dir)
            .unwrap_or_else(|e| panic!("refreshing golden `{task_id}`: {e}"));
        return;
    }

    let committed = GoldenTrajectory::load(&dir, task_id).unwrap_or_else(|e| {
        panic!("loading golden `{task_id}`: {e}\nrun `make record-golden` to (re)record it")
    });
    let diff = committed.diff(recorded.events());
    assert!(
        diff.is_empty(),
        "the recorded trajectory for `{task_id}` diverged from its golden.\n\
         `left` is the golden, `right` is this run: {diff:#?}\n\
         If the change is intended, refresh with `make record-golden` and \
         review the fixture diff as an event-contract change."
    );
}

/// The deterministic-flip path: a single-task goal whose test command flips
/// fail→pass submits fast, so the model judge is never consulted (L-E11).
#[tokio::test]
async fn golden_single_task_deterministic_flip() {
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
        config,
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    check_golden(
        "single_task_deterministic_flip",
        "A single-task goal whose test command flips fail->pass: deterministic \
         verdict, model judge skipped.",
        drain(&mut rx),
    );
}

/// The judge-escalation path: no test command, so the deterministic ladder
/// cannot conclude and a model judge is consulted — a materially different
/// stage sequence from the flip path above.
#[tokio::test]
async fn golden_judge_escalation_without_a_test_command() {
    let provider = ScriptedProvider::new(vec![
        text_result("lookup"),
        text_result("done"),
        text_result("PASS looks right"),
    ]);
    let resolver = OneProvider(&provider);
    // A non-empty diff means files were touched, so the zero-diff guard revokes
    // the lookup fast path's judge-skip.
    let runner = ScriptedRunner::new(vec![], "@@ -1 +1 @@\n-a\n+b");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        test_command: None,
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
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
        config,
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Explain the retry policy", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    check_golden(
        "judge_escalation_without_a_test_command",
        "A file-touching goal with no test command: the deterministic ladder \
         cannot conclude, so the model judge is consulted.",
        drain(&mut rx),
    );
}

/// The two recorded flows must not collapse into the same trajectory — a
/// golden set whose members are indistinguishable would pass no matter what
/// the pipeline did, so the suite's own discriminating power is asserted here.
#[test]
fn the_recorded_flows_are_structurally_distinct() {
    let dir = golden_dir();
    let flip = GoldenTrajectory::load(&dir, "single_task_deterministic_flip")
        .expect("the flip golden is committed");
    let escalation = GoldenTrajectory::load(&dir, "judge_escalation_without_a_test_command")
        .expect("the escalation golden is committed");
    assert!(
        !flip.diff(escalation.events()).is_empty(),
        "a deterministic-flip run and a judge-escalation run must differ; \
         identical goldens would assert nothing"
    );
}

/// Every committed golden is a drift baseline, never independent evidence.
/// The day a reference recording lands it will be a `Reference` source, and
/// this test is where that distinction stops being a comment.
#[test]
fn the_committed_goldens_are_labelled_as_baselines_not_references() {
    let dir = golden_dir();
    for task_id in [
        "single_task_deterministic_flip",
        "judge_escalation_without_a_test_command",
    ] {
        let golden = GoldenTrajectory::load(&dir, task_id).expect("golden is committed");
        assert!(
            !golden.manifest().source.is_reference(),
            "`{task_id}` claims to be a reference trajectory, but no reference \
             adapter exists yet (see tests/reference_conformance.rs)"
        );
    }
}
