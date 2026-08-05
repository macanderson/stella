//! The verifier's answer key (#1293): cases where we know for certain whether
//! the work was actually good, and the machinery that connects them back to
//! the judgement made at the time.
//!
//! # Why an answer key is the blocking problem
//!
//! Part of Stella's verification is a model reading the work and saying
//! whether it looks correct, and that judgement has been measured badly
//! lenient — 46% agreement with a benchmark's own grader. Every threshold
//! built on top of it (`verifier_evidence_demand`, the escalation
//! rungs, the best-of-N ordering) is currently a guess, and it *stays* a guess
//! until something independent says which verdicts were wrong. Tuning without
//! that is not tuning; it is changing numbers.
//!
//! #871 built the first source: a terminal CI verdict recorded in the same
//! session stream. Two gaps remained, and this module closes both.
//!
//! # Gap 1: reverts were not a source at all
//!
//! When a human reverts a commit, that is a person saying "this was wrong",
//! usually long after a verifier said it was fine — the exact ground truth the
//! verifier is missing. It was never collected. [`parse_revert_targets`] reads it
//! out of git's own revert message.
//!
//! # Gap 2: reconciliation ended when the session did
//!
//! `calibration` reconciles forward within one stream, so a pass recorded
//! after that session's last CI observation stayed unreconciled forever — and
//! the verdict that would have settled it (a CI run finishing minutes later, a
//! revert landing next week) had nowhere to go. [`PendingPass`] carries those
//! passes out of the fold with the commits and PRs they cover, and
//! [`reconcile`] settles them against evidence gathered from anywhere: another
//! session's stream, the git history, a caller's own source.
//!
//! # The asymmetry, stated once
//!
//! A revert is evidence of **wrongness** only. A commit that was never
//! reverted is not evidence that the work was good — nobody may have looked,
//! and most correct work is never reverted for the same reason most correct
//! work is never mentioned. So a revert reconciles a pass as a false positive,
//! while the absence of one leaves it unreconciled and OUT of the denominator.
//! Counting un-reverted commits as confirmations would improve every measured
//! rate by construction, which is the one direction a calibration instrument
//! must never drift.

use std::collections::{BTreeMap, BTreeSet};

use stella_protocol::{AgentEvent, CiStatus};

use super::CalibrationReport;

/// A pass verdict that no in-stream verdict settled — carried out of the fold
/// so evidence arriving after the session ended can still reach it.
///
/// The `commits` and `prs` are what the pass is *about*: everything the
/// session recorded after it, on the same "a later observation covers the
/// passes before it" rule the in-stream CI reconciliation already uses. A
/// session that recorded neither leaves a pass that nothing can ever settle,
/// which is honest — there is no artifact to observe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPass {
    /// The session stream this pass came from, for a caller reporting *which*
    /// records a late verdict settled.
    pub session: String,
    /// Whether a model verifier produced it (`false` = a deterministic ladder
    /// pass, the comparison cohort).
    pub verifier: bool,
    /// Commit SHAs the session recorded after this pass.
    pub commits: Vec<String>,
    /// PR URLs the session recorded after this pass.
    pub prs: Vec<String>,
}

/// Evidence about work Stella produced, gathered from wherever it exists.
///
/// Deliberately keyed on artifacts (a commit SHA, a PR URL) rather than on
/// sessions: that is what lets a verdict recorded in one session — or in no
/// session at all, like a revert in the git log — settle a pass recorded in
/// another. Session-keyed reconciliation is precisely what left the record
/// stream-local.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroundTruth {
    /// Commits a later commit reverted. Membership means "a human said this
    /// was wrong"; absence means nothing at all (see the module docs).
    pub reverted_commits: BTreeSet<String>,
    /// PR URL → whether its terminal CI verdict was FAILING.
    pub pr_ci_failing: BTreeMap<String, bool>,
}

impl GroundTruth {
    /// Fold every terminal CI observation in one recorded stream into the
    /// PR-keyed map — the half of the answer key that already exists in the
    /// event journal, read across sessions instead of within one.
    ///
    /// A later terminal verdict for the same PR overwrites an earlier one:
    /// CI is re-run, and the last word about a PR is the one that stands.
    #[must_use]
    pub fn with_stream(mut self, events: &[AgentEvent]) -> Self {
        for event in events {
            let AgentEvent::Pr {
                url, ci: Some(ci), ..
            } = event
            else {
                continue;
            };
            let failing = match ci {
                CiStatus::Passing => false,
                CiStatus::Failing => true,
                // Absence of a verdict is not a verdict.
                CiStatus::Pending | CiStatus::Running => continue,
            };
            self.pr_ci_failing.insert(url.clone(), failing);
        }
        self
    }

