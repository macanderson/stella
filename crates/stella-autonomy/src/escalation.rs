//! Escalation as a cooldown, not a tombstone.
//!
//! The loop labels an issue it could not finish. Dropping it for good is
//! right when the work is too hard. It is wrong when the machine broke: a
//! stale install, a provider outage, a failed setup. Treat the two alike
//! and one bad hour costs the backlog an issue.
//!
//! So an escalation carries a reason and a count. The reason sets how soon
//! to try again. The count sets when to stop. Each rule here is a plain
//! function over owned data. `stella-cli` reads the tracker, writes the
//! record, and asks these rules what it means.
//!
//! Reached as `stella_autonomy::escalation::*`. Nothing is re-exported at
//! the crate root. A type named [`crate::EscalationReason`] is already
//! there, and it is about a pull request. One name at one address has to
//! mean one thing.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Opening text of the record's marker in an issue body.
pub const MARKER_OPEN: &str = "<!-- stella-escalation ";

/// Closing text of that marker.
pub const MARKER_CLOSE: &str = " -->";

/// What broke, when the machine broke rather than the work being too hard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvCause {
    /// The turn made one call over and over and got the same bytes back.
    /// A stale checkout does this to every command that reads it.
    StuckLoop,
    /// The model provider refused the call or dropped it.
    ProviderError,
    /// A tool the turn needs was missing, or would not build.
    InstallFailure,
}

/// Why the loop handed an issue back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    /// The machine failed. The issue itself may be fine.
    Environmental(EnvCause),
    /// The turn ran and could not do the work.
    BeyondLoop,
}

/// What the loop has already tried on one issue.
///
/// It goes in the issue body, inside [`MARKER_OPEN`]. So it lives through a
/// restart and a move to a new box, and it needs no new store. The queue
/// read already carries every body, so reading it back is free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationRecord {
    /// How many times this issue has been escalated, this one included.
    pub attempts: u32,
    /// Why the last one happened.
    pub last_reason: EscalationReason,
    /// When the last one happened, in RFC 3339, for a person to read.
    pub last_at: String,
    /// The same moment in whole seconds since the epoch, for the math.
    /// The text form takes a date parser to compare, and this crate has
    /// none. [`crate::CycleRecord`] keeps both fields for the same reason.
    pub last_at_unix: i64,
}

/// How long each kind of escalation waits, and when waiting ends.
///
/// Set in `[self_driving.escalation]` in `stella.toml`, beside the rest of
/// the loop's judgement calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EscalationPolicy {
    /// Seconds to wait when the machine broke.
    pub environmental_cooldown_secs: u64,
    /// Seconds to wait when the turn could not do the work.
    pub beyond_loop_cooldown_secs: u64,
    /// Tries after which the issue is parked and never taken again.
    pub park_after: u32,
}

impl Default for EscalationPolicy {
    /// Ten minutes for the machine, six hours for the work, parked on the
    /// third try.
    ///
    /// A broken box clears in minutes. A rate-limit window closes, an
    /// install is fixed, an outage ends. Ten minutes covers that, and the
    /// issue is back inside the hour.
    ///
    /// A turn that ran and could not do the work needs something to change
    /// first: a comment, a new model, a blocker that lands. None of that
    /// happens in ten minutes. Trying again that fast buys the same
    /// failure at full price.
    ///
    /// Three tries, because the count is what stops a broken box from
    /// spending the whole backlog. Two would park work on one bad
    /// afternoon. Ten would buy the same failure ten times.
    fn default() -> Self {
        Self {
            environmental_cooldown_secs: 600,
            beyond_loop_cooldown_secs: 21_600,
            park_after: 3,
        }
    }
}

/// Phrases that name a broken box, and the cause each one names.
///
/// A fixed list, not a model call. The residue gate's detector has the same
/// shape. Each phrase is text this tree writes: the loop guard says
/// `byte-identical`, a failed spawn says `could not start`, and the rest
/// are the words a provider uses for a bad call.
const ENVIRONMENTAL_PHRASES: &[(&str, EnvCause)] = &[
    ("byte-identical", EnvCause::StuckLoop),
    ("identical output", EnvCause::StuckLoop),
    ("identical arguments", EnvCause::StuckLoop),
    ("stuck loop", EnvCause::StuckLoop),
    ("rate limit", EnvCause::ProviderError),
    ("rate-limit", EnvCause::ProviderError),
    ("too many requests", EnvCause::ProviderError),
    ("overloaded", EnvCause::ProviderError),
    ("service unavailable", EnvCause::ProviderError),
    ("bad gateway", EnvCause::ProviderError),
    ("internal server error", EnvCause::ProviderError),
    ("temporarily unavailable", EnvCause::ProviderError),
    ("connection reset", EnvCause::ProviderError),
    ("connection refused", EnvCause::ProviderError),
    ("connection closed", EnvCause::ProviderError),
    ("timed out", EnvCause::ProviderError),
    ("could not start", EnvCause::InstallFailure),
    ("command not found", EnvCause::InstallFailure),
    ("no such file or directory", EnvCause::InstallFailure),
    ("install failed", EnvCause::InstallFailure),
    ("node_modules", EnvCause::InstallFailure),
];

