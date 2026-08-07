//! End-to-end scenarios for the verification hardening series: typed
//! infra outcomes (#860), the confirmation run on flip (#859), the
//! diagnostics regression veto (#861), and the cumulative distress-guidance
//! trigger (#1780). Each drives the real pipeline over scripted ports — no
//! API key, no subprocess — and pins the decision the ladder must reach.

use super::*;
use crate::LineMutation;

/// #860 acceptance: a baseline that TIMES OUT observed no failing assertion,
/// so a candidate whose suite then passes has no fail→pass flip — the run
/// must escalate to the model verifier, never credit `DeterministicPass`. Before
/// the typed outcome, the timeout's non-zero exit locked the oracle onto a
/// phantom `Failing` and the faster candidate "flipped" it.
#[tokio::test]
async fn a_timed_out_baseline_never_manufactures_a_flip() {
    // triage → "single"; worker → final text; verifier → verdict (the ladder
    // escalates because no flip evidence exists).
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — change looks consistent with the goal"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::scripted(
        vec![TestScript::TimeOut, TestScript::Pass],
        "@@ -1 +1 @@\n-old\n+new",
    );
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        !verdict.deterministic,
        "a timed-out baseline plus a passing candidate is NOT a deterministic flip"
    );
    let events = drain(&mut rx);
    assert!(
        stages(&events).contains(&StageKind::Verdict),
        "no flip evidence exists, so the ladder must escalate to the verifier"
    );
}

/// #860 acceptance, candidate side: a suite that times out AFTER the change
/// is inconclusive (verifier), not a deterministic red (revise). Spending a
/// revision turn "fixing" a timeout the change may not have caused burned a
/// full worker call on infra noise; the verifier sees `test_run=timed_out` and
/// reasons about it instead.
#[tokio::test]
async fn a_timed_out_candidate_suite_escalates_instead_of_revising() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the timeout is pre-existing infra noise, the diff is sound"),
    ]);
    let resolver = OneProvider(&provider);
    // Baseline genuinely fails (oracle arms), candidate run times out.
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::TimeOut],
        "@@ -1 +1 @@\n-old\n+new",
    );
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
        // The subject here is which rung an unobservable candidate suite lands
        // on. The #1295 corroboration ask fires strictly *after* that, on the
        // verifier's pass, and would spend a revision this scenario is not about
        // — `tests/verifier_evidence_demand.rs` covers that behaviour directly.
        verifier_evidence_demand: false,
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let events = drain(&mut rx);
    assert!(
        stages(&events).contains(&StageKind::Verdict),
        "an unobservable suite is inconclusive — verifier, not revise"
    );
    // No deterministic red verdict may be emitted for infra noise: every
    // deterministic Verdict{passed:false} is the revise path's badge.
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::Verdict {
                passed: false,
                evidence
            } if evidence.deterministic
        )),
        "a timeout must not be reported as a deterministic test failure"
    );
    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(!verdict.deterministic);
}

/// #859 acceptance: a flaky flip — fail on the baseline, pass on the
/// candidate, fail again on the confirmation re-run — must not fast-submit.
/// The pre-submit audit demotes the oracle to `Unstable` and the ladder
/// escalates to the verifier with `unstable_flip=true` in its evidence.
#[tokio::test]
async fn a_flaky_flip_fails_its_confirmation_and_escalates() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the change is right; the test itself is flaky"),
    ]);
    let resolver = OneProvider(&provider);
    // Baseline fail → candidate pass (flip) → confirmation FAIL (flake).
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::Pass, TestScript::Fail],
        "@@ -1 +1 @@\n-old\n+new",
    );
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        !verdict.deterministic,
        "a flip that failed its confirmation must never wear the deterministic badge"
    );
    let events = drain(&mut rx);
    assert!(
        stages(&events).contains(&StageKind::Verdict),
        "the unconfirmed flip escalates to the verifier instead of fast-submitting"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::Verdict {
                passed: true,
                evidence
            } if evidence.deterministic
        )),
        "no deterministic pass may be emitted for an unconfirmed flip"
    );
}