    /// Add reverted commit SHAs — from [`parse_revert_targets`], or from any
    /// other source a host trusts.
    #[must_use]
    pub fn with_reverts(mut self, shas: impl IntoIterator<Item = String>) -> Self {
        self.reverted_commits.extend(shas);
        self
    }

    /// What this says about one pending pass, or `None` when it says nothing.
    /// `Some(true)` = the work was wrong.
    fn verdict_for(&self, pass: &PendingPass) -> Option<Settled> {
        // Reverts first: a revert is a human's considered statement about the
        // merged result, made later than CI and knowing more. A commit that
        // passed CI and was then reverted is exactly the false positive this
        // whole instrument exists to count, and taking CI's word first would
        // record it as a success.
        if pass
            .commits
            .iter()
            .any(|sha| self.reverted_commits.contains(sha))
        {
            return Some(Settled::Reverted);
        }
        for pr in &pass.prs {
            if let Some(failing) = self.pr_ci_failing.get(pr) {
                return Some(if *failing {
                    Settled::CiFailing
                } else {
                    Settled::CiPassing
                });
            }
        }
        None
    }
}

/// How a late verdict settled one pending pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settled {
    Reverted,
    CiFailing,
    CiPassing,
}

impl Settled {
    fn is_false_positive(self) -> bool {
        matches!(self, Settled::Reverted | Settled::CiFailing)
    }
}

/// Settle `pending` against `truth`, folding the results into `report`
/// (#1293).
///
/// Every settled pass joins the reconciled cohort it belongs to, so the
/// measured false-positive rates finally include the verdicts that used to
/// fall off the end of their session. A pass nothing can settle is left
/// alone: it stays unreconciled, out of every denominator, exactly as before.
///
/// Returns how many passes were settled — the number a caller reports as
/// "late evidence reached N earlier verdicts", which is the thing that was
/// silently zero before.
pub fn reconcile(
    report: &mut CalibrationReport,
    pending: &[PendingPass],
    truth: &GroundTruth,
) -> u32 {
    let mut settled = 0;
    for pass in pending {
        let Some(verdict) = truth.verdict_for(pass) else {
            continue;
        };
        settled += 1;
        let false_positive = u32::from(verdict.is_false_positive());
        let reverted = u32::from(verdict == Settled::Reverted);
        if pass.verifier {
            report.verifier_reconciled += 1;
            report.verifier_false_positives += false_positive;
            report.verifier_reverted += reverted;
        } else {
            report.deterministic_reconciled += 1;
            report.deterministic_false_positives += false_positive;
            report.deterministic_reverted += reverted;
        }
    }
    settled
}

