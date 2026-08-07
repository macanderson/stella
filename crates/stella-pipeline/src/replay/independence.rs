//! The verifier cohort partitioned by who graded it (#1865) — split out of
//! `replay.rs` to keep it under the file-size ratchet; a child module, so
//! the fold internals stay reachable via `super::*` in its tests.
//!
//! #1795 stamps `LadderSnapshot::verifier_independent` on every model
//! verdict: `Some(false)` means the verdict call resolved to the worker's
//! own model. The 46%-agreement measurement cited in `crate::verify` was
//! made under exactly that self-graded condition, and the question this
//! partition exists to answer is whether it was a self-grading artifact:
//! is a self-graded PASS measurably less trustworthy than an independent
//! one? One folded cohort cannot say; three can.

/// The model-verifier tallies of a [`super::CalibrationReport`], partitioned
/// by the grader-independence fact (#1865).
///
/// Only the model-verifier cohort is partitioned: deterministic passes buy
/// no model verdict, so independence is not a fact about them (see
/// `LadderSnapshot::verifier_independent`). Each pass lands in exactly one
/// cohort, so the three `passes` fields sum to the report's
/// `verifier_passes` — and the same holds for `reconciled` and
/// `false_positives`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraderCohorts {
    /// Verdicts the worker's own model graded (`verifier_independent ==
    /// Some(false)`) — the condition the 46%-agreement number was measured
    /// under.
    pub self_graded: GraderTally,
    /// Verdicts a distinct model graded (`Some(true)`).
    pub independent: GraderTally,
    /// Verdicts that recorded no grader fact: pre-#1795 snapshots,
    /// worker-unresolvable runs, and verdicts with no snapshot at all.
    /// Kept apart and OUT of both rates — never assumed either way.
    pub unknown: GraderTally,
}

impl GraderCohorts {
    /// The cohort a recorded grader fact belongs to.
    pub(super) fn tally_mut(&mut self, grader_independent: Option<bool>) -> &mut GraderTally {
        match grader_independent {
            Some(false) => &mut self.self_graded,
            Some(true) => &mut self.independent,
            None => &mut self.unknown,
        }
    }

    /// Combine per-session cohorts into a workspace total.
    pub(super) fn merge(mut self, other: Self) -> Self {
        self.self_graded = self.self_graded.merge(other.self_graded);
        self.independent = self.independent.merge(other.independent);
        self.unknown = self.unknown.merge(other.unknown);
        self
    }
}

/// One grader cohort's tallies — the same three counters the report keeps
/// for the whole verifier cohort, so the two rates read identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraderTally {
    /// Model-verifier PASS verdicts observed in this cohort.
    pub passes: u32,
    /// …of which a later terminal verdict (CI or revert) reconciled.
    pub reconciled: u32,
    /// …reconciled as wrong: the grader approved work that was rejected.
    pub false_positives: u32,
}

impl GraderTally {
    fn merge(mut self, other: Self) -> Self {
        self.passes += other.passes;
        self.reconciled += other.reconciled;
        self.false_positives += other.false_positives;
        self
    }

    /// The measured false-positive rate over RECONCILED passes, or `None`
    /// when nothing was reconciled — an unmeasured rate is reported as
    /// unmeasured, never as zero.
    #[must_use]
    pub fn false_positive_rate(&self) -> Option<f64> {
        (self.reconciled > 0).then(|| f64::from(self.false_positives) / f64::from(self.reconciled))
    }
}

/// One cohort line of the calibration render — shared with
/// [`super::render_calibration`] so the partitioned and unpartitioned
/// cohorts read in exactly one voice.
pub(super) fn cohort_line(
    label: &str,
    passes: u32,
    reconciled: u32,
    false_positives: u32,
) -> String {
    let rate = if reconciled > 0 {
        format!(
            "{:.0}% false-positive rate",
            100.0 * f64::from(false_positives) / f64::from(reconciled)
        )
    } else {
        "rate unmeasured (no CI ground truth recorded yet)".to_string()
    };
    format!(
        "{label}: {passes} pass(es), {reconciled} reconciled against CI, \
         {false_positives} CI-failed — {rate}"
    )
}

