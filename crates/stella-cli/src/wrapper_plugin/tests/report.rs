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
///
/// The arbitration record is built the way `WrapperDispatch::run` builds it —
/// an abstention per fault, then the arbiter's own claim from the verdict —
/// so the lines asserted here are the lines a real run produces.
fn faulted_report() -> stella_runtime::wrapper::DispatchReport {
    use stella_runtime::wrapper::{ArbiterClaim, TurnHoldBudget, fold_stamps};

    let fault = stella_runtime::wrapper::WrapperError::EmptyArgv;
    let grant = stella_plugin::LoopGrant {
        participation: stella_plugin::Participation::Arbiter,
        ..stella_plugin::LoopGrant::default()
    };
    let verdict = stella_plugin::Verdict::Undecided {
        reason: stella_plugin::UndecidedReason::NoOracle,
        // No oracle ran, so no requirement was individually decided; the
        // board below declares none either.
        undecided: Vec::new(),
    };
    let arbitration = fold_stamps(
        None,
        &[
            ArbiterClaim::did_not_answer("vera", &fault, &grant),
            ArbiterClaim::from_verdict("vera", &verdict, &grant, 0),
        ],
        TurnHoldBudget {
            turn_holds_spent: 0,
            host_max_holds: 2,
        },
    );
    stella_runtime::wrapper::DispatchReport {
        variant: "vera".to_string(),
        rounds: 1,
        verdict: verdict.clone(),
        outcome: stella_plugin::Outcome::Undecided {
            reason: stella_plugin::UndecidedReason::NoOracle,
        },
        // A rule with no requirements draws no rows: nothing was declared, so
        // there is no gate to report on.
        board: stella_protocol::GateBoard::default(),
        // Built by the same fold a real round uses, so this fixture cannot
        // drift from the record a dispatch actually hands back.
        snapshot: stella_runtime::wrapper::stamp::snapshot(
            &stella_plugin::VerdictRule::default(),
            &stella_plugin::EvidenceSet::unobserved(),
            &verdict,
        ),
        faults: vec![fault],
        arbitration,
    }
}

/// **Witness.** A run whose arbiter did not answer says so.
///
/// A fault line says what broke without saying what the gate did about it, so
/// on its own the trace of a run whose arbiter crashed reads exactly like the
/// trace of a run whose arbiter was satisfied. The abstention line is the
/// missing half, and it names the arbiter so a composition's reader knows
/// which one fell silent.
///
/// The same report also holds the arbiter's *answered* abstention —
/// `Verdict::Undecided`, an observer that looked and could not tell — and
/// that one draws no line. One "did not answer" per report, for the observer
/// that genuinely did not.
#[test]
fn an_arbiter_that_did_not_answer_gets_a_line_of_its_own() {
    let roster = roster(vec![installed(WRAPPER_MANIFEST, "/tmp/budget-keeper")]);
    let wrapper = bound(&roster, "budget-v1", &mut |_| {}).expect("the fixture binds");
    let lines = super::report_lines(
        None,
        OutputFormat::Text,
        &faulted_report(),
        &wrapper.gates,
        &[],
        &[],
        &[],
    );

    let attributed: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("did not answer"))
        .collect();
    assert_eq!(
        attributed.len(),
        1,
        "one line for the observer that never answered: {lines:#?}"
    );
    assert!(
        attributed[0].contains("arbiter vera did not answer"),
        "the line names the arbiter that fell silent: {}",
        attributed[0]
    );
    assert!(
        attributed[0].contains("nothing was held open"),
        "and says the turn was not blocked by it: {}",
        attributed[0]
    );
}

