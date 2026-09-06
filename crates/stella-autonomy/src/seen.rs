//! The dedup set, and the one thing that ages a line out of it.
//!
//! `seen.txt` holds one digest per line. A digest in it drops a repeat
//! before anything is sent. That is what stops the loop filing one defect
//! twice.
//!
//! Nothing ages a line out on its own. So a defect the loop found, filed,
//! fixed and closed can come back a year later, and the set drops it in
//! silence. "We know about this" and "we knew about this once, and the cause
//! is back" are different answers. The second is the one the loop is for.
//!
//! What decays a line is the closure of the issue it became. A closure that
//! cites a change is the loop saying that defect is fixed. Stating the same
//! finding after that is news, so the line stops suppressing and the finding
//! is filed again.
//!
//! A closure that cites nothing decays nothing. Somebody declined that work,
//! or it was a copy of another issue, and neither of those is a fix.
//!
//! Two rules keep an old set safe:
//!
//! - A line with no filing record beside it never decays. Every `seen.txt`
//!   written before this module existed is such a line, so an upgrade files
//!   nothing again.
//! - One closed issue does not decay a line that another issue still holds
//!   open.
//!
//! [`crate::finding_digest`] is untouched here. What a line says is a
//! byte-for-byte contract with every machine that has run the loop, so the
//! record that decays a line lives in a second file.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// One filing the loop made: which finding, and which issue it became.
///
/// One line of `filings.jsonl` in the loop's state directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    /// The dedup digest, spelled as `seen.txt` spells it.
    pub digest: String,
    /// The issue key the tracker gave back, with no leading `#`.
    pub key: String,
    /// When it was filed, RFC3339. Empty on a record that predates this
    /// field.
    #[serde(default)]
    pub at: String,
}

impl Filing {
    /// Record that `digest` was filed as `key` at `at`.
    #[must_use]
    pub fn new(digest: &str, key: &str, at: &str) -> Self {
        Self {
            digest: digest.trim().to_owned(),
            key: issue_key(key).to_owned(),
            at: at.trim().to_owned(),
        }
    }
}

/// An issue key with its decoration removed, so `#412` and ` 412 ` match.
fn issue_key(raw: &str) -> &str {
    raw.trim().trim_start_matches('#').trim()
}

/// The seen lines that still drop a repeat.
///
/// `fixed` holds the keys of issues the loop closed on a cited change. Pass
/// the receipts it wrote as it closed them, and nothing else: a closure that
/// cited no change is not a claim that anything was fixed.
///
/// Order is kept, so the result reads as the file reads.
#[must_use]
pub fn live(seen: &[String], filings: &[Filing], fixed: &[String]) -> Vec<String> {
    let fixed: BTreeSet<&str> = fixed.iter().map(|key| issue_key(key.as_str())).collect();
    seen.iter()
        .filter(|line| !decayed(line.trim(), filings, &fixed))
        .cloned()
        .collect()
}

/// Whether one digest has stopped suppressing.
fn decayed(digest: &str, filings: &[Filing], fixed: &BTreeSet<&str>) -> bool {
    if digest.is_empty() {
        return false;
    }
    let mut records = filings
        .iter()
        .filter(|filing| filing.digest.trim() == digest)
        .peekable();
    // No record at all. The line predates the ledger, so its age is unknown,
    // and an unknown age reads as live. Reading it the other way would file
    // every finding the loop has ever filed, all at once.
    if records.peek().is_none() {
        return false;
    }
    records.all(|filing| fixed.contains(issue_key(&filing.key)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding_digest;

    fn filed(title: &str, key: &str) -> Filing {
        Filing::new(&finding_digest(title), key, "2026-09-05T00:00:00Z")
    }

    const DEFECT: &str = "crates/stella-core/src/driver.rs:812 retry counter never reset";
    const KEY: &str = "4100";

    /// **The witness.** A defect the loop filed, fixed and closed is offered
    /// again once its cause comes back.
    ///
    /// Before this module, `seen.txt` was the whole answer and held no issue
    /// key, so nothing could ask whether the issue had closed. The digest
    /// stayed in the set and the returning defect was dropped in silence.
    #[test]
    fn a_defect_whose_fix_was_closed_is_offered_again() {
        let seen = vec![finding_digest(DEFECT)];
        let filings = vec![filed(DEFECT, KEY)];

        let kept = live(&seen, &filings, &[format!("#{KEY}")]);

        assert!(
            kept.is_empty(),
            "a closed fix must stop suppressing, got {kept:?}"
        );
    }

    /// A repeat of a finding whose issue is still open is still dropped.
    #[test]
    fn a_repeat_is_dropped_while_its_issue_is_open() {
        let seen = vec![finding_digest(DEFECT)];
        let filings = vec![filed(DEFECT, KEY)];

        let kept = live(&seen, &filings, &[]);

        assert_eq!(kept, seen, "an open issue holds its line");
    }

    /// A `seen.txt` written before `filings.jsonl` existed decays nothing.
    #[test]
    fn a_line_with_no_filing_record_never_decays() {
        let seen = vec![
            finding_digest(DEFECT),
            finding_digest("a second defect entirely"),
        ];

        let kept = live(&seen, &[], &[KEY.to_owned()]);

        assert_eq!(kept, seen, "a legacy set is read whole");
    }

    /// One closed issue does not decay a line another issue still holds.
    #[test]
    fn an_open_filing_beside_a_closed_one_holds_the_line() {
        let seen = vec![finding_digest(DEFECT)];
        let filings = vec![filed(DEFECT, KEY), filed(DEFECT, "4200")];

        let kept = live(&seen, &filings, &[KEY.to_owned()]);

        assert_eq!(kept, seen, "the open issue still holds the line");
    }

    /// A closure that cited nothing is not a fix, so it decays nothing. The
    /// caller passes only cited closures, and an empty `fixed` list is what
    /// that looks like here.
    #[test]
    fn a_closure_citing_nothing_decays_nothing() {
        let seen = vec![finding_digest(DEFECT)];
        let filings = vec![filed(DEFECT, KEY)];

        let kept = live(&seen, &filings, &[]);

        assert_eq!(kept, seen);
    }

    /// Blank lines and unmatched records leave the set alone.
    #[test]
    fn a_blank_line_survives_and_an_unmatched_record_is_ignored() {
        let seen = vec![String::new(), finding_digest(DEFECT)];
        let filings = vec![filed("some other finding", KEY)];

        let kept = live(&seen, &filings, &[KEY.to_owned()]);

        assert_eq!(kept, seen);
    }
}
