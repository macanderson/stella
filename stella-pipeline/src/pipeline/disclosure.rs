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
use crate::witness::warrant::warrant;

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
            ladder: None,
            ..deterministic_fail_evidence(tail)
        };
        (evidence, brief)
    }

    /// A clean, reasoned completion when the change itself warranted no test.
    ///
    /// Reached only from the ladder's inconclusive arm, where "inconclusive"
    /// covers two very different things: a real change nothing proved, and a
    /// change with nothing to prove. The diff separates them; the prompt never
    /// could. `None` means buy the judge call after all.
    ///
    /// The verdict records *why* no test was written rather than leaving an
    /// unexplained absence — the pipeline's half of the contract contributors
    /// are already held to.
    ///
    /// `snapshot` is the same decision-time provenance every other verdict
    /// carries (#865) — this one used to ship `ladder: None`, leaving the one
    /// verdict that waives evidence as the one verdict `replay` could not
    /// explain.
    pub(super) fn warranted_completion(
        &self,
        state: &CandidateState,
        snapshot: &stella_protocol::LadderSnapshot,
    ) -> Option<JudgeEvidence> {
        let reason = warrant(&state.diff_text, state.file_changes).reason()?;
        if reason.warrants_independent_review() {
            return None;
        }
        let evidence = JudgeEvidence {
            summary: format!("no witness test warranted: {}", reason.sentence()),
            deterministic: true,
            evidence_refs: vec![format!("warrant:{reason:?}")],
            ladder: Some(Box::new(snapshot.clone())),
        };
        self.emit(AgentEvent::JudgeVerdict {
            passed: true,
            evidence: evidence.clone(),
        });
        Some(evidence)
    }

    /// The verdict for a judge waiver that stands: triage said `JUDGE: no`
    /// and the warrant agrees ([`Pipeline::judge_waiver_stands`]). A pass
    /// carrying no independent evidence — the caller scores it `Unverified`,
    /// never `DeterministicPass`, so a review-waived candidate cannot tie a
    /// genuinely flip-verified sibling in best-of-N and then win the
    /// smaller-diff tiebreak. The summary states plainly what was not done:
    /// falling through to `heuristic_fallback` instead would report "judge
    /// unavailable", which describes a judge that broke, not one that was
    /// deliberately waived.
    pub(super) fn waived_completion(
        &self,
        snapshot: &stella_protocol::LadderSnapshot,
    ) -> JudgeEvidence {
        let evidence = JudgeEvidence {
            summary: "model review waived by triage; no independent verification was performed"
                .to_string(),
            deterministic: false,
            evidence_refs: vec![],
            ladder: Some(Box::new(snapshot.clone())),
        };
        self.emit(AgentEvent::JudgeVerdict {
            passed: true,
            evidence: evidence.clone(),
        });
        evidence
    }

    /// Whether triage's `JUDGE: no` waiver may stand, decided from the change
    /// itself rather than the prompt.
    ///
    /// The waiver was answered before any work existed, and it is only ever
    /// consulted once the ladder has come back inconclusive — the state in
    /// which its own premise ("success is self-evident or a test proves it")
    /// has been falsified. So the warrant gets the final word: the waiver
    /// stands only where the diff shows nothing an independent reviewer could
    /// catch (the same classes [`Pipeline::warranted_completion`] completes
    /// without a judge). A behavioral diff, a deletion, or anything the diff
    /// machinery could not read keeps its reviewer, whatever triage guessed.
    pub(super) fn judge_waiver_stands(state: &CandidateState) -> bool {
        warrant(&state.diff_text, state.file_changes)
            .reason()
            .is_some_and(|reason| !reason.warrants_independent_review())
    }
}
