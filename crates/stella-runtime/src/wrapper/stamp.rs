// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The record a decided round leaves behind, and the name on it.
//!
//! [`judge`](super::judge) answers a [`Verdict`]. That answer is lost unless
//! something writes it down. This module writes it down: it folds a round's
//! evidence into a [`LadderSnapshot`] and puts one [`VerdictStamp`] on it.
//!
//! The name on the stamp is read from the manifest the host loaded. A plugin
//! cannot name itself here, and a payload that tries is ignored. So a claim
//! can always be traced back to the thing a person agreed to install.
//!
//! [`stamped`] reads no clock and touches no file. The caller passes the two
//! times in, which is what lets a test pin a whole stamp to the byte.
//! [`HostClock`] is the source a real run passes them from.

use stella_core::context_record::{RecordHashError, record_hash};
use stella_core::ports::Clock;
use stella_plugin::{
    EvidenceProvenance, EvidenceSet, FlipObservation, TamperFinding, UndecidedReason, Verdict,
    VerdictRule,
};
use stella_protocol::{FlipOutcome, LadderRung, LadderSnapshot, StampAssessment, VerdictStamp};

/// The name a stamp carries when the host reached the answer itself.
pub const HOST_AUTHOR: &str = "engine";

/// The host's own clock, counting from the Unix epoch.
///
/// Two stamps are compared across runs and across machines, so they have to
/// count from a shared start. A clock that counts from the moment a process
/// began would make the gap between two stamps mean nothing. `stella-cli`'s
/// `WallClock` answers the same port the same way and for the same reason.
///
/// A system clock set before the epoch reads as `0` rather than failing: a
/// wrong time is a bad stamp, and a run that stops for one is worse.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostClock;

impl Clock for HostClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            })
    }
}

/// When an observer decided, and how long it took.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StampTiming {
    /// Milliseconds since the Unix epoch, read from the host's clock.
    pub decided_at_ms: u64,
    /// How long the observer took, in milliseconds.
    pub duration_ms: u64,
    /// The observer ran out of time. Its answer is then what it had when the
    /// clock stopped.
    pub timed_out: bool,
}

/// The name to write, given whose observation this is.
///
/// A plugin gets the id from the manifest the host loaded. Nothing else can
/// reach this field, so no plugin can sign another one's name. Evidence the
/// host concluded for itself is [`HOST_AUTHOR`].
#[must_use]
pub fn author(provenance: EvidenceProvenance, manifest_id: &str) -> &str {
    match provenance {
        EvidenceProvenance::PluginReported => manifest_id,
        EvidenceProvenance::HostObserved => HOST_AUTHOR,
    }
}

/// Fold one round's evidence and answer into the ladder's record.
///
/// Carries no stamp. [`stamped`] adds one, and both give the same rung: a
/// stamp records an observer, it never gives one a vote.
///
/// # What this record cannot say
///
/// The socket carries a flip, a tamper finding, and numbers the oracle named
/// itself. It carries no test command, no per-tree run, and no diff. Those
/// fields stay at the value that means "nothing looked" — `None`, an empty
/// list, and `diff_available: false`. A zero here is never a measurement.
///
/// The numbers a plugin reports are keyed by names its own oracle declared,
/// so none of them maps onto a field here. Guessing which reported name means
/// "lines changed" would invent a shared vocabulary that does not exist.
#[must_use]
pub fn snapshot(rule: &VerdictRule, evidence: &EvidenceSet, verdict: &Verdict) -> LadderSnapshot {
    LadderSnapshot {
        rung: Some(rung(rule, verdict)),
        tracked_command: None,
        oracle_trace: Vec::new(),
        flip: flip(evidence.flip),
        unstable_flip: false,
        flip_refused_different_failure: false,
        touched_tests_passed: None,
        test_infra: None,
        diff_lines: 0,
        diff_budget: 0,
        diff_available: false,
        mutating_actions: 0,
        new_diag_errors: 0,
        new_diag_warnings: 0,
        witness_intact: witness_intact(&evidence.tamper),
        witness_mutation: None,
        diff_coverage: None,
        verify_done_flip: false,
        no_test_surface: false,
        errored_commands: 0,
        // No model graded this round. `judge` is synchronous and reads no
        // network, so there is no grader whose independence could be a fact.
        verifier_independent: None,
        stamps: Vec::new(),
    }
}

