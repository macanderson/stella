//! Everything the loop did, said out loud and written down.
//!
//! An autonomous system that changes a shared repository has to be able to
//! answer, months later and to somebody who was not there: *what did it do, to
//! what, when, and how did that turn out?* A scrollback buffer cannot answer
//! that — it is the least durable surface in the system and it is gone the
//! moment the terminal closes.
//!
//! So every action goes to two places at once. The operator watching sees a
//! line; the record keeps the same fact as one JSON object per line in
//! `audit.jsonl`, beside the loop's other durable state.
//!
//! # Why both, rather than a log file a renderer tails
//!
//! Because they answer different questions and would otherwise drift. The
//! screen line is written for a person watching a run in progress and is
//! allowed to be a sentence. The record is written for a person reconstructing
//! a run afterwards and has to be queryable — which action, against which
//! issue, at what time, with what outcome. Deriving one from the other means
//! parsing prose, and prose is exactly what changes when somebody improves a
//! message.
//!
//! # Append-only, and never the reason a step fails
//!
//! Entries are appended and never rewritten: an audit trail that can be edited
//! by the thing it audits is not one. And a failure to write the record is
//! reported but does not stop the action — a loop that refused to work because
//! it could not log would be trading the work for the paperwork, and the
//! action's effect on the repository is what a reader is reconstructing anyway.

use std::io::Write as _;

use serde::{Deserialize, Serialize};

use super::state::LoopState;

/// What kind of thing happened.
///
/// A closed set rather than free text, so the record can be counted and
/// filtered without parsing sentences. The sentence is [`AuditEntry::outcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Action {
    /// The session started.
    SessionStarted,
    /// The session stopped, and why.
    SessionStopped,
    /// Work already in flight was picked up after a restart.
    Resumed,
    /// An issue was taken off the ranked queue.
    Claimed,
    /// A turn was started against an issue.
    WorkStarted,
    /// A turn left changes on a branch.
    WorkChanged,
    /// A turn ran and changed nothing.
    WorkNoChange,
    /// A turn did not complete.
    WorkFailed,
    /// An issue was skipped because something else appeared to be on it.
    Deferred,
    /// An issue was marked as attempted and unresolved.
    Escalated,
    /// The project's own checks were started against a change, here.
    VerifyStarted,
    /// They passed.
    Verified,
    /// They did not.
    VerifyFailed,
    /// A required check was not enforced, and the grounds for that.
    Waived,
    /// The base branch was found broken with no issue open, so one was filed.
    FiledBaseBreakage,
    /// A turn was asked to place an issue nobody had judged.
    TriageStarted,
    /// An issue was placed, and the labels written.
    Triaged,
    /// A pull request was opened.
    PrOpened,
    /// A pull request was observed.
    PrObserved,
    /// A pull request was merged.
    PrMerged,
    /// A pull request was handed to a human.
    PrEscalated,
    /// An issue was filed.
    IssueFiled,
    /// An issue was closed.
    IssueClosed,
    /// The loop waited, and for what.
    Waited,
    /// Something failed in a way the loop treated as transient.
    Transient,
}

impl Action {
    /// The word that leads a screen line.
    fn label(self) -> &'static str {
        match self {
            Self::SessionStarted => "started",
            Self::SessionStopped => "stopped",
            Self::Resumed => "resumed",
            Self::Claimed => "claimed",
            Self::WorkStarted => "working",
            Self::WorkChanged => "changed",
            Self::WorkNoChange => "no change",
            Self::WorkFailed => "failed",
            Self::Deferred => "deferred",
            Self::Escalated => "escalated",
            Self::VerifyStarted => "verifying",
            Self::Verified => "verified",
            Self::VerifyFailed => "unverified",
            Self::Waived => "waived",
            Self::FiledBaseBreakage => "filed",
            Self::TriageStarted => "triaging",
            Self::Triaged => "triaged",
            Self::PrOpened => "pr opened",
            Self::PrObserved => "pr observed",
            Self::PrMerged => "pr merged",
            Self::PrEscalated => "pr escalated",
            Self::IssueFiled => "filed",
            Self::IssueClosed => "closed",
            Self::Waited => "waiting",
            Self::Transient => "retrying",
        }
    }
}