/// A scripted lint probe (#861): pops one scripted snapshot per call, and
/// reports a clean tree once the script runs out.
pub(super) struct ScriptedLint {
    snapshots: std::sync::Mutex<VecDeque<Option<Vec<LintRecord>>>>,
}

impl ScriptedLint {
    pub(super) fn new(snapshots: Vec<Option<Vec<LintRecord>>>) -> Self {
        Self {
            snapshots: std::sync::Mutex::new(snapshots.into_iter().collect()),
        }
    }
}

#[async_trait]
impl LintProbe for ScriptedLint {
    async fn snapshot(&self, _root: Option<&str>) -> Option<Vec<LintRecord>> {
        self.snapshots
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Some(Vec::new()))
    }
}

pub(super) fn lint_error(file: &str, message: &str) -> LintRecord {
    LintRecord {
        file: file.to_string(),
        error: true,
        code: Some("E0308".to_string()),
        message: message.to_string(),
    }
}

/// #861 acceptance: a candidate that flips its witness AND introduces a
/// fresh type error must not fast-submit — the regression veto routes it to
/// the verifier with the diagnostics delta in evidence. Lint stays excluded
/// from the oracle: it vetoes the submit, it never becomes the verification.
#[tokio::test]
async fn a_fresh_diagnostic_error_vetoes_the_fast_submit() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the error is cosmetic, ship it"),
    ]);
    let resolver = OneProvider(&provider);
    // Baseline fail → candidate pass: a genuine flip, green tests.
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    // Eager baseline snapshot: clean. Audit snapshot: one new error.
    let lint = ScriptedLint::new(vec![
        Some(Vec::new()),
        Some(vec![lint_error("src/lib.rs", "mismatched types")]),
    ]);
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
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: Some(&lint),
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        !verdict.deterministic,
        "a flip that introduced a fresh error must not wear the deterministic badge"
    );
    let events = drain(&mut rx);
    assert!(
        stages(&events).contains(&StageKind::Verdict),
        "the regression veto must escalate to the verifier"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::Verdict {
                passed: true,
                evidence
            } if evidence.deterministic
        )),
        "no deterministic pass may be emitted past the veto"
    );
}

/// The veto degrades open: the same flip with NO lint probe wired keeps its
/// deterministic fast-submit — absence of lint restores the pre-#861 ladder
/// instead of inventing an obstacle.
#[tokio::test]
async fn without_a_lint_probe_the_flip_still_fast_submits() {
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed);
    assert!(verdict.deterministic, "no probe, no veto — the flip stands");
    // #865: the verdict carries its own provenance — the ladder inputs it
    // was decided from, including the oracle trace with the confirmation
    // run — and replay renders "why" from it without re-deriving.
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("a fast-submit verdict records the snapshot it was decided from");
    assert!(snapshot.flip_achieved);
    assert!(!snapshot.unstable_flip);
    assert_eq!(snapshot.tracked_command.as_deref(), Some("cargo test -p x"));
    assert_eq!(
        snapshot.oracle_trace.len(),
        3,
        "baseline fail + candidate pass + confirmation pass"
    );
    // #1043: the snapshot names the rung it came to rest on, so a reader does
    // not have to infer "deterministic pass" from flags that a *waived* review
    // sets identically.
    assert_eq!(
        snapshot.rung,
        Some(stella_protocol::LadderRung::SubmitFast),
        "a fast-submit verdict records its rung"
    );
    // And the label that rung earns is the hard +1.0, before shaping.
    assert_eq!(
        crate::reward::outcome_term(
            stella_protocol::LadderRung::SubmitFast,
            verdict.passed,
            &crate::reward::OutcomeWeights::default(),
        ),
        Ok(1.0)
    );
    let why = crate::replay::verdict_provenance(&stella_protocol::VerdictEvidence {
        summary: verdict.summary.clone(),
        deterministic: verdict.deterministic,
        evidence_refs: vec![],
        ladder: verdict.ladder.clone(),
    })
    .expect("provenance renders from the recorded snapshot");
    assert!(why.starts_with("rung=submit_fast"), "got: {why}");
    assert!(why.contains("flip=achieved"), "got: {why}");
    assert!(why.contains("baseline:fail → candidate:pass"), "got: {why}");
    let events = drain(&mut rx);
    assert!(!stages(&events).contains(&StageKind::Verdict));
}

