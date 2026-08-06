//! Parked waits (#1471): the types and pure decision logic behind waiting on
//! external state without spending model steps.
//!
//! Waiting used to be a model behavior: poll a tool, read the unchanged
//! output, poll again — every iteration a full model round-trip on a growing
//! transcript, and every long in-tool sleep a prompt-cache expiry. A parked
//! wait inverts that: a tool deposits a [`WaitRequest`] during its execution,
//! and the engine re-runs a cheap read-only *probe* between model calls,
//! re-invoking the model exactly once — when the probed state changes or the
//! deadline passes — with the delta riding a volatile tail message. The
//! polling itself never touches the transcript, so an arbitrarily long wait
//! costs O(1) model steps and forces no compaction.
//!
//! This module is decision logic only (invariant 2): what counts as a change,
//! how many polls a deadline affords, and what the wake message says are all
//! pure functions over owned data. The actual sleeping and probing live in
//! `driver::waiting`, driven through the engine's existing ports
//! ([`crate::ports::ToolExecutor`], [`crate::retry::Sleeper`]) — no new I/O
//! surface exists for this feature.

use serde::{Deserialize, Serialize};
use stella_protocol::ToolOutput;

/// Floor on the probe interval. A probe is a real subprocess or network round
/// trip (`gh run list`, a file stat) against something that changes on
/// CI-and-deploy timescales; probing faster than this buys nothing and spends
/// someone else's rate limit (#923).
pub const MIN_POLL_INTERVAL_SECS: u64 = 5;

/// Ceiling on a parked wait's deadline. Two hours covers the workspace's
/// longest legitimate wait (the fleet CI watcher's cumulative cap is the same
/// figure); anything longer is a stuck condition the model should be woken to
/// reconsider, not slept through.
pub const MAX_PARK_SECS: u64 = 2 * 60 * 60;

/// Default deadline when the depositing tool does not name one.
pub const DEFAULT_PARK_SECS: u64 = 30 * 60;

/// The marker prefix on every wake message the engine injects, so transcript
/// consumers (and tests) can recognize an engine-authored wake the same way
/// `LOOP_STEER_PREFIX` marks a loop steer.
pub const WAKE_MARKER: &str = "[parked wait";

/// One tool invocation a parked wait replays on the engine's own clock,
/// with no model involvement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitCall {
    /// Tool name, resolved through the same executor that ran the
    /// depositing call.
    pub name: String,
    /// The exact input to replay. Composed by the depositing tool from its
    /// own already-approved input — never from model-supplied text it did
    /// not validate — so replaying it opens no approval surface the
    /// original call did not already pass.
    pub input: serde_json::Value,
}

/// A tool's request to park the turn until an observed condition changes.
///
/// Deposited during [`crate::ports::ToolExecutor::execute`] and drained by
/// the engine at the following step boundary
/// ([`crate::ports::ToolExecutor::drain_wait_request`]) — the same
/// "written by one object, drained by another" seam sub-agent spend uses,
/// and for the same structural reason: the tool is finished by the time the
/// engine is at a boundary where parking is safe (invariant 6).
///
/// Serde round-trips (invariant 4) because the value crosses the
/// `stella-tools` → `stella-core` boundary, and because a checkpointed turn
/// may someday want to persist one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitRequest {
    /// Human-readable condition, e.g. `CI for --branch 'main' settles`.
    /// Names the wait in the park announcement and the wake message.
    pub description: String,
    /// Cheap read-only call re-executed once per interval. The turn wakes
    /// when its output's fingerprint first differs from `baseline`. The
    /// engine refuses to park on a probe whose schema does not declare
    /// `read_only` — a mutating probe would repeat a write on a timer the
    /// model never sees.
    pub probe: WaitCall,
    /// Fingerprint of the probe's output at deposit time. The depositing
    /// tool observed the condition once itself (that observation is why it
    /// is asking to wait), so it supplies the baseline rather than having
    /// the engine burn a redundant probe to rediscover it.
    pub baseline: String,
    /// Richer call run once on wake; its output rides the wake message so
    /// the model resumes with the fresh state, not a stale poll history.
    /// `None` wakes with the probe's own final output.
    pub on_wake: Option<WaitCall>,
    /// Seconds between probes. Clamped up to [`MIN_POLL_INTERVAL_SECS`].
    pub poll_interval_secs: u64,
    /// Wall-clock bound on the whole wait. `0` means
    /// [`DEFAULT_PARK_SECS`]; clamped down to [`MAX_PARK_SECS`].
    pub timeout_secs: u64,
}

