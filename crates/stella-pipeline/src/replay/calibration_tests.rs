//! The verifier-calibration fold's tests: the pass cohorts, the
//! verifier-alone rate that tunes `verifier_evidence_demand`, and the
//! agreement between the live corroboration predicate and this module's
//! snapshot-side reading of it.
//!
//! A child module of `replay` so `stands_alone` — private, and deliberately so
//! — stays reachable, split into its own file because `replay.rs` was one
//! addition from the 1500-line ceiling.

use super::*;
use stella_protocol::{CiStatus, PrStatus, VerdictEvidence};

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
    AgentEvent::Pr {
        url: "https://example.test/pr/1".into(),
        status: PrStatus::Open,
        number: Some(1),
        ci: Some(status),
    }
}

/// The #871 acceptance: a VerifierPass that later fails CI is recorded as a
/// false positive; the deterministic cohort reconciles beside it.
#[test]
fn a_verifier_pass_that_fails_ci_is_a_false_positive() {
    let events = vec![
        verdict(true, false), // verifier PASS
        verdict(true, true),  // deterministic pass
        ci(CiStatus::Failing),
    ];
    let report = calibration(&events);
    assert_eq!(report.unproven_passes, 1);
    assert_eq!(report.unproven_reconciled, 1);
    assert_eq!(report.unproven_false_positives, 1);
    assert_eq!(report.unproven_false_positive_rate(), Some(1.0));
    assert_eq!(report.deterministic_false_positives, 1);
}

/// Pending/Running reconcile nothing, and passes after the last terminal
/// CI verdict stay unreconciled — an unmeasured rate reads None.
#[test]
fn non_terminal_ci_and_trailing_passes_stay_unreconciled() {
    let events = vec![
        verdict(true, false),
        ci(CiStatus::Running),
        ci(CiStatus::Passing),
        verdict(true, false), // after the last terminal verdict
    ];
    let report = calibration(&events);
    assert_eq!(report.unproven_passes, 2);
    assert_eq!(report.unproven_reconciled, 1);
    assert_eq!(report.unproven_false_positives, 0);
    assert_eq!(report.unproven_false_positive_rate(), Some(0.0));

    let unmeasured = calibration(&[verdict(true, false)]);
    assert_eq!(unmeasured.unproven_false_positive_rate(), None);
}

/// Failed verdicts and revise-loop reds never enter the tallies — the
/// question is the false-POSITIVE rate of passes.
#[test]
fn failed_verdicts_are_not_tallied() {
    let events = vec![
        verdict(false, true),
        verdict(false, false),
        ci(CiStatus::Passing),
    ];
    let report = calibration(&events);
    assert_eq!(report.unproven_passes, 0);
    assert_eq!(report.deterministic_passes, 0);
}

#[test]
fn merge_adds_componentwise() {
    let a = calibration(&[verdict(true, false), ci(CiStatus::Failing)]);
    let b = calibration(&[verdict(true, false), ci(CiStatus::Passing)]);
    let merged = a.merge(b);
    assert_eq!(merged.unproven_passes, 2);
    assert_eq!(merged.unproven_reconciled, 2);
    assert_eq!(merged.unproven_false_positives, 1);
}

