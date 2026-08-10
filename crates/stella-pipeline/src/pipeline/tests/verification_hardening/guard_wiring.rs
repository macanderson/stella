//! The two mechanical guards on the deterministic rung must stay **fed**
//! (#2607) — a child module for the same two reasons `flip_halt_arming` is,
//! and for one of its own.
//!
//! Verification's deterministic ladder has exactly two checks that stop a
//! *trivially-passing* test from earning the strongest badge the system
//! issues: the tautology audit (#870 — a witness that kills no mutant is not
//! proof) and the diff-coverage overlap (#1291 — a run that executed none of
//! the changed lines is not proof of those lines). While a model verdict sat
//! behind them they were belt-and-braces; as judgement stops buying a model
//! call, they become the only thing between a test nobody vetted and a
//! `deterministic` pass.
//!
//! Both are plumbed as **fields on an inputs struct**, and both defaults mean
//! "no problem found". So deleting the code that computes them is not a
//! compile error and not a test failure — it is a silent downgrade to
//! "everything is fine". #2607 is the record of a branch that did exactly
//! that: `witness_tautological` survived as the literal `false`,
//! `diff_coverage` never left `Unmeasured`, both guard modules stayed in the
//! tree with no non-test caller, and nothing went red.
//!
//! Every test here therefore asserts on the *feeding*, not only on the
//! verdict: that the probe was called, and that the ladder snapshot carries a
//! **measured** value rather than the benign default. Those two assertions
//! are what a deletion cannot satisfy.
//!
//! Two tests rather than the one the shape suggests, because the guards run
//! in sequence under the same `SubmitFast` precondition: coverage is checked
//! first, and a withheld credit there means the mutation audit is never
//! reached (`ordering_leaves_the_mutation_audit_unpaid_for` pins that). A
//! single scenario that is bad on both axes would prove only the first guard
//! is wired — so each guard gets a scenario where it is the *sole* reason the
//! credit is withheld.

use super::*;

/// The lines `MUTABLE_DIFF` adds to `src/fix.rs`, on the new side: the hunk
/// starts at 1 and both added lines are ones a coverage tool attributes.
const CHANGED_LINES: &[u32] = &[1, 2];

/// The candidate's changed file — the path both guards read out of the diff.
const CHANGED_FILE: &str = "src/fix.rs";

/// One authored-witness run over both probes: an isolated candidate whose
/// suite goes fail→pass and whose diff is `MUTABLE_DIFF`, so the mutation
/// generator has two mutable lines and the coverage overlap has two changed
/// ones.
///
/// The mutation audit only ever runs for an AUTHORED witness, which is why
/// this is the isolated-workspace path rather than the `--test-command` one
/// `diff_coverage` uses.
async fn run_with_both_guards(
    mutation: &dyn MutationProbe,
    coverage: &dyn crate::ports::CoverageProbe,
    provider: &ScriptedProvider,
) -> (PipelineOutcome, Vec<AgentEvent>) {
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

    let resolver = OneProvider(provider);
    let diagnostics = NeverRunner;
    let repo_status = NeverRepoStatus;
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
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
            diagnostics: &diagnostics,
            tests: &diagnostics,
            lint: None,
            mutation: Some(mutation),
            coverage: Some(coverage),
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: Some(&port),
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the retry bug", &mut messages, &mut budget)
        .await
        .expect("run succeeds");
    (outcome, drain(&mut rx))
}

/// The provider script every scenario here shares: triage → worker →
/// witness author. No fourth result is needed — the withheld credit is
/// terminal (`LadderDecision::Unverified` asks nobody), which the tests
/// assert directly.
fn scripted_roles() -> ScriptedProvider {
    ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("wrote the test.\nTEST_COMMAND: cargo test --test witness witness -- --exact"),
    ])
}

/// The tautology guard, alone on the wire (#2607). Coverage is measured and
/// **overlapping**, so it credits the pass and the mutation audit is the only
/// thing that can withhold it: a witness that stays green under every mutant
/// of the changed lines must not fast-submit.
///
/// The two assertions a silent unwiring cannot satisfy are the probe's call
/// count and the snapshot's `witness_mutation`. Reading `None` there — the
/// benign default — is precisely the state this test exists to catch, and it
/// is a distinguishable value only because the ladder input is tri-state.
#[tokio::test]
async fn the_tautology_guard_is_fed_and_withholds_the_deterministic_credit() {
    let provider = scripted_roles();
    let mutation = ScriptedMutation::new(MutantOutcome::Witness { passed: true });
    let coverage = ScriptedCoverage::measuring(&[(CHANGED_FILE, CHANGED_LINES)]);
    let (outcome, events) = run_with_both_guards(&mutation, &coverage, &provider).await;

    assert_eq!(
        coverage.calls(),
        1,
        "the coverage guard ran — this scenario is not the one testing it, but an \
         unfed coverage probe would make the mutation assertions below vacuous"
    );
    assert!(
        mutation.calls() > 0,
        "THE guard assertion: the mutation audit was fed. A pipeline that stopped \
         calling it reaches this line with 0 and a `witness_tautological`-shaped \
         default that reads as innocence"
    );

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.passed,
        "a tautological witness is not a finding that the change is wrong"
    );
    assert!(
        !verdict.deterministic,
        "what it loses is the deterministic credit: {}",
        verdict.summary
    );
    assert!(
        !stages(&events).contains(&StageKind::Verdict),
        "the withheld credit is terminal — no model is asked to overrule it"
    );
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(
        snapshot.witness_mutation,
        Some(false),
        "the provenance must record a MEASURED tautology; `None` here is the \
         unfed-guard state, not a finding"
    );
    assert_eq!(
        snapshot.diff_coverage.as_deref(),
        Some("covered"),
        "and the other guard measured, so the withheld credit is attributable"
    );
    assert!(
        verdict.summary.contains("witness_mutation=tautological"),
        "a withheld credit must NAME the guard that withheld it — a reader who has \
         only the verdict must not have to open a snapshot to learn which of the two \
         objected: {}",
        verdict.summary
    );
}