impl WaitRequest {
    /// The probe interval with the floor applied.
    #[must_use]
    pub fn interval_secs(&self) -> u64 {
        self.poll_interval_secs.max(MIN_POLL_INTERVAL_SECS)
    }

    /// The deadline with default and ceiling applied.
    #[must_use]
    pub fn deadline_secs(&self) -> u64 {
        let requested = if self.timeout_secs == 0 {
            DEFAULT_PARK_SECS
        } else {
            self.timeout_secs
        };
        requested.min(MAX_PARK_SECS)
    }

    /// How many probes the deadline affords, always at least one.
    ///
    /// The engine deliberately counts polls instead of reading a clock: the
    /// deadline arithmetic stays a pure function of the request (this
    /// crate holds no time source — the injected sleeper owns real time),
    /// and a test with a no-op sleeper walks the same bound production
    /// does. The bound under-counts by the probes' own execution time,
    /// which errs toward waking early — the safe direction.
    #[must_use]
    pub fn max_polls(&self) -> u64 {
        (self.deadline_secs() / self.interval_secs()).max(1)
    }
}

/// Why a parked wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    /// The probe's fingerprint diverged from the baseline.
    Changed,
    /// Every afforded poll observed the baseline (or failed); the deadline
    /// is spent.
    DeadlineExpired,
}

/// The fingerprint of one probe observation, or `None` when the probe
/// errored — a transient `gh`/network hiccup must keep the turn waiting
/// rather than waking it with a phantom change (the deadline is the
/// backstop for a probe that never stops failing). Whitespace is trimmed so
/// a trailing-newline difference between the depositing tool's baseline and
/// the replayed probe cannot manufacture a wake.
#[must_use]
pub fn probe_fingerprint(output: &ToolOutput) -> Option<String> {
    match output {
        ToolOutput::Ok { content } => Some(content.trim().to_string()),
        ToolOutput::Error { .. } => None,
    }
}

/// Whether one observation ends the wait: `Some(Changed)` on the first
/// fingerprint that differs from the baseline, `Some(DeadlineExpired)` once
/// `polls_used` exhausts the afforded polls, `None` to keep waiting.
#[must_use]
pub fn decide(
    request: &WaitRequest,
    observed: Option<&str>,
    polls_used: u64,
) -> Option<WakeReason> {
    if let Some(fingerprint) = observed {
        if fingerprint != request.baseline {
            return Some(WakeReason::Changed);
        }
    }
    if polls_used >= request.max_polls() {
        return Some(WakeReason::DeadlineExpired);
    }
    None
}

