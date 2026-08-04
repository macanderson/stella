//! The agent-degradation gate (`docs/design/verification-gate.md`, layer 1).
//!
//! A fixed matrix of full-pipeline scenarios — real triage, execution,
//! verification ladder, and verifier over scripted doubles — each pinning THREE
//! things at once:
//!
//! 1. **The decision**: the verdict and whether it was deterministic.
//! 2. **The spend**: the exact model calls, by role and in order, and the
//!    exact number of test-suite invocations. A change that keeps every
//!    verdict but buys a verifier call where the ladder used to decide for free
//!    is a cost regression, and it fails HERE, on the PR, with the extra
//!    call named — not three weeks later on an invoice.
//! 3. **The escalation shape**: whether the verifier stage ran at all.
//!
//! Runs on every `cargo test` — deterministic, offline, no API key. When a
//! scenario fails on an *intended* behavior change, update its expectation
//! in the same PR so the new decision policy is stated as a reviewable diff
//! line. Never widen an expectation to "whatever it now does".

use super::verification_hardening::{ScriptedLint, lint_error};
use super::*;

/// What one scenario pins. `roles` is the full ordered model-call sequence
/// (from `StepManifest` events) — order matters, because "verifier before
/// worker" and "worker before verifier" are different pipelines.
struct Expect {
    passed: bool,
    deterministic: bool,
    verifier_stage: bool,
    roles: &'static [ModelCallRole],
    test_runs: u32,
}

struct Scenario {
    name: &'static str,
    goal: &'static str,
    /// Scripted model responses, in call order (triage, worker, [verifier]).
    provider: Vec<CompletionResult>,
    /// Scripted suite results, in run order; an exhausted queue passes.
    tests: Vec<TestScript>,
    /// The diff the workspace reports.
    diff: &'static str,
    /// Scripted lint snapshots (`None` = no probe wired).
    lint: Option<Vec<Option<Vec<LintRecord>>>>,
    test_command: Option<&'static str>,
    max_revisions: u32,
    expect: Expect,
}

async fn run_scenario(s: Scenario) {
    let provider = ScriptedProvider::new(s.provider);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::scripted(s.tests, s.diff);
    let lint = s.lint.map(ScriptedLint::new);
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        test_command: s.test_command.map(str::to_string),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        max_revisions: s.max_revisions,
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
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: lint.as_ref().map(|l| l as &dyn LintProbe),
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

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run(s.goal, &mut messages, &mut budget)
        .await
        .unwrap_or_else(|e| panic!("[{}] run failed: {e:?}", s.name));

    let name = s.name;
    let verdict = outcome
        .verdict
        .unwrap_or_else(|| panic!("[{name}] no verdict was produced"));
    assert_eq!(
        verdict.passed, s.expect.passed,
        "[{name}] verdict.passed changed — the decision policy drifted"
    );
    assert_eq!(
        verdict.deterministic, s.expect.deterministic,
        "[{name}] verdict.deterministic changed — deterministic credit moved"
    );

    let events = drain(&mut rx);
    let verifier_ran = stages(&events).contains(&StageKind::Verifier);
    assert_eq!(
        verifier_ran, s.expect.verifier_stage,
        "[{name}] verifier stage presence changed — escalation shape drifted"
    );
    let roles: Vec<ModelCallRole> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::StepManifest { role, .. } => Some(*role),
            _ => None,
        })
        .collect();
    assert_eq!(
        roles, s.expect.roles,
        "[{name}] the model-call sequence changed — this is SPEND: every \
         entry is a paid call. If the new sequence is intended, update this \
         scenario in the same PR."
    );
    assert_eq!(
        runner.test_runs(),
        s.expect.test_runs,
        "[{name}] the suite-run count changed — this is COMPUTE spend. If \
         intended, update this scenario in the same PR."
    );
}

use ModelCallRole::{Verdict, Triage, Worker};

/// The paved road: a genuine fail→pass flip fast-submits deterministically.
/// Exactly two paid calls (triage, worker) and three suite runs (baseline,
/// candidate, the #859 confirmation). The verifier is never consulted.
#[tokio::test]
async fn gate_clean_flip_fast_submits_with_two_calls_and_three_runs() {
    run_scenario(Scenario {
        name: "clean_flip",
        goal: "Fix the failing test",
        provider: vec![text_result("single"), text_result("done")],
        tests: vec![TestScript::Fail, TestScript::Pass, TestScript::Pass],
        diff: "@@ -1 +1 @@\n-old\n+new",
        lint: None,
        test_command: Some("cargo test -p x"),
        max_revisions: 2,
        expect: Expect {
            passed: true,
            deterministic: true,
            verifier_stage: false,
            roles: &[Triage, Worker],
            test_runs: 3,
        },
    })
    .await;
}

/// A flaky flip (#859): the confirmation re-run fails, so the determinism is
/// withheld and ONE verifier call is bought. Still three suite runs — the
/// confirmation is the third; nothing retries it.
#[tokio::test]
async fn gate_flaky_flip_buys_exactly_one_verifier_call() {
    run_scenario(Scenario {
        name: "flaky_flip",
        goal: "Fix the failing test",
        provider: vec![
            text_result("single"),
            text_result("done"),
            text_result("PASS — the fix is right; the test itself is flaky"),
        ],
        tests: vec![TestScript::Fail, TestScript::Pass, TestScript::Fail],
        diff: "@@ -1 +1 @@\n-old\n+new",
        lint: None,
        test_command: Some("cargo test -p x"),
        max_revisions: 2,
        expect: Expect {
            passed: true,
            deterministic: false,
            verifier_stage: true,
            roles: &[Triage, Worker, Verdict],
            test_runs: 3,
        },
    })
    .await;
}

