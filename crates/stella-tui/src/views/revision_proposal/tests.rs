// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the proposal block says, and in which metal.

use super::*;
use stella_protocol::DivergenceCause;
use stella_protocol::plan_graph::PlanRevision;

fn proposal(issue: Option<&str>) -> RevisionProposal {
    RevisionProposal {
        revision: PlanRevision::new(4).expect("r4"),
        subject: "repair a_short_cycle_is_detected".into(),
        gate: "tests".into(),
        cause: DivergenceCause::new("assertion `left == right` failed").expect("a cause"),
        issue: issue.map(str::to_owned),
    }
}

fn text(rows: &[Line<'static>]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// SPEC 8.1 item 3, written out: the drift glyph, the revision the approval
/// would author, the task in the gate's own words, the cause, and the action
/// row naming all three keys.
#[test]
fn the_block_states_the_revision_the_cause_and_the_three_keys() {
    let rows = text(&proposal_rows(&proposal(Some("#151"))));
    assert_eq!(
        rows,
        vec![
            " │ ⌥ propose r4: add task \"repair a_short_cycle_is_detected\"".to_owned(),
            " │     cause  tests · assertion `left == right` failed".to_owned(),
            " │     issue  #151".to_owned(),
            " │     a approve r4 · e edit · x dismiss".to_owned(),
            " │     merge blocked · unblocks on green".to_owned(),
        ]
    );
}

/// A proposal whose evidence named no issue draws no issue row, rather than a
/// labelled blank one.
#[test]
fn no_linked_issue_draws_no_issue_row() {
    let rows = text(&proposal_rows(&proposal(None)));
    assert!(!rows.iter().any(|row| row.contains("issue")), "{rows:#?}");
    assert_eq!(rows.len(), 4);
}

/// The glyph is the gate-enforced [`glyph::DRIFT`] and not the `⑂` both
/// issues wrote: `stella_tui_theme::glyph::ALL` is checked, so a second drift
/// mark would be a mark no other surface draws.
#[test]
fn the_marker_is_the_theme_drift_glyph() {
    let rows = text(&proposal_rows(&proposal(None)));
    assert!(rows[0].contains(glyph::DRIFT), "{}", rows[0]);
    assert!(!rows[0].contains('⑂'), "{}", rows[0]);
}

/// Drift metal, never the alarm: the proposal answers a red gate row and must
/// not become a second one (SPEC 2's scarcity rule).
#[test]
fn the_proposal_spends_gold_and_never_red() {
    let rows = proposal_rows(&proposal(Some("#151")));
    let colors: Vec<_> = rows
        .iter()
        .flat_map(|row| row.spans.iter())
        .filter_map(|span| span.style.fg)
        .collect();
    assert!(
        colors.contains(&token::GOLD_BRIGHT),
        "the headline takes drift gold: {colors:?}"
    );
    assert!(
        !colors.contains(&token::RED),
        "a proposal is not an alarm: {colors:?}"
    );
    assert!(
        !colors.contains(&token::GREEN),
        "and it settles nothing: {colors:?}"
    );
}