/// The same record, with one stamp on it.
///
/// The hash is taken over [`LadderSnapshot::stamp_preimage`], which is this
/// record with the stamp list dropped. So a second observer can stamp the same
/// record later without breaking the claim already on it.
///
/// # Errors
///
/// [`RecordHashError`] when the record cannot be turned into canonical bytes.
/// A caller that meets one keeps the answer and reports the failure: the hash
/// is taken after the decision and can never change it.
pub fn stamped(
    rule: &VerdictRule,
    evidence: &EvidenceSet,
    verdict: &Verdict,
    manifest_id: &str,
    evidence_refs: Vec<String>,
    timing: StampTiming,
) -> Result<LadderSnapshot, RecordHashError> {
    let record = snapshot(rule, evidence, verdict);
    let preimage = record.stamp_preimage()?;
    let preimage_hash = record_hash(&preimage)?;
    Ok(record.with_stamp(VerdictStamp {
        author: author(evidence.provenance, manifest_id).to_string(),
        // A manifest declares no version, and the one word a plugin could
        // offer for this field is the one word it must not choose. An absent
        // version says nothing; a borrowed one would say something false.
        author_version: None,
        assessment: assessment(rule, verdict),
        summary: summary(rule, verdict),
        preimage_hash,
        evidence_refs,
        decided_at_ms: timing.decided_at_ms,
        duration_ms: timing.duration_ms,
        timed_out: timing.timed_out,
    }))
}

/// What this observer concluded.
///
/// A rule with no requirement is `not_applicable` rather than `done`. `judge`
/// answers [`Verdict::Met`] for it, and that answer is right — a wrapper that
/// only contributes context has nothing to hold open. It also has nothing to
/// claim, and a stamp reading `done` would claim the work was checked.
fn assessment(rule: &VerdictRule, verdict: &Verdict) -> StampAssessment {
    match verdict {
        Verdict::Met { .. } if rule.requirements.is_empty() => StampAssessment::NotApplicable,
        Verdict::Met { .. } => StampAssessment::Done,
        Verdict::Unmet { .. } => StampAssessment::NotDone,
        Verdict::Undecided { .. } => StampAssessment::Inconclusive,
    }
}

/// Which rung the record rests on.
///
/// Every arm is decided here, and none of them reads a stamp. The two words
/// that sound alike are the ones to get right: `unverifiable` says nothing
/// could look, and `unverified` says something looked and did not settle it.
///
/// - A met rule with no requirement is `waived`. Nothing was asked for, so a
///   pass here carries no evidence either way.
/// - A met rule is `submit_fast`. Whether a flip carried it is a separate
///   field of the record.
/// - An unmet rule is `revise`: the evidence decided against the work.
/// - A witness that fails the same way before and after the work is
///   `witness_unsatisfiable`. It is a claim about the instrument.
/// - No oracle, no probe, and an unobservable flip are all `unverifiable`.
///   Nothing was in a position to look.
/// - A missing number, an unreadable check, and an unchecked tamper policy are
///   `unverified`. A probe looked, and what it returned settled nothing.
fn rung(rule: &VerdictRule, verdict: &Verdict) -> LadderRung {
    match verdict {
        Verdict::Met { .. } if rule.requirements.is_empty() => LadderRung::Waived,
        Verdict::Met { .. } => LadderRung::SubmitFast,
        Verdict::Unmet { .. } => LadderRung::Revise,
        Verdict::Undecided { reason, .. } => match reason {
            UndecidedReason::WitnessUnsatisfiable => LadderRung::WitnessUnsatisfiable,
            UndecidedReason::NoOracle
            | UndecidedReason::Undecidable { .. }
            | UndecidedReason::FlipUnobservable => LadderRung::Unverifiable,
            UndecidedReason::MeasurementMissing { .. }
            | UndecidedReason::UnreadableCheck { .. }
            | UndecidedReason::TamperUnchecked => LadderRung::Unverified,
        },
    }
}