/// A timed-out baseline (#860) arms nothing: no flip exists, so no
/// confirmation run is ever bought — two suite runs, one verifier call.
#[tokio::test]
async fn gate_timed_out_baseline_never_buys_a_confirmation() {
    run_scenario(Scenario {
        name: "timeout_baseline",
        goal: "Fix the failing test",
        provider: vec![
            text_result("single"),
            text_result("done"),
            text_result("PASS — change is consistent with the goal"),
        ],
        tests: vec![TestScript::TimeOut, TestScript::Pass],
        diff: "@@ -1 +1 @@\n-old\n+new",
        lint: None,
        test_command: Some("cargo test -p x"),
        max_revisions: 2,
        expect: Expect {
            passed: true,
            deterministic: false,
            verifier_stage: true,
            roles: &[Triage, Worker, Verdict],
            test_runs: 2,
        },
    })
    .await;
}

/// The regression veto (#861) fires BEFORE the confirmation run: a fresh
/// lint error escalates after only two suite runs — the confirmation's
/// suite run is never spent on a submit the veto already blocked.
#[tokio::test]
async fn gate_lint_veto_skips_the_confirmation_run() {
    run_scenario(Scenario {
        name: "lint_veto",
        goal: "Fix the failing test",
        provider: vec![
            text_result("single"),
            text_result("done"),
            text_result("PASS — the new error is pre-existing debt"),
        ],
        tests: vec![TestScript::Fail, TestScript::Pass],
        diff: "@@ -1 +1 @@\n-old\n+new",
        lint: Some(vec![
            Some(Vec::new()),
            Some(vec![lint_error("src/lib.rs", "mismatched types")]),
        ]),
        test_command: Some("cargo test -p x"),
        max_revisions: 2,
        expect: Expect {
            passed: true,
            deterministic: false,
            verifier_stage: true,
            roles: &[Triage, Worker, Verdict],
            test_runs: 2,
        },
    })
    .await;
}

/// A red suite is a deterministic failure: no verifier call is ever bought to
/// "confirm" it (L-E11). With revisions exhausted the run fails on exactly
/// two paid calls.
#[tokio::test]
async fn gate_red_tests_fail_without_buying_a_verifier() {
    run_scenario(Scenario {
        name: "red_tests",
        goal: "Fix the failing test",
        provider: vec![text_result("single"), text_result("done")],
        tests: vec![TestScript::Fail, TestScript::Fail],
        diff: "@@ -1 +1 @@\n-old\n+new",
        lint: None,
        test_command: Some("cargo test -p x"),
        max_revisions: 0,
        expect: Expect {
            passed: false,
            deterministic: true,
            verifier_stage: false,
            roles: &[Triage, Worker],
            test_runs: 2,
        },
    })
    .await;
}

/// A turn that dispatched nothing is a deterministic no-op finding
/// (`NothingAttempted`), never an abstention that reads as a pass — the
/// eleven 0.0-scored Terminal-Bench trials, pinned as spend: two paid
/// calls, zero suite runs, no verifier.
#[tokio::test]
async fn gate_a_no_op_turn_fails_closed_without_spend() {
    run_scenario(Scenario {
        name: "no_op_turn",
        goal: "Fix the failing test",
        provider: vec![text_result("single"), text_result("done")],
        tests: vec![],
        diff: "",
        lint: None,
        test_command: None,
        max_revisions: 0,
        expect: Expect {
            passed: false,
            deterministic: true,
            verifier_stage: false,
            roles: &[Triage, Worker],
            test_runs: 0,
        },
    })
    .await;
}

/// Fix-by-disappearance (#867): the baseline names its failing test, the
/// candidate's suite passes with a complete listing that no longer contains
/// it (deleted/renamed). The exit code says flip; the same-failure rule says
/// no — one verifier call, and no confirmation run is bought for a flip that
/// was never credited.
#[tokio::test]
async fn gate_a_vanished_failing_test_earns_no_flip() {
    run_scenario(Scenario {
        name: "vanished_failure",
        goal: "Fix the failing test",
        provider: vec![
            text_result("single"),
            text_result("done"),
            text_result("FAIL — the failing test was removed, not fixed"),
        ],
        tests: vec![
            TestScript::FailWith(
                "test suite::test_a ... FAILED\n\
                 test result: FAILED. 0 passed; 1 failed",
            ),
            TestScript::PassWith(
                "test suite::test_b ... ok\n\
                 test result: ok. 1 passed; 0 failed",
            ),
        ],
        diff: "@@ -1 +1 @@\n-old\n+new",
        lint: None,
        test_command: Some("cargo test -p x"),
        max_revisions: 0,
        expect: Expect {
            passed: false,
            deterministic: false,
            verifier_stage: true,
            roles: &[Triage, Worker, Verdict],
            test_runs: 2,
        },
    })
    .await;
}
