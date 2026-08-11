//! #2873: the ladder does not decide anything from a tool-call tally.
//!
//! `LadderInputs::file_change_events` counts `AgentEvent::FileChange`s, which
//! are emitted only by tools whose *input* names a path. `bash` names nothing,
//! and inside a candidate workspace the count is pinned at zero besides — the
//! candidate's tools reach neither the emitter the pipeline tallies nor the
//! session `FileTouchPort` it reads. Measured on the 2026-08-11 Terminal-Bench
//! panel: of 53 pipeline trials that reached a verdict, the 49 that ran in a
//! candidate all recorded `0` before the verdict, with `mutating_actions`
//! between 6 and 147 beside it.
//!
//! Three predicates conjoined on it anyway, so three decisions branched on an
//! input that could not vary. These tests pin the field out of the decision:
//! git (`diff_available` with `diff_lines`) is the authority on whether the
//! tree changed, and `mutating_actions` on whether anything was asked to.
//!
//! Its own module because the parent is a long way into its ratchet, and
//! because these cases share a premise none of the others do: every assertion
//! here is about a field the ladder is required to *ignore*.

use crate::verify::{LadderDecision, LadderInputs, ladder_decision};
use stella_protocol::FlipOutcome;

/// Every `file_change_events` value a trial in the panel produced, plus the
/// saturating edge. `0` and a small count are the two the traces actually
/// contain; the rest guard against a predicate that reads the field through a
/// threshold rather than a zero test.
const TALLIES: [u32; 6] = [0, 1, 3, 19, 147, u32::MAX];

/// The decision must be a function of the evidence channels alone. Sweeping
/// the tally across a ladder input that reaches each of the five terminal
/// outcomes, the answer may not move.
///
/// The witness for #2873's `evidence_is_blind` and `nothing_was_attempted`
/// arms: before the fix, `nothing_attempted` and `unverifiable` below both
/// changed answer at the first non-zero tally.
#[test]
fn the_decision_never_moves_with_the_tool_tally() {
    let nothing_attempted = LadderInputs {
        flip: FlipOutcome::NotAchieved,
        touched_tests_passed: None,
        mutating_actions: 0,
        diff_lines: 0,
        diff_available: false,
        ..Default::default()
    };
    let unverifiable = LadderInputs {
        // Every observation channel dark, and calls were dispatched — the
        // blind rung, reached only after `nothing_was_attempted` declines.
        mutating_actions: 12,
        diff_available: false,
        ..nothing_attempted
    };
    let escaped = LadderInputs {
        // The probe ran and read an unchanged tree (#1701).
        diff_available: true,
        ..unverifiable
    };
    let revise = LadderInputs {
        touched_tests_passed: Some(false),
        ..escaped
    };
    let submit_fast = LadderInputs {
        flip: FlipOutcome::Achieved,
        touched_tests_passed: Some(true),
        diff_lines: 4,
        diff_budget: 100,
        ..escaped
    };

    for (label, base, expected) in [
        (
            "no mutating call was dispatched",
            &nothing_attempted,
            LadderDecision::NothingAttempted,
        ),
        (
            "every observation channel was dark",
            &unverifiable,
            LadderDecision::Unverifiable,
        ),
        (
            "the probe read an unchanged tree (#1701)",
            &escaped,
            LadderDecision::Unverifiable,
        ),
        ("a touched test is red", &revise, LadderDecision::Revise),
        (
            "a flip with green tests inside budget",
            &submit_fast,
            LadderDecision::SubmitFast,
        ),
    ] {
        for tally in TALLIES {
            let inputs = LadderInputs {
                file_change_events: tally,
                ..*base
            };
            assert_eq!(
                ladder_decision(&inputs),
                expected,
                "{label}: the decision moved when only file_change_events changed (= {tally})"
            );
        }
    }
}

/// The three predicates that used to read it, asserted one by one — so a
/// future edit that re-introduces the conjunct fails here with the predicate
/// named, rather than as a decision that shifted somewhere down the ladder.
#[test]
fn no_predicate_reads_the_tool_tally() {
    let blind = LadderInputs {
        flip: FlipOutcome::NotAchieved,
        touched_tests_passed: None,
        diff_available: false,
        mutating_actions: 12,
        ..Default::default()
    };
    let attempted_nothing = LadderInputs {
        mutating_actions: 0,
        ..blind
    };
    let escaped = LadderInputs {
        diff_available: true,
        diff_lines: 0,
        ..blind
    };

    for tally in TALLIES {
        let with = |base: &LadderInputs| LadderInputs {
            file_change_events: tally,
            ..*base
        };
        assert!(
            with(&blind).evidence_is_blind(),
            "evidence_is_blind read the tally (= {tally}): no channel could look"
        );
        assert!(
            with(&attempted_nothing).nothing_was_attempted(),
            "nothing_was_attempted read the tally (= {tally}): no mutating call was dispatched"
        );
        assert!(
            with(&escaped).effects_escaped_collection(),
            "effects_escaped_collection read the tally (= {tally}): git read the tree and \
             found it unchanged, which a tool tally cannot overrule"
        );
    }
}

/// The verdict a human reads must not present the tool tally as a statement
/// about the tree. Both `UNVERIFIABLE` summaries carried the sentence
/// "file-change events recorded = 0" — one of them hardcoded — and a reader of
/// the panel's `configure-git-webserver` verdict saw it beside 24 mutating
/// calls that had, in fact, changed files.
#[test]
fn the_unverifiable_summary_makes_no_file_change_count_claim() {
    let escaped = LadderInputs {
        mutating_actions: 24,
        diff_available: true,
        diff_lines: 0,
        ..Default::default()
    };
    let blind = LadderInputs {
        diff_available: false,
        ..escaped
    };
    for (label, inputs) in [("escaped", &escaped), ("blind", &blind)] {
        let summary = crate::verify::unverifiable_evidence(inputs).summary;
        assert!(
            !summary.contains("file-change event"),
            "{label}: the summary still reports a tool tally as evidence: {summary}"
        );
        assert!(
            summary.contains("24"),
            "{label}: and the dispatch count that contradicts a clean tree has to stay: {summary}"
        );
    }
}
