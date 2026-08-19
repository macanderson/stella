//! The calibration fold's tests: the pass cohorts, the uncorroborated rate an
//! evidence-demand policy is set from, the grader partition, and the late
//! reconciliation that reaches a verdict after its session ended.
//!
//! A child module so `stands_alone` — private, and deliberately so — stays
//! reachable, in its own file so neither it nor `calibration.rs` approaches the
//! file-size ceiling.

use super::ground_truth::{GroundTruth, reconcile};
use super::*;
use stella_protocol::{FlipOutcome, LadderRung, PrStatus, VerdictEvidence};

/// The in-stream reading alone, for the tests that do not exercise the late
/// path. `calibration` returns the unsettled passes beside the report (#1293);
/// most assertions here are about the report.
fn fold(events: &[AgentEvent]) -> CalibrationReport {
    calibration(events).0
}

fn verdict(passed: bool, deterministic: bool) -> AgentEvent {
    AgentEvent::Verdict {
        passed,
        evidence: VerdictEvidence {
            summary: String::new(),
            deterministic,
            evidence_refs: vec![],
            ladder: None,
        },
    }
}

fn ci(status: CiStatus) -> AgentEvent {
    pr("https://example.test/pr/1", Some(status))
}

fn pr(url: &str, ci: Option<CiStatus>) -> AgentEvent {
    AgentEvent::Pr {
        url: url.into(),
        status: PrStatus::Open,
        number: Some(1),
        ci,
    }
}

fn commit(sha: &str) -> AgentEvent {
    AgentEvent::Commit {
        sha: sha.into(),
        message: "feat: the work".into(),
    }
}

/// A snapshot observing nothing, for tests that set only the channels they
/// exercise. The diff fields are non-zero because "the tree changed" is never
/// corroboration here — a literal that looked all-dark would suggest it might
/// be.
///
/// Written out field by field rather than spread from a `Default`: a channel
/// added to [`LadderSnapshot`] must make someone here decide whether
/// [`stands_alone`] reads it, and a compile error is the only way to force that
/// question.
fn bare_snapshot() -> LadderSnapshot {
    LadderSnapshot {
        rung: None,
        tracked_command: None,
        oracle_trace: vec![],
        flip: FlipOutcome::NotAchieved,
        unstable_flip: false,
        flip_refused_different_failure: false,
        touched_tests_passed: None,
        test_infra: None,
        diff_lines: 4,
        diff_budget: 400,
        diff_available: true,
        mutating_actions: 2,
        new_diag_errors: 0,
        new_diag_warnings: 0,
        witness_intact: None,
        witness_mutation: None,
        diff_coverage: None,
        verify_done_flip: false,
        no_test_surface: false,
        errored_commands: 0,
        verifier_independent: None,
    }
}

/// A verdict carrying its ladder snapshot, so the uncorroborated fold has
/// something to read. `corroborated` decides whether the recorded turn had a
/// flip behind it.
fn snapshotted(passed: bool, deterministic: bool, corroborated: bool) -> AgentEvent {
    let snapshot = LadderSnapshot {
        flip: if corroborated {
            FlipOutcome::Achieved
        } else {
            FlipOutcome::NotAchieved
        },
        touched_tests_passed: corroborated.then_some(true),
        ..bare_snapshot()
    };
    AgentEvent::Verdict {
        passed,
        evidence: VerdictEvidence {
            summary: String::new(),
            deterministic,
            evidence_refs: vec![],
            ladder: Some(Box::new(snapshot)),
        },
    }
}

/// An unproven PASS carrying the grader fact under test; `None` with
/// `snapshot: false` is a verdict with no ladder snapshot at all — the pre-#865
/// shape.
fn graded_pass(grader_independent: Option<bool>, snapshot: bool) -> AgentEvent {
    let ladder = snapshot.then(|| {
        Box::new(LadderSnapshot {
            verifier_independent: grader_independent,
            ..bare_snapshot()
        })
    });
    AgentEvent::Verdict {
        passed: true,
        evidence: VerdictEvidence {
            summary: String::new(),
            deterministic: false,
            evidence_refs: vec![],
            ladder,
        },
    }
}

