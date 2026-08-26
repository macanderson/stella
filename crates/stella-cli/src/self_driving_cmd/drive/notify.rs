// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What `drive` tells a subscriber, and when it stops repeating itself
//! (#4017).
//!
//! The dispatch itself lives in [`super::super::hooks`]; this holds the part
//! that is `drive`'s own — which of its moments are events, and the latches
//! that keep a polling loop from turning a standing condition into a stream of
//! identical wakes. An event fires on a **change of answer**, never on an
//! observation, because a subscriber woken every `poll_secs` by the same green
//! stops reading and the next wake it ignores is the one that mattered.

use std::path::Path;

use stella_autonomy::PrState;

use super::super::hooks::{self, HookEvent, HookIssueInfo, HookPullRequestInfo, HookRunInfo};
use super::Spent;
use crate::settings::Settings;

/// Report a pull request's newly-observed state, if it changed.
///
/// The event comes from the state the pure machine landed in rather than from
/// the action it chose. The state is the observation; the action is what this
/// build happens to do about it, and a subscriber asking "is the base broken"
/// must not have to know which of those it is reading — this build does not
/// author fixes, and one that did would take different actions from identical
/// observations.
///
/// [`HookEvent::ChecksFailed`] and [`HookEvent::BaseBroken`] are two events for
/// the reason the machine has two states: one is this change's fault and one is
/// not. Collapsing them is how a loop spends its budget fixing somebody else's
/// breakage.
pub(super) fn observed(
    root: &Path,
    settings: &Settings,
    entry: &mut Spent,
    state: PrState,
    pr: &str,
) {
    if entry.reported == Some(state) {
        return;
    }
    entry.reported = Some(state);
    let event = match state {
        PrState::CiRed => HookEvent::ChecksFailed,
        PrState::BaseBroken => HookEvent::BaseBroken,
        PrState::Conflicted => HookEvent::PullRequestConflicted,
        PrState::ReadyForReview | PrState::Approved => HookEvent::ChecksGreen,
        // Nothing has been observed about the checks yet, or the answer is
        // about a human rather than about the build. Total by `match` so a
        // new state has to be placed rather than silently reported as
        // nothing.
        PrState::Draft
        | PrState::CiPending
        | PrState::ReviewChangesRequested
        | PrState::Merged
        | PrState::Escalated => return,
    };
    hooks::pull_request(root, settings, event, HookPullRequestInfo::new(pr), None);
}

/// Report one pull-request transition this loop performed.
///
/// Separate from [`observed`] and unlatched, because these are things the loop
/// *did*: each happens once, and a second one is a second merge rather than a
/// repeated reading.
pub(super) fn performed(
    root: &Path,
    settings: &Settings,
    event: HookEvent,
    pr: &str,
    issue: Option<&str>,
) {
    let subject = match issue {
        Some(issue) => HookPullRequestInfo::new(pr).for_issue(issue),
        None => HookPullRequestInfo::new(pr),
    };
    hooks::pull_request(root, settings, event, subject, None);
}

/// Report a run-level event: the spend ceiling, or a refusal to run.
pub(super) fn run(
    root: &Path,
    settings: &Settings,
    event: HookEvent,
    session: &str,
    reason: String,
) {
    hooks::drive(
        root,
        settings,
        event,
        HookRunInfo::new(session),
        Some(reason),
    );
}

/// Report an issue handed back to a human.
///
/// Fired beside the audit record rather than instead of it, and only where the
/// escalation label actually landed: the ledger is what a person reads
/// afterwards, this is what a subscriber acts on now, and "escalated" has to
/// mean the same thing to both.
pub(super) fn escalated(root: &Path, settings: &Settings, key: &str, why: &str) {
    hooks::tracker(
        root,
        settings,
        HookEvent::IssueEscalated,
        HookIssueInfo::new(key),
        Some(why.to_string()),
    );
}

/// Report that the loop declined to run, unless it already said so.
///
/// `last` is the refusal this run last reported; a standing block answers the
/// machine the same way on every poll, and a subscriber woken every
/// `poll_secs` for the same reason would stop reading. `drive` clears `last`
/// on any pass that is not blocked, so a block that returns is reported again.
pub(super) fn refused(
    root: &Path,
    settings: &Settings,
    session: &str,
    last: &mut Option<String>,
    refusal: String,
) {
    if last.as_deref() == Some(refusal.as_str()) {
        return;
    }
    *last = Some(refusal.clone());
    run(root, settings, HookEvent::DriveRefused, session, refusal);
}
