// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for the plan graph's four claims: approval writes the planned path,
//! execution writes the actual one, a revision retains the plan it superseded,
//! and every difference between the two lanes is a divergence carrying a cause.

use proptest::prelude::*;

use super::*;

fn tasks(ids: &[&str]) -> Vec<TaskNode> {
    ids.iter()
        .map(|id| TaskNode::new(*id, format!("step {id}")))
        .collect()
}

fn cause(text: &str) -> DivergenceCause {
    DivergenceCause::new(text).expect("a non-empty cause")
}

fn approved(ids: &[&str]) -> PlanGraph {
    PlanGraph::approve(tasks(ids)).expect("a plan of distinct tasks")
}

fn ids(nodes: &[TaskNode]) -> Vec<&str> {
    nodes.iter().map(|task| task.id.as_str()).collect()
}

// ── SPEC 7.4: approval writes `[:NEXT]`, execution writes `[:THEN]` ─────────

/// A plan approval is a `[:NEXT]` chain: the plan node, then each task after
/// the one before it, at r1.
#[test]
fn approval_writes_the_planned_path_as_a_next_chain() {
    let graph = approved(&["1", "2", "3"]);

    assert_eq!(graph.revision(), PlanRevision::FIRST);
    assert_eq!(ids(&graph.planned(PlanRevision::FIRST)), ["1", "2", "3"]);
    assert_eq!(graph.planned_count(), 3);
    assert_eq!(graph.actual_count(), 0, "approving runs nothing");

    let edges = graph.edges();
    assert_eq!(edges.len(), 3);
    assert!(
        edges
            .iter()
            .all(|e| e.kind == PlanEdgeKind::Next && e.revision == PlanRevision::FIRST),
        "{edges:?}"
    );
    assert_eq!(
        edges[0].from,
        PlanEdgeSource::Plan,
        "the first task follows the plan itself"
    );
    assert_eq!(edges[1].from, PlanEdgeSource::Task("1".into()));
    assert_eq!(edges[2].from, PlanEdgeSource::Task("2".into()));
    assert_eq!(
        edges.iter().map(|e| e.position).collect::<Vec<_>>(),
        [0, 1, 2]
    );
}

/// Running tasks writes the `[:THEN]` lane in the order they actually ran,
/// which is not obliged to be the order the plan put them in.
#[test]
fn execution_writes_the_actual_path_in_the_order_it_happened() {
    let mut graph = approved(&["1", "2", "3"]);
    for id in ["2", "1", "3"] {
        graph.ran(id).expect("every id is in the plan");
    }

    assert_eq!(ids(&graph.actual()), ["2", "1", "3"]);
    assert_eq!(graph.actual_count(), 3);
    let then: Vec<&PlanEdge> = graph
        .edges()
        .iter()
        .filter(|e| e.kind == PlanEdgeKind::Then)
        .collect();
    assert_eq!(then[0].from, PlanEdgeSource::Plan);
    assert_eq!(then[1].from, PlanEdgeSource::Task("2".into()));
    assert_eq!(then[2].from, PlanEdgeSource::Task("1".into()));
}

/// A board can report the same task starting twice — a resumed turn re-reads
/// its own snapshot — and an actual lane that grew a row each time would
/// report an ordering that never happened.
#[test]
fn running_the_same_task_twice_records_one_edge() {
    let mut graph = approved(&["1", "2"]);
    graph.ran("1").expect("planned");
    graph.ran("1").expect("planned");
    assert_eq!(ids(&graph.actual()), ["1"]);
}

// ── the DoD: divergence against the plan as approved ────────────────────────

/// **planned == actual ⇒ zero divergences.** The plan ran exactly as agreed,
/// so there is nothing to record and nothing to explain.
#[test]
fn a_plan_that_ran_as_approved_records_no_divergence() {
    let mut graph = approved(&["1", "2", "3"]);
    for id in ["1", "2", "3"] {
        graph.ran(id).expect("planned");
    }
    assert_eq!(graph.planned(PlanRevision::FIRST), graph.actual());
    assert!(graph.divergences().is_empty(), "{:?}", graph.divergences());
}

