//! The #1671 witness: a checkpoint-restored pipeline run re-enters at the
//! execute stage and ends with a real verdict.
//!
//! On `main` the seam does not exist — `Pipeline::run` is the only entry
//! point, so a killed pipeline run can only be resumed as a bare engine turn
//! and completes with no verdict at all (the exact defect #1615 reported).
//! The fail half of this witness is therefore type-level (`Pipeline::resume`
//! and `PipelineResume` do not compile there), and the assertions pin the
//! behavior that makes the addition worth having: the verify stage runs, the
//! verdict is recorded, the pre-plan stages are *not* re-run, and the plan
//! steps the crash never reached are.
//!
//! A child module, so it reaches the scripted ports above via `super::*`.

use std::collections::HashMap;

use super::*;
use stella_core::step::{BudgetSnapshot, CHECKPOINT_VERSION, Checkpoint};
use stella_protocol::BudgetMode;

/// The turn a `kill -9` interrupted mid-execute: the transcript the engine
/// checkpointed at its last committed step boundary, with the next provider
/// call still owed.
fn killed_mid_execute() -> Checkpoint {
    Checkpoint {
        version: CHECKPOINT_VERSION,
        step: 1,
        messages: vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("fix the parser so `cargo test -p x` passes"),
            CompletionMessage::assistant("working on it — first edit committed"),
        ],
        budget: BudgetSnapshot {
            mode: BudgetMode::Off,
            turn_limit_usd: None,
            session_limit_usd: None,
            turn_spent_usd: 0.0,
            session_spent_usd: 0.0,
        },
        total_cost_usd: 0.0,
        calibration_model: None,
        loop_steered: false,
        loop_steered_pattern: Vec::new(),
        loop_steered_inputs: None,
        transcript_rewrites: 0,
    }
}

/// A restorable spec over `checkpoint` with the recorded red baseline the
/// crashed process observed on its pre-execution tree.
fn spec(checkpoint: Checkpoint) -> PipelineResume {
    PipelineResume {
        checkpoint,
        goal: "fix the parser".into(),
        task_class: TaskClass::SingleTask,
        plan: None,
        next_step: None,
        untracked_before: HashMap::new(),
        baseline: Some(RecordedBaseline {
            command: "cargo test -p x".into(),
            passed: false,
            output_tail: "test parse ... FAILED".into(),
        }),
    }
}

macro_rules! resume_scenario {
    ($provider:expr, $runner:expr, $spec:expr) => {{
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
            PipelineConfig {
                test_command: Some("cargo test -p x".into()),
                diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
                ..PipelineConfig::default()
            },
        );
        let outcome = pipeline.resume($spec).await.expect("resume succeeds");
        (outcome, drain(&mut rx), provider.prompts())
    }};
}

/// **The #1615/#1671 witness.** A run killed between execute steps resumes,
/// the verify stage runs, and a verdict is recorded — where the bare-turn
/// resume completed with no verdict at all. The recorded red baseline plus
/// the candidate's green observation is a genuine fail→pass flip, so the
/// verdict is the deterministic ladder's, not a model's opinion.
#[tokio::test]
async fn a_resumed_pipeline_run_verifies_and_records_a_verdict() {
    let (outcome, events, prompts) = resume_scenario!(
        ScriptedProvider::new(vec![text_result("finished the fix")]),
        // Candidate observation + confirmation — the baseline is NOT re-run:
        // it rides the frame as the crashed process's own observation.
        ScriptedRunner::new(vec![true, true], "@@ -1 +1 @@\n-old\n+new"),
        spec(killed_mid_execute())
    );

    assert_eq!(outcome.status, PipelineStatus::Completed);
    let verdict = outcome
        .verdict
        .expect("the verify stage ran and recorded a verdict");
    assert!(
        verdict.passed && verdict.deterministic,
        "a recorded red baseline plus a green candidate is the deterministic flip: {verdict:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Stage {
                name: StageKind::Verify
            }
        )),
        "the verify stage must run on the resumed work"
    );
    assert_eq!(
        prompts.len(),
        1,
        "exactly the interrupted turn's continuation — a second prompt would \
         mean triage or planning re-ran on a resume: {prompts:?}"
    );
}

