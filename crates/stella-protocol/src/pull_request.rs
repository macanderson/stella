//! The pull request kernel, and the port a forge is reached through.
//!
//! The sibling of [`issue`](crate::issue), one plane over. Issues got their
//! seam first. The loop reads `Issue` values, and a tracker is an
//! `IssueProvider`. Pull requests did not. The delivery half of the loop
//! still spelled `gh pr view` and `gh pr merge` into its own reader, and two
//! GitHub REST endpoints beside them. This module is the matching seam. A
//! forge becomes an implementation of [`PullRequestProvider`], and the loop
//! reads [`PullRequest`] values.
//!
//! # The model carries facts, never verdicts
//!
//! A provider answers what the forge said. Draft or not, what state, which
//! checks with which outcome, who reviewed. It decides nothing else. Which
//! checks may block a merge is policy. So is whether a red base excuses a red
//! pull request, and what to do next. All of it stays above the port, in
//! `stella-cli`'s `deliver` module and in `stella-autonomy`'s pure machine.
//!
//! # The port is synchronous
//!
//! [`IssueProvider`](crate::issue::IssueProvider) is async, and its callers
//! each build a runtime to block on it. This one is not. Every caller is a
//! plain verb of the delivery loop, and the shipped adapter runs a
//! subprocess, which blocks whatever calls it. A provider that speaks HTTP
//! holds its own runtime, the way those callers already do.

use serde::{Deserialize, Serialize};

/// A forge's own name for one pull request — `"4022"` on GitHub.
///
/// A newtype for [`IssueKey`](crate::issue::IssueKey)'s reason. A bare
/// `String` beside a branch name and a title is a swap the compiler cannot
/// see.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PullRequestKey(pub String);

impl PullRequestKey {
    /// The key as the forge spells it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PullRequestKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for PullRequestKey {
    fn from(raw: &str) -> Self {
        Self(raw.to_owned())
    }
}

/// What went wrong reaching a forge.
///
/// The cases are [`IssueError`](crate::issue::IssueError)'s, chosen the same
/// way: by what a caller has to do differently. An unavailable forge is
/// retried or degraded. An unauthenticated one needs a human. A missing pull
/// request is a permanent answer. A payload that will not parse is a bug in
/// the provider.
#[derive(Debug, thiserror::Error)]
pub enum PullRequestError {
    /// The provider's transport is not installed or not on `PATH`.
    #[error("pull request provider `{provider}` is unavailable: {reason}")]
    Unavailable {
        /// The provider id that could not be reached.
        provider: String,
        /// What was missing, in terms a human can act on.
        reason: String,
    },
    /// The transport is present but the caller is not authenticated to it.
    #[error("pull request provider `{provider}` is not authenticated: {reason}")]
    Unauthenticated {
        /// The provider id that refused.
        provider: String,
        /// What the forge said.
        reason: String,
    },
    /// No pull request with that key.
    #[error("no such pull request: {key}")]
    NotFound {
        /// The key that resolved to nothing.
        key: PullRequestKey,
    },
    /// The forge answered, and the answer did not parse.
    #[error(
        "pull request provider `{provider}` returned a payload this build cannot read: {reason}"
    )]
    Malformed {
        /// The provider id whose payload was rejected.
        provider: String,
        /// The parse failure.
        reason: String,
    },
    /// The forge answered with a failure of its own.
    #[error("pull request provider `{provider}` failed: {reason}")]
    Failed {
        /// The provider id that failed.
        provider: String,
        /// What it said.
        reason: String,
    },
}

/// Where one check got to.
///
/// Four outcomes rather than a pass/fail pair, because the loop branches on
/// all four. [`Running`](CheckOutcome::Running) means wait.
/// [`Inert`](CheckOutcome::Inert) means this check will never answer, so
/// nothing should wait on it. The other two are the verdicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    /// It ran and it passed.
    Passed,
    /// It ran and it failed. A service that broke while running its own check
    /// belongs here too. For every purpose the loop has, that is a failure.
    Failed,
    /// It has not answered yet.
    Running,
    /// It was skipped, cancelled, or reported neutral. Not a pass and not a
    /// failure: it did not run. A loop that waited on one would wait for ever
    /// on a job that is skipped by design.
    Inert,
}