/// A scripted mutation probe (#870): every mutant gets the same outcome,
/// and calls are counted so scenarios can pin the early-exit discipline.
pub(super) struct ScriptedMutation {
    outcome: MutantOutcome,
    calls: std::sync::atomic::AtomicU32,
}

impl ScriptedMutation {
    pub(super) fn new(outcome: MutantOutcome) -> Self {
        Self {
            outcome,
            calls: std::sync::atomic::AtomicU32::new(0),
        }
    }
    pub(super) fn calls(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl MutationProbe for ScriptedMutation {
    async fn run_mutant(
        &self,
        _root: Option<&str>,
        _mutation: &LineMutation,
        _invocation: &TestInvocation,
    ) -> MutantOutcome {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.outcome
    }
}

/// A diff whose added lines carry two mutable tokens, so the #870 generator
/// proposes two mutants.
const MUTABLE_DIFF: &str = "--- a/src/fix.rs\n\
                            +++ b/src/fix.rs\n\
                            @@ -1,2 +1,2 @@\n\
                            -    if x > hi { hi } else { x }\n\
                            +    if x >= hi { hi } else { x }\n\
                            +    let ready = true;\n";

/// #870 acceptance: an authored witness that stays green under EVERY trivial
/// mutation of the changed lines is tautological — it reacts to the change
/// without constraining it. The flip stands as an observation, but the
/// deterministic credit is withheld and the verifier decides with
/// `witness_tautological=true` in evidence.
#[tokio::test]
async fn a_tautological_witness_is_downgraded_to_the_verifier() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("wrote the test.\nTEST_COMMAND: cargo test --test witness witness -- --exact"),
        text_result("PASS — the change is sound even though the witness is weak"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true, true], Ok(vec![]), log.clone())
        .with_diff(MUTABLE_DIFF)
        .with_repo_status(SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/witness.rs", "w1")],
        ]));
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
    );
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log);
    // The witness stays green while the changed lines are broken.
    let probe = ScriptedMutation::new(MutantOutcome::Witness { passed: true });
    let (outcome, events, _) = run_isolated_full(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the retry bug",
        router(),
        Some(&probe),
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    let verdict = outcome.verdict.expect("verified");
    assert!(
        !verdict.deterministic,
        "a tautological witness must not buy a deterministic pass: {}",
        verdict.summary
    );
    assert_eq!(
        probe.calls(),
        2,
        "both proposed mutants ran (none was killed, so no early exit)"
    );
    assert!(
        stages(&events).contains(&StageKind::Verdict),
        "the downgrade escalates to the verifier"
    );
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(
        snapshot.witness_mutation,
        Some(false),
        "provenance records that the witness survived no mutant"
    );
}