/// The commit SHAs a `git log` body says were reverted (#1293).
///
/// Reads git's own revert message — `git revert` writes `This reverts commit
/// <sha>.` into the body, and has since 2005 — rather than guessing from a
/// `Revert "…"` subject. The subject line is a convention a human can retype
/// or a tool can rewrite; the body line is generated, carries the full SHA,
/// and is what makes the link machine-checkable.
///
/// Deliberately tolerant of what surrounds it: the caller may pass a whole
/// `git log` output, one commit body, or several concatenated. Anything that
/// is not the marker is skipped, and a marker whose SHA is not hex is skipped
/// too — a wrong link here would attribute a human's "this was wrong" to a
/// verdict that never made the claim, which is worse than missing the signal.
#[must_use]
pub fn parse_revert_targets(log: &str) -> BTreeSet<String> {
    const MARKER: &str = "This reverts commit ";
    let mut out = BTreeSet::new();
    for line in log.lines() {
        let Some(rest) = line.trim().strip_prefix(MARKER) else {
            continue;
        };
        let sha: String = rest
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .flat_map(char::to_lowercase)
            .collect();
        // Git abbreviates to at least 7 characters and writes the full 40 (or
        // 64, for a sha256 repository) in its own revert message. Below 7 is
        // not a commit id, whatever else it might be.
        if sha.len() >= 7 {
            out.insert(sha);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::PrStatus;

    fn pass(verifier: bool, commits: &[&str], prs: &[&str]) -> PendingPass {
        PendingPass {
            session: "sess-1".into(),
            verifier,
            commits: commits.iter().map(|s| (*s).to_string()).collect(),
            prs: prs.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// The revert marker git itself writes, in the shapes it actually
    /// appears in — indented inside a `git log` body, with the trailing
    /// period, and beside a subject line that must NOT be parsed.
    #[test]
    fn revert_targets_come_from_gits_own_marker() {
        let log = "\
commit ffff1111
    Revert \"feat: add the widget\"

    This reverts commit 0A1B2C3D4E5F60718293A4B5C6D7E8F901234567.

commit eeee2222
    feat: add the widget

commit dddd3333
    Revert \"something\" (no body marker, so nothing to link)
";
        let targets = parse_revert_targets(log);
        assert_eq!(
            targets.iter().collect::<Vec<_>>(),
            vec!["0a1b2c3d4e5f60718293a4b5c6d7e8f901234567"],
            "only the generated marker links a revert, and the SHA is normalized"
        );
    }

    /// Nothing that is not a commit id may become one: a short or non-hex
    /// tail is skipped rather than recorded as a link.
    #[test]
    fn a_malformed_marker_links_nothing() {
        assert!(parse_revert_targets("This reverts commit abc.").is_empty());
        assert!(parse_revert_targets("This reverts commit zzzzzzzzzz.").is_empty());
        assert!(parse_revert_targets("nothing here").is_empty());
    }

    /// #1293's first half: a revert reaches back to the judgement it
    /// contradicts, and is counted as a false positive of the cohort that
    /// produced it.
    #[test]
    fn a_revert_settles_the_pass_that_approved_the_commit() {
        let mut report = CalibrationReport::default();
        let pending = vec![
            pass(true, &["aaaa1111aaaa"], &[]),
            pass(false, &["bbbb2222bbbb"], &[]),
        ];
        let truth = GroundTruth::default().with_reverts(["aaaa1111aaaa".to_string()]);
        assert_eq!(reconcile(&mut report, &pending, &truth), 1);
        assert_eq!(report.verifier_reconciled, 1);
        assert_eq!(report.verifier_false_positives, 1);
        assert_eq!(report.verifier_reverted, 1);
        assert_eq!(
            report.deterministic_reconciled, 0,
            "an un-reverted commit settles nothing: absence of a revert is not a confirmation"
        );
    }

    /// #1293's second half: a terminal CI verdict recorded in a DIFFERENT
    /// session settles a pass its own session never saw one for.
    #[test]
    fn a_verdict_from_another_session_still_reaches_an_earlier_pass() {
        let later_session = vec![AgentEvent::Pr {
            url: "https://example.test/pr/7".into(),
            status: PrStatus::Open,
            number: Some(7),
            ci: Some(CiStatus::Failing),
        }];
        let truth = GroundTruth::default().with_stream(&later_session);
        let pending = vec![pass(true, &[], &["https://example.test/pr/7"])];
        let mut report = CalibrationReport::default();
        assert_eq!(reconcile(&mut report, &pending, &truth), 1);
        assert_eq!(report.verifier_reconciled, 1);
        assert_eq!(report.verifier_false_positives, 1);
        assert_eq!(
            report.verifier_reverted, 0,
            "CI failing is not a revert — the two are counted apart"
        );
    }

    /// A pass CI approved and a human then reverted is a FALSE POSITIVE, not
    /// a success: the revert is the later and better-informed statement, so
    /// it is consulted first.
    #[test]
    fn a_revert_outranks_a_green_ci_verdict() {
        let truth = GroundTruth::default()
            .with_stream(&[AgentEvent::Pr {
                url: "https://example.test/pr/9".into(),
                status: PrStatus::Merged,
                number: Some(9),
                ci: Some(CiStatus::Passing),
            }])
            .with_reverts(["cccc3333cccc".to_string()]);
        let mut report = CalibrationReport::default();
        let pending = vec![pass(
            true,
            &["cccc3333cccc"],
            &["https://example.test/pr/9"],
        )];
        assert_eq!(reconcile(&mut report, &pending, &truth), 1);
        assert_eq!(report.verifier_false_positives, 1);
        assert_eq!(report.verifier_reverted, 1);
    }

    /// Non-terminal CI contributes nothing, and a pass with no artifact at
    /// all stays unreconciled rather than being assumed either way.
    #[test]
    fn nothing_settles_a_pass_with_no_evidence() {
        let truth = GroundTruth::default().with_stream(&[AgentEvent::Pr {
            url: "https://example.test/pr/11".into(),
            status: PrStatus::Open,
            number: Some(11),
            ci: Some(CiStatus::Running),
        }]);
        let mut report = CalibrationReport::default();
        let pending = vec![
            pass(true, &[], &["https://example.test/pr/11"]),
            pass(true, &[], &[]),
        ];
        assert_eq!(reconcile(&mut report, &pending, &truth), 0);
        assert_eq!(report.verifier_reconciled, 0);
    }
}
