//! End-to-end scenarios for the verification hardening series: typed
//! infra outcomes (#860), the confirmation run on flip (#859), the
//! diagnostics regression veto (#861), and the cumulative distress-guidance
//! trigger (#1780). Each drives the real pipeline over scripted ports — no
//! API key, no subprocess — and pins the decision the ladder must reach.

use super::*;
use crate::LineMutation;

/// The authored-witness arming of the mid-turn flip halt (#1793) — a child
/// rather than a sibling module so it reaches the shared fakes through this
/// file's own `use super::*`, and so the already-oversized `tests.rs` does
/// not grow another module declaration. The shell doubles the halt needs live
/// in that module, beside the only tests that script them.
mod flip_halt_arming;

/// The witness repair wall-clock bound (#2141) — a child module for the same
/// two reasons `flip_halt_arming` is: it reaches the shared fakes through
/// this file's own `use super::*`, and the already-oversized `tests.rs` does
/// not grow another module declaration. The stalling provider it needs lives
/// there, beside the only test that scripts it.
mod witness_repair_bound;

/// The untracked-diff read bound (#2110) — a child module for the same two
/// reasons `flip_halt_arming` is: it reaches the shared fakes through this
/// file's own `use super::*`, and the already-oversized `tests.rs` does not
/// grow another module declaration. The counting diagnostic runner it needs
/// lives there, beside the only test that scripts it.
mod untracked_render_bound;

/// The #1291 diff-coverage scenarios — a child module for the same two
/// reasons `flip_halt_arming` is: it reaches the shared fakes through this
/// file's own `use super::*`, and the already-oversized `tests.rs` does not
/// grow another module declaration. The scripted coverage probe lives there,
/// beside the only tests that script it.
mod diff_coverage;

/// The wiring guard for the two mechanical checks on the deterministic rung
/// (#2607) — a child module for the same two reasons `flip_halt_arming` is,
/// and because it is the only place both probes are wired on one run. Its
/// module doc states what it is protecting and why a verdict assertion alone
/// cannot.
mod guard_wiring;

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
        !stages(&events).contains(&StageKind::Verdict),
        "no flip evidence exists, so the ladder abstains — it does not buy an opinion about evidence it already knows is missing"
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
        !stages(&events).contains(&StageKind::Verdict),
        "an unobservable suite is inconclusive, and inconclusive is now the answer: no verdict call, and deliberately not a revision either"
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
        !stages(&events).contains(&StageKind::Verdict),
        "the unconfirmed flip withholds the deterministic credit and stops there — a model cannot confirm what a re-run could not"
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
        !stages(&events).contains(&StageKind::Verdict),
        "the regression veto withholds the fast-submit and stops there: the fresh error is the finding, and no opinion is bought about it"
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

/// A scripted coverage probe (#1291): answers with a fixed report, or `None`
/// for "no tooling could measure this", and counts the runs it was asked for.
///
/// Lives beside [`ScriptedMutation`] rather than in `diff_coverage` because
/// two child modules script it — `guard_wiring` wires both probes on one run
/// to pin that neither guard has stopped being fed (#2607).
pub(super) struct ScriptedCoverage {
    report: Option<crate::verify::coverage::CoverageReport>,
    calls: std::sync::atomic::AtomicU32,
}

impl ScriptedCoverage {
    pub(super) fn measuring(entries: &[(&str, &[u32])]) -> Self {
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
    pub(super) fn unavailable() -> Self {
        Self {
            report: None,
            calls: std::sync::atomic::AtomicU32::new(0),
        }
    }
    pub(super) fn calls(&self) -> u32 {
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
        !stages(&events).contains(&StageKind::Verdict),
        "the downgrade withholds the deterministic credit and stops there"
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
        !stages(&events).contains(&StageKind::Verdict),
        "an unobservable suite is inconclusive, and inconclusive is the answer: no \
         verdict call, and deliberately not a revision either"
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
