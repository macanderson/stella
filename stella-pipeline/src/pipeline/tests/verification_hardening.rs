//! End-to-end scenarios for the verification hardening series: typed
//! infra outcomes (#860), the confirmation run on flip (#859), and the
//! diagnostics regression veto (#861). Each drives the real pipeline
//! over scripted ports — no API key, no subprocess — and pins the
//! decision the ladder must reach.

use super::*;
use crate::LineMutation;

/// #860 acceptance: a baseline that TIMES OUT observed no failing assertion,
/// so a candidate whose suite then passes has no fail→pass flip — the run
/// must escalate to the model judge, never credit `DeterministicPass`. Before
/// the typed outcome, the timeout's non-zero exit locked the oracle onto a
/// phantom `Failing` and the faster candidate "flipped" it.
#[tokio::test]
async fn a_timed_out_baseline_never_manufactures_a_flip() {
    // triage → "single"; worker → final text; judge → verdict (the ladder
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
        stages(&events).contains(&StageKind::Judge),
        "no flip evidence exists, so the ladder must escalate to the judge"
    );
}

/// #860 acceptance, candidate side: a suite that times out AFTER the change
/// is inconclusive (judge), not a deterministic red (revise). Spending a
/// revision turn "fixing" a timeout the change may not have caused burned a
/// full worker call on infra noise; the judge sees `test_run=timed_out` and
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
        stages(&events).contains(&StageKind::Judge),
        "an unobservable suite is inconclusive — judge, not revise"
    );
    // No deterministic red verdict may be emitted for infra noise: every
    // deterministic JudgeVerdict{passed:false} is the revise path's badge.
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::JudgeVerdict {
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
/// escalates to the judge with `unstable_flip=true` in its evidence.
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
        stages(&events).contains(&StageKind::Judge),
        "the unconfirmed flip escalates to the judge instead of fast-submitting"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::JudgeVerdict {
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
/// the judge with the diagnostics delta in evidence. Lint stays excluded
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
        stages(&events).contains(&StageKind::Judge),
        "the regression veto must escalate to the judge"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::JudgeVerdict {
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
    let why = crate::replay::verdict_provenance(&stella_protocol::JudgeEvidence {
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
    assert!(!stages(&events).contains(&StageKind::Judge));
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
/// deterministic credit is withheld and the judge decides with
/// `witness_tautological=true` in evidence.
#[tokio::test]
async fn a_tautological_witness_is_downgraded_to_the_judge() {
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
        stages(&events).contains(&StageKind::Judge),
        "the downgrade escalates to the judge"
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
    assert!(!stages(&events).contains(&StageKind::Judge));
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(snapshot.witness_mutation, Some(true));
}
