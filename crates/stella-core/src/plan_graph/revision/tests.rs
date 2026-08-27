// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the revision gate promises, and the two halves of "nothing runs until
//! approval".

use super::*;
use stella_protocol::GateRow;

fn plan() -> PlanGraph {
    PlanGraph::approve(vec![
        TaskNode::new("1", "read the routes"),
        TaskNode::new("2", "persist the digest set"),
        TaskNode::new("3", "verify"),
    ])
    .expect("a plan of three tasks")
}

fn green(name: &str) -> GateRow {
    GateRow {
        name: name.into(),
        state: GateState::Green,
        deterministic: true,
    }
}

fn failed(name: &str, case: &str, log: &str) -> GateRow {
    GateRow {
        name: name.into(),
        state: GateState::Failed {
            case: case.into(),
            log: log.into(),
        },
        deterministic: true,
    }
}

fn board(gates: Vec<GateRow>) -> GateBoard {
    GateBoard {
        patch: Some("patch-7".into()),
        gates,
    }
}

/// One failing gate, one proposal, at the number the reader will be shown.
fn one_failure() -> GateBoard {
    board(vec![
        green("fmt"),
        failed(
            "tests",
            "stella_core::loop_detect::a_short_cycle_is_detected",
            "assertion `left == right` failed\n  left: 3\n right: 2\n",
        ),
    ])
}

// ── the acceptance criterion: nothing runs until approval ──────────────────

/// SPEC 8.1 item 3's last sentence, as one question with one answer. A fresh
/// gate admits; a gate with a proposal standing does not; approving releases
/// it, and so does dismissing it.
#[test]
fn nothing_runs_while_a_proposal_stands() {
    let mut graph = plan();
    let mut gate = RevisionGate::default();
    assert!(gate.admits(), "a gate with nothing pending must admit");

    gate.observe(
        graph.revision().next(),
        &graph.planned(graph.revision()),
        &one_failure(),
    )
    .expect("a failing board puts a revision up");
    assert!(
        !gate.admits(),
        "a standing proposal withholds — this is the whole of \"nothing runs until approval\""
    );

    gate.approve(&mut graph)
        .expect("approve the standing proposal");
    assert!(gate.admits(), "approval releases the withholding");
}

/// The other half, and the one a caller cannot route around: the proposed task
/// is not in the plan until the revision is written, and [`PlanGraph::ran`]
/// refuses a task the current revision does not contain. So a call site that
/// never consulted [`RevisionGate::admits`] still cannot record the inserted
/// task as having run.
#[test]
fn the_proposed_task_cannot_run_until_the_revision_is_approved() {
    let mut graph = plan();
    let mut gate = RevisionGate::default();
    gate.observe(
        graph.revision().next(),
        &graph.planned(graph.revision()),
        &one_failure(),
    )
    .expect("a failing board puts a revision up");

    // The id the approval will assign, derived the way the board would.
    let proposed_id = next_task_id(&graph.planned(graph.revision()));
    assert_eq!(proposed_id, "4");
    assert_eq!(
        graph.ran(&proposed_id),
        Err(PlanGraphError::UnplannedTask {
            id: proposed_id.clone(),
            revision: 1,
        }),
        "the proposed task must not be runnable before the revision exists"
    );

    gate.approve(&mut graph).expect("approve");
    graph
        .ran(&proposed_id)
        .expect("the approved task runs under the revision that inserted it");
}

/// Approval goes through [`PlanGraph::revise`], so the `[:NEXT]` edge, the
/// retained predecessor and the divergence carrying the gate's cause are the
/// existing machinery's rather than a second code path's.
#[test]
fn approval_writes_the_next_edge_and_records_the_gates_cause_as_the_drift() {
    let mut graph = plan();
    let mut gate = RevisionGate::default();
    gate.observe(
        graph.revision().next(),
        &graph.planned(graph.revision()),
        &one_failure(),
    )
    .expect("proposal");

    let revision = gate.approve(&mut graph).expect("approve");
    assert_eq!(revision.to_string(), "r2");

    let planned = graph.planned(revision);
    assert_eq!(
        planned.last().map(|task| task.subject.as_str()),
        Some("repair stella_core::loop_detect::a_short_cycle_is_detected"),
        "the insertion lands at the end of the plan: {planned:?}"
    );

    // `r1` is still readable, which is what keeps SPEC 7.3's `planned 3`
    // computable after the plan grew.
    assert_eq!(graph.planned_count(), 3);

    let drift = graph.divergences();
    assert_eq!(drift.len(), 1, "{drift:?}");
    assert_eq!(drift[0].revision, revision);
    assert_eq!(
        drift[0].cause.as_str(),
        "stella_core::loop_detect::a_short_cycle_is_detected",
        "the drift carries the gate's own words, not the gate's name"
    );
}

