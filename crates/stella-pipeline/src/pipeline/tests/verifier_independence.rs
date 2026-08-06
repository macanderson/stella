//! #1795: nothing enforced verifier != worker for the VERDICT call.
//!
//! The witness author has a hard independence gate; the verdict call had
//! none — with one configured provider the "independent code reviewer" is
//! the model that wrote the code, and its PASS ends the run behind a
//! once-per-run prose caveat. These scenarios pin the two halves of the fix:
//! the opt-in refusal (`require_independent_verifier`, parity with the
//! witness gate, before any paid call), and the structured fact — every
//! model verdict's ladder snapshot states whether its grader was independent
//! (`LadderSnapshot::verifier_independent`), so a stored verdict says so
//! without the transcript.

use super::*;

/// A router whose worker and verifier pins resolve to `worker_model` and
/// `verifier_model` — the two-line variable every scenario here turns on.
fn pinned_router(worker_model: &str, verifier_model: &str) -> Router {
    let mut roles = RoleTable::new();
    roles.pin(Role::Worker, ModelRef::new("scripted", worker_model));
    roles.pin(Role::Verifier, ModelRef::new("scripted", verifier_model));
    Router::new(
        roles,
        vec![ProviderProfile::new(
            "scripted",
            ModelRef::new("scripted", "worker"),
            ModelRef::new("scripted", "triage"),
            ModelRef::new("scripted", "verifier"),
        )],
        CircuitBreaker::new(Box::new(ZeroClock)),
    )
}

/// One scripted run against `router`, returning the outcome (or refusal) and
/// the drained event stream.
macro_rules! independence_scenario {
    ($provider:expr, $runner:expr, $router:expr, $config:expr) => {{
        let provider = $provider;
        let resolver = OneProvider(&provider);
        let runner = $runner;
        let repo_status = SeqRepoStatus::new(vec![vec![]]);
        let tools = EmptyTools;
        let recall = NoContextRecall;
        let repo = NoRepoStructure;
        let approvals = AutoApproveGate;
        let sleeper = NoopSleeper;
        let router = $router;
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
            .run("Fix the failing test", &mut messages, &mut budget)
            .await;
        (outcome, drain(&mut rx))
    }};
}

/// The verdict-path script every recording scenario shares: red baseline,
/// then an inconclusive candidate run, so the ladder escalates to the model
/// verifier — whose FAIL ends the run at `max_revisions: 0` with the stamped
/// snapshot as the run's verdict.
fn verdict_config() -> PipelineConfig {
    PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        // No repair round: the first model verdict is the outcome under test,
        // and a revise turn after it would only lengthen the script.
        max_revisions: 0,
        distress_guidance: false,
        ..PipelineConfig::default()
    }
}

/// The refusal witness (parity with
/// `requiring_an_independent_witness_refuses_before_spending_anything`): with
/// the flag set and the verdict resolving to the worker's own model, the run
/// refuses BEFORE any paid call — the provider is scripted with no responses,
/// so reaching one would panic rather than merely fail an assertion.
#[tokio::test]
async fn requiring_an_independent_verifier_refuses_before_spending_anything() {
    let (outcome, events) = independence_scenario!(
        ScriptedProvider::new(vec![]),
        ScriptedRunner::new(vec![], ""),
        pinned_router("same-model", "same-model"),
        PipelineConfig {
            require_independent_verifier: true,
            ..PipelineConfig::default()
        }
    );

    let error = outcome.expect_err("a self-grading verdict under the flag must refuse");
    match &error.cause {
        PipelineError::VerifierNotIndependent(reason) => assert!(
            reason.contains("no model independent of the worker"),
            "the refusal must carry the reason: {reason}"
        ),
        other => panic!("expected VerifierNotIndependent, got {other:?}"),
    }
    assert_eq!(
        error.total_cost_usd, 0.0,
        "the refusal exists so the caller never buys a trajectory it must discard"
    );
    assert!(
        stages(&events).is_empty(),
        "no stage may open before the refusal"
    );
}

/// Default off: a single-key BYOK seat must keep working, exactly as the
/// witness sibling's opt-in test pins for its flag.
#[tokio::test]
async fn the_verifier_independence_requirement_is_opt_in() {
    assert!(
        !PipelineConfig::default().require_independent_verifier,
        "a single-provider setup legitimately grades with the worker's model; \
         the verdict records the fact instead (see the snapshot scenarios below)"
    );
}

/// A distinct verifier satisfies the requirement, so the flag is inert on the
/// arm it protects — the treatment arm must RUN, not merely fail loudly.
#[tokio::test]
async fn requiring_an_independent_verifier_lets_a_distinct_verifier_through() {
    let (outcome, _events) = independence_scenario!(
        ScriptedProvider::new(vec![
            text_result("single"),
            text_result("done"),
            text_result("FAIL — the change misses the empty case"),
        ]),
        ScriptedRunner::scripted(
            vec![TestScript::Fail, TestScript::TimeOut],
            "@@ -1 +1 @@\n-old\n+new"
        ),
        pinned_router("worker-model", "reviewer-model"),
        PipelineConfig {
            require_independent_verifier: true,
            ..verdict_config()
        }
    );

    let outcome = outcome.expect("an independent verifier satisfies the requirement");
    assert!(
        matches!(outcome.status, PipelineStatus::VerificationFailed { .. }),
        "the run proceeded to a real verdict: {:?}",
        outcome.status
    );
}

/// The recording witness, self-graded side: with worker and verifier resolving
/// to one model (the single-provider seat), the model verdict's snapshot
/// states `verifier_independent: false` — a structured fact a reader of the
/// STORED verdict can see, unlike the prose caveat that scrolls away.
#[tokio::test]
async fn a_self_graded_verdict_states_the_fact_on_its_snapshot() {
    let (outcome, _events) = independence_scenario!(
        ScriptedProvider::new(vec![
            text_result("single"),
            text_result("done"),
            text_result("FAIL — the change misses the empty case"),
        ]),
        ScriptedRunner::scripted(
            vec![TestScript::Fail, TestScript::TimeOut],
            "@@ -1 +1 @@\n-old\n+new"
        ),
        pinned_router("same-model", "same-model"),
        verdict_config()
    );

    let outcome = outcome.expect("the soft path proceeds and records");
    let verdict = outcome.verdict.expect("the model verdict is the outcome");
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(
        snapshot.rung,
        Some(stella_protocol::LadderRung::ModelVerdict)
    );
    assert_eq!(
        snapshot.verifier_independent,
        Some(false),
        "a worker grading its own work must be stated on the stored verdict, \
         not only in a scrolling warning"
    );
}

/// The other polarity: a grader distinct from the worker is stated as
/// independent — `Some(true)`, distinguishable from both self-grading and
/// from a snapshot recorded before the fact existed.
#[tokio::test]
async fn an_independent_verdict_states_that_too() {
    let (outcome, _events) = independence_scenario!(
        ScriptedProvider::new(vec![
            text_result("single"),
            text_result("done"),
            text_result("FAIL — the change misses the empty case"),
        ]),
        ScriptedRunner::scripted(
            vec![TestScript::Fail, TestScript::TimeOut],
            "@@ -1 +1 @@\n-old\n+new"
        ),
        pinned_router("worker-model", "reviewer-model"),
        verdict_config()
    );

    let outcome = outcome.expect("the run proceeds");
    let verdict = outcome.verdict.expect("the model verdict is the outcome");
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(snapshot.verifier_independent, Some(true));
}
