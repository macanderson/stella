//! Deterministic failure accounting at the worker-feedback boundary.

use super::*;
use crate::witness::airlock::{FailureBrief, redact};

impl CandidateState {
    /// Record one deterministic failure and return its repeat count.
    pub(super) fn record_failure(&mut self, fingerprint: FailureFingerprint) -> u32 {
        let repeats = self
            .failures
            .iter()
            .filter(|seen| **seen == fingerprint)
            .count()
            .saturating_add(1);
        self.failures.push(fingerprint);
        u32::try_from(repeats).unwrap_or(u32::MAX)
    }
}

impl Pipeline<'_> {
    /// Record the deterministic ladder's abstention on both the prose and
    /// proof channels.
    pub(super) fn unverifiable(&self, reason: &str) {
        self.warn(format!("verification could not be performed: {reason}"));
        self.emit_proof(stella_protocol::ProofStep::VerificationUnavailable {
            reason: reason.to_string(),
        });
    }

    /// Record that verification was performed and did not prove the outcome:
    /// the channels could look, they looked, and the evidence fell short.
    ///
    /// Both channels for the same reason as [`Self::unverifiable`], and a
    /// *separate* method from it on purpose. That one says the instruments were
    /// blind; this one says they worked and the answer was not enough. Merging
    /// them would be the #973 conflation with its polarity flipped — a claim
    /// about the evidence delivered as a claim about the instruments — and it
    /// points a reader at the wrong repair.
    pub(super) fn unproven_verdict(&self, reason: &str) {
        self.warn(format!("verification did not prove this turn: {reason}"));
        self.emit_proof(stella_protocol::ProofStep::VerificationUnproven {
            reason: reason.to_string(),
        });
    }

    /// Build operator-facing failure evidence while preserving repeat identity.
    /// The returned brief is not sent to the worker; the worker receives the
    /// structured execution receipt assembled in `revision`.
    pub(super) fn deterministic_disclosure(
        state: &mut CandidateState,
        sealed: &SealedFailure<'_>,
        tail: &str,
    ) -> (VerdictEvidence, FailureBrief) {
        let fingerprint = sealed.fingerprint();
        let repeats = state.record_failure(fingerprint.clone());
        let brief = redact(sealed, grain_for_repeats(repeats));
        let evidence = VerdictEvidence {
            evidence_refs: brief.evidence_refs(&fingerprint),
            ladder: None,
            ..deterministic_fail_evidence(tail)
        };
        (evidence, brief)
    }
}
