//! Streaming→non-streaming fallback state, shared by any adapter that can
//! re-issue a completion as a unary request when its stream is broken.
//!
//! Two broken-stream shapes motivate this (issue #2686): a proxy that
//! buffers the SSE body, so the response hangs before its first byte; and a
//! gateway that answers 200 with an **empty stream** (EOF before any data).
//! Both are properties of the *path*, not of the request — the same payload
//! completes fine over a non-streaming call — and both used to burn the
//! whole retry budget re-issuing the identical streaming request into the
//! same broken pipe.
//!
//! The recovery is deliberately split across two attempts rather than hidden
//! inside one provider call: the faulted streaming attempt **fails with a
//! retryable error**, so the caller's retry machinery (which already bills
//! every discarded attempt through its `UsageIncomplete` observer and bounds
//! the attempt count) stays the single owner of retries and accounting. This
//! latch only changes what the *next* attempt sends. An adapter must consult
//! [`StreamRecovery::use_unary`] per attempt, report a qualifying stream
//! fault via [`StreamRecovery::note_stream_fault`], and report every unary
//! attempt's outcome via [`StreamRecovery::note_unary_outcome`].
//!
//! The state machine is a bounded latch, after zappy's "reactive resilience"
//! watchdog-and-fallback (adapted to this workspace's accounting rules):
//!
//! ```text
//! Streaming ──stream fault (hung/empty, nothing arrived)──▶ Probing
//! Probing   ──unary attempt succeeds──▶ Confirmed (sticky for this session)
//! Probing   ──unary attempt fails──▶ Streaming
//! ```
//!
//! `Confirmed` is sticky because the trigger faults are deterministic: a
//! proxy that buffers SSE buffers it on every request, so re-probing the
//! stream would pay the full first-byte deadline once per step for the rest
//! of the session. A failed probe reverts instead of latching — one
//! unproven fault must not condemn streaming (and with it mid-stream
//! observation/speculation) when the provider was simply down, and the
//! ping-pong this allows is bounded by the caller's retry policy like every
//! other attempt.

use std::sync::atomic::{AtomicU8, Ordering};

use stella_protocol::ProviderError;

/// One streaming attempt's failure, classified for the fallback decision.
///
/// `fallback_eligible` is true only for the two shapes where the retry
/// loses nothing by going out unary: the stream hung or died having
/// delivered **no completion signal whatsoever** (no content, no usage
/// frame, no terminal marker). A mid-stream death with salvage keeps its
/// existing retry-as-a-stream classification, so partially-streamed content
/// is never the fallback's problem — there is nothing to tombstone.
#[derive(Debug)]
pub(crate) struct StreamFault {
    pub(crate) error: ProviderError,
    pub(crate) fallback_eligible: bool,
}

impl StreamFault {
    pub(crate) fn ineligible(error: ProviderError) -> Self {
        Self {
            error,
            fallback_eligible: false,
        }
    }
}

/// Every plain `ProviderError` raised inside a stream aggregator (malformed
/// frames, in-band error frames, truncated tool input) is ineligible: only
/// the aggregator's explicit hung/empty exits construct an eligible fault.
impl From<ProviderError> for StreamFault {
    fn from(error: ProviderError) -> Self {
        Self::ineligible(error)
    }
}

const STREAMING: u8 = 0;
const PROBING: u8 = 1;
const CONFIRMED: u8 = 2;

/// Per-provider-instance fallback latch. One instance lives one session, so
/// the latch's lifetime is the session's — exactly the scope on which a
/// broken streaming path is a stable fact.
///
/// All transitions are CAS-guarded: concurrent calls through one provider
/// instance may race, and the worst a lost race can do is leave the latch in
/// the state the other caller proved, never skip a state.
#[derive(Debug, Default)]
pub(crate) struct StreamRecovery {
    state: AtomicU8,
}

impl StreamRecovery {
    /// Whether the next attempt should be a unary (non-streaming) request.
    pub(crate) fn use_unary(&self) -> bool {
        self.state.load(Ordering::Acquire) != STREAMING
    }

    /// A streaming attempt faulted in a fallback-eligible way (hung before
    /// its first byte, or ended as an empty stream): arm the probe so the
    /// caller's retry of this attempt goes out unary.
    ///
    /// Returns whether this call armed it — `false` when a probe was already
    /// armed or confirmed, so the caller can word its error without promising
    /// a switch some other attempt already made.
    pub(crate) fn note_stream_fault(&self) -> bool {
        self.state
            .compare_exchange(STREAMING, PROBING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// A unary attempt resolved. Success while probing confirms the latch
    /// for the rest of the session; failure while probing reverts to
    /// streaming (the fault evidently wasn't the stream's). A confirmed
    /// latch is sticky — a later transient unary failure must not send the
    /// session back to a stream already proven broken.
    pub(crate) fn note_unary_outcome(&self, success: bool) {
        let next = if success { CONFIRMED } else { STREAMING };
        let _ = self
            .state
            .compare_exchange(PROBING, next, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Route a [`StreamFault`] back into the retry ladder — the one call
    /// every streaming adapter makes at the end of its aggregation.
    ///
    /// An eligible fault (hung before its first byte / empty stream) arms the
    /// latch, so the caller's retry of this attempt goes out unary, and its
    /// error stays retryable `Transport`: the ordinary retry machinery both
    /// drives that switch and bills this discarded attempt through its
    /// `UsageIncomplete` observer, exactly like every other doomed attempt.
    /// The message names the switch only when THIS fault armed the latch, so
    /// an error never promises a fallback some other attempt already made.
    pub(crate) fn absorb(&self, fault: StreamFault) -> ProviderError {
        if fault.fallback_eligible && self.note_stream_fault() {
            return match fault.error {
                ProviderError::Transport { message, partial } => ProviderError::Transport {
                    message: format!("{message}; retrying this attempt as a non-streaming request"),
                    partial,
                },
                other => other,
            };
        }
        fault.error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_streaming_and_arms_a_probe_on_the_first_fault() {
        let recovery = StreamRecovery::default();
        assert!(!recovery.use_unary(), "healthy sessions stream");
        assert!(recovery.note_stream_fault(), "first fault arms the probe");
        assert!(recovery.use_unary(), "the retry must go out unary");
        assert!(
            !recovery.note_stream_fault(),
            "an already-armed probe reports no new transition"
        );
    }

    #[test]
    fn a_successful_probe_confirms_the_latch_for_good() {
        let recovery = StreamRecovery::default();
        recovery.note_stream_fault();
        recovery.note_unary_outcome(true);
        assert!(recovery.use_unary(), "confirmed sessions stay unary");
        // Sticky: a later transient unary failure must not send the session
        // back to a stream already proven broken.
        recovery.note_unary_outcome(false);
        assert!(recovery.use_unary(), "confirmation is sticky");
    }

    #[test]
    fn a_failed_probe_reverts_to_streaming() {
        let recovery = StreamRecovery::default();
        recovery.note_stream_fault();
        recovery.note_unary_outcome(false);
        assert!(
            !recovery.use_unary(),
            "one unproven fault must not condemn streaming when the provider \
             was simply down"
        );
    }

    #[test]
    fn a_unary_outcome_without_a_probe_is_a_no_op() {
        let recovery = StreamRecovery::default();
        recovery.note_unary_outcome(true);
        assert!(
            !recovery.use_unary(),
            "success reported outside a probe must not invent a latch"
        );
    }
}