/// **An inserted task ⇒ exactly one divergence, carrying its cause.** SPEC
/// 8.1's gate-failure scene: a check fails, the plan gains a repair step, and
/// the revision that added it says which compiler error demanded it.
#[test]
fn an_inserted_task_is_the_one_divergence_and_it_carries_its_cause() {
    let mut graph = approved(&["1", "2", "3"]);
    let revision = graph
        .revise(tasks(&["1", "2", "3b", "3"]), cause("E0432"))
        .expect("a plan may gain a step");
    assert_eq!(revision, PlanRevision::FIRST.next());

    let divergences = graph.divergences();
    assert_eq!(divergences.len(), 1, "{divergences:?}");
    let drift = &divergences[0];
    assert_eq!(drift.kind, DivergenceKind::Inserted);
    assert_eq!(drift.task.id, "3b");
    assert_eq!(drift.revision, revision);
    assert_eq!(drift.cause.as_str(), "E0432");
}

/// The other direction: a revision that takes a task out is drift too, and it
/// carries the same revision's cause.
#[test]
fn a_dropped_task_is_a_divergence_carrying_the_revision_that_dropped_it() {
    let mut graph = approved(&["1", "2", "3"]);
    graph
        .revise(tasks(&["1", "3"]), cause("the migration is not needed"))
        .expect("a plan may lose a step");

    let divergences = graph.divergences();
    assert_eq!(divergences.len(), 1, "{divergences:?}");
    assert_eq!(divergences[0].kind, DivergenceKind::Dropped);
    assert_eq!(divergences[0].task.id, "2");
    assert_eq!(divergences[0].cause.as_str(), "the migration is not needed");
}

/// SPEC 7.3's footer — `planned 6 · actual 7 · ⌥ 1 drift` — is three numbers
/// off one graph, and the `6` is only recoverable because r1 survived r2.
#[test]
fn the_plan_footer_counts_come_off_one_graph() {
    let mut graph = approved(&["1", "2", "3", "4", "5", "6"]);
    graph
        .revise(
            tasks(&["1", "2", "3", "3b", "4", "5", "6"]),
            cause("gate `typed-errors` failed"),
        )
        .expect("a plan may gain a step");
    for id in ["1", "2", "3", "3b", "4", "5", "6"] {
        graph.ran(id).expect("planned");
    }

    assert_eq!(graph.planned_count(), 6);
    assert_eq!(graph.actual_count(), 7);
    assert_eq!(graph.divergences().len(), 1);
}

/// A re-proposal after a refusal is a revision whose plan did not move. It
/// gets its number — the breadcrumb has to say which draft is on screen —
/// and it contributes no drift, because the drift count counts changes to the
/// plan rather than conversations about it.
#[test]
fn re_proposing_an_unchanged_plan_numbers_it_without_inventing_drift() {
    let mut graph = approved(&["1", "2", "3"]);
    let revision = graph
        .revise(
            tasks(&["1", "2", "3"]),
            cause("the driver asked for smaller"),
        )
        .expect("a plan may be put up again");
    assert_eq!(revision.to_string(), "r2");
    assert!(graph.divergences().is_empty(), "{:?}", graph.divergences());
}

// ── revision authoring retains the prior plan ──────────────────────────────

/// The DoD bullet, stated directly: `r{n+1}` exists **beside** its
/// predecessor, not over it.
#[test]
fn a_revision_retains_the_plan_it_superseded() {
    let mut graph = approved(&["1", "2", "3"]);
    graph
        .revise(tasks(&["1", "2", "3b", "3"]), cause("E0432"))
        .expect("revise");
    graph
        .revise(
            tasks(&["1", "2", "3b", "3", "4"]),
            cause("the gate needs a verify step"),
        )
        .expect("revise again");

    assert_eq!(graph.revision().to_string(), "r3");
    assert_eq!(
        ids(&graph.planned(PlanRevision::FIRST)),
        ["1", "2", "3"],
        "the approved plan is still readable at r3"
    );
    assert_eq!(
        graph.nodes().len(),
        3,
        "one plan node per revision: {:?}",
        graph.nodes()
    );
    assert_eq!(
        graph.nodes()[0].cause,
        None,
        "the approval explains nothing"
    );
    assert!(
        graph.nodes()[1..].iter().all(|n| n.cause.is_some()),
        "every revision after the first says why"
    );
}

// ── nothing runs until the plan says so ────────────────────────────────────

/// **The refusal that is the mechanism.** To run a task the plan does not
/// have, a caller must revise the plan first — and revising takes a cause it
/// cannot fabricate. This is SPEC 8.1's "a proposed plan revision, never a
/// silent fix" as a type error.
#[test]
fn the_actual_lane_never_departs_from_the_plan_without_a_revision() {
    let mut graph = approved(&["1", "2"]);
    assert_eq!(
        graph.ran("3b"),
        Err(PlanGraphError::UnplannedTask {
            id: "3b".into(),
            revision: 1
        })
    );
    assert_eq!(graph.actual_count(), 0, "a refused run records nothing");

    graph
        .revise(tasks(&["1", "2", "3b"]), cause("E0432"))
        .expect("revise");
    graph.ran("3b").expect("now it is in the plan");
    assert_eq!(ids(&graph.actual()), ["3b"]);
}

