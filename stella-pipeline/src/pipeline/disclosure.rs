//! The orchestration seam of the feedback airlock: the few `Pipeline` and
//! `CandidateState` methods that decide what a verification failure is allowed
//! to tell the worker.
//!
//! The airlock's rules are pure and live in [`crate::witness::airlock`]. What
//! lives here is only the wiring that needs `&self` — emitting a policy event
//! when a disclosure is refused, and threading failure identity through the
//! candidate's revise loop. Split out of `pipeline.rs` for the same reason
//! `witness_stage.rs` was: it is one nameable concern, and `pipeline.rs` is
//! already the crate's largest file.
//!
//! Design: [`docs/design/witness-protocol.md`](../../../../docs/design/witness-protocol.md) §4.

use stella_protocol::PolicyKind;

use super::*;
use crate::witness::airlock::FailureBrief;

impl CandidateState {
    /// Record one deterministic failure and return how many times *this same*
    /// failure has now occurred, counting this one. `1` is a first sighting.
    ///
    /// Repetition — not the raw revision count — is what tightens disclosure:
    /// a worker producing a *new* failure each round is making progress and
    /// keeps full grain, while one stuck on the same failure is not helped by
    /// a third copy of the same brief.
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
    /// The witness artifacts backing this run, withheld from every disclosure
    /// grain — naming one hands the worker the detector itself.
    pub(super) fn witness_paths(witness: Option<&Witness>) -> Vec<String> {
        let mut paths: Vec<String> = witness
            .map(|witness| witness.files.keys().cloned().collect())
            .unwrap_or_default();
        paths.sort();
        paths
    }

    /// Let one model-authored text cross inbound to the worker, or drop it.
    ///
    /// Distress guidance and judge reasoning are both written by a model that
    /// was shown the raw deterministic evidence, so either can quote the
    /// detector back at the worker. `None` means the scrubber rejected it; the
    /// emitted policy event carries a leak *token*, never the offending text.
    pub(super) fn airlock_forward(
        &self,
        text: &str,
        subject: &str,
        sealed: &SealedFailure<'_>,
    ) -> Option<String> {
        match scrub(text, sealed) {
            Ok(()) => Some(text.to_string()),
            Err(leak) => {
                self.emit(AgentEvent::PolicyDecision {
                    kind: PolicyKind::Blocked,
                    subject: subject.to_string(),
                    outcome: leak.token().to_string(),
                });
                None
            }
        }
    }

    /// Turn one deterministic failure into the pair the revise loop needs: the
    /// operator-facing evidence, and the worker-facing brief.
    ///
    /// Two audiences, two texts. The operator sees the runner's real output —
    /// they are not the adversary, and hiding the failure from the human would
    /// make the tool unusable. The worker sees only what the grain allows.
    pub(super) fn deterministic_disclosure(
        state: &mut CandidateState,
        sealed: &SealedFailure<'_>,
        tail: &str,
    ) -> (JudgeEvidence, FailureBrief) {
        let fingerprint = sealed.fingerprint();
        let repeats = state.record_failure(fingerprint.clone());
        let brief = redact(sealed, grain_for_repeats(repeats));
        let evidence = JudgeEvidence {
            evidence_refs: brief.evidence_refs(&fingerprint),
            ..deterministic_fail_evidence(tail)
        };
        (evidence, brief)
    }
}