/// The #871 acceptance: an unproven pass that later fails CI is recorded as a
/// false positive; the deterministic cohort reconciles beside it.
#[test]
fn an_unproven_pass_that_fails_ci_is_a_false_positive() {
    let events = vec![
        verdict(true, false), // unproven PASS
        verdict(true, true),  // deterministic pass
        ci(CiStatus::Failing),
    ];
    let report = fold(&events);
    assert_eq!(report.unproven_passes, 1);
    assert_eq!(report.unproven_reconciled, 1);
    assert_eq!(report.unproven_false_positives, 1);
    assert_eq!(report.unproven_false_positive_rate(), Some(1.0));
    assert_eq!(report.deterministic_false_positives, 1);
}

/// Pending/Running reconcile nothing, and passes after the last terminal CI
/// verdict stay unreconciled — an unmeasured rate reads `None`.
#[test]
fn non_terminal_ci_and_trailing_passes_stay_unreconciled() {
    let events = vec![
        verdict(true, false),
        ci(CiStatus::Running),
        ci(CiStatus::Passing),
        verdict(true, false), // after the last terminal verdict
    ];
    let report = fold(&events);
    assert_eq!(report.unproven_passes, 2);
    assert_eq!(
        report.unproven_reconciled, 1,
        "only the pass before the terminal verdict was settled in-stream"
    );
    assert_eq!(report.unproven_false_positives, 0);
    assert_eq!(report.unproven_false_positive_rate(), Some(0.0));

    let unmeasured = fold(&[verdict(true, false)]);
    assert_eq!(unmeasured.unproven_false_positive_rate(), None);
}

/// Failed verdicts and revise-loop reds never enter the pass tallies — the
/// question is the false-POSITIVE rate of passes — but they are still observed,
/// because "no verdict was recorded" is a different fact from "no verdict
/// passed" (#3866).
#[test]
fn failed_verdicts_are_observed_but_not_tallied_as_passes() {
    let events = vec![
        verdict(false, true),
        verdict(false, false),
        ci(CiStatus::Passing),
    ];
    let report = fold(&events);
    assert_eq!(report.unproven_passes, 0);
    assert_eq!(report.deterministic_passes, 0);
    assert_eq!(report.verdicts_observed, 2);
}

#[test]
fn merge_adds_componentwise() {
    let a = fold(&[verdict(true, false), ci(CiStatus::Failing)]);
    let b = fold(&[verdict(true, false), ci(CiStatus::Passing)]);
    let merged = a.merge(b);
    assert_eq!(merged.verdicts_observed, 2);
    assert_eq!(merged.unproven_passes, 2);
    assert_eq!(merged.unproven_reconciled, 2);
    assert_eq!(merged.unproven_false_positives, 1);
}

/// #1295: the rate an evidence-demand policy is set from is measured off what
/// is already recorded — every snapshotted verdict counts toward the
/// denominator, and an unproven PASS with nothing behind it is separately
/// identified as a turn such a policy would have sent back.
#[test]
fn the_uncorroborated_rate_is_measured_from_recorded_snapshots() {
    let events = vec![
        snapshotted(true, false, false),  // unproven PASS, nothing behind it
        snapshotted(true, true, true),    // deterministic pass, flip achieved
        snapshotted(false, false, false), // a FAILING verdict, also uncorroborated
        snapshotted(true, false, true),   // unproven PASS with a flip behind it
    ];
    let report = fold(&events);
    assert_eq!(report.snapshotted_verdicts, 4);
    assert_eq!(
        report.uncorroborated_verdicts, 2,
        "the denominator is every verdict, not only the passing ones — the question is how \
         often the CONDITION holds"
    );
    assert_eq!(report.uncorroborated_rate(), Some(0.5));
    assert_eq!(
        report.unproven_passes_standing_alone, 1,
        "only the unproven PASS with no flip and no green test would be sent back"
    );
    // The pass cohorts are untouched by this fold.
    assert_eq!(report.unproven_passes, 2);
    assert_eq!(report.deterministic_passes, 1);
}