/// One check as a forge reports it, reduced to what a decision reads.
///
/// Whatever dialect the forge speaks, a provider answers a name and an
/// outcome. The name is the join key. A pull request's failure is excused by
/// its base only when the check *of that name* fails there too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// What the forge calls this check.
    pub name: String,
    /// Where it got to.
    pub outcome: CheckOutcome,
}

/// Where a pull request has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    /// Still open, merged into nothing yet.
    Open,
    /// Merged.
    Merged,
    /// Closed without merging.
    Closed,
}

impl PullRequestState {
    /// Whether nothing is left to decide about this pull request.
    ///
    /// Both end states, so a caller cannot ask about one and forget the
    /// other. A merged pull request reports its mergeability as
    /// [`Unknown`](MergeStatus::Unknown). A decision machine reads that as
    /// "wait", and waits for ever unless something says the wait is over.
    #[must_use]
    pub fn settled(self) -> bool {
        matches!(self, Self::Merged | Self::Closed)
    }
}

/// Whether the forge believes this pull request can be merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStatus {
    /// It merges cleanly.
    Clean,
    /// It conflicts with its base.
    Conflicted,
    /// Nobody can say yet. A forge reports this before it has worked the
    /// answer out. So it gets its own case, not a hopeful
    /// [`Clean`](MergeStatus::Clean). Reading it as clean is how a merge is
    /// tried into a conflict.
    Unknown,
}

/// What human review concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Someone approved it.
    Approved,
    /// Someone asked for changes.
    ChangesRequested,
    /// Nobody has reviewed it, or the repository asks for no review.
    None,
}

/// One read of a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    /// The forge's number for it.
    pub key: PullRequestKey,
    /// Open, merged, or closed.
    pub state: PullRequestState,
    /// Whether it is still a draft.
    pub draft: bool,
    /// The branch it merges into, as the forge names it. Never a
    /// remote-tracking name. A caller that resolved `origin/<base>` locally
    /// would read whatever the last fetch left behind.
    pub base_ref: String,
    /// The branch it merges from.
    pub head_ref: String,
    /// Its title.
    pub title: String,
    /// Its description.
    pub body: String,
    /// Whether the forge thinks it merges.
    pub merge_status: MergeStatus,
    /// What review concluded.
    pub review: ReviewDecision,
    /// Every check the forge ties to its head, in whatever dialect they
    /// arrived.
    pub checks: Vec<Check>,
    /// The labels on it.
    pub labels: Vec<String>,
}

/// A prospective pull request, before a forge has assigned it a number.
///
/// A separate type from [`PullRequest`], not one with an empty key.
/// [`IssueDraft`](crate::issue::IssueDraft) splits off from an issue for the
/// same reason. State, mergeability, review and checks are the forge's to
/// decide. A type whose wrong states cannot be written beats one whose caller
/// has to remember which fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestDraft {
    /// The branch to merge from. It must already be pushed.
    pub head_ref: String,
    /// The title.
    pub title: String,
    /// The description.
    pub body: String,
    /// Whether to open it as a draft.
    pub draft: bool,
}

/// A pull request as a listing reports it.
///
/// Lighter than [`PullRequest`]. A listing carries no checks, and a caller
/// reading a list does not want one check read per row. The fields are what
/// the loop's three listings ask about: which branch a restarted run was
/// carrying, and whether another pull request already names an issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSummary {
    /// The forge's number for it.
    pub key: PullRequestKey,
    /// Its title.
    pub title: String,
    /// Its description.
    pub body: String,
    /// The branch it merges from.
    pub head_ref: String,
}

/// Reading and driving pull requests on one forge.
///
/// Every method answers a fact or makes one change. None of them decides
/// anything. See the module docs.
pub trait PullRequestProvider: Send + Sync {
    /// Stable id for this provider, e.g. `"github"` — what an error names and
    /// what a workspace binds to.
    fn id(&self) -> &str;

    /// Open a pull request and return the key the forge assigned it.
    ///
    /// The key is the whole return value, for the reason
    /// [`IssueProvider::file`](crate::issue::IssueProvider::file)'s is. A pull
    /// request the caller cannot name again can be neither read nor merged.
    fn open(&self, draft: &PullRequestDraft) -> Result<PullRequestKey, PullRequestError>;