/// **Witness.** A claim of `done` beside a rung that says a real test still
/// fails prints a line saying so, rather than reading like an ordinary pass.
///
/// A build that never hands `fold_stamps` the round's rung leaves
/// `Arbitration::rung` at `None` always, so `refutes_done` can never answer
/// `true` there and this line can never print. This report is built the way a
/// real dispatch builds one when it does pass the rung through — a `Done`
/// claim sitting beside a rung `deterministic_failure` reads as a real
/// failure — so the assertion below distinguishes the two.
#[test]
fn a_done_claim_beside_a_failing_rung_says_the_rung_wins() {
    use stella_runtime::wrapper::{ArbiterClaim, TurnHoldBudget, fold_stamps};

    let grant = stella_plugin::LoopGrant {
        participation: stella_plugin::Participation::Arbiter,
        ..stella_plugin::LoopGrant::default()
    };
    let verdict = stella_plugin::Verdict::Met {
        evidence: stella_plugin::EvidenceProvenance::PluginReported,
    };
    let arbitration = fold_stamps(
        Some(stella_protocol::LadderRung::Revise),
        &[ArbiterClaim::from_verdict("vera", &verdict, &grant, 0)],
        TurnHoldBudget {
            turn_holds_spent: 0,
            host_max_holds: 2,
        },
    );
    let report = stella_runtime::wrapper::DispatchReport {
        verdict: verdict.clone(),
        outcome: stella_plugin::Outcome::Met {
            evidence: stella_plugin::EvidenceProvenance::PluginReported,
        },
        snapshot: stella_runtime::wrapper::stamp::snapshot(
            &stella_plugin::VerdictRule::default(),
            &stella_plugin::EvidenceSet::unobserved(),
            &verdict,
        ),
        faults: Vec::new(),
        arbitration,
        ..faulted_report()
    };

    let lines = super::report_lines(None, OutputFormat::Text, &report, &[], &[], &[], &[]);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("the rung's evidence wins")),
        "a done claim beside a failing rung must not read as an ordinary pass: {lines:#?}"
    );
}

/// A fan-out's spend is printed, and the clamp is printed with it.
///
/// The largest spend a plugin can cause on one host call — N *writing* worker
/// turns — so a run that reported child turns and stayed silent here would be
/// visible about the cheap spend and quiet about the expensive one. The
/// requested width appears only when it differs from what ran, because "asked
/// 5, ran 3" and "asked 3, ran 3" are different facts about a plugin and only
/// the host knows which one happened.
///
/// **Also asserts the line names the plugin that spent it**, and prints the
/// seat in the wire vocabulary telemetry carries — a lowercase `"worker"` —
/// never `{:?}`'s `"Worker"`. A build that prints `seat Worker` with no
/// plugin fails both assertions.
#[test]
fn a_fanout_reports_what_it_bought_and_names_a_clamp() {
    use stella_protocol::event::ModelCallRole;
    use stella_runtime::wrapper::CandidateFanoutSpend;

    let clamped = super::fanout_spend_lines(&[CandidateFanoutSpend {
        plugin: "candidates-wrapper".into(),
        role: "attempt".into(),
        seat: ModelCallRole::Worker,
        requested_width: 5,
        width: 3,
        cost_usd: 0.4200,
        completed: 2,
    }])
    .remove(0);
    assert!(clamped.contains("3 candidate turn(s)"), "{clamped}");
    assert!(clamped.contains("attempt"), "{clamped}");
    assert!(clamped.contains("0.4200"), "the money is named: {clamped}");
    assert!(clamped.contains("2 finished"), "{clamped}");
    assert!(
        clamped.contains("candidates-wrapper"),
        "the line names the plugin that spent it, matching the child-turn line beside it: \
         {clamped}"
    );
    assert!(
        clamped.contains("seat worker"),
        "the seat prints in the wire vocabulary telemetry carries, not a Rust identifier: \
         {clamped}"
    );
    assert!(
        !clamped.contains("Worker"),
        "no Debug rendering of the enum survives to the report: {clamped}"
    );
    assert!(
        clamped.contains("asked for 5"),
        "a clamp the plugin could not see is a clamp it will report as its \
         own choice: {clamped}"
    );

    let unclamped = super::fanout_spend_lines(&[CandidateFanoutSpend {
        plugin: "candidates-wrapper".into(),
        role: "attempt".into(),
        seat: ModelCallRole::Worker,
        requested_width: 3,
        width: 3,
        cost_usd: 0.1,
        completed: 3,
    }])
    .remove(0);
    assert!(
        !unclamped.contains("asked for"),
        "nothing was clamped, so nothing is said about it: {unclamped}"
    );

    assert!(
        super::fanout_spend_lines(&[]).is_empty(),
        "a plugin that never fanned out keeps a silent run silent"
    );
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
        plugin: "budget-keeper".to_string(),
        role: "reviewer".to_string(),
        seat: "research".to_string(),
        cost_usd: 0.02,
        steps: 1,
        completed: true,
    }];
    let fanouts = [CandidateFanoutSpend {
        plugin: "budget-keeper".to_string(),
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