/// The sound-witness path: the first mutant kills the witness, the check
/// stops early (no second mutant is paid for), and the deterministic pass
/// stands with the finding in provenance.
#[tokio::test]
async fn a_witness_that_kills_a_mutant_keeps_its_deterministic_pass() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("wrote the test.\nTEST_COMMAND: cargo test --test witness witness -- --exact"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true, true], Ok(vec![]), log.clone())
        .with_diff(MUTABLE_DIFF)
        .with_repo_status(SeqRepoStatus::new(vec![
            vec![],
            vec![("tests/witness.rs", "w1")],
        ]));
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
    );
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log);
    let probe = ScriptedMutation::new(MutantOutcome::Witness { passed: false });
    let (outcome, events, _) = run_isolated_full(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the retry bug",
        router(),
        Some(&probe),
    )
    .await;
    let outcome = outcome.expect("run succeeds");

    let verdict = outcome.verdict.expect("verified");
    assert!(verdict.passed);
    assert!(verdict.deterministic, "the sound witness keeps its credit");
    assert_eq!(
        probe.calls(),
        1,
        "the first killed mutant proves the witness; the second is never paid for"
    );
    assert!(!stages(&events).contains(&StageKind::Verdict));
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(snapshot.witness_mutation, Some(true));
}

/// #1294 acceptance, the retry half: a suite the machine killed for memory
/// observed no assertion, so the pipeline RE-RUNS it rather than telling the
/// worker its (possibly correct) change failed. With the retry landing green,
/// the fail→pass flip is credited exactly as if the kill had never happened.
///
/// The scripted runs are, in order: the failing baseline (arms the oracle),
/// the OOM'd post-execute observation, its retry (green), and the pre-submit
/// confirmation (green). Four runs, one verdict, no revision turn.
#[tokio::test]
async fn an_out_of_memory_test_run_is_retried_instead_of_revised() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::scripted(
        vec![
            TestScript::Fail,
            TestScript::OutOfMemory,
            TestScript::Pass,
            TestScript::Pass,
        ],
        "@@ -1 +1 @@\n-old\n+new",
    );
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed);
    assert!(
        verdict.deterministic,
        "the retry observed the flip the memory kill hid: {}",
        verdict.summary
    );
    assert_eq!(
        runner.test_runs(),
        4,
        "baseline + the killed run + its one retry + the confirmation"
    );
    let events = drain(&mut rx);
    assert!(
        !stages(&events).contains(&StageKind::Verdict),
        "a deterministic flip needs no verifier"
    );
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(
        snapshot.test_infra, None,
        "the retry produced a real observation, so no non-observation is reported"
    );
}

/// #1294 acceptance, the honesty half: a suite that is killed for memory
/// *again* on its retry still reports `out_of_memory` — never a deterministic
/// red. The whole point of the outcome is that nothing was learned about the
/// code, so the run escalates to the verifier (which is told which of the two
/// happened) instead of spending a revision "fixing" work no test ever saw.
#[tokio::test]
async fn a_persistent_memory_kill_is_never_a_deterministic_test_failure() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — the kill is memory pressure, not a defect in the diff"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::scripted(
        vec![
            TestScript::Fail,
            TestScript::OutOfMemory,
            TestScript::OutOfMemory,
        ],
        "@@ -1 +1 @@\n-old\n+new",
    );
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
        // Same reason as the scenario above: the subject is that a memory
        // kill is reported as `out_of_memory` rather than as a red suite. The
        // #1295 ask fires after that, on the verifier's pass, and would spend a
        // revision this scenario neither scripts nor is about.
        verifier_evidence_demand: false,
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let events = drain(&mut rx);
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::Verdict {
                passed: false,
                evidence
            } if evidence.deterministic
        )),
        "a memory kill must never be reported as a deterministic test failure"
    );
    assert!(
        stages(&events).contains(&StageKind::Verdict),
        "an unobservable suite is inconclusive — verifier, not revise"
    );
    let verdict = outcome.verdict.expect("a verdict was produced");
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(
        snapshot.test_infra.as_deref(),
        Some("out_of_memory"),
        "the reader must be able to tell a memory kill from a timeout or a missing toolchain"
    );
    assert_eq!(
        snapshot.touched_tests_passed, None,
        "no assertion was observed either way"
    );
    assert_eq!(
        runner.test_runs(),
        3,
        "baseline + the killed run + its one bounded retry — retries never run away"
    );
}

