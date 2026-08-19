//! `stella calibration`'s measurement model (#871, #1293, #3866): fold the
//! pass verdicts a recorded event stream holds against the evidence that later
//! confirmed or contradicted them, and report each cohort's measured
//! false-positive rate.
//!
//! Pure functions over borrowed events — no I/O, no store, no git. The
//! command's I/O half (enumerating sessions, reading the journal, shelling out
//! to `git log`) lives in [`crate::inspect`] and hands its bytes here.
//!
//! # Why this is a module and not a crate (#3866)
//!
//! The model this replaces lived in `stella_pipeline::replay`, and went out of
//! the tree with that crate (#3865). Reinstating it had two homes to choose
//! between: a leaf crate on the `stella-diff`/`stella-embed` pattern, or a
//! module of the binary that asks the question.
//!
//! It is a module because a crate boundary is a public API commitment, and
//! there is exactly one consumer. The logic is pure and independence-testable,
//! which is the argument *for* a crate — but that argument buys a second
//! consumer's isolation, and no second consumer exists: nothing else in this
//! workspace measures verdicts, and a verification plugin lives in another
//! repository behind the wrapper socket rather than linking this. The day one
//! appears, promoting this directory to a leaf crate is a move, not a rewrite;
//! promoting it *now* would ship a README, an AGENTS.md row, and a stable
//! surface for nobody.
//!
//! # What produces the verdicts this reads
//!
//! Nothing in this workspace, any more. The built-in staged pipeline was the
//! only in-tree emitter of [`AgentEvent::Verdict`] and was deleted in #3865;
//! `Verdict` stays on the wire as `RecordedOnly` because it is the natural
//! event a verification plugin re-emits (`stella-protocol`'s consumer ledger,
//! #3790). So this instrument reads two populations and no third: sessions
//! recorded while the built-in ladder still ran, and streams an installed
//! verification plugin writes now.
//!
//! That is why [`render_calibration`] says so out loud when it folds no
//! verdict at all. A report of "0 passes, 0 false positives" reads exactly
//! like a flawless verifier to anyone who does not already know that no
//! verifier ran, and an instrument whose measured failure mode is leniency
//! must never be readable as flattery.

pub(crate) mod ground_truth;
pub(crate) mod independence;

use ground_truth::PendingPass;
use independence::GraderCohorts;
use stella_protocol::{AgentEvent, CiStatus, LadderSnapshot};