/// The coverage guard, alone on the wire (#2607). The witness WOULD kill a
/// mutant — it constrains the change — so the measured non-overlap is the
/// only thing that can withhold the credit.
///
/// It also pins the ordering the module doc turns on: coverage is checked
/// first and its finding drops the `SubmitFast` precondition, so the mutation
/// audit is never reached. That is deliberate (one instrumented run is
/// cheaper than up to three witness runs) and it is why the tautology guard
/// needs its own scenario above rather than sharing this one.
#[tokio::test]
async fn the_coverage_guard_is_fed_and_ordering_leaves_the_mutation_audit_unpaid_for() {
    let provider = scripted_roles();
    let mutation = ScriptedMutation::new(MutantOutcome::Witness { passed: false });
    // The suite ran, and it ran OTHER lines of the changed file.
    let coverage = ScriptedCoverage::measuring(&[(CHANGED_FILE, &[40, 41])]);
    let (outcome, events) = run_with_both_guards(&mutation, &coverage, &provider).await;

    assert_eq!(
        coverage.calls(),
        1,
        "THE guard assertion: the coverage audit was fed. A pipeline that stopped \
         calling it reaches this line with 0 and a `DiffCoverage::Unmeasured` that \
         credits the pass"
    );
    assert_eq!(
        mutation.calls(),
        0,
        "ordering: a credit already withheld makes the dearer audit moot, so it is \
         not paid for"
    );

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(verdict.passed, "unproven is not failed");
    assert!(!verdict.deterministic, "{}", verdict.summary);
    assert!(!stages(&events).contains(&StageKind::Verdict));
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(
        snapshot.diff_coverage.as_deref(),
        Some("not_covered"),
        "a MEASURED non-overlap; `unmeasured` here is the unfed-guard state"
    );
    assert_eq!(
        snapshot.witness_mutation, None,
        "and the audit that never ran says so, rather than reporting the \
         innocence a `false` used to imply (#2607)"
    );
    assert!(
        verdict.summary.contains("diff_coverage=not_covered"),
        "the other guard's withheld credit must name ITSELF, not the generic \
         'the deterministic checks did not combine into a pass' the two used to \
         share: {}",
        verdict.summary
    );
}

/// The control both scenarios above need. Without it, "withhold when a guard
/// objects" is indistinguishable from "never credit a deterministic pass
/// again" — the failure mode that would make this module pass while the
/// ladder was broken in the opposite direction.
///
/// Same run, same probes, both guards satisfied: the witness kills the first
/// mutant and the suite executed the changed lines, so the strongest badge is
/// earned in full and both findings are on the record.
#[tokio::test]
async fn both_guards_satisfied_still_earns_the_deterministic_badge() {
    let provider = scripted_roles();
    let mutation = ScriptedMutation::new(MutantOutcome::Witness { passed: false });
    let coverage = ScriptedCoverage::measuring(&[(CHANGED_FILE, CHANGED_LINES)]);
    let (outcome, events) = run_with_both_guards(&mutation, &coverage, &provider).await;

    assert_eq!(coverage.calls(), 1);
    assert_eq!(
        mutation.calls(),
        1,
        "the first killed mutant proves the witness; the second is never paid for"
    );

    let verdict = outcome.verdict.expect("a verdict was produced");
    assert!(
        verdict.passed && verdict.deterministic,
        "{}",
        verdict.summary
    );
    assert!(!stages(&events).contains(&StageKind::Verdict));
    assert_eq!(
        outcome.score,
        Some(crate::candidate::CandidateScore::DeterministicPass),
        "two measured guards that both cleared the candidate is the ladder's \
         strongest result, unchanged"
    );
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries its snapshot");
    assert_eq!(snapshot.witness_mutation, Some(true));
    assert_eq!(snapshot.diff_coverage.as_deref(), Some("covered"));
}

/// The unwired shape stated as a value, so the difference this module guards
/// is written down in one place a reader can check without running a
/// pipeline: the state a deleted audit leaves behind is the state that
/// credits a pass, on BOTH axes. That is why the tests above assert on the
/// call counts and the snapshot rather than on the verdict alone — a verdict
/// assertion cannot tell an unfed guard from a satisfied one.
#[test]
fn the_unfed_state_of_both_guards_is_the_state_that_credits_a_pass() {
    use crate::verify::coverage::DiffCoverage;
    use crate::verify::mutation::MutationAudit;

    assert!(MutationAudit::default().credits_a_deterministic_pass());
    assert!(DiffCoverage::default().credits_a_deterministic_pass(false));
    // ...and each is distinguishable from the audit that ran and cleared the
    // candidate, which is the whole of #2607's second half.
    assert_ne!(MutationAudit::default(), MutationAudit::Constrains);
    assert_ne!(DiffCoverage::default(), DiffCoverage::Covered);
    assert_eq!(MutationAudit::default().recorded(), None);
}
