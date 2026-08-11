// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! #2540: the ladder rung for a witness that cannot discriminate.
//!
//! Its own module for the reason the neighbouring one has: the parent file is
//! near the size ratchet, and these cases share a premise none of the other
//! ladder tests do. Everywhere else on the ladder a red test is the clearest
//! evidence there is. Here it is evidence about the *instrument*, and the
//! whole question is which of the two a red witness is — decided by whether
//! the failure moved when the work did.

use crate::verify::{
    LadderDecision, LadderInputs, ladder_decision, witness_unsatisfiable_evidence,
};

/// A witness that failed post-execute. Whether that red belongs to the work or
/// to the instrument is exactly what the flag below decides.
fn red_witness() -> LadderInputs {
    LadderInputs {
        touched_tests_passed: Some(false),
        diff_lines: 12,
        diff_budget: 400,
        diff_available: true,
        mutating_actions: 4,
        ..Default::default()
    }
}

/// The rung exists and outranks `Revise`.
///
/// The ordering is the entire point of the rung: `Revise` is the ladder's
/// first step, and it hands the failure back to the worker as the thing to
/// fix. A witness that says the same thing however the work changes cannot be
/// fixed by the worker, and telling it otherwise is what turned Terminal-Bench
/// `fix-git` — a task Stella had solved at 5.7 minutes — into a 13-minute
/// deadline loss.
#[test]
fn a_witness_unmoved_by_a_revision_outranks_blaming_the_worker() {
    let inputs = LadderInputs {
        witness_unmoved_by_revision: true,
        ..red_witness()
    };
    assert_eq!(
        ladder_decision(&inputs),
        LadderDecision::WitnessUnsatisfiable,
        "a red witness that did not respond to the revision is a fact about the witness"
    );
}

/// …and only then. The same red without the flag is ordinary feedback, and
/// stripping the worker of it would cost every task whose first attempt misses.
#[test]
fn a_witness_whose_failure_moved_still_sends_the_worker_to_revise() {
    assert_eq!(ladder_decision(&red_witness()), LadderDecision::Revise);
}

/// The flag alone cannot manufacture the rung: with no failing test there is
/// no failure to be unmoved, and a stale `true` must not reroute a turn that
/// the rest of the ladder is entitled to decide.
#[test]
fn the_flag_carries_nothing_without_a_red_witness_beside_it() {
    let green = LadderInputs {
        touched_tests_passed: Some(true),
        witness_unmoved_by_revision: true,
        ..red_witness()
    };
    assert_ne!(
        ladder_decision(&green),
        LadderDecision::WitnessUnsatisfiable,
        "the rung reports a failure that did not move, not a flag"
    );
}

/// `Default` denies it, like every other dispensation on this struct: a caller
/// that forgets the field gets the conservative answer.
#[test]
fn the_default_input_does_not_claim_an_unsatisfiable_witness() {
    assert!(!LadderInputs::default().witness_unmoved_by_revision);
}

/// The evidence names the shared fingerprint and refuses to read as a failure.
///
/// Both halves matter. The fingerprint is the only thing that distinguishes
/// this from "the test is red", so a summary without it asks a reader to take
/// the finding on trust; and the sentence disclaiming a finding about the work
/// is what stops the rung being read as a verdict against the change — the
/// exact conflation the `-able`/`-ed` rungs already guard.
#[test]
fn the_evidence_names_the_fingerprint_and_disclaims_a_finding_about_the_work() {
    let evidence = witness_unsatisfiable_evidence(Some("sh witness_check.sh"), "deadbeefdeadbeef");
    assert!(
        evidence.summary.starts_with("WITNESS UNSATISFIABLE"),
        "{}",
        evidence.summary
    );
    assert!(
        evidence.summary.contains("deadbeefdeadbeef"),
        "the shared fingerprint is the finding: {}",
        evidence.summary
    );
    assert!(
        evidence.summary.contains("sh witness_check.sh"),
        "{}",
        evidence.summary
    );
    assert!(
        evidence
            .summary
            .contains("NOT a finding that the work is absent or wrong"),
        "{}",
        evidence.summary
    );
    assert!(
        !evidence.deterministic,
        "nothing deterministic was established about the change"
    );
}
