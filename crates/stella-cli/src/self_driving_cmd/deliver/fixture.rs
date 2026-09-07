// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A forge that lives in memory, for the tests of everything above the port.
//!
//! It spawns no process and opens no socket. So a test that drives the
//! delivery loop's pull request steps through it needs neither `gh` nor a
//! network. [`witness`](super::witness) rests on that.
//!
//! It records every call in order. The journal lets a test assert the steps
//! that ran, not just the state they left. Without it, a merge that never
//! asked the forge to merge would pass on a fixture that merges by itself.

use std::collections::HashMap;
use std::sync::Mutex;

use stella_protocol::pull_request::{
    Check, MergeStatus, PullRequest, PullRequestDraft, PullRequestError, PullRequestKey,
    PullRequestProvider, PullRequestState, PullRequestSummary, ReviewDecision,
};

/// What the forge holds, behind one lock.
///
/// One lock rather than one per field. Every method here is short. A test
/// that has to work out which halves of the forge moved together is a test
/// somebody will read wrong.
#[derive(Default)]
struct Inner {
    /// Pull requests by key.
    prs: HashMap<String, PullRequest>,
    /// Checks by ref — a branch name or a commit id.
    checks: HashMap<String, Vec<Check>>,
    /// The check names the repository requires, if it declares any.
    required: Vec<String>,
    /// Commit ids on the base, newest first.
    commits: Vec<String>,
    /// The number the next opened pull request gets.
    next: u64,
    /// Every call, in order.
    journal: Vec<String>,
    /// Whether syncing a branch with its base succeeds.
    ///
    /// A forge declines when the branch is already level with its base. That
    /// refusal sends the loop to its re-run fallback.
    sync_succeeds: bool,
}

/// A forge in memory.
pub(crate) struct FixtureForge {
    inner: Mutex<Inner>,
}

impl FixtureForge {
    /// An empty forge that numbers its first pull request `1`.
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next: 1,
                sync_succeeds: true,
                ..Inner::default()
            }),
        }
    }

    /// Borrow the state, recovering a lock a panicking test poisoned.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Record a call.
    fn note(&self, call: impl Into<String>) {
        self.lock().journal.push(call.into());
    }

    /// Say what `git_ref` reports.
    pub(crate) fn set_checks(&self, git_ref: &str, checks: Vec<Check>) {
        self.lock().checks.insert(git_ref.to_owned(), checks);
    }

    /// Say what the repository requires.
    pub(crate) fn set_required(&self, required: &[&str]) {
        self.lock().required = required.iter().map(|name| (*name).to_owned()).collect();
    }

    /// Say which commits the base carries, newest first.
    pub(crate) fn set_commits(&self, commits: &[&str]) {
        self.lock().commits = commits.iter().map(|sha| (*sha).to_owned()).collect();
    }

    /// Say whether a base sync succeeds.
    pub(crate) fn set_sync_succeeds(&self, succeeds: bool) {
        self.lock().sync_succeeds = succeeds;
    }

    /// Read one pull request back.
    pub(crate) fn pr(&self, key: &str) -> Option<PullRequest> {
        self.lock().prs.get(key).cloned()
    }

    /// Every call so far, in order.
    pub(crate) fn journal(&self) -> Vec<String> {
        self.lock().journal.clone()
    }

    /// The failure a forge reports when it has no such pull request.
    fn missing(key: &PullRequestKey) -> PullRequestError {
        PullRequestError::NotFound { key: key.clone() }
    }

    /// The failure a forge reports when it declines.
    fn refused(reason: &str) -> PullRequestError {
        PullRequestError::Failed {
            provider: "fixture".to_owned(),
            reason: reason.to_owned(),
        }
    }
}

impl PullRequestProvider for FixtureForge {
    fn id(&self) -> &str {
        "fixture"
    }