/// Calibration tallies folded from recorded event streams (#871).
///
/// The event store already persists every verdict (with the ladder snapshot it
/// was decided from, #865) and every PR/CI observation — so calibration is a
/// *reading* of what exists, not a new write path. A `Verdict` with
/// `deterministic: false, passed: true` is an **unproven** pass; the next
/// **terminal** CI verdict in the same stream (`Pr { ci: Some(Passing |
/// Failing) }`) reconciles every unreconciled pass before it. Deterministic
/// passes are tallied identically, because a false-positive *rate* means
/// nothing without the cohort it should be compared against.
///
/// # Why the cohort is not called "model verifier" (#2635)
///
/// It was, and the name was exact while the only way to emit
/// `passed: true, deterministic: false` was a model verifier ruling on
/// inconclusive evidence. #2584 removed that call, and the same flag was then
/// set by the ladder's own non-determinate outcomes — `unverified_evidence`,
/// `unverifiable_evidence`, and `waived_completion` for a triage waiver. So on
/// any session recorded after #2584 the cohort counts **unproven passes, not
/// judged ones**, and no model was involved in a single one of them.
///
/// The rate itself was always a real number — the false-positive rate of passes
/// nothing proved, which is worth knowing. What was wrong was the label, and a
/// misnamed measurement is worse than a missing one. The fields state the
/// property they actually select on.
///
/// Sessions spanning #2584 still mix both populations under one heading, and
/// nothing in the record distinguishes them. That is a fact about the old data,
/// not something a rename can repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct CalibrationReport {
    /// Every `Verdict` event folded, pass or fail, snapshot or not.
    ///
    /// The denominator of nothing — it exists so a report can tell "no verdict
    /// was ever recorded" from "verdicts were recorded and none of them
    /// passed". Those render identically in every other field, and the first
    /// is now the ordinary state of a workspace with no verification plugin
    /// installed (#3866).
    pub verdicts_observed: u32,
    /// Passes observed with no deterministic evidence behind them.
    pub unproven_passes: u32,
    /// …of which later evidence — terminal CI, or a revert — reconciled.
    pub unproven_reconciled: u32,
    /// …reconciled as WRONG: the run reported a pass that CI rejected or a
    /// human reverted.
    pub unproven_false_positives: u32,
    /// Deterministic (ladder) passes observed — the comparison cohort.
    pub deterministic_passes: u32,
    pub deterministic_reconciled: u32,
    pub deterministic_false_positives: u32,
    /// Verdicts carrying a ladder snapshot — the denominator for the
    /// uncorroborated rate below (#1295). Verdicts recorded before snapshots
    /// existed (#865) are excluded rather than assumed either way.
    pub snapshotted_verdicts: u32,
    /// …of which NOTHING mechanical corroborated the result: no achieved flip,
    /// no touched-test run observed green, and no worker-run `verify_done`
    /// confirmation.
    ///
    /// This is [`stands_alone`] read back off the record. Counted over every
    /// snapshotted verdict, not only over passes, because the question is how
    /// often the *condition* holds — which is what decides whether a policy
    /// gated on it taxes every turn or only the suspicious minority.
    pub uncorroborated_verdicts: u32,
    /// Unproven passes where that condition held: a pass with nothing
    /// mechanical behind it at all.
    ///
    /// Kept beside the cohort rather than folded into it: it is a near-subset
    /// of [`Self::unproven_passes`], but the two are folded from different
    /// sources — this one from the ladder snapshot, the cohort from the
    /// verdict's own `deterministic` flag — so a divergence between them is a
    /// real signal that one of the two projections has drifted.
    pub unproven_passes_standing_alone: u32,
    /// …of the reconciled unproven passes, how many were settled by a **revert**
    /// rather than by CI (#1293).
    ///
    /// Counted apart because it is a different and stronger claim. CI failing
    /// can mean a flake, a neighbouring change, or an infrastructure day; a
    /// human reverting a commit is a person deciding, later and with more
    /// information, that the work should not have landed. A cohort whose false
    /// positives are mostly reverts is failing differently from one whose false
    /// positives are mostly red CI, and a single number would hide it.
    pub unproven_reverted: u32,
    /// The same, for the deterministic cohort.
    pub deterministic_reverted: u32,
    /// The unproven cohort above, partitioned by who graded it (#1865):
    /// self-graded / independent / unknown, with unknown never assumed into
    /// either measured cohort. The three partitions sum to the unpartitioned
    /// cohort tallies.
    pub by_grader: GraderCohorts,
}

impl CalibrationReport {
    /// Combine per-session reports into a workspace total.
    pub fn merge(mut self, other: Self) -> Self {
        self.verdicts_observed += other.verdicts_observed;
        self.unproven_passes += other.unproven_passes;
        self.unproven_reconciled += other.unproven_reconciled;
        self.unproven_false_positives += other.unproven_false_positives;
        self.deterministic_passes += other.deterministic_passes;
        self.deterministic_reconciled += other.deterministic_reconciled;
        self.deterministic_false_positives += other.deterministic_false_positives;
        self.snapshotted_verdicts += other.snapshotted_verdicts;
        self.uncorroborated_verdicts += other.uncorroborated_verdicts;
        self.unproven_passes_standing_alone += other.unproven_passes_standing_alone;
        self.unproven_reverted += other.unproven_reverted;
        self.deterministic_reverted += other.deterministic_reverted;
        self.by_grader = self.by_grader.merge(other.by_grader);
        self
    }