// ── what a board is allowed to provoke ─────────────────────────────────────

/// A green board asks nothing. Neither does an undecided one: an abstention
/// blames the instrument rather than the worker, and proposing a repair for a
/// gate nobody could decide would put the reader's name on a guess.
#[test]
fn a_board_with_no_determinate_failure_proposes_nothing() {
    let graph = plan();
    for gates in [
        vec![green("fmt"), green("tests")],
        vec![
            green("fmt"),
            GateRow {
                name: "tests".into(),
                state: GateState::Undecided {
                    reason: "the plugin reported no evidence for this gate".into(),
                },
                deterministic: true,
            },
        ],
        vec![],
    ] {
        let mut gate = RevisionGate::default();
        assert!(
            gate.observe(
                graph.revision().next(),
                &graph.planned(graph.revision()),
                &board(gates.clone())
            )
            .is_none(),
            "{gates:?} must not put a revision up"
        );
        assert!(gate.admits(), "and must not withhold anything");
    }
}

/// A second board must not retitle the thing the reader is deciding about.
#[test]
fn a_second_board_does_not_replace_a_standing_proposal() {
    let graph = plan();
    let mut gate = RevisionGate::default();
    gate.observe(
        graph.revision().next(),
        &graph.planned(graph.revision()),
        &one_failure(),
    )
    .expect("proposal");
    let standing = gate.pending().cloned().expect("standing");

    assert!(
        gate.observe(
            graph.revision().next(),
            &graph.planned(graph.revision()),
            &board(vec![failed(
                "clippy",
                "unused import",
                "warning: unused import"
            )]),
        )
        .is_none(),
        "a later board must not replace a standing proposal"
    );
    assert_eq!(gate.pending(), Some(&standing));
}

/// The board asking for a repair somebody already planned is not a change of
/// plan, so nothing is put up and nothing is withheld.
#[test]
fn a_repair_the_plan_already_contains_is_not_proposed_again() {
    let mut graph = plan();
    let mut gate = RevisionGate::default();
    gate.observe(
        graph.revision().next(),
        &graph.planned(graph.revision()),
        &one_failure(),
    )
    .expect("proposal");
    gate.approve(&mut graph).expect("approve");

    let mut second = RevisionGate::default();
    assert!(
        second
            .observe(
                graph.revision().next(),
                &graph.planned(graph.revision()),
                &one_failure()
            )
            .is_none(),
        "the same failure against a plan that now answers it proposes nothing"
    );
}

/// The proposal names what the reader can check against the board above it:
/// the failing case where the gate named one, the gate itself where it did not.
#[test]
fn the_subject_is_the_gates_own_words() {
    let graph = plan();
    let mut named = RevisionGate::default();
    assert_eq!(
        named
            .observe(
                graph.revision().next(),
                &graph.planned(graph.revision()),
                &one_failure()
            )
            .map(|p| p.subject.clone()),
        Some("repair stella_core::loop_detect::a_short_cycle_is_detected".to_owned())
    );

    let mut unnamed = RevisionGate::default();
    assert_eq!(
        unnamed
            .observe(
                graph.revision().next(),
                &graph.planned(graph.revision()),
                &board(vec![failed("tests", "  ", "thread 'main' panicked")]),
            )
            .map(|p| p.subject.clone()),
        Some("repair the failing tests gate".to_owned())
    );
}

/// SPEC 8.1's "any linked issue": read out of the gate's own evidence, absent
/// where the evidence named none.
#[test]
fn the_linked_issue_comes_from_the_evidence_or_not_at_all() {
    let graph = plan();
    let mut gate = RevisionGate::default();
    assert_eq!(
        gate.observe(
            graph.revision().next(),
            &graph.planned(graph.revision()),
            &board(vec![failed(
                "tests",
                "dedup digest is not stable",
                "see #151 — the seen-set is rebuilt per run",
            )]),
        )
        .and_then(|p| p.issue.clone()),
        Some("#151".to_owned())
    );

    let mut none = RevisionGate::default();
    assert_eq!(
        none.observe(
            graph.revision().next(),
            &graph.planned(graph.revision()),
            &one_failure()
        )
        .and_then(|p| p.issue.clone()),
        None,
        "no issue in the evidence renders as no cell, never as a guess"
    );

    // Rust evidence is full of hashes that are not issue numbers, and the
    // link must survive one standing in front of it.
    let mut attributes = RevisionGate::default();
    assert_eq!(
        attributes
            .observe(
                graph.revision().next(),
                &graph.planned(graph.revision()),
                &board(vec![failed(
                    "tests",
                    "#[test] a_short_cycle_is_detected",
                    "the seen-set is rebuilt per run — see #151",
                )]),
            )
            .and_then(|p| p.issue.clone()),
        Some("#151".to_owned())
    );
}

