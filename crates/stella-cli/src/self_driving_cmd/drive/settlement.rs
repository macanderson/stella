//! How a pull request ends, and what its ending finishes.
//!
//! A sibling of `drive.rs` rather than lines inside it: that file sits close
//! under the 1500-line ceiling `make file-size` enforces and carries no
//! baseline entry, so a crossing fails the gate outright (AGENTS.md § *God
//! files* — plan around them, never into them). The subject stands on its own
//! anyway — it is pure, and the whole of it is the distinction the loop was
//! missing (#4119).

use stella_autonomy::{CarriedPr, IssueRef, PrRef};

/// How one pass of [`super::advance`] left a pull request.
///
/// Three outcomes rather than the `bool` this returned, because the caller has
/// to tell a merge from the other ways of settling: an issue is closed by its
/// fix landing, and by nothing else. Escalating means a human now owns it and
/// the work is not done; a `bool` collapsed those two into "stop carrying it"
/// and left the loop with no point at which it knew a fix had shipped (#4119).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Settlement {
    /// The fix landed on the base branch.
    Merged,
    /// Done with, by any route that is not a merge — escalated, rebased, or
    /// already settled by a human before this pass looked.
    Otherwise,
    /// Still moving; the loop carries it to the next poll.
    Pending,
}

/// Which issue, if any, a settled pull request has just finished.
///
/// Pure, so the decision has a seam of its own rather than only being
/// reachable through a forge read. Three answers, and two of them are `None`
/// for reasons a reader has to be able to tell apart:
///
/// - **A merge finishes the issue it carried.** That is the whole of #4119:
///   the loop already knows both halves at this instant and knew them nowhere
///   else.
/// - **Any other settlement finishes nothing.** An escalated or rebased pull
///   request has been handed to a human, and the work it was for is not done.
/// - **A pull request with no issue on it finishes nothing either.**
///   `super::resume` re-derives what an earlier run left open from the forge,
///   which yields a `CarriedPr` with `issue: None` — the loop knows the pull
///   request but not what it was for, and guessing would close by coincidence.
pub(super) fn issue_finished_by(
    settled: Settlement,
    pr: &PrRef,
    carrying: &[CarriedPr],
) -> Option<IssueRef> {
    if settled != Settlement::Merged {
        return None;
    }
    carrying
        .iter()
        .find(|carried| &carried.pr == pr)
        .and_then(|carried| carried.issue.clone())
}