    /// How often a verdict had no mechanical corroboration behind it, over the
    /// verdicts that recorded enough to say (#1295).
    ///
    /// The number an **evidence-demand policy** is set from: "the ladder
    /// settled nothing, so spend one more turn asking the worker for evidence"
    /// is worth its turn only where this is the suspicious minority. A majority
    /// reproduces the Terminal-Bench measurement that switched the built-in
    /// version of that policy off (#1211 §1): the condition held on most turns
    /// precisely because most turns had no tracked command, so the ask could
    /// not be satisfied by any worker and the turn it cost was pure loss.
    ///
    /// The policy itself left the tree with the staged pipeline (#3865); the
    /// rate did not stop being the right input for the plugin-side one that
    /// replaces it.
    #[must_use]
    pub fn uncorroborated_rate(&self) -> Option<f64> {
        rate(self.uncorroborated_verdicts, self.snapshotted_verdicts)
    }

    /// The measured false-positive rate over RECONCILED unproven passes, or
    /// `None` when nothing was reconciled — an unmeasured rate is reported as
    /// unmeasured, never as zero.
    #[must_use]
    pub fn unproven_false_positive_rate(&self) -> Option<f64> {
        rate(self.unproven_false_positives, self.unproven_reconciled)
    }

    /// Same rate for the deterministic cohort.
    #[must_use]
    pub fn deterministic_false_positive_rate(&self) -> Option<f64> {
        rate(
            self.deterministic_false_positives,
            self.deterministic_reconciled,
        )
    }
}

/// A measured proportion, or `None` when the denominator is empty.
///
/// Every rate in this module goes through here so that "unmeasured" is one
/// decision made once. A zero denominator rendering as `0%` is the single
/// mistake this instrument cannot afford: it reports a verifier that was never
/// measured as a verifier that never erred.
#[must_use]
pub(crate) fn rate(numerator: u32, denominator: u32) -> Option<f64> {
    (denominator > 0).then(|| f64::from(numerator) / f64::from(denominator))
}

/// Render a [`CalibrationReport`] for a human — the `stella calibration` body.
/// States unmeasured rates as unmeasured and names the cohort sizes, so a rate
/// is never read without its denominator.
pub(crate) fn render_calibration(report: &CalibrationReport) -> String {
    use independence::cohort_line as cohort;

    // The state a workspace with no verification plugin installed is now in,
    // and the one reading that is most easily misread (#3866). Said first and
    // instead of the tallies, because every tally below it would be a zero,
    // and a page of zeros reads as a clean bill of health.
    if report.verdicts_observed == 0 {
        return "pass calibration (#871) — nothing to calibrate\n  \
                No recorded session holds a verification verdict.\n  \
                Since #3865 this workspace ships no built-in verification path, so nothing\n  \
                emits one: `stella run` is the raw step loop, and verdicts come only from an\n  \
                installed verification plugin (`stella plugin install`, then\n  \
                `stella run --pipeline <plugin-id>`) or from sessions recorded before that\n  \
                deletion.\n  \
                This is NOT a measured result — an empty record is not a perfect verifier."
            .to_string();
    }

    // #1293: reverts are counted apart from CI, because they are a different
    // and stronger statement — a human deciding later that the work should not
    // have landed. Stated only when there are any: a zero here would read as
    // "no work was reverted", when the ordinary case is that the caller
    // gathered no revert evidence at all.
    let reverts = match (report.unproven_reverted, report.deterministic_reverted) {
        (0, 0) => String::new(),
        (unproven, deterministic) => format!(
            "\n  of those, settled by a REVERT (a human said it was wrong, not CI): \
             {unproven} unproven, {deterministic} deterministic"
        ),
    };
    // #1865: the unproven line above, partitioned by who graded it. Rendered
    // only once there is an unproven pass to partition — the section answers a
    // question about recorded unproven verdicts, and with none recorded there
    // is no cohort to compare.
    let grader = if report.unproven_passes > 0 {
        format!(
            "\n{}",
            independence::render_grader_cohorts(&report.by_grader)
        )
    } else {
        String::new()
    };
    // #1295: the rate an evidence-demand policy is set from. Rendered beside
    // the calibration cohorts because it is read for the same reason — to
    // replace an argument about the verifier with a number — and stated with
    // its denominator, like every other rate here.
    let uncorroborated = match report.uncorroborated_rate() {
        Some(rate) => format!(
            "  uncorroborated rate: {:.0}% of {} snapshotted verdict(s) had no flip, no green \
             touched test and no verify_done confirmation ({} of them were reported as passes)\n  \
             → a MINORITY is the condition an evidence-demand send-back is worth a turn for; \
             a majority reproduces the measurement that switched the built-in one off (#1295, \
             #1211 §1)",
            100.0 * rate,
            report.snapshotted_verdicts,
            report.unproven_passes_standing_alone,
        ),
        None => "  uncorroborated rate: unmeasured (no verdict carried a ladder snapshot yet)"
            .to_string(),
    };
    format!(
        "pass calibration (#871) — passes reconciled against later CI verdicts and reverts\n  \
         {} verdict(s) folded\n{}\n{}{reverts}{grader}\n\
         note: a pass is reconciled by a terminal CI verdict or by a revert of\n\
         a commit it covers, from any session or from the git history (#1293).\n\
         A pass no evidence reaches stays UNRECONCILED and out of every\n\
         denominator — absence of a revert is never a confirmation.\n\
         the `unproven` cohort is every pass the ladder could not settle —\n\
         unverified, unverifiable, or waived. No model judged any of them\n\
         (#2584, #2635).\n\
         {uncorroborated}",
        report.verdicts_observed,
        cohort(
            "  unproven      ",
            report.unproven_passes,
            report.unproven_reconciled,
            report.unproven_false_positives
        ),
        cohort(
            "  deterministic",
            report.deterministic_passes,
            report.deterministic_reconciled,
            report.deterministic_false_positives
        ),
    )
}

