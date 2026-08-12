//! #2908: an unprovable *claim* must not end the *run*.
//!
//! The staged pipeline wraps the bare agent loop, so it may only add a claim
//! about the work — never subtract the work itself. These scenarios pin the
//! shape that broke it on ArenaBench `w10p`: with no `--test-command` and a
//! witness author that produced no `TEST_COMMAND`, the ladder reached
//! `Unverified`, the run ended at the verdict, and the worker was still in the
//! middle of the task the bare arm went on to finish.
//!
//! Every assertion below is about the pipeline's *stopping condition*, not
//! about its claim: the verdict stays unproven in all of them.

use super::*;

/// Ports + config wiring shared by the scenarios, parameterized by the tool
/// registry because "did the worker dispatch anything" is the subject here and
/// [`EmptyTools`] cannot express it.
macro_rules! run_scenario {
    ($provider:expr, $tools:expr, $runner:expr, $config:expr) => {{
        let provider = $provider;
        let resolver = OneProvider(&provider);
        let runner = $runner;
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
            $config,
        );
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let outcome = pipeline
            .run("Build the wheel", &mut messages, &mut budget)
            .await
            .expect("run succeeds");
        (outcome, drain(&mut rx), provider.prompts())
    }};
}

/// A distinct mutating call per turn — identical ones would trip the engine's
/// stuck-loop ladder and end the run for an unrelated reason.
fn writing_call(nth: u32) -> CompletionResult {
    CompletionResult {
        tool_calls: vec![ToolCall {
            call_id: format!("call-{nth}"),
            name: "write_file".into(),
            input: serde_json::json!({ "nth": nth }),
        }],
        ..text_result("working")
    }
}

/// The Terminal-Bench shape: no configured command, no author to supply one.
fn unprovable_config() -> PipelineConfig {
    PipelineConfig {
        test_command: None,
        roster: Roster::default().with_enabled(ModelCallRole::WitnessAuthor, false),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    }
}

/// **The witness.** Nothing could prove the turn, so the worker gets the turn
/// the bare loop would have given it — and the run ends where the worker ends
/// it, not where the ladder ran out of things to say.
///
/// On `main` the ladder's `Unverified` arm is terminal: the run stops with the
/// worker mid-task, `revisions` reads 0, and the fourth scripted completion is
/// never asked for.
#[tokio::test]
async fn an_unprovable_result_returns_the_worker_to_work() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        writing_call(1),
        text_result("done"),
        // The continuation: the worker takes the handback, asks for no tools,
        // and that is the bare loop's own terminal state.
        text_result("nothing further is needed"),
    ]);
    let runner = ScriptedRunner::scripted(Vec::new(), "@@ -1 +1 @@\n-old\n+new");

    let (outcome, _events, prompts) =
        run_scenario!(provider, OneWritingTool, runner, unprovable_config());

    assert_eq!(
        outcome.revisions, 1,
        "the unprovable result buys the worker one more turn — on main this is 0 \
         and the run ends with the task unfinished"
    );
    assert_eq!(
        prompts.len(),
        4,
        "triage, the worker's two steps, and the continuation turn"
    );
    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.summary.starts_with("UNVERIFIED"),
        "only the run continues — the claim is still unproven: {}",
        verdict.summary
    );
    assert_eq!(
        outcome.score,
        Some(crate::candidate::CandidateScore::Unverified),
        "and the score says so too: nothing proved this"
    );
}

/// The handback names no defect. This text is dispatched exactly where the
/// pipeline knows least, and a fabricated failure here is how a worker gets
/// talked out of correct work.
#[tokio::test]
async fn the_handback_reports_no_failure() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        writing_call(1),
        text_result("done"),
        text_result("nothing further is needed"),
    ]);
    let runner = ScriptedRunner::scripted(Vec::new(), "@@ -1 +1 @@\n-old\n+new");

    let (_outcome, _events, prompts) =
        run_scenario!(provider, OneWritingTool, runner, unprovable_config());

    let text = prompts
        .last()
        .expect("the continuation turn was dispatched")
        .clone();
    assert!(
        text.contains("could not observe this work"),
        "the worker is told what is missing: {text}"
    );
    assert!(
        !text.contains("did not pass"),
        "nothing failed, and the handback must not say one did: {text}"
    );
    assert!(
        text.contains("say so and stop"),
        "and it offers the exit the stopping condition reads: {text}"
    );
}

/// The guard against the obvious failure mode. A worker that keeps calling
/// tools keeps its turns — that is the finding — and the run ends at the first
/// continuation that asks for nothing, exactly as a bare turn ends at the first
/// step that requests no tools.
#[tokio::test]
async fn continuations_stop_when_the_worker_stops_calling_tools() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        writing_call(1),
        text_result("done"),
        // First continuation: still working.
        writing_call(2),
        text_result("that is the last of it"),
        // Second continuation: no tool call — the worker is finished.
        text_result("nothing further is needed"),
    ]);
    let runner = ScriptedRunner::scripted(Vec::new(), "@@ -1 +1 @@\n-old\n+new");

    let (outcome, _events, prompts) =
        run_scenario!(provider, OneWritingTool, runner, unprovable_config());

    assert_eq!(
        outcome.revisions, 2,
        "the working continuation earned another; the idle one ended the run"
    );
    assert_eq!(
        prompts.len(),
        6,
        "triage, the worker's two steps, the first continuation's two, and the \
         idle continuation that settles it"
    );
    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.summary.starts_with("UNVERIFIED"),
        "still unproven, however many turns the work took: {}",
        verdict.summary
    );
}

/// The outer bound is the repair gate, not a count invented for this path: a
/// run with no allowance left settles immediately, and the budget guard is
/// consulted between turns rather than inside one (invariant #6).
#[tokio::test]
async fn no_allowance_means_no_continuation() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        writing_call(1),
        text_result("done"),
    ]);
    let runner = ScriptedRunner::scripted(Vec::new(), "@@ -1 +1 @@\n-old\n+new");
    let config = PipelineConfig {
        max_revisions: 0,
        ..unprovable_config()
    };

    let (outcome, _events, prompts) = run_scenario!(provider, OneWritingTool, runner, config);

    assert_eq!(outcome.revisions, 0, "no allowance, no turn");
    assert_eq!(
        prompts.len(),
        3,
        "triage and the worker's two steps — nothing more"
    );
    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.summary.starts_with("UNVERIFIED"),
        "{}",
        verdict.summary
    );
}