/// A snapshot observing nothing, for tests that set only the channels
/// they exercise. The diff fields are non-zero because "the tree changed"
/// is never corroboration here — a literal that looked all-dark would
/// suggest it might be.
fn bare_snapshot() -> stella_protocol::LadderSnapshot {
    stella_protocol::LadderSnapshot {
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

/// A verdict carrying its ladder snapshot, so the verifier-alone fold has
/// something to read. `corroborated` decides whether the recorded turn
/// had a flip behind it.
fn snapshotted(passed: bool, deterministic: bool, corroborated: bool) -> AgentEvent {
    let snapshot = stella_protocol::LadderSnapshot {
        flip: if corroborated {
            stella_protocol::FlipOutcome::Achieved
        } else {
            stella_protocol::FlipOutcome::NotAchieved
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

/// #1295: the rate that decides whether asking for evidence is worth a
/// turn is measured off what is already recorded — every snapshotted
/// verdict counts toward the denominator, and a model-verifier PASS with
/// nothing behind it is separately identified as a turn the gated
/// behaviour would have sent back.
#[test]
fn the_verifier_alone_rate_is_measured_from_recorded_snapshots() {
    let events = vec![
        snapshotted(true, false, false),  // verifier PASS, nothing behind it
        snapshotted(true, true, true),    // deterministic pass, flip achieved
        snapshotted(false, false, false), // a FAILING verdict, also uncorroborated
        snapshotted(true, false, true),   // verifier PASS with a flip behind it
    ];
    let report = calibration(&events);
    assert_eq!(report.snapshotted_verdicts, 4);
    assert_eq!(
        report.uncorroborated_verdicts, 2,
        "the denominator is every verdict, not only the passing ones — the question is how \
         often the CONDITION holds"
    );
    assert_eq!(report.uncorroborated_rate(), Some(0.5));
    assert_eq!(
        report.unproven_passes_standing_alone, 1,
        "only the verifier PASS with no flip and no green test would be sent back"
    );
    // The pass cohorts are untouched by the new fold.
    assert_eq!(report.unproven_passes, 2);
    assert_eq!(report.deterministic_passes, 1);
}

/// #2194: the projection and the live predicate agree on every
/// combination of the three channels a corroboration decision reads.
///
/// This is the assertion whose absence let the two drift. `stands_alone`
/// used to *restate* `verifier_pass_stands_alone` in two conjuncts; #2191
/// taught the live predicate a third (`verify_done_flip`) and the copy
/// stayed at two, so `uncorroborated_verdicts` — the number that decides
/// `verifier_evidence_demand` — over-counted every `verify_done`-rescued
/// turn. The projection now calls the predicate rather than repeating it,
/// and this pins the remaining hazard: a channel the predicate reads must
/// survive the trip through the wire snapshot, or the same drift returns
/// wearing a `Default` instead of a missing conjunct.
#[test]
fn the_snapshot_projection_agrees_with_the_live_predicate() {
    for flip in [
        stella_protocol::FlipOutcome::Unobserved,
        stella_protocol::FlipOutcome::NotAchieved,
        stella_protocol::FlipOutcome::Achieved,
    ] {
        for touched_tests_passed in [None, Some(false), Some(true)] {
            for verify_done_flip in [false, true] {
                let inputs = LadderInputs {
                    flip,
                    touched_tests_passed,
                    verify_done_flip,
                    ..LadderInputs::default()
                };
                let snapshot = stella_protocol::LadderSnapshot {
                    flip,
                    touched_tests_passed,
                    verify_done_flip,
                    ..bare_snapshot()
                };
                // Through the wire, because that is how a replay reader
                // meets a snapshot: off disk, not from memory.
                let json = serde_json::to_string(&snapshot).unwrap();
                let recorded: stella_protocol::LadderSnapshot =
                    serde_json::from_str(&json).unwrap();
                assert_eq!(
                    stands_alone(&recorded),
                    inputs.verifier_pass_stands_alone(),
                    "projection disagrees at flip={flip:?} \
                     touched={touched_tests_passed:?} verify_done={verify_done_flip}"
                );
            }
        }
    }
}

/// The consequence, folded: a verdict rescued by a worker-run
/// `verify_done` confirmation is corroborated, and must not land in the
/// uncorroborated tally that tunes `verifier_evidence_demand`.
#[test]
fn a_verify_done_rescued_verdict_is_not_counted_uncorroborated() {
    let rescued = AgentEvent::Verdict {
        passed: true,
        evidence: VerdictEvidence {
            summary: "heuristic fallback passed on the worker's verify_done confirmation".into(),
            deterministic: false,
            evidence_refs: vec![],
            ladder: Some(Box::new(stella_protocol::LadderSnapshot {
                verify_done_flip: true,
                ..bare_snapshot()
            })),
        },
    };
    let report = calibration(&[rescued]);
    assert_eq!(report.snapshotted_verdicts, 1);
    assert_eq!(
        report.uncorroborated_verdicts, 0,
        "a confirmed verify_done flip is deterministic corroboration, exactly as the \
         engine weighed it at decision time"
    );
    assert_eq!(report.unproven_passes_standing_alone, 0);
}

/// A rate with no denominator is reported as unmeasured, never as 0% —
/// the same discipline the false-positive rates already keep. Verdicts
/// recorded before ladder snapshots existed (#865) contribute nothing
/// rather than being assumed corroborated.
#[test]
fn a_stream_without_snapshots_leaves_the_uncorroborated_rate_unmeasured() {
    let report = calibration(&[verdict(true, false), verdict(false, true)]);
    assert_eq!(report.snapshotted_verdicts, 0);
    assert_eq!(report.uncorroborated_rate(), None);
    assert!(
        render_calibration(&report).contains("uncorroborated rate: unmeasured"),
        "an unmeasured rate must say so: {}",
        render_calibration(&report)
    );
}

/// #2635: the cohort names the property it selects on — a pass with no
/// deterministic evidence — and nothing in the output claims a model judged it.
///
/// The predicate `passed && !deterministic` was exact while the only way to set
/// it was a model verifier ruling on inconclusive evidence. #2584 removed that
/// call, and the ladder's own non-determinate outcomes now set the same flag:
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
            ladder: Some(Box::new(stella_protocol::LadderSnapshot {
                rung: Some(stella_protocol::LadderRung::Unverified),
                ..bare_snapshot()
            })),
        },
    };
    let report = calibration(&[unsettled]);
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