/// Fold one event stream into a [`CalibrationReport`] (#871), returning the
/// pass verdicts the stream could not settle itself (#1293).
///
/// Reconciliation here is stream-local and forward-only: a terminal CI verdict
/// covers the passes that PRECEDED it (the PR carries the session's adopted
/// work), and passes after the last terminal CI observation stay unreconciled.
/// `Pending`/`Running` reconcile nothing — absence of a verdict is not a
/// verdict.
///
/// The returned [`PendingPass`] values are what makes a *late* verdict usable:
/// a CI run that finishes after the session, a revert that lands next week, a
/// terminal verdict recorded in some other session's stream. Feed them to
/// [`ground_truth::reconcile`] with whatever evidence exists.
///
/// The attribution rule for artifacts mirrors the CI one already in place: a
/// commit or PR recorded at index *i* covers every unsettled pass before it.
/// That is deliberately generous, and it is the only rule available — a
/// session's events do not carry which verdict a given commit implements. The
/// consequence is stated rather than hidden: a session that produced two passes
/// and one commit attributes that commit's fate to both, so a revert there
/// counts two false positives. Sessions that adopt one change are the ordinary
/// case, and over-attributing a revert errs toward reporting the verifier as
/// *worse* than it is — the safe direction for an instrument whose measured
/// failure is leniency.
pub(crate) fn calibration(events: &[AgentEvent]) -> (CalibrationReport, Vec<PendingPass>) {
    let mut report = CalibrationReport::default();
    // Unsettled pass verdicts, in order, with the artifacts recorded since
    // each one. `verifier` distinguishes the two cohorts.
    let mut pending: Vec<PendingPass> = Vec::new();
    for event in events {
        match event {
            // Every verdict — pass or fail — contributes to the uncorroborated
            // denominator (#1295): the question is how often verification
            // reaches a verdict with nothing mechanical behind it, and a
            // failing turn is as much a part of that population as a passing
            // one. The pass cohorts below are counted separately, on the same
            // pass.
            AgentEvent::Verdict { passed, evidence } => {
                report.verdicts_observed += 1;
                if let Some(snapshot) = evidence.ladder.as_deref() {
                    report.snapshotted_verdicts += 1;
                    if stands_alone(snapshot) {
                        report.uncorroborated_verdicts += 1;
                        if *passed && !evidence.deterministic {
                            report.unproven_passes_standing_alone += 1;
                        }
                    }
                }
                if !*passed {
                    continue;
                }
                // #1865: which model graded a verdict rides the ladder snapshot
                // (#1795); absent — pre-#1795, worker-unresolvable, or no
                // snapshot at all — stays UNKNOWN, never assumed.
                let grader_independent = evidence
                    .ladder
                    .as_deref()
                    .and_then(|snapshot| snapshot.verifier_independent);
                if evidence.deterministic {
                    report.deterministic_passes += 1;
                } else {
                    report.unproven_passes += 1;
                    report.by_grader.tally_mut(grader_independent).passes += 1;
                }
                pending.push(PendingPass {
                    verifier: !evidence.deterministic,
                    grader_independent,
                    commits: Vec::new(),
                    prs: Vec::new(),
                });
            }
            // Every commit and PR the session records after a pass is an
            // artifact that pass covers — the handle a late verdict is keyed
            // on. Attached to the unsettled passes only: one already reconciled
            // in-stream has had its answer.
            AgentEvent::Commit { sha, .. } => {
                for pass in &mut pending {
                    pass.commits.push(sha.clone());
                }
            }
            AgentEvent::Pr { url, ci, .. } => {
                for pass in &mut pending {
                    pass.prs.push(url.clone());
                }
                let failing = match ci {
                    Some(CiStatus::Passing) => false,
                    Some(CiStatus::Failing) => true,
                    // Absence of a verdict is not a verdict — the PR is still
                    // recorded above, so a terminal result arriving later (this
                    // session or another) can still settle these.
                    Some(CiStatus::Pending | CiStatus::Running) | None => continue,
                };
                for pass in pending.drain(..) {
                    if pass.verifier {
                        report.unproven_reconciled += 1;
                        report.unproven_false_positives += u32::from(failing);
                        let tally = report.by_grader.tally_mut(pass.grader_independent);
                        tally.reconciled += 1;
                        tally.false_positives += u32::from(failing);
                    } else {
                        report.deterministic_reconciled += 1;
                        report.deterministic_false_positives += u32::from(failing);
                    }
                }
            }
            _ => {}
        }
    }
    (report, pending)
}