/// The grader-independence section of the calibration render (#1865): the
/// self-graded and independent false-positive rates side by side, with the
/// unknown cohort stated apart and excluded from both.
pub(super) fn render_grader_cohorts(cohorts: &GraderCohorts) -> String {
    let unknown = match cohorts.unknown.passes {
        0 => String::new(),
        n => format!(
            "\n    ({n} pass(es) recorded no grader fact — pre-#1795 or worker-unresolvable — \
             and are excluded from both rates)"
        ),
    };
    format!(
        "  grader independence (#1795) — was the 46%-agreement number a self-grading artifact?\n  {}\n  {}{unknown}",
        cohort_line(
            "  self-graded ",
            cohorts.self_graded.passes,
            cohorts.self_graded.reconciled,
            cohorts.self_graded.false_positives,
        ),
        cohort_line(
            "  independent ",
            cohorts.independent.passes,
            cohorts.independent.reconciled,
            cohorts.independent.false_positives,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use stella_protocol::{CiStatus, PrStatus, VerdictEvidence};

    /// A model-verifier PASS carrying the grader fact under test; `None`
    /// with `snapshot: false` is a verdict with no ladder snapshot at all —
    /// the pre-#865 shape.
    fn verifier_pass(grader_independent: Option<bool>, snapshot: bool) -> AgentEvent {
        let ladder = snapshot.then(|| {
            Box::new(stella_protocol::LadderSnapshot {
                rung: None,
                tracked_command: None,
                oracle_trace: vec![],
                flip_achieved: false,
                unstable_flip: false,
                flip_refused_different_failure: false,
                touched_tests_passed: None,
                test_infra: None,
                diff_lines: 4,
                diff_budget: 400,
                diff_available: true,
                file_change_events: 1,
                mutating_actions: 2,
                new_diag_errors: 0,
                new_diag_warnings: 0,
                witness_intact: None,
                witness_mutation: None,
                diff_coverage: None,
                verifier_independent: grader_independent,
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

    fn ci(status: CiStatus) -> AgentEvent {
        AgentEvent::Pr {
            url: "https://example.test/pr/1".into(),
            status: PrStatus::Open,
            number: Some(1),
            ci: Some(status),
        }
    }

    /// The #1865 acceptance: a scripted stream carrying both polarities (and
    /// an unknown) folds each pass into exactly one cohort, and the in-stream
    /// CI reconciliation settles each into its own cohort's rate — unknown
    /// counted apart, never folded into either.
    #[test]
    fn the_fold_partitions_the_verifier_cohort_by_grader_independence() {
        let events = vec![
            verifier_pass(Some(false), true), // self-graded
            verifier_pass(Some(true), true),  // independent
            verifier_pass(None, true),        // snapshotted, fact not recorded
            verifier_pass(None, false),       // no snapshot at all (pre-#865)
            ci(CiStatus::Failing),
        ];
        let report = calibration(&events);
        assert_eq!(report.verifier_passes, 4);
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
            total, report.verifier_passes,
            "the partition is exhaustive: every verifier pass lands in exactly one cohort"
        );
    }

    /// The late path keeps the fact: a pass carried out of its session still
    /// knows who graded it, so a revert landing next week settles the right
    /// cohort — not a merged one.
    #[test]
    fn late_reconciliation_settles_the_cohort_the_pass_belongs_to() {
        let events = vec![
            verifier_pass(Some(false), true),
            AgentEvent::Commit {
                sha: "abc123abc123".into(),
                message: "feat: the work".into(),
            },
        ];
        let (mut report, pending) = calibration_pending("sess-a", &events);
        assert_eq!(report.by_grader.self_graded.passes, 1);
        assert_eq!(report.by_grader.self_graded.reconciled, 0);
        let truth = ground_truth::GroundTruth::default().with_reverts(["abc123abc123".to_string()]);
        assert_eq!(ground_truth::reconcile(&mut report, &pending, &truth), 1);
        assert_eq!(report.by_grader.self_graded.reconciled, 1);
        assert_eq!(report.by_grader.self_graded.false_positives, 1);
        assert_eq!(
            report.by_grader.independent.reconciled + report.by_grader.unknown.reconciled,
            0,
            "the revert reaches only the cohort that made the call"
        );
    }

    /// Deterministic passes carry no grader — nothing graded, so they must
    /// not leak into any cohort, unknown included.
    #[test]
    fn deterministic_passes_join_no_grader_cohort() {
        let events = vec![
            AgentEvent::Verdict {
                passed: true,
                evidence: VerdictEvidence {
                    summary: String::new(),
                    deterministic: true,
                    evidence_refs: vec![],
                    ladder: None,
                },
            },
            ci(CiStatus::Passing),
        ];
        let report = calibration(&events);
        assert_eq!(report.deterministic_passes, 1);
        assert_eq!(report.by_grader, GraderCohorts::default());
    }

    /// Workspace totals partition too: `merge` is componentwise across the
    /// cohorts, the same as every other tally in the report.
    #[test]
    fn merge_adds_cohorts_componentwise() {
        let a = calibration(&[verifier_pass(Some(false), true), ci(CiStatus::Failing)]);
        let b = calibration(&[verifier_pass(Some(true), true), ci(CiStatus::Passing)]);
        let merged = a.merge(b);
        assert_eq!(merged.by_grader.self_graded.false_positives, 1);
        assert_eq!(merged.by_grader.independent.reconciled, 1);
        assert_eq!(merged.by_grader.independent.false_positives, 0);
    }

    /// The render answers the question side by side, and states the unknown
    /// cohort apart instead of folding it into either rate.
    #[test]
    fn the_render_shows_both_rates_and_excludes_unknown() {
        let events = vec![
            verifier_pass(Some(false), true),
            verifier_pass(Some(true), true),
            verifier_pass(None, false),
            ci(CiStatus::Failing),
        ];
        let rendered = render_calibration(&calibration(&events));
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
}