/// One thing the loop did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AuditEntry {
    /// When, RFC3339 UTC.
    pub at: String,
    /// Which run this belongs to, so a record spanning restarts can be split
    /// back into sessions.
    pub run_id: String,
    /// What kind of thing happened.
    pub action: Action,
    /// What it happened to — an issue key, a pull request number.
    ///
    /// `None` for the session-level entries, which are about the loop rather
    /// than about any one piece of work.
    pub subject: Option<String>,
    /// What happened, in a sentence a person can read.
    pub outcome: String,
}

/// Record an action: print it, then append it.
///
/// Printed first, deliberately. If the append fails the operator has still
/// seen the line, and a warning follows it — the reverse order would make a
/// disk problem look like the loop having stalled.
pub(super) fn record(st: &LoopState, action: Action, subject: Option<&str>, outcome: &str) {
    let subject = subject.map(str::to_owned);

    match &subject {
        Some(s) => println!("[{}] {} {s} — {outcome}", short_time(), action.label()),
        None => println!("[{}] {} — {outcome}", short_time(), action.label()),
    }
    // Flushed so a piped or redirected run shows progress as it happens rather
    // than in blocks when the buffer fills. A loop that runs for hours behind a
    // pipe is one whose output nobody sees until it ends.
    let _ = std::io::stdout().flush();

    let entry = AuditEntry {
        at: crate::timefmt::rfc3339_utc_now(),
        run_id: st
            .current_run_id()
            .unwrap_or_else(|| "unassigned".to_owned()),
        action,
        subject,
        outcome: outcome.to_owned(),
    };

    if let Err(error) = append(st, &entry) {
        // Reported, and not fatal. A loop that refused to work because it could
        // not write its log would trade the work for the paperwork.
        eprintln!("warning: could not append to the audit log: {error}");
    }
}

fn append(st: &LoopState, entry: &AuditEntry) -> std::io::Result<()> {
    let line = serde_json::to_string(entry)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(st.audit_path())?;
    writeln!(file, "{line}")
}

/// `HH:MM:SS`, which is what a person watching a run needs.
///
/// The full timestamp is in the record; a screen line repeating the date on
/// every row would push the actual message off a narrow terminal.
fn short_time() -> String {
    let full = crate::timefmt::rfc3339_utc_now();
    full.split('T')
        .nth(1)
        .and_then(|t| t.split(['.', 'Z']).next())
        .unwrap_or(&full)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action has a label, so no screen line can render as an empty word.
    #[test]
    fn every_action_has_a_label() {
        for action in [
            Action::SessionStarted,
            Action::SessionStopped,
            Action::Resumed,
            Action::Claimed,
            Action::WorkStarted,
            Action::WorkChanged,
            Action::WorkNoChange,
            Action::WorkFailed,
            Action::Deferred,
            Action::Escalated,
            Action::PrOpened,
            Action::PrObserved,
            Action::PrMerged,
            Action::PrEscalated,
            Action::IssueFiled,
            Action::IssueClosed,
            Action::Waited,
            Action::Transient,
        ] {
            assert!(!action.label().is_empty(), "{action:?} has no label");
        }
    }

    /// **The audit witness.** An entry keeps the four things a reader
    /// reconstructing a run needs — when, which run, what kind, and to what —
    /// as *fields*, not as a sentence somebody has to parse.
    #[test]
    fn an_entry_is_queryable_rather_than_prose() {
        let entry = AuditEntry {
            at: "2026-08-19T21:00:00Z".into(),
            run_id: "r-1".into(),
            action: Action::PrOpened,
            subject: Some("4021".into()),
            outcome: "opened for #3591, ci in progress".into(),
        };

        let json = serde_json::to_string(&entry).expect("serialize");
        let back: AuditEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(entry, back);

        // The action is a token, not a word inside the sentence.
        assert!(json.contains(r#""action":"pr_opened""#), "{json}");
        assert!(json.contains(r#""subject":"4021""#), "{json}");
    }

    /// The screen line is short-form time; the record keeps the full stamp.
    #[test]
    fn the_screen_time_is_the_clock_not_the_date() {
        let t = short_time();
        assert_eq!(t.len(), 8, "expected HH:MM:SS, got {t:?}");
        assert_eq!(t.matches(':').count(), 2, "{t:?}");
    }
}
