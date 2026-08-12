//! #1702: a plan step that is already satisfied costs approximately nothing.
//!
//! `execute_plan` used to drive every step of a multi-step plan
//! unconditionally, so a worker that completed the whole task in step 1 was
//! walked through the remaining steps re-confirming its own finished work —
//! on the measured run, 63% of the cost bought no work. The step prompt now
//! offers a close-out channel (`plan_steps::PLAN_COMPLETE_MARKER`), and the
//! loop ends when the worker takes it. These scenarios pin both directions:
//! the close-out skips the redundant walk, and a worker that never declares
//! completion keeps the full walk exactly as before.
//!
//! #2933 adds a second, narrower close-out below (`a_no_tool_call_step_ends_
//! the_walk_with_one_consolidated_check`): a step whose turn makes literally
//! no tool call — `step_prompt` already tells the worker to answer exactly
//! that way when a step is already covered — collapses the rest of the walk
//! into one consolidated check instead of one turn per remaining step.

use super::*;

/// One scripted multi-step run: triage → an 8-step plan → the step loop,
/// with a tracked test command so verification is deterministic (red
/// baseline, green candidate, green confirmation) and never spends a model
/// call the assertion would have to account for.
macro_rules! plan_walk_scenario {
    ($provider:expr) => {
        plan_walk_scenario!($provider, EmptyTools)
    };
    ($provider:expr, $tools:expr) => {{
        let provider = $provider;
        let resolver = OneProvider(&provider);
        let runner = ScriptedRunner::new(vec![false, true, true], "@@ -1 +1 @@\n-old\n+new");
        let tools = $tools;
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
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let outcome = pipeline
            .run(
                "Install and verify nginx end to end",
                &mut messages,
                &mut budget,
            )
            .await
            .expect("run succeeds");
        (outcome, drain(&mut rx), provider.prompts())
    }};
}

/// An 8-step plan, all filled with distinct step replies so the queue can
/// serve the full walk when the loop takes it.
fn eight_step_plan() -> CompletionResult {
    text_result(r#"["s1","s2","s3","s4","s5","s6","s7","s8"]"#)
}

/// The witness. The worker finishes the whole goal in step 1 and says so;
/// the remaining 7 steps must not run. Fails on `main`, where the loop
/// walks all 8 steps (10 model calls) whatever the worker declared.
#[tokio::test]
async fn a_declared_complete_goal_ends_the_step_walk() {
    let (outcome, _events, prompts) = plan_walk_scenario!(ScriptedProvider::new(vec![
        text_result("multi"),
        eight_step_plan(),
        text_result("PLAN COMPLETE: installed, configured, and verified in one pass."),
        // Servable only by a loop that walks finished work — the assertion
        // below is that none of these are ever popped.
        text_result("Step 2 was already done"),
        text_result("Step 3 was already done"),
        text_result("Step 4 was already done"),
        text_result("Step 5 was already done"),
        text_result("Step 6 was already done"),
        text_result("Step 7 was already done"),
        text_result("Step 8 was already done"),
    ]));

    assert_eq!(outcome.status, PipelineStatus::Completed);
    let verdict = outcome.verdict.expect("the flip produced a verdict");
    assert!(verdict.passed && verdict.deterministic);
    assert_eq!(
        prompts.len(),
        3,
        "triage, plan, and the ONE step turn that closed the plan out — more \
         means the loop walked steps the worker had declared finished (#1702)"
    );
    assert!(
        outcome.final_text.starts_with("PLAN COMPLETE"),
        "the close-out reply is the run's final text: {}",
        outcome.final_text
    );
}

/// The other direction: a worker that never takes the close-out channel AND
/// keeps dispatching tool calls every step gets the full walk, one turn per
/// step, exactly as before — the early exit is the worker's own declaration
/// or (#2933) a turn that acted on nothing, never a guess about a worker that
/// is visibly still working.
#[tokio::test]
async fn an_undeclared_goal_still_walks_every_step() {
    // Each step's turn is two model calls: the tool dispatch, then a
    // plain-text reply that closes the turn's own inner tool loop (the
    // engine keeps calling the model within a turn until one comes back with
    // no more tool calls) — so a real acting turn never trips #2933's
    // zero-tool-call trigger.
    let mut script = vec![text_result("multi"), eight_step_plan()];
    for n in 1..=8 {
        script.push(writing_tool_result(&format!("did step {n}")));
        script.push(text_result(&format!("step {n} complete")));
    }
    let (outcome, _events, prompts) =
        plan_walk_scenario!(ScriptedProvider::new(script), OneWritingTool);

    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(
        prompts.len(),
        18,
        "triage, plan, and two model calls per step (tool dispatch + close) \
         across all 8 steps — no step is skipped while the worker keeps \
         acting: {prompts:?}"
    );
    assert!(
        prompts.last().unwrap().contains("Step 8/8: s8"),
        "the last step's turn carries the last step: {}",
        prompts.last().unwrap()
    );
    assert!(
        prompts[2].contains(plan_steps::PLAN_COMPLETE_MARKER),
        "every step prompt offers the close-out channel: {}",
        prompts[2]
    );
}

/// #2933 witness: a step turn that dispatches literally no tool call is
/// answering, not working — `step_prompt` already instructs the worker to
/// reply that way when a step is already covered, and one such reply usually
/// means the rest of the plan is covered too. The walk must not replay the
/// remaining steps one per turn; it asks once, then stops.
///
/// Fails on `main`, where the loop keeps feeding steps 3–8 one at a time
/// (10 total prompts) no matter how many of them come back with zero tool
/// calls.
#[tokio::test]
async fn a_no_tool_call_step_ends_the_walk_with_one_consolidated_check() {
    let (outcome, _events, prompts) = plan_walk_scenario!(
        ScriptedProvider::new(vec![
            text_result("multi"),
            eight_step_plan(),
            // Step 1 does the real work: a tool dispatch, then the close of
            // that turn's own inner loop.
            writing_tool_result("did step 1"),
            text_result("step 1 complete"),
            // Step 2 answers with no tool call at all: #2933's trigger.
            text_result("Step 2 was already done"),
            // The one consolidated follow-up covering steps 3-8.
            text_result("Everything else was already covered too."),
        ]),
        OneWritingTool
    );

    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(
        prompts.len(),
        6,
        "triage, plan, step 1's dispatch + close, step 2, and ONE \
         consolidated follow-up — not one further turn per remaining step: \
         {prompts:?}"
    );
    assert!(
        prompts[5].contains("s3") && prompts[5].contains("s8"),
        "the consolidated follow-up must name every remaining step: {}",
        prompts[5]
    );
    assert_eq!(
        outcome.final_text, "Everything else was already covered too.",
        "the consolidated follow-up's reply is the run's final text"
    );
}