/// Whether a recorded verdict had no mechanical corroboration behind it
/// (#1295): no achieved flip, no touched-test run observed green, and no
/// worker-run `verify_done` confirmation.
///
/// # This is now the definition, and that is a change worth naming (#3866)
///
/// It used to *call* `LadderInputs::verifier_pass_stands_alone` rather than
/// restate it, and the doc comment on it said why in some detail: for a while
/// it restated the predicate in two conjuncts, and when #2191 taught the live
/// one a third channel (`verify_done_flip`) the copy here stayed at two — so
/// [`CalibrationReport::uncorroborated_verdicts`] counted verdicts as
/// uncorroborated over runs where the engine, at decision time, held
/// corroboration (#2194).
///
/// That predicate went out of the tree with `stella-pipeline` (#3865), so
/// calling it is no longer an option and this restatement is the only copy
/// there is. The old hazard — two copies drifting apart — is therefore gone by
/// construction. The one that replaces it is different and should not be
/// mistaken for it: a verification plugin implements its own ladder on the far
/// side of the wrapper socket, and may credit corroboration through a channel
/// [`LadderSnapshot`] has no field for. This can only ever report what the
/// snapshot recorded, which is why the rendered line names the three channels
/// it reads instead of claiming to be exhaustive.
///
/// A readable diff or a recorded file touch is deliberately not among them:
/// they prove the tree **changed**, never that the change is **correct**, and
/// only the second claim is what a pass makes.
fn stands_alone(snapshot: &LadderSnapshot) -> bool {
    !snapshot.flip.is_achieved()
        && snapshot.touched_tests_passed != Some(true)
        && !snapshot.verify_done_flip
}

#[cfg(test)]
mod tests;
