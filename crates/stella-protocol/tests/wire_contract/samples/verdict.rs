// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The verdict half of the sample corpus: the ladder vocabularies, and the
//! snapshots that carry them onto the wire.
//!
//! Split out of `samples.rs` when that file met the file-size ceiling
//! (`scripts/check-file-size.sh`). This is the half that grows when the
//! ladder does.

use stella_protocol::ladder::{
    FlipOutcome, LadderRung, LadderSnapshot, OracleObservation, ProofTree, StampAssessment,
    VerdictStamp,
};
use stella_protocol::{AgentEvent, VerdictEvidence};

pub(crate) fn all_ladder_rungs() -> Vec<LadderRung> {
    use LadderRung::*;
    vec![
        SubmitFast,
        Revise,
        NothingAttempted,
        Unverifiable,
        Unverified,
        WitnessUnsatisfiable,
        Waived,
    ]
}

pub(crate) fn all_flip_outcomes() -> Vec<FlipOutcome> {
    use FlipOutcome::*;
    vec![Unobserved, NotAchieved, Achieved]
}

pub(crate) fn all_stamp_assessments() -> Vec<StampAssessment> {
    use StampAssessment::*;
    vec![Done, NotDone, Inconclusive, NotApplicable]
}

/// A snapshot with every evidence channel set, so each sweep below names only
/// the fields it samples.
fn sampled_ladder() -> LadderSnapshot {
    LadderSnapshot {
        rung: None,
        tracked_command: Some("cargo test -p x".into()),
        oracle_trace: vec![OracleObservation {
            tree: ProofTree::Candidate,
            passed: true,
        }],
        flip: FlipOutcome::Achieved,
        unstable_flip: false,
        flip_refused_different_failure: false,
        touched_tests_passed: Some(true),
        test_infra: Some("timed_out".into()),
        diff_lines: 12,
        diff_budget: 400,
        diff_available: true,
        mutating_actions: 3,
        new_diag_errors: 0,
        new_diag_warnings: 1,
        witness_intact: Some(true),
        witness_mutation: Some(true),
        diff_coverage: Some("covered".into()),
        verify_done_flip: true,
        no_test_surface: true,
        errored_commands: 2,
        verifier_independent: Some(false),
        stamps: vec![],
    }
}

/// Two observers on one record, so a sample carries a split, not one claim.
/// The engine's stamp leaves the optional fields unset. The plugin's sets
/// them. Both shapes reach the wire.
fn sample_stamps(assessment: StampAssessment) -> Vec<VerdictStamp> {
    vec![
        VerdictStamp {
            author: "engine".into(),
            author_version: None,
            assessment: StampAssessment::Inconclusive,
            summary: "no probe could look".into(),
            preimage_hash: format!("sha256:{}", "a1".repeat(32)),
            evidence_refs: vec![],
            decided_at_ms: 1_767_225_600_000,
            duration_ms: 12,
            timed_out: false,
        },
        VerdictStamp {
            author: "vera".into(),
            author_version: Some("2.1.0".into()),
            assessment,
            summary: "sampled for the stamp assessment".into(),
            preimage_hash: format!("sha256:{}", "a1".repeat(32)),
            evidence_refs: vec!["trace:t1#verify".into()],
            decided_at_ms: 1_767_225_604_210,
            duration_ms: 4_210,
            timed_out: true,
        },
    ]
}

/// The verdict samples: one per rung, one per flip outcome, one per stamp
/// assessment.
pub(crate) fn verdict_events() -> Vec<AgentEvent> {
    let mut events = Vec::new();
    // Every ladder rung. Each one has to reach the wire on its own. The rung
    // is the only thing that tells two verdicts apart when the `passed` and
    // `deterministic` flags spell them the same way: a deterministic pass, a
    // waived review, a verifier that answered, one that could not.
    events.extend(
        all_ladder_rungs()
            .into_iter()
            .map(|rung| AgentEvent::Verdict {
                passed: rung.is_deterministic(),
                evidence: VerdictEvidence {
                    summary: "sampled for the rung".into(),
                    deterministic: rung.is_deterministic(),
                    evidence_refs: vec![],
                    ladder: Some(Box::new(LadderSnapshot {
                        rung: Some(rung),
                        ..sampled_ladder()
                    })),
                },
            }),
    );
    // Every flip outcome. The rung sweep above pins only `achieved`. The two
    // it misses are the pair the tri-state splits: `not_achieved` is a claim
    // about the work, `unobserved` is a claim about the tool. A sample for
    // just one of them would leave that split unproven at the one surface
    // where the old bool lost it: the stored verdict.
    events.extend(
        all_flip_outcomes()
            .into_iter()
            .map(|flip| AgentEvent::Verdict {
                passed: flip.is_achieved(),
                evidence: VerdictEvidence {
                    summary: "sampled for the flip outcome".into(),
                    deterministic: flip.is_achieved(),
                    evidence_refs: vec![],
                    ladder: Some(Box::new(LadderSnapshot {
                        rung: None,
                        // `unobserved` is the state where no command was
                        // ever tracked. The sample pairs the two the way a
                        // run would.
                        tracked_command: flip.was_observed().then(|| "cargo test -p x".to_string()),
                        oracle_trace: vec![],
                        flip,
                        touched_tests_passed: None,
                        test_infra: None,
                        diff_lines: 3,
                        mutating_actions: 1,
                        new_diag_warnings: 0,
                        witness_intact: None,
                        witness_mutation: None,
                        diff_coverage: None,
                        verify_done_flip: false,
                        no_test_surface: !flip.was_observed(),
                        errored_commands: 0,
                        verifier_independent: None,
                        ..sampled_ladder()
                    })),
                },
            }),
    );
    // Every stamp assessment. The sweeps above pin an unstamped snapshot,
    // the shape stored verdicts have. A stamped one is the other shape a
    // consumer must parse. `inconclusive` and `not_done` are the pair to
    // keep apart.
    events.extend(
        all_stamp_assessments()
            .into_iter()
            .map(|assessment| AgentEvent::Verdict {
                passed: false,
                evidence: VerdictEvidence {
                    summary: "sampled for the stamp assessment".into(),
                    deterministic: false,
                    evidence_refs: vec![],
                    ladder: Some(Box::new(LadderSnapshot {
                        rung: Some(LadderRung::Unverified),
                        flip: FlipOutcome::NotAchieved,
                        stamps: sample_stamps(assessment),
                        ..sampled_ladder()
                    })),
                },
            }),
    );
    events
}
