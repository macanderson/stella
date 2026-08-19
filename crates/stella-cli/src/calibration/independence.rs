//! The unproven cohort partitioned by who graded it (#1865).
//!
//! #1795 stamps `LadderSnapshot::verifier_independent` on every graded verdict:
//! `Some(false)` means the verdict call resolved to the worker's own model. The
//! 46%-agreement measurement this whole instrument exists to replace was made
//! under exactly that self-graded condition, and the question this partition
//! answers is whether it was a self-grading artifact: is a self-graded PASS
//! measurably less trustworthy than an independent one? One folded cohort
//! cannot say; three can.

use super::rate;

/// The unproven tallies of a [`super::CalibrationReport`], partitioned by the
/// grader-independence fact (#1865).
///
/// Only the unproven cohort is partitioned: a deterministic pass buys no
/// grader, so independence is not a fact about it (see
/// `LadderSnapshot::verifier_independent`). Each pass lands in exactly one
/// cohort, so the three `passes` fields sum to the report's `unproven_passes` —
/// and the same holds for `reconciled` and `false_positives`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GraderCohorts {
    /// Verdicts the worker's own model graded (`verifier_independent ==
    /// Some(false)`) — the condition the 46%-agreement number was measured
    /// under.
    pub self_graded: GraderTally,
    /// Verdicts a distinct model graded (`Some(true)`).
    pub independent: GraderTally,
    /// Verdicts that recorded no grader fact: pre-#1795 snapshots,
    /// worker-unresolvable runs, and verdicts with no snapshot at all. Kept
    /// apart and OUT of both rates — never assumed either way.
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

/// One grader cohort's tallies — the same three counters the report keeps for
/// the whole unproven cohort, so the two rates read identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GraderTally {
    /// Unproven PASS verdicts observed in this cohort.
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

    /// The measured false-positive rate over RECONCILED passes, or `None` when
    /// nothing was reconciled — an unmeasured rate is reported as unmeasured,
    /// never as zero.
    #[must_use]
    pub fn false_positive_rate(&self) -> Option<f64> {
        rate(self.false_positives, self.reconciled)
    }
}

/// One cohort line of the calibration render — shared with
/// [`super::render_calibration`] so the partitioned and unpartitioned cohorts
/// read in exactly one voice.
pub(super) fn cohort_line(
    label: &str,
    passes: u32,
    reconciled: u32,
    false_positives: u32,
) -> String {
    let measured = match rate(false_positives, reconciled) {
        Some(rate) => format!("{:.0}% false-positive rate", 100.0 * rate),
        None => "rate unmeasured (no CI ground truth recorded yet)".to_string(),
    };
    format!(
        "{label}: {passes} pass(es), {reconciled} reconciled against CI, \
         {false_positives} CI-failed — {measured}"
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
        "  grader independence (#1795) — was the 46%-agreement number a self-grading artifact?\n  \
         {}\n  {}{unknown}",
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