/// One line a person can read: what was checked, and what it showed.
///
/// Prose for a reader. Nothing parses it.
fn summary(rule: &VerdictRule, verdict: &Verdict) -> String {
    match verdict {
        Verdict::Met { .. } if rule.requirements.is_empty() => {
            "the rule declares nothing to check".to_string()
        }
        Verdict::Met { .. } => format!(
            "every one of the {} declared requirements is met",
            rule.requirements.len()
        ),
        Verdict::Unmet { unmet, .. } => match unmet.first() {
            Some(first) => format!(
                "{} of {} requirements unmet: {first}",
                unmet.len(),
                rule.requirements.len()
            ),
            // `judge` never builds an empty list on this arm, so this is a
            // record that came from somewhere else. It still gets a line.
            None => "a requirement is unmet, and the record names none".to_string(),
        },
        Verdict::Undecided { reason, .. } => format!("nothing settled it: {reason}"),
    }
}

/// What the wrapper saw of the flip, in the record's own words.
///
/// A witness that cannot tell the two trees apart maps to `not_achieved`: it
/// ran, and no flip came of it. That the witness itself is at fault is the
/// rung's job to say, and it says it.
fn flip(observed: FlipObservation) -> FlipOutcome {
    match observed {
        FlipObservation::Achieved => FlipOutcome::Achieved,
        FlipObservation::NotAchieved | FlipObservation::Unsatisfiable => FlipOutcome::NotAchieved,
        // Neither of these is a finding about the work. Nothing was measured.
        FlipObservation::NotAttempted | FlipObservation::Unobservable => FlipOutcome::Unobserved,
    }
}

