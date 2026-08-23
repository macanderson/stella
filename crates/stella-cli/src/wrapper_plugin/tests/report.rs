// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a wrapper run tells the terminal: the report lines a dispatch prints,
//! and which process owes the `--no-pipeline` deprecation notice.
//!
//! A submodule of `wrapper_plugin::tests` rather than more lines in it, for the
//! reason that file is itself a sibling of `wrapper_plugin.rs`: it sits at the
//! 1500-line ratchet, and these two suites are a coherent unit of their own.

use super::*;

/// A report with one fault of each kind, so [`report_lines`] has something to
/// render on every one of its arms.
fn faulted_report() -> stella_runtime::wrapper::DispatchReport {
    stella_runtime::wrapper::DispatchReport {
        variant: "vera".to_string(),
        rounds: 1,
        verdict: stella_plugin::Verdict::Undecided {
            reason: stella_plugin::UndecidedReason::NoOracle,
        },
        outcome: stella_plugin::Outcome::Undecided {
            reason: stella_plugin::UndecidedReason::NoOracle,
        },
        faults: vec![stella_runtime::wrapper::WrapperError::EmptyArgv],
    }
}

/// **Witness (#3883).** Every line a wrapper's report prints names the lane it
/// came from, when there is more than one lane to confuse.
///
/// `stella fleet --max-concurrency > 1` binds one wrapper per worker attempt
/// and every one of them prints onto the same stderr, so an operator reading
/// `! wrapper: …` had no way to tell which task's plugin said it. Asserted
/// over the pure renderer rather than by capturing stderr, and over *all* the
/// lines rather than the first, because a partial prefix is the shape that
/// looks right until two workers disagree.
#[test]
fn a_scoped_report_names_its_task_on_every_line() {
    use stella_protocol::event::ModelCallRole;
    use stella_runtime::wrapper::{CandidateFanoutSpend, ChildTurnSpend};

    let roster = roster(vec![installed(WRAPPER_MANIFEST, "/tmp/budget-keeper")]);
    let wrapper = bound(&roster, "budget-v1", &mut |_| {}).expect("the fixture binds");
    let report = faulted_report();
    let spends = [ChildTurnSpend {
        role: "reviewer".to_string(),
        seat: ModelCallRole::Research,
        cost_usd: 0.02,
        steps: 1,
        completed: true,
    }];
    let fanouts = [CandidateFanoutSpend {
        role: "attempt".to_string(),
        seat: ModelCallRole::Worker,
        requested_width: 2,
        width: 2,
        cost_usd: 0.1,
        completed: 2,
    }];
    let test_runs = [stella_runtime::wrapper::TestRunRecord {
        candidate: "host-tree".to_string(),
        assertions: stella_plugin::TestBaseline::Passed,
    }];

    let scoped = super::report_lines(
        Some("build-parser"),
        OutputFormat::Text,
        &report,
        &wrapper.gates,
        &spends,
        &fanouts,
        &test_runs,
    );
    assert!(
        scoped.len() >= 4,
        "the fault, the two spend lines and the summary: {scoped:#?}"
    );
    for line in &scoped {
        assert!(
            line.contains("[build-parser]"),
            "an unattributed line is one an operator cannot place: {line}"
        );
    }
    assert!(
        scoped.iter().any(|line| line.starts_with("  ! ")),
        "the marker keeps its column: {scoped:#?}"
    );
    assert!(
        scoped.iter().any(|line| line.starts_with("  ◇ ")),
        "and so does the summary's: {scoped:#?}"
    );

    // The doors with one lane are unchanged — the wording is the same
    // sentence with nothing in front of it.
    let unscoped = super::report_lines(
        None,
        OutputFormat::Text,
        &report,
        &wrapper.gates,
        &spends,
        &fanouts,
        &test_runs,
    );
    assert_eq!(unscoped.len(), scoped.len());
    for (bare, tagged) in unscoped.iter().zip(&scoped) {
        assert!(!bare.contains("[build-parser]"), "{bare}");
        assert_eq!(
            tagged.replace("[build-parser] ", ""),
            *bare,
            "one renderer, one wording — the scope is the only difference"
        );
    }
}

/// **Witness (#4418).** The candidate sweep's line comes off the same
/// renderer as every other wrapper line.
///
/// It used to format `  ! wrapper: ` by hand at the one call site, which is
/// harmless on a one-lane door and is the copy that drifts the next time the
/// marker or the scope tag's position moves. Asserted against
/// [`report_lines`]' own output rather than against a literal, because a
/// literal here would be the third copy.
#[test]
fn the_candidate_sweep_renders_through_the_shared_wrapper_line() {
    let leaked = ["/tmp/ws/.stella/candidates/a: still checked out".to_string()];

    let bare = super::sweep_lines(None, &leaked);
    let roster = roster(vec![installed(WRAPPER_MANIFEST, "/tmp/budget-keeper")]);
    let wrapper = bound(&roster, "budget-v1", &mut |_| {}).expect("the fixture binds");
    let faults = super::report_lines(
        None,
        OutputFormat::Text,
        &faulted_report(),
        &wrapper.gates,
        &[],
        &[],
        &[],
    );
    let prefix_of = |line: &str| {
        line.split_once("wrapper: ")
            .map(|(head, _)| format!("{head}wrapper: "))
    };
    assert_eq!(
        prefix_of(&bare[0]),
        prefix_of(&faults[0]),
        "one marker, one producer"
    );

    let scoped = super::sweep_lines(Some("build-parser"), &leaked);
    assert_eq!(
        scoped[0],
        bare[0].replace("wrapper: ", "[build-parser] wrapper: "),
        "and the scope tag follows the marker here too"
    );

    assert!(
        super::sweep_lines(None, &[]).is_empty(),
        "a run that leaked nothing stays silent"
    );
}

/// **Witness (#3774).** Which process owes the notice is decided by whether
/// the child's own copy is relayed back, not by whether there is a child.
///
/// `Detached` supervises exactly as `Attached` does, and that is the whole
/// defect: gating on `supervises()` alone left the launching terminal of a
/// `--detach` run with nothing, because `detach::release` never follows the
/// child's console and the child's copy went to a log file. The two asserts
/// on the same posture are what separate the fix from the bug — a rule that
/// answered "does this process supervise?" cannot tell these two apart.
#[test]
fn a_detached_launcher_owes_the_notice_and_an_attached_one_does_not() {
    use crate::daemon::detach::Posture;

    assert!(Posture::Detached.supervises());
    assert_eq!(
        no_pipeline_notice_for(Posture::Detached, true),
        no_pipeline_deprecation_notice(true),
        "nothing relays a detached child's stderr, so the launcher says it"
    );

    assert!(Posture::Attached.supervises());
    assert_eq!(
        no_pipeline_notice_for(Posture::Attached, true),
        None,
        "the child's own copy is relayed live, so a second one would double it"
    );

    assert_eq!(
        no_pipeline_notice_for(Posture::Foreground, true),
        no_pipeline_deprecation_notice(true),
        "there is no child at all"
    );
    for posture in [Posture::Foreground, Posture::Attached, Posture::Detached] {
        assert_eq!(
            no_pipeline_notice_for(posture, false),
            None,
            "the flag was not passed, so nothing is owed under any posture"
        );
    }
}