/// A scripted coverage probe (#1291): answers with a fixed report, or `None`
/// for "no tooling could measure this", and counts the runs it was asked for.
struct ScriptedCoverage {
    report: Option<crate::verify::coverage::CoverageReport>,
    calls: std::sync::atomic::AtomicU32,
}

impl ScriptedCoverage {
    fn measuring(entries: &[(&str, &[u32])]) -> Self {
        Self {
            report: Some(
                entries
                    .iter()
                    .map(|(path, lines)| ((*path).to_string(), lines.iter().copied().collect()))
                    .collect(),
            ),
            calls: std::sync::atomic::AtomicU32::new(0),
        }
    }
    fn unavailable() -> Self {
        Self {
            report: None,
            calls: std::sync::atomic::AtomicU32::new(0),
        }
    }
    fn calls(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl crate::ports::CoverageProbe for ScriptedCoverage {
    async fn covered_lines(
        &self,
        _root: Option<&str>,
        _invocation: &TestInvocation,
    ) -> Option<crate::verify::coverage::CoverageReport> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.report.clone()
    }
}

/// A run whose flip is real and whose diff touches `src/lib.rs` line 12 —
/// the shape both coverage scenarios below share, so only the probe differs.
async fn run_with_coverage(
    probe: &dyn crate::ports::CoverageProbe,
    strict: bool,
    provider: &ScriptedProvider,
) -> (PipelineOutcome, Vec<AgentEvent>) {
    let resolver = OneProvider(provider);
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::Pass, TestScript::Pass],
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,2 +10,3 @@\n fn f() {\n-    old();\n+    changed();\n }",
    );
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
        require_diff_coverage: strict,
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
            coverage: Some(probe),
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
        .run("Change the thing", &mut messages, &mut budget)
        .await
        .expect("run succeeds");
    (outcome, drain(&mut rx))
}

/// #1291 acceptance: a flip whose test never executed the changed lines is a
/// coincidence, not evidence. The deterministic credit is withheld and the
/// turn escalates — **unproven**, and never a failure: no deterministic red
/// is emitted, and the verifier is told in so many words that this is not a
/// finding that the change is wrong.
#[tokio::test]
async fn a_flip_whose_test_never_ran_the_changed_lines_is_unproven_not_failed() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — read the diff, the change is right"),
    ]);
    // The suite ran, and it ran OTHER lines of the same file.
    let probe = ScriptedCoverage::measuring(&[("src/lib.rs", &[40, 41, 42])]);
    let (outcome, events) = run_with_coverage(&probe, false, &provider).await;

    assert_eq!(probe.calls(), 1, "the probe runs once, in the audit");
    assert!(
        stages(&events).contains(&StageKind::Verdict),
        "a coincidental pass must be escalated, not fast-submitted"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::Verdict {
                passed: false,
                evidence
            } if evidence.deterministic
        )),
        "no-overlap is 'unproven', never a deterministic failure"
    );
    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed, "the verifier's PASS still stands");
    assert!(
        !verdict.deterministic,
        "what it lost is the DETERMINISTIC credit"
    );
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(
        snapshot.diff_coverage.as_deref(),
        Some("not_covered"),
        "the result must say plainly what was found"
    );
    let asked = provider
        .prompts()
        .iter()
        .any(|prompt| prompt.contains("not a finding that the change is wrong"));
    assert!(
        asked,
        "the verifier must be told this is an unproven overlap, not a defect"
    );
}

