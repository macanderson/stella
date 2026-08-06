//! #1701: the ladder rung for work whose effects this run never collected.
//!
//! Its own module because the parent was one edit away from the 1500-line
//! ratchet, and because these cases share a premise the rest of the ladder
//! tests do not: every probe here *worked*. The blind rung's tests next door
//! ask what happens when nothing could look. These ask the harder question —
//! what a clear-eyed zero means when the pipeline's own record says calls able
//! to write the workspace were dispatched.

use crate::verify::{LadderDecision, LadderInputs, ladder_decision, unverifiable_evidence};

/// The inputs shared by both cases: the nginx trial, read off its own stream.
fn escaped() -> LadderInputs {
    LadderInputs {
        flip_achieved: false,
        touched_tests_passed: None,
        diff_lines: 0,
        diff_budget: 100,
        // The half that matters. The probe ran, succeeded, and reported an
        // unchanged tree — this is not the blind case wearing a disguise.
        diff_available: true,
        file_change_events: 0,
        mutating_actions: 10,
        ..Default::default()
    }
}

/// The nginx trial's ladder inputs. Ten calls able to write, a diff probe that
/// ran and read an unchanged tree, and nothing else that could speak. The run
/// reported `passed: true, deterministic: true, rung: waived`.
///
/// The mirror image of `every_channel_blind_abstains_…` in the parent module:
/// there, no channel could look; here, they looked and none of them saw work
/// that was demonstrably done. Both are the same epistemic state — the turn
/// went unobserved — so both abstain, and neither may claim.
#[test]
fn effects_that_escaped_collection_abstain_rather_than_claim_a_clean_tree() {
    let inputs = escaped();
    assert!(inputs.effects_escaped_collection());
    assert!(
        !inputs.nothing_was_attempted(),
        "ten dispatched calls is the opposite of the no-op rung's premise"
    );
    assert_eq!(ladder_decision(&inputs), LadderDecision::Unverifiable);

    // And the summary must not borrow the blind rung's excuse. This probe
    // worked; saying it could not read the tree would send a reader to repair
    // the one part of this that was functioning.
    let evidence = unverifiable_evidence(&inputs);
    assert!(evidence.summary.starts_with("UNVERIFIABLE"));
    assert!(!evidence.deterministic);
    assert!(
        !evidence.summary.contains("could not read the working tree"),
        "the probe read it fine: {}",
        evidence.summary
    );
    assert!(
        evidence.summary.contains("10"),
        "and the count that contradicts a clean tree has to appear: {}",
        evidence.summary
    );
}

/// The guard rails on the rung: any single corroborating observation takes the
/// turn back to a rung that can credit it. Abstention is for the state where
/// *nothing* saw the work, not for every empty diff.
#[test]
fn one_corroborating_observation_lifts_the_turn_off_the_abstain_rung() {
    for (label, inputs) in [
        (
            "a recorded file touch proves the tree moved",
            LadderInputs {
                file_change_events: 3,
                ..escaped()
            },
        ),
        (
            "a green test is an observation of the work",
            LadderInputs {
                touched_tests_passed: Some(true),
                ..escaped()
            },
        ),
        (
            "a flip is the strongest corroboration there is",
            LadderInputs {
                flip_achieved: true,
                ..escaped()
            },
        ),
    ] {
        assert!(!inputs.effects_escaped_collection(), "{label}");
        assert_ne!(
            ladder_decision(&inputs),
            LadderDecision::Unverifiable,
            "{label}"
        );
    }
}
