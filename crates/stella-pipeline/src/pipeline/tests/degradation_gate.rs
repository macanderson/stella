//! The agent-degradation gate (`docs/spec/verification-gate.md`, layer 1).
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
    let verifier_ran = stages(&events).contains(&StageKind::Verdict);
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

use ModelCallRole::{Triage, Verdict, Worker};

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

/// #2128 witness: a reasoning model that spends its whole output cap on
/// reasoning returns `finish_reason: length` with empty text. That response
/// carries no verdict token, so before this the verdict silently degraded to
/// the heuristic and — with no flip to rescue it — failed a candidate no
/// model had actually judged. Now the starvation signature buys ONE retry at
/// a raised cap, and the retried verdict is the one that decides.
///
/// The extra `Verdict` in `roles` is the point of pinning it here: the retry
/// is a real paid call and this gate is where spend changes get argued.
#[tokio::test]
async fn gate_a_starved_verdict_call_is_retried_rather_than_degraded() {
    run_scenario(Scenario {
        name: "starved_verdict",
        goal: "Fix the failing test",
        provider: vec![
            text_result("single"),
            text_result("done"),
            starved_result(),
            text_result("PASS — change is consistent with the goal"),
        ],
        // A timed-out baseline: no flip is ever armed, so the ladder is
        // inconclusive and escalates — the exact shape that made an empty
        // verdict decide the run.
        tests: vec![TestScript::TimeOut, TestScript::Pass],
        diff: "@@ -1 +1 @@\n-old\n+new",
        lint: None,
        test_command: Some("cargo test -p x"),
        // Zero, so the failing heuristic this test refutes cannot be masked
        // by a revision loop: on the old code the run ends `passed: false`
        // on three calls.
        max_revisions: 0,
        expect: Expect {
            passed: true,
            deterministic: false,
            verifier_stage: true,
            roles: &[Triage, Worker, Verdict, Verdict],
            test_runs: 2,
        },
    })
    .await;
}

/// The cap-starvation signature (#2128): the provider stopped at the token
/// limit having emitted nothing. On a reasoning model this is what a role cap
/// sized for the visible output contract produces — the reasoning stream
/// bills against the same budget.
fn starved_result() -> CompletionResult {
    CompletionResult {
        finish_reason: Some(FinishReason::Length),
        ..text_result("")
    }
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

/// A tool registry advertising only `verify_done`, whose result carries the
/// confirmed-witness marker the real tool prints
/// (`stella-tools/src/verify.rs`). Mutating like the real one, so a turn that
/// calls it is never written off as a no-op.
struct ConfirmingVerifyDone;

#[async_trait]
impl ToolExecutor for ConfirmingVerifyDone {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "verify_done".into(),
            description: "prove the change with a witness test".into(),
            input_schema: serde_json::json!({ "type": "object" }),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: "WITNESS CONFIRMED — deterministic definition of done met:\n\
                      - new code:      `pytest -q` exit 0 (PASS)\n\
                      - previous code: baseline abc1234 (pinned) → exit 1 (FAIL)"
                .into(),
        }
    }
}

/// One completion that calls `verify_done` and nothing else.
fn verify_done_call() -> CompletionResult {
    CompletionResult {
        tool_calls: vec![ToolCall {
            call_id: "verify-1".into(),
            name: "verify_done".into(),
            input: serde_json::json!({}),
        }],
        ..text_result("")
    }
}

/// #2129 witness: the worker's own `verify_done` run confirmed a genuine
/// baseline-pinned fail→pass flip, and the verdict model then answered
/// without a verdict token — degrading to the heuristic. The heuristic used
/// to read only the pipeline's own flip oracle, which tracks a different
/// command, so it asserted "no flip" over a trace that literally contained
/// `WITNESS CONFIRMED` and re-opened finished work: in match
/// `cc00894779ff`'s `extract-elf` trial that cost 780s of 1022s reworking a
/// task already proven done.
///
/// A `verify_done` confirmation is a deterministic tool observation, not
/// another model's opinion, so it must survive a verifier outage exactly the
/// way the oracle's own flip does (#1788).
#[tokio::test]
async fn a_confirmed_verify_done_flip_survives_a_degraded_verdict() {
    // triage; worker calls verify_done; worker finishes; a verdict reply
    // carrying no PASS/FAIL token, which is what degrades to the heuristic.
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        verify_done_call(),
        text_result("done — the witness proves it"),
        text_result("Here is my assessment of the change."),
    ]);
    let resolver = OneProvider(&provider);
    // Both observations time out: no flip is armed and no touched test is
    // ever confirmed green, so the heuristic has nothing BUT the
    // `verify_done` confirmation to stand on.
    let runner = ScriptedRunner::scripted(
        vec![TestScript::TimeOut, TestScript::TimeOut],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let tools = ConfirmingVerifyDone;
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
        max_revisions: 0,
        distress_guidance: false,
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

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Extract the ELF payload", &mut messages, &mut budget)
        .await
        .expect("the run reaches a verdict");

    assert!(
        matches!(outcome.status, PipelineStatus::Completed { .. }),
        "a confirmed verify_done flip must survive the verifier outage that \
         re-opened this exact shape; got {:?}",
        outcome.status
    );
    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed, "the heuristic must credit the confirmation");
    assert!(
        verdict.summary.contains("verify_done"),
        "the evidence must name what it credited, so a reader can audit it: {}",
        verdict.summary
    );

    // The degradation really happened — without this, a run that somehow
    // never escalated would satisfy the assertions above for the wrong
    // reason.
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Proof {
                step: ProofStep::VerdictDegraded { .. }
            }
        )),
        "the verdict degraded to the heuristic; that is the state under test"
    );
}