/// #1434: management calls ship as `[system(instructions), user(payload)]` —
/// the byte-stable instruction block rides where the provider adapters'
/// cache machinery can mark it, and everything per-call rides after it.
/// Asserted through a real escalation, so the shape pinned here is the one
/// `metered_raw_call` actually dispatched, not what a prompt builder returns
/// in isolation.
#[tokio::test]
async fn management_calls_ship_a_stable_system_prefix_before_the_volatile_payload() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — read the diff, the change is right"),
    ]);
    let probe = ScriptedCoverage::measuring(&[("src/lib.rs", &[40, 41, 42])]);
    let (_outcome, _events) = run_with_coverage(&probe, false, &provider).await;

    let shapes = provider.shapes();
    let triage = shapes.first().expect("the triage call was made");
    assert_eq!(triage.len(), 2, "triage ships [system, user]: {triage:#?}");
    assert_eq!(triage[0].0, stella_protocol::MessageRole::System);
    assert!(triage[0].1.contains("Classify the following user message"));
    assert_eq!(triage[1].0, stella_protocol::MessageRole::User);
    assert!(
        triage[1].1.starts_with("Task:\n"),
        "only the task itself is volatile: {}",
        triage[1].1
    );

    let verdict = shapes.last().expect("the verdict call was made");
    assert_eq!(
        verdict.len(),
        2,
        "the verdict ships [system, user]: {verdict:#?}"
    );
    assert_eq!(verdict[0].0, stella_protocol::MessageRole::System);
    let reference = crate::verify::verifier_prompt(
        "an unrelated goal",
        "+an unrelated diff\n",
        "unrelated evidence",
        &crate::verify::diff_render::DiffContext::default(),
    );
    assert_eq!(
        verdict[0].1, reference.instructions,
        "the system block is the input-independent instruction constant — \
         byte-equal whatever the call was about"
    );
    assert_eq!(verdict[1].0, stella_protocol::MessageRole::User);
    assert!(
        verdict[1].1.starts_with("## Goal\n"),
        "the payload leads with the volatile sections: {}",
        verdict[1].1
    );
}

/// #1291, the honest-degradation half: a workspace with no coverage tooling
/// pays no verifier call — the alternative taxes every run to be told what the
/// evidence already said — but the run is scored **UNVERIFIED**, not as a
/// deterministic pass. "A test passed and nobody could check it touched this
/// change" is unproven, and the score is where the system makes its claim, so
/// the honest answer costs a ranking position rather than a model call.
/// Flipping `require_diff_coverage` on additionally turns it into an
/// escalation.
#[tokio::test]
async fn an_unmeasurable_overlap_is_scored_unproven_without_costing_a_verifier_call() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let probe = ScriptedCoverage::unavailable();
    let (outcome, events) = run_with_coverage(&probe, false, &provider).await;

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed, "an unproven run is not a failed run");
    assert!(
        !stages(&events).contains(&StageKind::Verdict),
        "being honest about an unmeasured overlap must not cost a reviewer"
    );
    assert_eq!(
        outcome.score,
        Some(crate::candidate::CandidateScore::Unverified),
        "an unmeasured overlap may not wear the ladder's strongest badge"
    );
    assert!(
        verdict.summary.starts_with("UNPROVEN"),
        "the verdict must LEAD with the finding, not bury it: {}",
        verdict.summary
    );
    assert!(
        verdict.summary.contains("diff_coverage=unmeasured"),
        "and must say which of the three answers it is: {}",
        verdict.summary
    );
    assert_eq!(
        verdict
            .ladder
            .as_deref()
            .expect("snapshot")
            .diff_coverage
            .as_deref(),
        Some("unmeasured")
    );

    // Same run, strict: the operator asked for the overlap to be proven, so
    // the unmeasured answer escalates instead of crediting.
    let strict_provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS — no coverage tool here, but the change reads right"),
    ]);
    let strict_probe = ScriptedCoverage::unavailable();
    let (strict_outcome, strict_events) =
        run_with_coverage(&strict_probe, true, &strict_provider).await;
    assert!(
        stages(&strict_events).contains(&StageKind::Verdict),
        "strict mode turns 'unmeasured' into an escalation"
    );
    let strict_verdict = strict_outcome.verdict.expect("a verdict was produced");
    assert!(
        strict_verdict.passed,
        "strictness must not convert 'unproven' into 'wrong'"
    );
}