/// The volatile tail message the model wakes to. Marked with
/// [`WAKE_MARKER`], and explicit that the wait cost no model steps — the
/// model must not conclude it has been polling and start compensating.
#[must_use]
pub fn wake_message(
    request: &WaitRequest,
    reason: WakeReason,
    polls_used: u64,
    detail: Option<&str>,
) -> String {
    let waited_secs = polls_used * request.interval_secs();
    let headline = match reason {
        WakeReason::Changed => format!(
            "{WAKE_MARKER} complete] {} — the watched state changed after ~{waited_secs}s of \
             engine-side probing (no model steps were spent waiting).",
            request.description
        ),
        WakeReason::DeadlineExpired => format!(
            "{WAKE_MARKER} timed out] {} — still unchanged after ~{waited_secs}s of engine-side \
             probing (no model steps were spent waiting). Reassess rather than re-polling: the \
             condition may be stuck.",
            request.description
        ),
    };
    match detail {
        Some(detail) if !detail.trim().is_empty() => {
            format!("{headline}\n\nCurrent state:\n{detail}")
        }
        _ => headline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(interval: u64, timeout: u64) -> WaitRequest {
        WaitRequest {
            description: "CI for --branch 'main' settles".into(),
            probe: WaitCall {
                name: "ci_status".into(),
                input: serde_json::json!({ "probe": true, "branch": "main" }),
            },
            baseline: "pending".into(),
            on_wake: None,
            poll_interval_secs: interval,
            timeout_secs: timeout,
        }
    }

    #[test]
    fn clamps_apply_floor_default_and_ceiling() {
        // A zero interval cannot busy-spin someone else's API.
        assert_eq!(request(0, 600).interval_secs(), MIN_POLL_INTERVAL_SECS);
        // A zero timeout means the default, not an instant expiry.
        assert_eq!(request(15, 0).deadline_secs(), DEFAULT_PARK_SECS);
        // A u64::MAX timeout cannot park a turn for a week.
        assert_eq!(request(15, u64::MAX).deadline_secs(), MAX_PARK_SECS);
        // Even a deadline shorter than one interval affords one probe.
        assert_eq!(request(60, 1).max_polls(), 1);
    }

    #[test]
    fn a_changed_fingerprint_wakes_and_an_unchanged_one_waits() {
        let req = request(15, 600);
        assert_eq!(decide(&req, Some("pending"), 1), None);
        assert_eq!(decide(&req, Some("settled"), 1), Some(WakeReason::Changed));
    }

    #[test]
    fn a_probe_error_keeps_waiting_instead_of_waking() {
        let req = request(15, 600);
        // An errored probe yields no fingerprint…
        assert_eq!(
            probe_fingerprint(&ToolOutput::Error {
                message: "gh: network unreachable".into()
            }),
            None
        );
        // …and no fingerprint is "keep waiting", never "changed".
        assert_eq!(decide(&req, None, 1), None);
    }

    #[test]
    fn the_deadline_expires_after_the_afforded_polls() {
        let req = request(15, 600); // affords 40 polls
        assert_eq!(req.max_polls(), 40);
        assert_eq!(decide(&req, Some("pending"), 39), None);
        assert_eq!(
            decide(&req, Some("pending"), 40),
            Some(WakeReason::DeadlineExpired)
        );
        // A change on the final poll still reports Changed, not a timeout.
        assert_eq!(decide(&req, Some("settled"), 40), Some(WakeReason::Changed));
    }

    #[test]
    fn fingerprints_trim_whitespace_so_a_newline_is_not_a_change() {
        assert_eq!(
            probe_fingerprint(&ToolOutput::Ok {
                content: "pending\n".into()
            })
            .as_deref(),
            Some("pending")
        );
    }

    #[test]
    fn wake_messages_carry_the_marker_and_the_detail() {
        let req = request(15, 600);
        let woke = wake_message(&req, WakeReason::Changed, 8, Some("all green"));
        assert!(woke.starts_with(WAKE_MARKER), "{woke}");
        assert!(woke.contains("all green"), "{woke}");
        assert!(woke.contains("~120s"), "{woke}");
        let timed_out = wake_message(&req, WakeReason::DeadlineExpired, 40, None);
        assert!(timed_out.contains("timed out"), "{timed_out}");
        // A blank detail attaches nothing — no dangling "Current state:".
        assert!(!timed_out.contains("Current state"), "{timed_out}");
    }

    /// Invariant 4: the request crosses the `stella-tools` → `stella-core`
    /// boundary, so it round-trips byte-for-byte.
    #[test]
    fn wait_request_round_trips_through_serde_json() {
        let req = WaitRequest {
            on_wake: Some(WaitCall {
                name: "ci_status".into(),
                input: serde_json::json!({ "branch": "main" }),
            }),
            ..request(15, 600)
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let back: WaitRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, req);
    }
}