/// Read an abort message and say which kind of escalation it is.
///
/// A message the list does not name is [`EscalationReason::BeyondLoop`].
/// That is the longer of the two waits. So a message nobody planned for can
/// only make the loop slower, never faster.
#[must_use]
pub fn classify(why: &str) -> EscalationReason {
    let lower = why.to_ascii_lowercase();
    for (phrase, cause) in ENVIRONMENTAL_PHRASES {
        if lower.contains(phrase) {
            return EscalationReason::Environmental(*cause);
        }
    }
    EscalationReason::BeyondLoop
}

/// How long after [`EscalationRecord::last_at`] this issue may be taken
/// again.
///
/// `None` means never. It has been escalated
/// [`EscalationPolicy::park_after`] times, so it is parked.
#[must_use]
pub fn retry_after(record: &EscalationRecord, policy: &EscalationPolicy) -> Option<Duration> {
    if parked(record, policy) {
        return None;
    }
    let secs = match record.last_reason {
        EscalationReason::Environmental(_) => policy.environmental_cooldown_secs,
        EscalationReason::BeyondLoop => policy.beyond_loop_cooldown_secs,
    };
    Some(Duration::from_secs(secs))
}

/// Whether this issue is out of attempts.
#[must_use]
pub fn parked(record: &EscalationRecord, policy: &EscalationPolicy) -> bool {
    record.attempts >= policy.park_after
}

/// Whether an escalated issue may be taken again at `now_unix`.
///
/// No record means no. An issue can carry the label and no record: a person
/// put it there, or an older build did. Neither says what broke or when. A
/// guess would put work back that somebody took out by hand.
#[must_use]
pub fn may_retry(
    record: Option<&EscalationRecord>,
    policy: &EscalationPolicy,
    now_unix: i64,
) -> bool {
    let Some(record) = record else {
        return false;
    };
    let Some(wait) = retry_after(record, policy) else {
        return false;
    };
    let ready_at = record
        .last_at_unix
        .saturating_add(i64::try_from(wait.as_secs()).unwrap_or(i64::MAX));
    now_unix >= ready_at
}

/// The record to write for one more escalation.
///
/// The count carries forward. A second abort is the second try, not a fresh
/// first one.
#[must_use]
pub fn next(
    previous: Option<&EscalationRecord>,
    reason: EscalationReason,
    at: &str,
    at_unix: i64,
) -> EscalationRecord {
    EscalationRecord {
        attempts: previous.map_or(0, |p| p.attempts).saturating_add(1),
        last_reason: reason,
        last_at: at.to_owned(),
        last_at_unix: at_unix,
    }
}

/// Read the record out of an issue body, if one is there.
///
/// A body with no marker gives `None`. So does a marker with no end, and
/// JSON this build cannot read. The caller then sees an issue with no
/// record, which is the parked answer.
#[must_use]
pub fn parse(body: &str) -> Option<EscalationRecord> {
    let start = body.rfind(MARKER_OPEN)? + MARKER_OPEN.len();
    let rest = &body[start..];
    let end = rest.find(MARKER_CLOSE)?;
    serde_json::from_str(rest[..end].trim()).ok()
}

/// Put the record into an issue body, over any record already there.
///
/// The old marker is cut out first. A body escalated five times carries one
/// record, not five. The marker is an HTML comment, so a person reading the
/// issue sees nothing.
///
/// A record that will not serialize leaves the body alone. Half a marker
/// would hand the next reader a broken record instead of none.
#[must_use]
pub fn stamp(body: &str, record: &EscalationRecord) -> String {
    let Ok(json) = serde_json::to_string(record) else {
        return body.to_owned();
    };
    let stripped = strip(body);
    let mut out = stripped.trim_end().to_owned();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(MARKER_OPEN);
    out.push_str(&json);
    out.push_str(MARKER_CLOSE);
    out.push('\n');
    out
}