/// The consequence of the refusal above, asserted as the property it is:
/// whatever ran and was not in the approved plan is exactly what the recorded
/// divergences name.
#[test]
fn everything_that_ran_unplanned_is_a_recorded_divergence() {
    let mut graph = approved(&["1", "2", "3"]);
    graph
        .revise(tasks(&["1", "2", "3b", "3"]), cause("E0432"))
        .expect("revise");
    graph
        .revise(
            tasks(&["1", "2", "3b", "3", "4b"]),
            cause("clippy::needless_borrow"),
        )
        .expect("revise");
    for id in ["1", "2", "3b", "3", "4b"] {
        graph.ran(id).expect("planned");
    }

    let plan = graph.planned(PlanRevision::FIRST);
    let approved_ids: Vec<&str> = ids(&plan);
    let unplanned: Vec<String> = graph
        .actual()
        .iter()
        .filter(|task| !approved_ids.contains(&task.id.as_str()))
        .map(|task| task.id.clone())
        .collect();
    let recorded: Vec<String> = graph
        .divergences()
        .iter()
        .filter(|d| d.kind == DivergenceKind::Inserted)
        .map(|d| d.task.id.clone())
        .collect();
    assert_eq!(unplanned, recorded);
    assert!(
        graph
            .divergences()
            .iter()
            .all(|d| !d.cause.as_str().trim().is_empty()),
        "a divergence with a blank reason is drift nobody recorded"
    );
}

// ── a lane is an order over distinct tasks ─────────────────────────────────

#[test]
fn a_plan_of_nothing_is_refused() {
    assert_eq!(
        PlanGraph::approve(Vec::new()),
        Err(PlanGraphError::EmptyPlan)
    );
    let mut graph = approved(&["1"]);
    assert_eq!(
        graph.revise(Vec::new(), cause("everything is off")),
        Err(PlanGraphError::EmptyPlan)
    );
}

#[test]
fn a_repeated_task_id_is_refused() {
    assert_eq!(
        PlanGraph::approve(tasks(&["1", "2", "1"])),
        Err(PlanGraphError::DuplicateTask { id: "1".into() })
    );
}

// ── replay ─────────────────────────────────────────────────────────────────

/// **The replay claim.** A graph written to a store and read back is the graph
/// that was written — through `serde_json`, byte-for-byte, as AGENTS.md #4
/// requires of everything crossing a boundary.
#[test]
fn a_graph_survives_the_round_trip_a_store_puts_it_through() {
    let mut graph = approved(&["1", "2", "3"]);
    graph.ran("1").expect("planned");
    graph
        .revise(tasks(&["1", "2", "3b", "3"]), cause("E0432"))
        .expect("revise");
    graph.ran("3b").expect("planned");
    graph.ran("2").expect("planned");

    let nodes = serde_json::to_string(graph.nodes()).expect("serialize nodes");
    let edges = serde_json::to_string(graph.edges()).expect("serialize edges");
    let restored = PlanGraph::restore(
        serde_json::from_str(&nodes).expect("deserialize nodes"),
        serde_json::from_str(&edges).expect("deserialize edges"),
    )
    .expect("a graph this module wrote is a graph it accepts");

    assert_eq!(restored, graph);
    assert_eq!(restored.divergences(), graph.divergences());
    assert_eq!(
        serde_json::to_string(restored.edges()).expect("re-serialize"),
        edges
    );
}

/// Read back out of order — which is what an unordered `SELECT` hands you —
/// the graph still comes back in one canonical order, so replay does not
/// depend on how the rows happened to be stored.
#[test]
fn a_graph_restored_from_shuffled_rows_comes_back_in_order() {
    let mut graph = approved(&["1", "2", "3"]);
    graph.ran("1").expect("planned");
    let mut shuffled = graph.edges().to_vec();
    shuffled.reverse();

    let restored =
        PlanGraph::restore(graph.nodes().to_vec(), shuffled).expect("order is not the record");
    assert_eq!(restored, graph);
}

