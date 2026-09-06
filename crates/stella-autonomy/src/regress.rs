//! Re-checking the fixes this loop has already claimed.
//!
//! A closed issue is a claim with a date on it. The change that fixed it was
//! on the base branch when the loop closed the issue. Any merge after that can
//! take it away again: a revert, a force push, a rebase that drops it.
//!
//! So the loop writes down a [`ClosureReceipt`] as it closes. The receipt says
//! what the closure cited, and whether that change was on the base at the
//! time. This module reads the receipts back and asks the same question again.
//!
//! A change that was there and is gone is a fact, not a claim, and it is filed
//! as a fresh defect. That is what makes this supply worth its cost: an audit
//! finding needs triage, and this one does not.
//!
//! A receipt that cited no change cannot be re-checked. Those are counted and
//! reported. The count says how often *done* was a claim rather than a proof,
//! and it is meant to be read.
//!
//! `doc:backlog-self-driving` §4.3 is the design.

use serde::{Deserialize, Serialize};

use crate::closure::Citation;
use crate::supply::Finding;

/// The label a returning defect carries.
///
/// A fix that came back out is a defect, and the type axis of a backlog
/// convention spells that `bug`. A convention with no such member refuses the
/// draft, and the refusal stands. Nothing here invents a word.
pub const DEFECT_LABEL: &str = "bug";

/// What the loop wrote down when it closed an issue.
///
/// One line of `receipts.jsonl` in the loop's own state directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosureReceipt {
    /// The issue key, as the tracker spells it, with no leading `#`.
    pub key: String,
    /// When it closed, RFC3339.
    #[serde(default)]
    pub closed_at: String,
    /// What the closure cited. `None` when it cited no change at all — a
    /// duplicate, or work somebody declined.
    #[serde(default)]
    pub by: Option<Citation>,
    /// Whether the cited change was on the base branch at the time.
    ///
    /// `None` when git could not answer, and on any receipt written before
    /// this field existed. An unknown reads as unsweepable, never as gone:
    /// telling *gone* from *never seen* is the whole job here.
    #[serde(default)]
    pub present_at_close: Option<bool>,
}

/// What re-checking one receipt found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// The change is still on the base.
    Holds,
    /// It was there, and it is gone.
    Regressed,
    /// It cannot be re-checked.
    Skipped(Skip),
}

/// Why a receipt cannot be re-checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The closure cited no change at all.
    NoChangeCited,
    /// Git could not say whether the change was on the base at close time.
    UnknownAtClose,
    /// The change was already absent when the issue closed.
    AbsentAtClose,
}

impl Skip {
    /// A short word for a report.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoChangeCited => "no change cited",
            Self::UnknownAtClose => "unknown at close",
            Self::AbsentAtClose => "absent at close",
        }
    }
}

/// Re-check one receipt against what the base holds now.
///
/// Only a change that was there and is gone counts. A change nobody could see
/// at close time is skipped, because *gone* and *never seen* would then look
/// the same, and filing the wrong one of those costs a person a wasted hour.
#[must_use]
pub fn check(receipt: &ClosureReceipt, present_now: bool) -> Check {
    if receipt.by.is_none() {
        return Check::Skipped(Skip::NoChangeCited);
    }
    match receipt.present_at_close {
        None => Check::Skipped(Skip::UnknownAtClose),
        Some(false) => Check::Skipped(Skip::AbsentAtClose),
        Some(true) if present_now => Check::Holds,
        Some(true) => Check::Regressed,
    }
}

/// The defect a returning bug is filed as.
#[must_use]
pub fn finding(receipt: &ClosureReceipt) -> Finding {
    let key = receipt.key.trim().trim_start_matches('#');
    let cited = receipt
        .by
        .as_ref()
        .map_or_else(|| "nothing".to_owned(), Citation::render);
    Finding {
        title: format!("the fix for #{key} has left the base branch"),
        body: format!(
            "This loop closed #{key} as done and cited {cited}. That change was on the base \
             branch then. It is not there now.\n\n\
             Something took it back out: a revert, a force push, or a rebase that dropped \
             it. Until it returns, the defect #{key} described is back.\n\n\
             ## How to check\n\n\
             1. Read #{key} for what the defect was.\n\
             2. Ask git what happened to {cited} — `git log` over the paths it touched.\n\
             3. Land the fix again, with a test that fails without it.\n\n\
             ## Definition of done\n\n\
             - [ ] The fix {cited} carried is back on the base branch, or a fresh change \
             carries the same fix.\n\
             - [ ] A test fails without that change and passes with it.\n"
        ),
        labels: vec![DEFECT_LABEL.to_owned()],
    }
}

/// What one pass over the receipts found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Receipts that could be re-checked.
    pub checked: u64,
    /// Receipts that could not be, one entry each.
    pub skipped: Vec<Skip>,
    /// The defects to file.
    pub findings: Vec<Finding>,
}