/// Whether the witness artifacts were the ones that were authored.
///
/// `None` is "no check ran", which is what an absent snapshot means. It is
/// never read as a pass.
fn witness_intact(finding: &TamperFinding) -> Option<bool> {
    match finding {
        TamperFinding::Clean => Some(true),
        TamperFinding::Tampered { .. } => Some(false),
        TamperFinding::NotChecked => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use stella_plugin::{
        FlipPolicy, ObservedEvidence, Oracle, TamperPolicy, UnmetBecause, UnmetRequirement,
    };

    use super::*;

    fn rule() -> VerdictRule {
        let mut requirements = BTreeMap::new();
        requirements.insert(
            "proven".to_string(),
            "a witness failed before the work and passes after it".to_string(),
        );
        VerdictRule {
            requirements,
            oracle: Some(Oracle {
                command: None,
                flip: FlipPolicy::Required,
                tamper: TamperPolicy::ArtifactIdentity,
                measurements: Vec::new(),
                checks: Vec::new(),
            }),
        }
    }

    fn reported(flip: FlipObservation) -> EvidenceSet {
        EvidenceSet::from_observed(
            ObservedEvidence {
                flip,
                measurements: BTreeMap::new(),
                detail: None,
            },
            TamperFinding::Clean,
        )
    }

    fn met() -> Verdict {
        Verdict::Met {
            evidence: EvidenceProvenance::PluginReported,
        }
    }

    fn unmet() -> Verdict {
        Verdict::Unmet {
            unmet: vec![UnmetRequirement {
                requirement: "proven".to_string(),
                statement: "a witness failed before the work".to_string(),
                because: UnmetBecause::NoFlip {
                    observed: FlipObservation::NotAchieved,
                },
                detail: None,
            }],
            undecided: Vec::new(),
        }
    }

    fn timing() -> StampTiming {
        StampTiming {
            decided_at_ms: 1_767_225_600_000,
            duration_ms: 4_210,
            timed_out: false,
        }
    }

    /// The name comes from the manifest the host loaded. Whose observation it
    /// is decides which name, and a plugin has no say in either.
    #[test]
    fn the_author_is_read_from_the_manifest() {
        assert_eq!(
            author(EvidenceProvenance::PluginReported, "witness-v1"),
            "witness-v1"
        );
        assert_eq!(
            author(EvidenceProvenance::HostObserved, "witness-v1"),
            "engine"
        );
    }

    /// The hash covers the record and not the stamps on it, so it can be
    /// recomputed from the stamped record a caller is handed.
    #[test]
    fn the_hash_recomputes_from_the_record_it_was_taken_over() {
        let record = stamped(
            &rule(),
            &reported(FlipObservation::Achieved),
            &met(),
            "witness-v1",
            Vec::new(),
            timing(),
        )
        .expect("a record hashes");

        let stamp = record.stamps.first().expect("one stamp was written");
        let preimage = record.stamp_preimage().expect("the record serializes");
        assert_eq!(stamp.preimage_hash, record_hash(&preimage).unwrap());
        assert!(stamp.preimage_hash.starts_with("sha256:"));
    }

    /// A second claim on the same record leaves the first one's hash alone.
    #[test]
    fn a_later_stamp_does_not_break_an_earlier_one() {
        let record = stamped(
            &rule(),
            &reported(FlipObservation::Achieved),
            &met(),
            "witness-v1",
            Vec::new(),
            timing(),
        )
        .expect("a record hashes");
        let first = record.stamps[0].clone();

        let twice = record.with_stamp(VerdictStamp {
            author: "engine".to_string(),
            ..first.clone()
        });
        let preimage = twice.stamp_preimage().expect("the record serializes");
        assert_eq!(first.preimage_hash, record_hash(&preimage).unwrap());
    }

    /// Stamping decides nothing. The rung is the same with a claim on the
    /// record and without one.
    #[test]
    fn a_stamp_never_moves_the_rung() {
        for (evidence, verdict) in [
            (reported(FlipObservation::Achieved), met()),
            (reported(FlipObservation::NotAchieved), unmet()),
        ] {
            let bare = snapshot(&rule(), &evidence, &verdict);
            let claimed = stamped(
                &rule(),
                &evidence,
                &verdict,
                "witness-v1",
                Vec::new(),
                timing(),
            )
            .expect("a record hashes");
            assert_eq!(bare.rung, claimed.rung);
            assert!(bare.stamps.is_empty());
            assert_eq!(claimed.stamps.len(), 1);
        }
    }

    /// A wrapper that declares no requirement claims nothing. Reading its pass
    /// as `done` would say the work was checked, and nothing was.
    #[test]
    fn a_rule_with_nothing_to_check_claims_nothing() {
        let empty = VerdictRule::default();
        let record = stamped(
            &empty,
            &reported(FlipObservation::NotAttempted),
            &met(),
            "steering-v1",
            Vec::new(),
            timing(),
        )
        .expect("a record hashes");

        assert_eq!(record.rung, Some(LadderRung::Waived));
        assert_eq!(
            record.stamps[0].assessment,
            StampAssessment::NotApplicable,
            "a pass with nothing asked for is not a claim that the work is done"
        );
    }

    /// An abstention does not read as a finding against the work.
    #[test]
    fn an_abstention_is_not_a_failure() {
        let undecided = Verdict::Undecided {
            reason: UndecidedReason::FlipUnobservable,
            undecided: Vec::new(),
        };
        let record = stamped(
            &rule(),
            &EvidenceSet::unobserved(),
            &undecided,
            "witness-v1",
            Vec::new(),
            timing(),
        )
        .expect("a record hashes");

        assert_eq!(record.rung, Some(LadderRung::Unverifiable));
        assert_eq!(record.stamps[0].assessment, StampAssessment::Inconclusive);
        assert_eq!(
            record.stamps[0].author, "engine",
            "an unobserved set is the host's own conclusion"
        );
        assert_eq!(record.flip, FlipOutcome::Unobserved);
        assert_eq!(record.witness_intact, None);
    }

    /// A tamper finding reaches the record in both directions. A modified
    /// artifact is a fact about the run, and dropping it would leave the
    /// record saying no check ran.
    #[test]
    fn a_modified_artifact_is_recorded() {
        let tampered = EvidenceSet {
            tamper: TamperFinding::Tampered {
                artifact: "tests/witness.rs".to_string(),
            },
            ..reported(FlipObservation::Achieved)
        };
        let record = snapshot(&rule(), &tampered, &unmet());
        assert_eq!(record.witness_intact, Some(false));
        assert_eq!(record.rung, Some(LadderRung::Revise));
    }

    /// A witness that fails the same way twice never ran a flip anyone can
    /// use, and the record says so without calling the work wrong.
    #[test]
    fn an_unsatisfiable_witness_is_not_an_unobserved_one() {
        let record = snapshot(
            &rule(),
            &reported(FlipObservation::Unsatisfiable),
            &Verdict::Undecided {
                reason: UndecidedReason::WitnessUnsatisfiable,
                undecided: Vec::new(),
            },
        );
        assert_eq!(record.flip, FlipOutcome::NotAchieved);
        assert_eq!(record.rung, Some(LadderRung::WitnessUnsatisfiable));
    }
}