/// A mid-plan kill: the resumed turn finishes its step, and the steps the
/// crash never reached still run — numbered from the plan's own cursor, not
/// re-planned — before verification.
#[tokio::test]
async fn a_mid_plan_resume_finishes_the_remaining_steps() {
    let mut spec = spec(killed_mid_execute());
    spec.task_class = TaskClass::MultiStep;
    spec.plan = Some(vec![
        PlanStep::new("edit the parser"),
        PlanStep::new("update the golden file"),
        PlanStep::new("run the suite"),
    ]);
    // The kill happened during step 1's turn (index 0).
    spec.next_step = Some(0);
    let (outcome, _events, prompts) = resume_scenario!(
        ScriptedProvider::new(vec![
            text_result("step 1 finished"),
            text_result("step 2 finished"),
            text_result("step 3 finished"),
        ]),
        ScriptedRunner::new(vec![true, true], "@@ -1 +1 @@\n-old\n+new"),
        spec
    );

    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert!(outcome.verdict.is_some(), "the plan tail still verifies");
    assert_eq!(
        prompts.len(),
        3,
        "the interrupted turn plus the two unreached steps: {prompts:?}"
    );
    assert!(
        prompts[1].contains("3"),
        "the continued walk numbers steps from the plan's cursor: {}",
        prompts[1]
    );
}

/// A recorded baseline for a *different* command than the one now configured
/// is discarded, not trusted: the flip cannot be credited against the wrong
/// test, so the ladder escalates (here: no verdict claim about determinism).
#[tokio::test]
async fn a_stale_baseline_for_another_command_is_not_credited() {
    let mut spec = spec(killed_mid_execute());
    spec.baseline = Some(RecordedBaseline {
        command: "pytest tests/".into(),
        passed: false,
        output_tail: "1 failed".into(),
    });
    let (outcome, _events, _prompts) = resume_scenario!(
        ScriptedProvider::new(vec![
            text_result("finished the fix"),
            // The escalated model verifier's reply, should the ladder ask.
            text_result("PASS\nlooks correct"),
        ]),
        ScriptedRunner::new(vec![true, true], "@@ -1 +1 @@\n-old\n+new"),
        spec
    );

    if let Some(verdict) = &outcome.verdict {
        assert!(
            !verdict.deterministic,
            "a flip must not be credited off a baseline for a different \
             command: {verdict:?}"
        );
    }
}

/// The validation gate: progress that cannot restore — missing class or
/// goal, pre-execute kill, isolated candidate — declines entirely rather
/// than restoring approximately.
#[test]
fn from_progress_declines_what_it_cannot_restore() {
    let full = || FrameProgress {
        task_class: Some(TaskClass::SingleTask),
        goal: Some("fix the parser".into()),
        executing: true,
        isolated: false,
        ..FrameProgress::default()
    };
    assert!(PipelineResume::from_progress(killed_mid_execute(), full()).is_some());

    let mut no_class = full();
    no_class.task_class = None;
    assert!(PipelineResume::from_progress(killed_mid_execute(), no_class).is_none());

    let mut no_goal = full();
    no_goal.goal = None;
    assert!(PipelineResume::from_progress(killed_mid_execute(), no_goal).is_none());

    let mut pre_execute = full();
    pre_execute.executing = false;
    assert!(PipelineResume::from_progress(killed_mid_execute(), pre_execute).is_none());

    let mut isolated = full();
    isolated.isolated = true;
    assert!(
        PipelineResume::from_progress(killed_mid_execute(), isolated).is_none(),
        "the candidate worktree died with the process — restoring into the \
         workspace instead would write the wrong tree"
    );
}

/// Invariant 4: the progress record crosses the pipeline↔host boundary as
/// JSON and must come back the same value — including an older frame with
/// every progress field absent reading as the empty default.
#[test]
fn frame_progress_round_trips_and_defaults() {
    let progress = FrameProgress {
        task_class: Some(TaskClass::MultiStep),
        goal: Some("fix the parser".into()),
        plan: Some(vec![PlanStep::new("edit"), PlanStep::new("test")]),
        next_step: Some(1),
        executing: true,
        isolated: false,
        untracked_before: HashMap::from([("notes.md".into(), "abc123".into())]),
        baseline: Some(RecordedBaseline {
            command: "cargo test -p x".into(),
            passed: false,
            output_tail: "FAILED".into(),
        }),
    };
    let json = serde_json::to_string(&progress).unwrap();
    let back: FrameProgress = serde_json::from_str(&json).unwrap();
    assert_eq!(progress, back);

    let empty: FrameProgress = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, FrameProgress::default());
}