/// Strict on the way in, because a plan graph read back *slightly* wrong is a
/// replay that quietly disagrees with the run it claims to reproduce.
#[test]
fn a_graph_that_is_not_one_is_refused_rather_than_repaired() {
    let graph = approved(&["1", "2"]);

    // A revision with no plan node above it.
    let mut orphaned = graph.edges().to_vec();
    orphaned.push(PlanEdge {
        kind: PlanEdgeKind::Next,
        revision: PlanRevision::FIRST.next(),
        from: PlanEdgeSource::Plan,
        to: TaskNode::new("9", "step 9"),
        position: 0,
    });
    assert!(PlanGraph::restore(graph.nodes().to_vec(), orphaned).is_err());

    // A hole in the revision ladder.
    assert_eq!(
        PlanGraph::restore(
            vec![
                PlanNode {
                    revision: PlanRevision::FIRST,
                    cause: None
                },
                PlanNode {
                    revision: PlanRevision::FIRST.next().next(),
                    cause: Some(cause("E0432"))
                },
            ],
            graph.edges().to_vec(),
        ),
        Err(PlanGraphError::MissingRevision { revision: 2 })
    );

    // A revision that does not say why.
    assert_eq!(
        PlanGraph::restore(
            vec![
                PlanNode {
                    revision: PlanRevision::FIRST,
                    cause: None
                },
                PlanNode {
                    revision: PlanRevision::FIRST.next(),
                    cause: None
                },
            ],
            graph.edges().to_vec(),
        ),
        Err(PlanGraphError::CauselessRevision { revision: 2 })
    );

    // An approval that claims to explain something.
    assert_eq!(
        PlanGraph::restore(
            vec![PlanNode {
                revision: PlanRevision::FIRST,
                cause: Some(cause("E0432"))
            }],
            graph.edges().to_vec(),
        ),
        Err(PlanGraphError::CausedApproval)
    );

    // A lane with a hole in it.
    let mut gapped = graph.edges().to_vec();
    gapped[1].position = 7;
    assert!(PlanGraph::restore(graph.nodes().to_vec(), gapped).is_err());

    // No plan at all.
    assert!(PlanGraph::restore(Vec::new(), Vec::new()).is_err());
}

// ── properties ─────────────────────────────────────────────────────────────

fn plan_ids() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(1_u32..12, 1..8).prop_map(|xs| {
        let mut seen = Vec::new();
        for x in xs {
            let id = x.to_string();
            if !seen.contains(&id) {
                seen.push(id);
            }
        }
        seen
    })
}

proptest! {
    /// The DoD's first property over arbitrary plans: run the approved plan,
    /// in any order, and nothing is recorded as drift. Order is not
    /// divergence — SPEC 7.4 asks the two lanes to be comparable, and a plan
    /// whose steps interleaved is still the plan that was agreed to.
    #[test]
    fn running_only_the_approved_plan_never_records_drift(
        plan in plan_ids(),
        rotation in 0_usize..8,
    ) {
        let nodes: Vec<TaskNode> = plan
            .iter()
            .map(|id| TaskNode::new(id.clone(), format!("step {id}")))
            .collect();
        let mut graph = PlanGraph::approve(nodes).expect("distinct, non-empty");
        let start = rotation % plan.len();
        for i in 0..plan.len() {
            graph.ran(&plan[(start + i) % plan.len()]).expect("planned");
        }
        prop_assert_eq!(graph.actual_count(), plan.len());
        prop_assert!(graph.divergences().is_empty());
    }

    /// The DoD's second property: however many tasks a revision inserts, that
    /// is how many divergences it records, and every one of them carries the
    /// revision's cause. The count is never guessed and never rounded.
    #[test]
    fn a_revision_records_one_divergence_per_task_it_moves(
        plan in plan_ids(),
        extra in prop::collection::vec(20_u32..30, 0..4),
    ) {
        let nodes: Vec<TaskNode> = plan
            .iter()
            .map(|id| TaskNode::new(id.clone(), format!("step {id}")))
            .collect();
        let mut graph = PlanGraph::approve(nodes.clone()).expect("distinct, non-empty");

        let mut inserted: Vec<String> = Vec::new();
        let mut revised = nodes;
        for id in extra {
            let id = id.to_string();
            if inserted.contains(&id) {
                continue;
            }
            revised.push(TaskNode::new(id.clone(), format!("step {id}")));
            inserted.push(id);
        }
        graph
            .revise(revised, cause("E0432"))
            .expect("a plan may gain steps");

        let drift = graph.divergences();
        prop_assert_eq!(drift.len(), inserted.len());
        for divergence in &drift {
            prop_assert_eq!(divergence.kind, DivergenceKind::Inserted);
            prop_assert_eq!(divergence.cause.as_str(), "E0432");
            prop_assert!(inserted.contains(&divergence.task.id));
        }
    }
}
