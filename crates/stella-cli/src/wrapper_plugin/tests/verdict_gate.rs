// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Whose conclusion decides the exit status (#3554).
//!
//! `tests/run_require_verdict_cli.rs` is the end-to-end witness — it is the
//! only place the process's actual exit code is observable. These cover the
//! arms that test cannot reach cheaply: `Undecided`, and the wording each
//! refusal carries.

use super::*;
use crate::wrapper_plugin::verdict_gate::verdict_refusal;
use stella_plugin::Outcome;

fn unmet(requirement: &str) -> Outcome {
    Outcome::Unmet {
        unmet: vec![stella_plugin::UnmetRequirement {
            requirement: requirement.to_string(),
            statement: "the statement".to_string(),
            because: stella_plugin::UnmetBecause::NoFlip {
                observed: stella_plugin::FlipObservation::NotAchieved,
            },
            detail: None,
        }],
        stopped: stella_plugin::StopReason::NotAnArbiter,
    }
}

/// The default is the decision, not the absence of one: installing a third
/// party's manifest must not by itself gain the power to fail a build.
#[test]
fn nothing_fails_without_the_flag() {
    assert_eq!(verdict_refusal(false, &unmet("proven")), None);
    assert_eq!(
        verdict_refusal(
            false,
            &Outcome::Undecided {
                reason: stella_plugin::UndecidedReason::NoOracle,
            }
        ),
        None
    );
}

#[test]
fn a_met_verdict_passes_the_gate() {
    assert_eq!(
        verdict_refusal(
            true,
            &Outcome::Met {
                evidence: stella_plugin::EvidenceProvenance::HostObserved,
            }
        ),
        None
    );
}

#[test]
fn an_unmet_verdict_names_the_requirements_it_left() {
    let refusal = verdict_refusal(true, &unmet("proven")).expect("unmet fails under the flag");
    assert!(refusal.contains("--require-verdict"), "{refusal}");
    assert!(refusal.contains("1 requirement"), "{refusal}");
    assert!(refusal.contains("proven"), "{refusal}");
}

/// A gate whose purpose is "do not ship what the wrapper did not vouch for"
/// cannot pass on "nothing decided it either way" — that is exactly the case
/// where nothing vouched. The message says which of the two it was, because
/// `Unmet` is work to redo and `Undecided` is usually a manifest problem.
#[test]
fn an_undecided_verdict_fails_too_and_says_why_nothing_decided() {
    let refusal = verdict_refusal(
        true,
        &Outcome::Undecided {
            reason: stella_plugin::UndecidedReason::NoOracle,
        },
    )
    .expect("undecided fails under the flag");
    assert!(refusal.contains("reached no verdict"), "{refusal}");
    assert!(refusal.contains("[oracle]"), "{refusal}");
}

/// The flag is honored on `--pipeline <variant>` and meaningless without one,
/// so the raw loop refuses it rather than accepting it and exiting 0 anyway.
#[test]
fn the_raw_loop_refuses_the_flag_and_a_wrapper_accepts_it() {
    let raw = reject_require_verdict_without_wrapper(PipelineChoice::Raw, true)
        .expect_err("--require-verdict reads nothing on the raw loop");
    assert!(raw.contains("--require-verdict"), "{raw}");
    assert!(raw.contains("--pipeline"), "{raw}");

    assert!(reject_require_verdict_without_wrapper(PipelineChoice::Raw, false).is_ok());
    assert!(reject_require_verdict_without_wrapper(PipelineChoice::Plugin("vera"), true).is_ok());
}