    /// Read one pull request, checks included.
    fn get(&self, key: &PullRequestKey) -> Result<PullRequest, PullRequestError>;

    /// Open pull requests, optionally narrowed by the forge's own text search.
    ///
    /// The search is the forge's, so its precision is the forge's too. A
    /// caller that needs an exact match filters what comes back. It does not
    /// trust the query.
    fn list_open(&self, search: Option<&str>) -> Result<Vec<PullRequestSummary>, PullRequestError>;

    /// Every check the forge associates with a ref — a branch name or a commit
    /// id.
    ///
    /// Named to the forge, never resolved locally first, so no local state is
    /// left to go stale.
    ///
    /// An error means nobody could say. That is not the same as an empty list.
    /// A caller weighing a pull request against its base needs both answers.
    /// An unread base is no evidence that the base is fine.
    fn checks_for_ref(&self, git_ref: &str) -> Result<Vec<Check>, PullRequestError>;

    /// The check names this repository requires to merge into `branch`.
    ///
    /// An error covers two cases. There may be no permission to read the
    /// protection document, or no protection set at all. A caller treats them
    /// the same, because it learns nothing about what is required. What it
    /// does then is policy, not the provider's business.
    fn required_checks(&self, branch: &str) -> Result<Vec<String>, PullRequestError>;

    /// The newest `depth` commit ids on `branch`, newest first.
    ///
    /// Feeds one question. Has a check failed on the base for long enough to
    /// be nobody's to fix?
    fn recent_commits(&self, branch: &str, depth: usize) -> Result<Vec<String>, PullRequestError>;

    /// Take a pull request out of draft.
    fn mark_ready(&self, key: &PullRequestKey) -> Result<(), PullRequestError>;

    /// Add a label.
    fn add_label(&self, key: &PullRequestKey, label: &str) -> Result<(), PullRequestError>;

    /// Merge it.
    ///
    /// A pull request that is **already** merged is where this method was
    /// going. So a provider answers `Ok` for it, not an error. Racing a human
    /// who merged by hand must not read as a failure.
    fn merge(&self, key: &PullRequestKey) -> Result<(), PullRequestError>;

    /// Merge the base branch into this pull request's head.
    ///
    /// The remedy for a check that ran against a base somebody has since
    /// repaired. A forge may decline, because the branch is already level with
    /// its base. That is an answer, and it arrives as an error a caller can
    /// fall back from.
    fn sync_with_base(&self, key: &PullRequestKey) -> Result<(), PullRequestError>;

    /// Run this pull request's failed checks again.
    ///
    /// The fallback when there is no staleness to clear. A pull request with
    /// nothing failing has nothing to re-run. That is an error, not a quiet
    /// success: a caller reached this because it saw red.
    fn rerun_failed_checks(&self, key: &PullRequestKey) -> Result<(), PullRequestError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both terminal states settle, and an open one does not.
    ///
    /// This is what stops a loop re-reading a pull request it has already
    /// merged. Merged reports [`MergeStatus::Unknown`], which reads as "wait"
    /// for as long as anything keeps asking.
    #[test]
    fn merged_and_closed_are_the_states_with_nothing_left_to_decide() {
        assert!(PullRequestState::Merged.settled());
        assert!(PullRequestState::Closed.settled());
        assert!(!PullRequestState::Open.settled());
    }

    /// Every type crossing a crate boundary round-trips (AGENTS.md #4).
    #[test]
    fn a_pull_request_round_trips_through_json() {
        let pr = PullRequest {
            key: PullRequestKey::from("4022"),
            state: PullRequestState::Open,
            draft: true,
            base_ref: "main".to_owned(),
            head_ref: "fix/4022".to_owned(),
            title: "a title".to_owned(),
            body: "Closes #4022".to_owned(),
            merge_status: MergeStatus::Clean,
            review: ReviewDecision::None,
            checks: vec![Check {
                name: "fmt + clippy + test".to_owned(),
                outcome: CheckOutcome::Passed,
            }],
            labels: vec!["stella-verified-locally".to_owned()],
        };
        let text = serde_json::to_string(&pr).expect("serialize");
        assert_eq!(
            serde_json::from_str::<PullRequest>(&text).expect("deserialize"),
            pr
        );
    }
}