/// Re-check every receipt.
///
/// `present_now` answers whether one cited change is on the base branch right
/// now. It is a parameter because asking git is work this crate does not do.
/// It is called only for a receipt that could regress, so a long ledger costs
/// one git call per fix the loop has claimed and none for the rest.
pub fn sweep(
    receipts: &[ClosureReceipt],
    mut present_now: impl FnMut(&Citation) -> bool,
) -> Report {
    let mut report = Report::default();
    for receipt in receipts {
        let present = match (&receipt.by, receipt.present_at_close) {
            (Some(cite), Some(true)) => present_now(cite),
            _ => false,
        };
        match check(receipt, present) {
            Check::Holds => report.checked += 1,
            Check::Regressed => {
                report.checked += 1;
                report.findings.push(finding(receipt));
            }
            Check::Skipped(skip) => report.skipped.push(skip),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(key: &str, by: Option<Citation>, present: Option<bool>) -> ClosureReceipt {
        ClosureReceipt {
            key: key.to_owned(),
            closed_at: "2026-09-01T00:00:00Z".to_owned(),
            by,
            present_at_close: present,
        }
    }

    fn pr(key: &str) -> Option<Citation> {
        Some(Citation::PullRequest {
            key: key.to_owned(),
        })
    }

    /// **The witness.** A fix that was on the base and has left it is filed
    /// again.
    #[test]
    fn a_fix_that_has_left_the_base_is_filed_again() {
        let rows = vec![receipt("4100", pr("4101"), Some(true))];

        let report = sweep(&rows, |_| false);

        assert_eq!(report.checked, 1);
        assert!(report.skipped.is_empty());
        assert_eq!(report.findings.len(), 1);
        assert!(
            report.findings[0].title.contains("4100"),
            "the finding must name the issue: {}",
            report.findings[0].title
        );
        assert!(report.findings[0].body.contains("4101"));
    }

    /// A fix still on the base files nothing.
    #[test]
    fn a_fix_still_on_the_base_files_nothing() {
        let rows = vec![receipt("4100", pr("4101"), Some(true))];

        let report = sweep(&rows, |_| true);

        assert_eq!(report.checked, 1);
        assert!(report.findings.is_empty());
    }

    /// A closure that cited no change cannot be re-checked, and the count of
    /// those is reported rather than hidden.
    #[test]
    fn a_closure_with_no_change_cited_is_counted_not_filed() {
        let rows = vec![receipt("4100", None, None)];

        let report = sweep(&rows, |_| false);

        assert_eq!(report.checked, 0);
        assert_eq!(report.skipped, vec![Skip::NoChangeCited]);
        assert!(report.findings.is_empty());
    }

    /// A change nobody could see at close time is skipped. Filing it would
    /// report *gone* for something that was never there.
    #[test]
    fn a_change_unseen_at_close_is_never_reported_as_gone() {
        let unknown = receipt("1", pr("2"), None);
        let absent = receipt("3", pr("4"), Some(false));

        let report = sweep(&[unknown, absent], |_| false);

        assert_eq!(report.checked, 0);
        assert_eq!(
            report.skipped,
            vec![Skip::UnknownAtClose, Skip::AbsentAtClose]
        );
        assert!(report.findings.is_empty());
    }

    /// A commit citation is checked the same way a pull request is.
    #[test]
    fn a_commit_citation_is_checked_too() {
        let rows = vec![receipt(
            "77",
            Some(Citation::Commit {
                sha: "f8935f2".to_owned(),
            }),
            Some(true),
        )];

        let report = sweep(&rows, |_| false);

        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].body.contains("commit f8935f2"));
    }

    /// A receipt round-trips, so a line written by one build is read by the
    /// next.
    #[test]
    fn a_receipt_round_trips() {
        let row = receipt("12", pr("13"), Some(true));

        let text = serde_json::to_string(&row).expect("serialize");
        let back: ClosureReceipt = serde_json::from_str(&text).expect("parse");

        assert_eq!(back, row);
    }

    /// An older line, written before the fields below `key` existed, still
    /// parses. It reads as unsweepable, which is the safe answer.
    #[test]
    fn an_older_receipt_line_still_parses() {
        let back: ClosureReceipt =
            serde_json::from_str(r#"{"key":"12"}"#).expect("parse the older shape");

        assert_eq!(back.by, None);
        assert_eq!(check(&back, false), Check::Skipped(Skip::NoChangeCited));
    }

    /// Two returning defects file under two keys, because the digest keeps
    /// the issue number.
    #[test]
    fn two_returning_fixes_are_two_findings() {
        let rows = vec![
            receipt("100", pr("101"), Some(true)),
            receipt("200", pr("201"), Some(true)),
        ];

        let report = sweep(&rows, |_| false);

        let first = crate::finding_digest(&report.findings[0].title);
        let second = crate::finding_digest(&report.findings[1].title);
        assert_ne!(first, second);
    }
}