/// The control for the downgrade above: when coverage IS measured and the
/// test did run the changed lines, the deterministic badge is earned in full.
/// Without this, "score unproven when unmeasured" would be indistinguishable
/// from "never score a deterministic pass again".
#[tokio::test]
async fn a_measured_overlap_still_earns_the_deterministic_badge() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    // Line 12 is the line the shared diff adds; the suite executed it.
    let probe = ScriptedCoverage::measuring(&[("src/lib.rs", &[11, 12, 13])]);
    let (outcome, events) = run_with_coverage(&probe, false, &provider).await;

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed && verdict.deterministic);
    assert!(!stages(&events).contains(&StageKind::Verdict));
    assert_eq!(
        outcome.score,
        Some(crate::candidate::CandidateScore::DeterministicPass),
        "a proven overlap is the ladder's strongest result, unchanged"
    );
    assert!(
        !verdict.summary.starts_with("UNPROVEN"),
        "a proven run must not be labelled unproven: {}",
        verdict.summary
    );
    assert_eq!(
        verdict
            .ladder
            .as_deref()
            .expect("snapshot")
            .diff_coverage
            .as_deref(),
        Some("covered")
    );
}

/// #1780: the distress trigger counts a candidate's deterministic failures
/// cumulatively — the two need not be consecutive. Here the candidate fails
/// deterministically, then takes a *different* path (the suite times out →
/// inconclusive → model verifier, which FAILs without touching the
/// deterministic-failure ledger), then fails deterministically again — and
/// that second red still buys the guidance call. This pins current behavior,
/// which is deliberate (#868 chose the cumulative ledger so a stuck worker is
/// steered early): the docs used to promise "second *consecutive*", and this
/// test is what keeps the code and the docs telling the same story.
#[tokio::test]
async fn a_second_deterministic_failure_fires_guidance_even_when_not_consecutive() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),                                     // worker
        text_result("first fix"), // revision 1 (deterministic red #1, no guidance)
        text_result("FAIL — the fix does not address the goal"), // model verifier (timeout round)
        text_result("second fix"), // revision 2 (verifier FAIL: not on the ledger)
        text_result("You are patching the symptom; fix the parser instead."), // guidance
        text_result("third fix"), // revision 3 (carries guidance)
    ]);
    let resolver = OneProvider(&provider);
    // Baseline red (the oracle arms), post-execute red → deterministic
    // failure #1 → revise on raw evidence; post-revision-1 TIMES OUT →
    // inconclusive, so the verifier is bought and FAILs → revise (nothing
    // recorded on the deterministic-failure ledger); post-revision-2 red →
    // deterministic failure #2 → distress guidance rides with revision 3;
    // post-revision-3 red → revisions exhausted → deterministic failed verdict.
    let runner = ScriptedRunner::scripted(
        vec![
            TestScript::Fail,
            TestScript::Fail,
            TestScript::TimeOut,
            TestScript::Fail,
            TestScript::Fail,
        ],
        "@@ -1 +1 @@\n-a\n+b",
    );
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
        // Room for the revision that carries the guidance: two revisions are
        // already spent before the second deterministic red lands.
        max_revisions: 3,
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let verdict = outcome.verdict.expect("verified");
    assert!(!verdict.passed);
    assert!(verdict.deterministic, "red tests are a deterministic fail");
    assert_eq!(outcome.revisions, 3);

    // The guidance text reached the worker's revision prompt after the SECOND
    // deterministic red, even though a verifier-FAIL round separated it from
    // the first — the ledger is cumulative, not a consecutiveness filter.
    let carried = messages.iter().any(|m| {
        m.content.contains("Independent reviewer course-correction")
            && m.content.contains("fix the parser instead")
    });
    assert!(
        carried,
        "guidance rides with the revision after the second deterministic red"
    );
    assert!(
        stages(&drain(&mut rx)).contains(&StageKind::Verdict),
        "the guidance call is an honest Verifier stage in the stream"
    );
}