/// #2194, restated for a workspace where the live predicate no longer exists
/// (#3866): every combination of the three channels [`stands_alone`] reads,
/// through the wire, against the same rule written the other way round.
///
/// This is the assertion whose absence let the two copies drift the first time.
/// `stands_alone` restated `LadderInputs::verifier_pass_stands_alone` in two
/// conjuncts; #2191 taught the live predicate a third (`verify_done_flip`) and
/// the copy stayed at two, so `uncorroborated_verdicts` over-counted every
/// `verify_done`-rescued turn. That predicate left the tree with
/// `stella-pipeline` (#3865), so there is nothing to agree WITH any more — what
/// is pinned instead is the definition itself, expressed as a disjunction of
/// positives here against the conjunction of negatives there, so dropping a
/// conjunct fails rather than silently widening the tally.
///
/// The JSON round trip is not decoration: a replay reader meets a snapshot off
/// disk, not from memory, so a channel that stops surviving serialization
/// reproduces the same drift wearing a `Default` instead of a missing conjunct.
#[test]
fn stands_alone_reads_exactly_three_channels_and_they_survive_the_wire() {
    for flip in [
        FlipOutcome::Unobserved,
        FlipOutcome::NotAchieved,
        FlipOutcome::Achieved,
    ] {
        for touched_tests_passed in [None, Some(false), Some(true)] {
            for verify_done_flip in [false, true] {
                let snapshot = LadderSnapshot {
                    flip,
                    touched_tests_passed,
                    verify_done_flip,
                    ..bare_snapshot()
                };
                let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
                let recorded: LadderSnapshot =
                    serde_json::from_str(&json).expect("snapshot round-trips");
                let any_corroboration = flip == FlipOutcome::Achieved
                    || touched_tests_passed == Some(true)
                    || verify_done_flip;
                assert_eq!(
                    stands_alone(&recorded),
                    !any_corroboration,
                    "corroboration misread at flip={flip:?} \
                     touched={touched_tests_passed:?} verify_done={verify_done_flip}"
                );
            }
        }
    }
}

/// The consequence, folded: a verdict rescued by a worker-run `verify_done`
/// confirmation is corroborated, and must not land in the uncorroborated tally.
#[test]
fn a_verify_done_rescued_verdict_is_not_counted_uncorroborated() {
    let rescued = AgentEvent::Verdict {
        passed: true,
        evidence: VerdictEvidence {
            summary: "heuristic fallback passed on the worker's verify_done confirmation".into(),
            deterministic: false,
            evidence_refs: vec![],
            ladder: Some(Box::new(LadderSnapshot {
                verify_done_flip: true,
                ..bare_snapshot()
            })),
        },
    };
    let report = fold(&[rescued]);
    assert_eq!(report.snapshotted_verdicts, 1);
    assert_eq!(
        report.uncorroborated_verdicts, 0,
        "a confirmed verify_done flip is deterministic corroboration, exactly as the \
         engine weighed it at decision time"
    );
    assert_eq!(report.unproven_passes_standing_alone, 0);
}

/// A rate with no denominator is reported as unmeasured, never as 0% — the same
/// discipline the false-positive rates already keep. Verdicts recorded before
/// ladder snapshots existed (#865) contribute nothing rather than being assumed
/// corroborated.
#[test]
fn a_stream_without_snapshots_leaves_the_uncorroborated_rate_unmeasured() {
    let report = fold(&[verdict(true, false), verdict(false, true)]);
    assert_eq!(report.snapshotted_verdicts, 0);
    assert_eq!(report.uncorroborated_rate(), None);
    assert!(
        render_calibration(&report).contains("uncorroborated rate: unmeasured"),
        "an unmeasured rate must say so: {}",
        render_calibration(&report)
    );
}

/// #3866: a workspace that recorded no verdict at all must not read as a
/// verifier that never erred.
///
/// This is now the ordinary state — the built-in verification path was deleted
/// (#3865), so nothing emits a `Verdict` unless a verification plugin is
/// installed — which makes the all-zero render the single most likely thing
/// anyone sees. It says what it is instead.
#[test]
fn an_empty_record_reports_itself_as_unmeasured_not_as_perfect() {
    let rendered = render_calibration(&fold(&[commit("abc123abc123"), ci(CiStatus::Passing)]));
    assert!(
        rendered.contains("nothing to calibrate"),
        "an empty record must name itself: {rendered}"
    );
    assert!(
        rendered.contains("NOT a measured result"),
        "and must refuse to be read as a clean bill of health: {rendered}"
    );
    assert!(
        !rendered.contains("false-positive rate"),
        "a rate over nothing must not be printed at all: {rendered}"
    );
    assert!(
        rendered.contains("plugin"),
        "and must point at the only thing that produces a verdict now: {rendered}"
    );
}