/// The body with each marker cut out.
fn strip(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find(MARKER_OPEN) {
        let after = &rest[start + MARKER_OPEN.len()..];
        let Some(end) = after.find(MARKER_CLOSE) else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &after[end + MARKER_CLOSE.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(attempts: u32, reason: EscalationReason, at_unix: i64) -> EscalationRecord {
        EscalationRecord {
            attempts,
            last_reason: reason,
            last_at: "2026-09-02T00:00:00Z".to_owned(),
            last_at_unix: at_unix,
        }
    }

    /// **The witness.** A turn that stopped because every command gave back
    /// the same bytes is a broken box, not work that is too hard. It waits
    /// the short cooldown and comes back.
    #[test]
    fn an_environmental_abort_waits_the_short_cooldown_and_then_may_be_retried() {
        let policy = EscalationPolicy::default();
        let reason = classify(
            "the turn exited 1 — the same `bash` call with identical arguments repeated 4 \
             times consecutively, producing byte-identical output every time",
        );
        assert_eq!(
            reason,
            EscalationReason::Environmental(EnvCause::StuckLoop),
            "a stuck-loop abort is about the machine, not the issue"
        );

        let first = next(None, reason, "2026-09-02T00:00:00Z", 1_000);
        assert_eq!(first.attempts, 1);
        assert_eq!(
            retry_after(&first, &policy),
            Some(Duration::from_secs(policy.environmental_cooldown_secs))
        );
        assert!(
            !may_retry(Some(&first), &policy, 1_000),
            "the cooldown has not run out at the moment it was written"
        );
        assert!(
            may_retry(
                Some(&first),
                &policy,
                1_000 + i64::try_from(policy.environmental_cooldown_secs).expect("fits")
            ),
            "once the cooldown is over the issue is takeable again, with no label removed"
        );
    }

    /// Work the loop could not do waits far longer than a broken box.
    #[test]
    fn a_beyond_loop_abort_waits_longer_than_an_environmental_one() {
        let policy = EscalationPolicy::default();
        assert_eq!(
            classify("the turn exited 0 — I cannot work out what this issue is asking for"),
            EscalationReason::BeyondLoop,
            "a message the phrase list does not name is the patient answer"
        );

        let env = record(
            1,
            EscalationReason::Environmental(EnvCause::ProviderError),
            0,
        );
        let beyond = record(1, EscalationReason::BeyondLoop, 0);
        assert!(
            retry_after(&env, &policy) < retry_after(&beyond, &policy),
            "environmental aborts are retried more eagerly"
        );
    }

    /// **The parking witness.** After `park_after` tries the issue stops
    /// coming back, however long anyone waits.
    #[test]
    fn an_issue_escalated_park_after_times_is_never_taken_again() {
        let policy = EscalationPolicy::default();
        let spent = record(
            policy.park_after,
            EscalationReason::Environmental(EnvCause::StuckLoop),
            0,
        );

        assert!(parked(&spent, &policy));
        assert_eq!(retry_after(&spent, &policy), None);
        assert!(
            !may_retry(Some(&spent), &policy, i64::MAX),
            "parked means parked — no wait makes it takeable"
        );
    }

    /// A label with no record means an older build, or a person's hand.
    /// Both stay out of the queue.
    #[test]
    fn an_escalation_with_no_record_is_never_retried() {
        assert!(!may_retry(None, &EscalationPolicy::default(), i64::MAX));
    }

    /// The record round-trips through a body. A second write takes the
    /// place of the first rather than stacking on it.
    #[test]
    fn stamping_a_body_twice_leaves_one_record() {
        let body = "## What happens\nThe queue never gets it back.\n";
        let first = record(1, EscalationReason::BeyondLoop, 10);
        let once = stamp(body, &first);
        assert_eq!(parse(&once), Some(first));
        assert!(once.starts_with("## What happens"), "the body is kept");

        let second = record(2, EscalationReason::Environmental(EnvCause::StuckLoop), 20);
        let twice = stamp(&once, &second);
        assert_eq!(parse(&twice), Some(second));
        assert_eq!(
            twice.matches(MARKER_OPEN).count(),
            1,
            "a body escalated twice carries one record, not two"
        );
        assert!(twice.starts_with("## What happens"));
    }

    /// A body nobody escalated reads as no record. So does one whose
    /// marker is broken.
    #[test]
    fn an_absent_or_damaged_marker_reads_as_no_record() {
        assert_eq!(parse("plain issue text"), None);
        assert_eq!(parse("<!-- stella-escalation {\"attempts\":1}"), None);
        assert_eq!(parse("<!-- stella-escalation not json -->"), None);
    }

    /// The count carries forward, which is what makes parking reachable.
    #[test]
    fn attempts_accumulate_across_escalations() {
        let first = next(None, EscalationReason::BeyondLoop, "t", 0);
        let second = next(
            Some(&first),
            EscalationReason::Environmental(EnvCause::InstallFailure),
            "t",
            1,
        );
        assert_eq!(second.attempts, 2);
        assert_eq!(
            second.last_reason,
            EscalationReason::Environmental(EnvCause::InstallFailure),
            "the newest reason decides the next cooldown"
        );
    }
}
