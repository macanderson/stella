// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! When each hook event fires, in the words an install prompt shows.
//!
//! One sentence per event, on `consent.rs`'s `point_moment` terms: a person
//! deciding whether to grant a hook is deciding when a third party's process
//! gets to run, and "PostToolUse" is not that sentence.
//!
//! **Exhaustive by construction** — a new [`crate::HookEvent`] does not compile
//! until it has words here, which is the point of a match over a lookup table.
//!
//! A sibling file rather than a section of `consent.rs`, per AGENTS.md §
//! "God files": the parent is at the 1500-line ratchet and the vocabulary this
//! mirrors grew by seventeen events (#4017).

use crate::manifest::HookEvent;

/// When `event` fires.
pub(super) fn hook_moment(event: HookEvent) -> &'static str {
    match event {
        HookEvent::SessionStart => "when a session starts",
        HookEvent::PreToolUse => "before every tool call",
        HookEvent::PostToolUse => "after every tool call",
        HookEvent::Stop => "when a turn is about to finish",
        HookEvent::PreCompact => "before the context is compacted",
        HookEvent::PreIssueWork => "before the self-driving loop works an issue",
        HookEvent::PostIssueWork => "after the self-driving loop works an issue",
        HookEvent::DriveRunStart => "when a self-driving run starts",
        HookEvent::DriveRunEnd => "when a self-driving run ends",
        HookEvent::DriveCycleStart => "when a self-driving cycle starts",
        HookEvent::DriveCycleEnd => "when a self-driving cycle ends",
        HookEvent::DriveIdle => "when a self-driving cycle produced nothing",
        HookEvent::IssueCreated => "when the loop files an issue",
        HookEvent::IssueClosed => "when the loop closes an issue",
        HookEvent::IssueEscalated => "when the loop hands an issue to a human",
        HookEvent::PullRequestOpened => "when the loop opens a pull request",
        HookEvent::PullRequestReadyForReview => "when the loop takes a pull request out of draft",
        HookEvent::PullRequestConflicted => "when a pull request's base moves under it",
        HookEvent::PullRequestMerged => "when a pull request lands",
        HookEvent::ChecksFailed => "when a pull request's own checks fail",
        HookEvent::BaseBroken => "when the base branch's checks fail",
        HookEvent::ChecksGreen => "when a pull request's checks pass",
        HookEvent::DriveBudgetExhausted => "when the loop reaches its spend ceiling",
        HookEvent::DriveRefused => "when the loop declines to run",
    }
}