/// A failure whose evidence says nothing cannot carry a cause, and this module
/// would rather ask nothing than put up a proposal with a blank reason on it.
#[test]
fn a_failure_with_no_evidence_at_all_proposes_nothing() {
    let graph = plan();
    let mut gate = RevisionGate::default();
    assert!(
        gate.observe(
            graph.revision().next(),
            &graph.planned(graph.revision()),
            &board(vec![failed("tests", "   ", "  \n\t\n")]),
        )
        .is_none()
    );
    assert!(gate.admits());
}

// ── the three verbs ────────────────────────────────────────────────────────

/// `e edit` changes what would be inserted and nothing else: the gate, the
/// cause and the number are what the evidence said.
#[test]
fn edit_changes_the_subject_and_leaves_the_evidence_alone() {
    let graph = plan();
    let mut gate = RevisionGate::default();
    gate.observe(
        graph.revision().next(),
        &graph.planned(graph.revision()),
        &one_failure(),
    )
    .expect("proposal");
    let before = gate.pending().cloned().expect("standing");

    gate.edit("rebuild the seen-set on every run")
        .expect("edit the subject");
    let after = gate.pending().expect("still standing");
    assert_eq!(after.subject, "rebuild the seen-set on every run");
    assert_eq!(after.cause, before.cause);
    assert_eq!(after.gate, before.gate);
    assert_eq!(after.revision, before.revision);
    assert!(!gate.admits(), "editing is not approving");

    assert_eq!(gate.edit("   "), Err(RevisionError::BlankSubject));
}

/// `x dismiss` declines the change without reverting anything: no revision was
/// authored, so there is nothing in the plan to take back out.
#[test]
fn dismiss_releases_the_withholding_and_writes_no_revision() {
    let graph = plan();
    let mut gate = RevisionGate::default();
    gate.observe(
        graph.revision().next(),
        &graph.planned(graph.revision()),
        &one_failure(),
    )
    .expect("proposal");

    let dropped = gate.dismiss().expect("what was standing");
    assert_eq!(dropped.gate, "tests");
    assert!(gate.admits());
    assert_eq!(graph.revision(), PlanRevision::FIRST);
    assert!(graph.divergences().is_empty(), "a dismissal is not drift");
    assert_eq!(gate.dismiss(), None);
}

/// The three verbs refuse when nobody proposed anything, rather than inventing
/// something to act on.
#[test]
fn the_verbs_refuse_when_nothing_is_pending() {
    let mut graph = plan();
    let mut gate = RevisionGate::default();
    assert_eq!(gate.edit("anything"), Err(RevisionError::NothingPending));
    assert_eq!(gate.approve(&mut graph), Err(RevisionError::NothingPending));
    assert_eq!(gate.dismiss(), None);
}

/// A proposal the reader was shown as `r2` must not silently author `r3`
/// because the plan moved underneath it.
#[test]
fn approving_a_stale_proposal_is_refused_rather_than_renumbered() {
    let mut graph = plan();
    let mut gate = RevisionGate::default();
    gate.observe(
        graph.revision().next(),
        &graph.planned(graph.revision()),
        &one_failure(),
    )
    .expect("proposal");

    graph
        .revise(
            graph.planned(graph.revision()),
            DivergenceCause::new("the plan was put to the person driving again").expect("a cause"),
        )
        .expect("an unrelated revision lands first");

    assert_eq!(
        gate.approve(&mut graph),
        Err(RevisionError::PlanMoved {
            proposed: 2,
            current: 3,
        })
    );
    assert!(
        !gate.admits(),
        "a stale proposal still withholds — it goes back in front of the driver"
    );
}

// ── the helpers ────────────────────────────────────────────────────────────

/// The insertion takes the id the board would give it, so an approved revision
/// and the task board never open two id spaces over one plan.
#[test]
fn the_next_id_counts_up_from_the_highest_the_plan_holds() {
    assert_eq!(next_task_id(&[]), "1");
    assert_eq!(
        next_task_id(&[TaskNode::new("1", "a"), TaskNode::new("7", "b")]),
        "8",
        "ids are never reused, so the next one is past the highest and not past the count"
    );
}

/// A cut cause is cut by characters, so it never lands inside one.
#[test]
fn a_cause_is_cut_by_characters_and_takes_the_first_line_that_says_anything() {
    assert_eq!(cause_line("\n\n  first line  \nsecond"), "first line");
    let wide = "é".repeat(200);
    assert_eq!(cause_line(&wide).chars().count(), CAUSE_CHARS);
}
