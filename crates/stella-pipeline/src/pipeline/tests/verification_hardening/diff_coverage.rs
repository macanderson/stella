//! The #1291 diff-coverage half of the verification hardening series: does
//! the authored witness actually execute the lines the change touched?
//! Split out of the parent module to keep it under the 1500-line ratchet.
//! The scripted probe used to live here beside the only tests that scripted
//! it; `guard_wiring` is the second scripter, so it moved up to the parent
//! where [`super::ScriptedMutation`] already sits.

use super::*;

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
        !stages(&events).contains(&StageKind::Verdict),
        "a coincidental pass is withheld, not fast-submitted — and withheld is the end of it"
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
    assert!(
        verdict.passed,
        "a coincidental pass is not a failure — nothing found a defect"
    );
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
    assert!(
        verdict
            .summary
            .contains("NOT a finding that the work is absent or wrong"),
        "an unproven overlap must SAY it is unproven rather than leaving a reader to \
         infer a defect from a missing badge: {}",
        verdict.summary
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

    // The instruction block is byte-equal whatever the call was about — the
    // property the cache machinery depends on. Asserted against a reference
    // built from unrelated inputs, so a builder that leaked any per-call text
    // into the system block fails here.
    let reference = crate::triage::triage_prompt("an unrelated task", "an unrelated structure");
    assert_eq!(
        triage[0].1, reference.instructions,
        "the system block is the input-independent instruction constant — \
         byte-equal whatever the call was about"
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
        !stages(&strict_events).contains(&StageKind::Verdict),
        "strict mode turns 'unmeasured' into a withheld credit, not a bought opinion"
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