/// #2635: the cohort names the property it selects on — a pass with no
/// deterministic evidence — and nothing in the output claims a model judged it.
///
/// The predicate `passed && !deterministic` was exact while the only way to set
/// it was a model verifier ruling on inconclusive evidence. #2584 removed that
/// call, and the ladder's own non-determinate outcomes set the same flag:
/// `unverified`, `unverifiable`, and a triage `waived`. So the cohort counts
/// unproven passes, no model was involved in any of them, and the old label was
/// a measurement artifact asserting a mechanism that no longer contributes to
/// it — which is worse than a missing number, because a consumer parsing
/// `--format json` read `verifier_*` and got something else.
#[test]
fn the_unproven_cohort_never_claims_a_model_verifier() {
    // A pass the ladder could not settle, carrying the rung that says so.
    let unsettled = AgentEvent::Verdict {
        passed: true,
        evidence: VerdictEvidence {
            summary: "UNVERIFIED: nothing deterministic settled this turn".into(),
            deterministic: false,
            evidence_refs: vec![],
            ladder: Some(Box::new(LadderSnapshot {
                rung: Some(LadderRung::Unverified),
                ..bare_snapshot()
            })),
        },
    };
    let report = fold(&[unsettled]);
    assert_eq!(
        report.unproven_passes, 1,
        "a pass nothing proved belongs to the unproven cohort"
    );

    let rendered = render_calibration(&report);
    for banned in ["model verifier", "model-verifier", "verifier calibration"] {
        assert!(
            !rendered.contains(banned),
            "the calibration output still names `{banned}`, a mechanism #2584 \
             removed and which contributes to none of these tallies:\n{rendered}"
        );
    }
    assert!(
        rendered.contains("unproven"),
        "the cohort must name what it selects on:\n{rendered}"
    );
}

// ---------------------------------------------------------------------------
// Late reconciliation (#1293): evidence that arrives after the session ended.
// ---------------------------------------------------------------------------

/// #1293 acceptance: a pass recorded after its session's last CI observation
/// used to be lost. It now leaves the fold carrying the commit it covers, and a
/// revert discovered later settles it.
#[test]
fn a_pass_after_the_last_ci_verdict_survives_its_session_and_a_revert_settles_it() {
    let events = vec![
        verdict(true, false),
        pr("https://example.test/pr/1", Some(CiStatus::Passing)),
        // Everything below is after the session's last terminal verdict — the
        // region that used to be unreachable.
        verdict(true, false),
        commit("abc123abc123"),
    ];
    let (mut report, pending) = calibration(&events);
    assert_eq!(report.unproven_passes, 2);
    assert_eq!(
        report.unproven_reconciled, 1,
        "only the first pass had an in-stream verdict"
    );
    assert_eq!(pending.len(), 1, "the trailing pass is carried out");
    assert_eq!(pending[0].commits, vec!["abc123abc123".to_string()]);

    let truth = GroundTruth::default().with_reverts(["abc123abc123".to_string()]);
    assert_eq!(reconcile(&mut report, &pending, &truth), 1);
    assert_eq!(report.unproven_reconciled, 2);
    assert_eq!(report.unproven_false_positives, 1);
    assert_eq!(report.unproven_reverted, 1);
    assert!(
        render_calibration(&report).contains("settled by a REVERT"),
        "the render must distinguish a human's revert from a red CI run: {}",
        render_calibration(&report)
    );
}

/// The cross-session half: session A's trailing pass is settled by a terminal
/// CI verdict recorded in session B, for the PR they share.
#[test]
fn a_terminal_verdict_in_a_later_session_settles_an_earlier_ones_pass() {
    let session_a = vec![verdict(true, false), pr("https://example.test/pr/4", None)];
    let session_b = vec![pr("https://example.test/pr/4", Some(CiStatus::Failing))];
    let (mut report, pending) = calibration(&session_a);
    assert_eq!(
        report.unproven_reconciled, 0,
        "a PR with no terminal verdict reconciles nothing in its own stream"
    );
    let truth = GroundTruth::default()
        .with_stream(&session_a)
        .with_stream(&session_b);
    assert_eq!(reconcile(&mut report, &pending, &truth), 1);
    assert_eq!(report.unproven_false_positives, 1);
    assert_eq!(
        report.unproven_reverted, 0,
        "CI is not a revert, and the two must not merge into one number"
    );
}

// ---------------------------------------------------------------------------
// Grader independence (#1865).
// ---------------------------------------------------------------------------