/// #1787 witness, fan-out half: TWO isolated candidates escalate to a
/// verifier whose replies never carry a verdict token, and the stream records
/// one structured [`ProofStep::VerdictDegraded`] fact per candidate — the
/// once-per-run prose warning cannot say which candidates the heuristic
/// judged, and before the fact existed nothing did.
#[tokio::test]
async fn a_two_candidate_fanout_records_which_candidates_degraded() {
    // triage, then per candidate one worker turn and one verifier escalation.
    // Every post-triage reply is deliberately tokenless: candidates run
    // concurrently, so whichever call pops which reply, no verifier can parse
    // a verdict out of it — the degradation is scripted independent of
    // completion order.
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("worked on it"),
        text_result("Here is my assessment of the change."),
        text_result("worked on it"),
        text_result("Here is my assessment of the change."),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![
            Ok(FakeWorkspace::new(0, vec![], Ok(vec![]), log.clone())),
            Ok(FakeWorkspace::new(1, vec![], Ok(vec![]), log.clone())),
        ],
        log,
    );
    // No test command and no witness author resolvable, so each candidate's
    // ladder is inconclusive over its diff and escalates to the verifier.
    let config = PipelineConfig {
        candidates: Some(2),
        max_revisions: 0,
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        distress_guidance: false,
        ..PipelineConfig::default()
    };

    let (outcome, events, _messages) = run_isolated(&provider, &port, config, "Fix the bug").await;
    outcome.expect("the run proceeds to a (failed) verdict");

    let mut degraded: Vec<u32> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Proof {
                step: ProofStep::VerdictDegraded { candidate, .. },
            } => Some(*candidate),
            _ => None,
        })
        .collect();
    degraded.sort_unstable();
    assert_eq!(
        degraded,
        vec![1, 2],
        "each candidate's degradation is recorded, keyed by ordinal"
    );
    let warnings = events
        .iter()
        .filter(|event| {
            matches!(event, AgentEvent::Error { message, .. }
                if message.contains("falls back to a deterministic heuristic"))
        })
        .count();
    assert_eq!(
        warnings, 1,
        "the transcript warning stays once per run; the per-candidate record is the proof step"
    );
}

/// #1787 witness, dedup half: ONE candidate whose escalation hits the same
/// non-compliant verifier on the first round AND on the revision records its
/// degradation fact exactly once — per candidate, not per round (the ladder
/// rung already counts rounds).
#[tokio::test]
async fn a_candidate_degrading_on_every_round_records_one_fact() {
    // triage; worker; tokenless verdict; revision turn; tokenless verdict.
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("Here is my assessment of the change."),
        text_result("revised"),
        text_result("Here is my assessment of the change."),
    ]);
    let resolver = OneProvider(&provider);
    // Red baseline, then a timed-out observation on every round: no flip, no
    // touched-test result, a diff — the ladder escalates each time.
    let runner = ScriptedRunner::scripted(
        vec![TestScript::Fail, TestScript::TimeOut, TestScript::TimeOut],
        "@@ -1 +1 @@\n-old\n+new",
    );
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = SeqRepoStatus::new(vec![vec![], vec![]]);
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        max_revisions: 1,
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("the run proceeds to a (failed) verdict");
    assert!(
        matches!(outcome.status, PipelineStatus::VerificationFailed { .. }),
        "both rounds degraded to the failing heuristic: {:?}",
        outcome.status
    );

    // Both scripted verdict calls were really made — without this, a run that
    // never reached the second escalation would pass the one-fact assertion
    // below for the wrong reason.
    assert_eq!(
        provider.prompts().len(),
        5,
        "triage, worker, verdict, revision, verdict"
    );

    let events = drain(&mut rx);
    let facts: Vec<u32> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Proof {
                step: ProofStep::VerdictDegraded { candidate, .. },
            } => Some(*candidate),
            _ => None,
        })
        .collect();
    assert_eq!(
        facts,
        vec![1],
        "two degraded rounds, one candidate, one fact"
    );
}
