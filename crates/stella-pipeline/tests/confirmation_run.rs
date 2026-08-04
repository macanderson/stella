//! Witness test for #859 — the confirmation run on flip.
//!
//! Lives outside the implementation so `verify_done` can prove the change is
//! real: before #859, `FlipOracle::confirm` and `FlipOracle::is_unstable` do
//! not exist, so this file fails to compile. With the change, it compiles and
//! passes.

use stella_pipeline::verify::ladder_decision;
use stella_pipeline::{FlipOracle, FlipState, LadderDecision, LadderInputs};

#[test]
fn a_flaky_flip_is_caught_by_the_confirmation_run() {
    // Baseline: the test fails.
    let mut oracle = FlipOracle::new();
    oracle.observe("cargo test -p x", false);
    assert_eq!(oracle.state(), FlipState::Failing);

    // The candidate run: the test passes — the oracle flips.
    oracle.observe("cargo test -p x", true);
    assert!(oracle.is_flipped());

    // The confirmation re-run (#859): the test fails again — it was a flake.
    // The oracle moves to Unstable, which is NOT flipped.
    oracle.confirm(false);
    assert!(
        !oracle.is_flipped(),
        "an unconfirmed flip must not be Flipped"
    );
    assert!(oracle.is_unstable());

    // The ladder must NOT credit a deterministic pass for an unstable oracle.
    let inputs = LadderInputs {
        flip_achieved: oracle.is_flipped(), // false
        touched_tests_passed: Some(true),
        diff_lines: 5,
        diff_budget: 100,
        diff_available: true,
        file_change_events: 1,
        mutating_actions: 1,
        ..Default::default()
    };
    assert_eq!(
        ladder_decision(&inputs),
        LadderDecision::ModelJudge,
        "an unconfirmed flip must escalate to the judge, not SubmitFast"
    );
}

#[test]
fn a_genuine_flip_survives_the_confirmation_run() {
    let mut oracle = FlipOracle::new();
    oracle.observe("cargo test -p x", false);
    oracle.observe("cargo test -p x", true);
    assert!(oracle.is_flipped());

    // The confirmation re-run passes — the flip is real.
    oracle.confirm(true);
    assert!(oracle.is_flipped());
    assert!(!oracle.is_unstable());
}