/// The #1865 acceptance: a scripted stream carrying both polarities (and an
/// unknown) folds each pass into exactly one cohort, and the in-stream CI
/// reconciliation settles each into its own cohort's rate — unknown counted
/// apart, never folded into either.
#[test]
fn the_fold_partitions_the_unproven_cohort_by_grader_independence() {
    let events = vec![
        graded_pass(Some(false), true), // self-graded
        graded_pass(Some(true), true),  // independent
        graded_pass(None, true),        // snapshotted, fact not recorded
        graded_pass(None, false),       // no snapshot at all (pre-#865)
        ci(CiStatus::Failing),
    ];
    let report = fold(&events);
    assert_eq!(report.unproven_passes, 4);
    assert_eq!(report.by_grader.self_graded.passes, 1);
    assert_eq!(report.by_grader.independent.passes, 1);
    assert_eq!(
        report.by_grader.unknown.passes, 2,
        "no snapshot and no recorded fact are both UNKNOWN — never assumed into a cohort"
    );
    assert_eq!(report.by_grader.self_graded.reconciled, 1);
    assert_eq!(report.by_grader.self_graded.false_positives, 1);
    assert_eq!(
        report.by_grader.self_graded.false_positive_rate(),
        Some(1.0)
    );
    assert_eq!(report.by_grader.independent.reconciled, 1);
    assert_eq!(report.by_grader.independent.false_positives, 1);
    assert_eq!(report.by_grader.unknown.reconciled, 2);
    let total = report.by_grader.self_graded.passes
        + report.by_grader.independent.passes
        + report.by_grader.unknown.passes;
    assert_eq!(
        total, report.unproven_passes,
        "the partition is exhaustive: every unproven pass lands in exactly one cohort"
    );
}

/// The late path keeps the fact: a pass carried out of its session still knows
/// who graded it, so a revert landing next week settles the right cohort — not
/// a merged one.
#[test]
fn late_reconciliation_settles_the_cohort_the_pass_belongs_to() {
    let events = vec![graded_pass(Some(false), true), commit("abc123abc123")];
    let (mut report, pending) = calibration(&events);
    assert_eq!(report.by_grader.self_graded.passes, 1);
    assert_eq!(report.by_grader.self_graded.reconciled, 0);
    let truth = GroundTruth::default().with_reverts(["abc123abc123".to_string()]);
    assert_eq!(reconcile(&mut report, &pending, &truth), 1);
    assert_eq!(report.by_grader.self_graded.reconciled, 1);
    assert_eq!(report.by_grader.self_graded.false_positives, 1);
    assert_eq!(
        report.by_grader.independent.reconciled + report.by_grader.unknown.reconciled,
        0,
        "the revert reaches only the cohort that made the call"
    );
}

/// Deterministic passes carry no grader — nothing graded, so they must not leak
/// into any cohort, unknown included.
#[test]
fn deterministic_passes_join_no_grader_cohort() {
    let report = fold(&[verdict(true, true), ci(CiStatus::Passing)]);
    assert_eq!(report.deterministic_passes, 1);
    assert_eq!(report.by_grader, GraderCohorts::default());
}

/// Workspace totals partition too: `merge` is componentwise across the cohorts,
/// the same as every other tally in the report.
#[test]
fn merge_adds_grader_cohorts_componentwise() {
    let a = fold(&[graded_pass(Some(false), true), ci(CiStatus::Failing)]);
    let b = fold(&[graded_pass(Some(true), true), ci(CiStatus::Passing)]);
    let merged = a.merge(b);
    assert_eq!(merged.by_grader.self_graded.false_positives, 1);
    assert_eq!(merged.by_grader.independent.reconciled, 1);
    assert_eq!(merged.by_grader.independent.false_positives, 0);
}

/// The render answers the question side by side, and states the unknown cohort
/// apart instead of folding it into either rate.
#[test]
fn the_render_shows_both_grader_rates_and_excludes_unknown() {
    let events = vec![
        graded_pass(Some(false), true),
        graded_pass(Some(true), true),
        graded_pass(None, false),
        ci(CiStatus::Failing),
    ];
    let rendered = render_calibration(&fold(&events));
    assert!(
        rendered.contains("self-graded"),
        "the self-graded cohort must be named: {rendered}"
    );
    assert!(
        rendered.contains("independent"),
        "the independent cohort must be named: {rendered}"
    );
    assert!(
        rendered.contains("excluded from both rates"),
        "the unknown cohort is stated apart, never folded in: {rendered}"
    );
}