    fn open(&self, draft: &PullRequestDraft) -> Result<PullRequestKey, PullRequestError> {
        self.note(format!("open {}", draft.head_ref));
        let mut inner = self.lock();
        let number = inner.next;
        inner.next += 1;
        let key = PullRequestKey(number.to_string());
        inner.prs.insert(
            key.0.clone(),
            PullRequest {
                key: key.clone(),
                state: PullRequestState::Open,
                draft: draft.draft,
                base_ref: "main".to_owned(),
                head_ref: draft.head_ref.clone(),
                title: draft.title.clone(),
                body: draft.body.clone(),
                merge_status: MergeStatus::Clean,
                review: ReviewDecision::None,
                checks: Vec::new(),
                labels: Vec::new(),
            },
        );
        Ok(key)
    }

    fn get(&self, key: &PullRequestKey) -> Result<PullRequest, PullRequestError> {
        self.note(format!("get {key}"));
        let inner = self.lock();
        let mut pr = inner
            .prs
            .get(key.as_str())
            .cloned()
            .ok_or_else(|| Self::missing(key))?;
        // A pull request's rollup is whatever its head reports. So a test
        // sets the checks once, and both reads answer from one table.
        if let Some(checks) = inner.checks.get(&pr.head_ref) {
            pr.checks = checks.clone();
        }
        Ok(pr)
    }

    fn list_open(&self, search: Option<&str>) -> Result<Vec<PullRequestSummary>, PullRequestError> {
        self.note(format!("list {}", search.unwrap_or("*")));
        let inner = self.lock();
        let mut rows: Vec<PullRequestSummary> = inner
            .prs
            .values()
            .filter(|pr| pr.state == PullRequestState::Open)
            .filter(|pr| {
                // The forge's own search is full text over the title and the
                // body. That is why the caller filters again, more narrowly.
                search.is_none_or(|needle| pr.title.contains(needle) || pr.body.contains(needle))
            })
            .map(|pr| PullRequestSummary {
                key: pr.key.clone(),
                title: pr.title.clone(),
                body: pr.body.clone(),
                head_ref: pr.head_ref.clone(),
            })
            .collect();
        rows.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(rows)
    }

    fn checks_for_ref(&self, git_ref: &str) -> Result<Vec<Check>, PullRequestError> {
        self.note(format!("checks {git_ref}"));
        self.lock()
            .checks
            .get(git_ref)
            .cloned()
            .ok_or_else(|| Self::refused(&format!("nothing known about `{git_ref}`")))
    }

    fn required_checks(&self, branch: &str) -> Result<Vec<String>, PullRequestError> {
        self.note(format!("required {branch}"));
        Ok(self.lock().required.clone())
    }

    fn recent_commits(&self, branch: &str, depth: usize) -> Result<Vec<String>, PullRequestError> {
        self.note(format!("commits {branch} {depth}"));
        Ok(self.lock().commits.iter().take(depth).cloned().collect())
    }

    fn mark_ready(&self, key: &PullRequestKey) -> Result<(), PullRequestError> {
        self.note(format!("ready {key}"));
        let mut inner = self.lock();
        let pr = inner
            .prs
            .get_mut(key.as_str())
            .ok_or_else(|| Self::missing(key))?;
        pr.draft = false;
        Ok(())
    }

    fn add_label(&self, key: &PullRequestKey, label: &str) -> Result<(), PullRequestError> {
        self.note(format!("label {key} {label}"));
        let mut inner = self.lock();
        let pr = inner
            .prs
            .get_mut(key.as_str())
            .ok_or_else(|| Self::missing(key))?;
        pr.labels.push(label.to_owned());
        Ok(())
    }

    fn merge(&self, key: &PullRequestKey) -> Result<(), PullRequestError> {
        self.note(format!("merge {key}"));
        let mut inner = self.lock();
        let pr = inner
            .prs
            .get_mut(key.as_str())
            .ok_or_else(|| Self::missing(key))?;
        pr.state = PullRequestState::Merged;
        Ok(())
    }

    fn sync_with_base(&self, key: &PullRequestKey) -> Result<(), PullRequestError> {
        self.note(format!("sync {key}"));
        if self.lock().sync_succeeds {
            Ok(())
        } else {
            Err(Self::refused("the branch is already level with its base"))
        }
    }

    fn rerun_failed_checks(&self, key: &PullRequestKey) -> Result<(), PullRequestError> {
        self.note(format!("rerun {key}"));
        Ok(())
    }
}
