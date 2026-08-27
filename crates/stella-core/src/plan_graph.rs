// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plan graph's decision logic — who may write a `[:NEXT]` or `[:THEN]`
//! edge, when a revision is authored, and what counts as drift
//! (`design/tui-v2/SPEC.md` §7.4).
//!
//! Pure, owned data, no I/O (AGENTS.md rule #2), which is what makes the
//! claims below testable without a store, a runtime or a terminal. The types
//! live in [`stella_protocol::plan_graph`]; this is the only place that
//! decides how they fit together.
//!
//! # Approval writes the planned path
//!
//! [`PlanGraph::approve`] is the only constructor. A plan graph therefore
//! cannot exist before somebody approved a plan, and `r1`'s `[:NEXT]` chain is
//! that approval written down. Every later revision is authored *beside* it —
//! [`PlanGraph::revise`] appends a plan node and a whole new chain and touches
//! nothing that was already there, so `r1` is still readable after `r4` exists.
//! That retention is what makes SPEC 7.3's footer (`planned 6 · actual 7 ·
//! ⌥ 1 drift`) computable at all: the `6` is a fact about the approved plan,
//! and a revision that overwrote its predecessor would destroy it.
//!
//! # Nothing runs until the plan says so
//!
//! [`PlanGraph::ran`] refuses a task the current revision does not contain
//! ([`PlanGraphError::UnplannedTask`]). That refusal is the mechanism behind
//! SPEC 8.1's "a **proposed plan revision**, never a silent fix": to run
//! something the plan did not have, a caller must first revise the plan, and
//! [`PlanGraph::revise`] takes a [`DivergenceCause`] it cannot fabricate.
//!
//! That has a consequence, and it is the whole argument: **the actual path is always a subsequence of the current plan, so
//! every difference between what ran and what was approved is a revision
//! somebody recorded a reason for.** There is no path through this API that
//! produces unexplained drift. `an_inserted_task_is_the_one_divergence_and_it_carries_its_cause`
//! and `the_actual_lane_never_departs_from_the_plan_without_a_revision` are
//! the two halves of that claim.
//!
//! # Divergence is derived
//!
//! [`PlanGraph::divergences`] recomputes its answer from the revisions every
//! time it is asked, and there is no field anywhere that stores one — the same
//! discipline [`stella_protocol::TaskContract::closure`] applies to done-ness,
//! and for the same reason: a stored verdict is a verdict its producer can
//! set, and the producer with the strongest incentive to say "no drift here"
//! is the one that drifted.
//!
//! # Replay
//!
//! [`PlanGraph::restore`] takes the nodes and edges back from a store and
//! rebuilds the graph, refusing anything that is not a graph this module could
//! have produced. It is strict because a plan graph read back *slightly*
//! wrong is a replay that quietly disagrees with the run it claims to
//! reproduce, and a loud refusal is the only version of that failure anybody
//! ever notices.

use std::collections::HashSet;

pub mod revision;
pub use revision::{RevisionError, RevisionGate};

use stella_protocol::plan_graph::{
    Divergence, DivergenceCause, DivergenceKind, PlanEdge, PlanEdgeKind, PlanEdgeSource, PlanNode,
    PlanRevision, TaskNode,
};

/// Why a plan-graph write was refused. Named errors, never a bare string
/// (AGENTS.md #5) — a caller has to tell "you have not revised the plan yet"
/// from "that store row is corrupt", and those are different repairs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanGraphError {
    #[error(
        "a plan needs at least one task; a plan of nothing is not a plan, and a revision that \
         empties one is an abandonment rather than a change of course"
    )]
    EmptyPlan,
    #[error(
        "task id {id} appears twice in the same plan — a lane is an order over distinct tasks, \
         and a repeated id makes `[:NEXT]` ambiguous about which one comes after it"
    )]
    DuplicateTask { id: String },
    #[error(
        "task {id} is not in r{revision} — revise the plan first, with the cause, so the \
         insertion is recorded before it runs rather than discovered afterwards"
    )]
    UnplannedTask { id: String, revision: u32 },
    #[error(
        "the stored plan graph skips r{revision}; revisions are contiguous from r1 because each \
         one supersedes exactly its predecessor"
    )]
    MissingRevision { revision: u32 },
    #[error(
        "r{revision} records no cause; every revision after the first says why the plan left the \
         path it was on"
    )]
    CauselessRevision { revision: u32 },
    #[error(
        "r1 records a cause; the plan as approved diverged from nothing, so there is nothing for \
         it to explain"
    )]
    CausedApproval,
    #[error(
        "the {kind} lane of r{revision} is broken at position {position}: a lane is a chain from \
         the plan node through each task in order, and this edge does not follow the one before it"
    )]
    BrokenLane {
        kind: &'static str,
        revision: u32,
        position: u32,
    },
}

/// A plan and what became of it: one plan node per revision, the `[:NEXT]`
/// chain each revision authored, and the `[:THEN]` chain of what actually ran.
///
/// Insertion-ordered and owned throughout, like [`crate::TaskBoard`]: all
/// mutation goes through the methods below so the rules in the module docs
/// hold by construction rather than by review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanGraph {
    /// One per revision, `r1` first. Never replaced — see the module docs.
    nodes: Vec<PlanNode>,
    /// Every edge in the graph, both lanes.
    edges: Vec<PlanEdge>,
}

impl PlanGraph {
    /// The plan as approved: `r1`, and the `[:NEXT]` chain over `tasks`.
    ///
    /// The only constructor, so a graph cannot describe a plan nobody agreed
    /// to.
    pub fn approve(tasks: Vec<TaskNode>) -> Result<Self, PlanGraphError> {
        check_lane(&tasks)?;
        let mut graph = Self {
            nodes: vec![PlanNode {
                revision: PlanRevision::FIRST,
                cause: None,
            }],
            edges: Vec::new(),
        };
        graph.write_chain(PlanEdgeKind::Next, PlanRevision::FIRST, &tasks);
        Ok(graph)
    }

    /// Rebuild a graph from what a store handed back, refusing anything this
    /// module could not have produced.
    ///
    /// The check is a *reconstruction*, not a field-by-field audit: the lanes
    /// are read out of `edges`, the edges are written again from those lanes,
    /// and the two are compared. One comparison therefore covers position
    /// contiguity, the chain's linkage, the lane head, and edge ordering at
    /// once, and it cannot fall behind the writer the way a hand-written list
    /// of rules would.
    pub fn restore(nodes: Vec<PlanNode>, edges: Vec<PlanEdge>) -> Result<Self, PlanGraphError> {
        let mut candidate = Self { nodes, edges };
        candidate.check_revisions()?;
        candidate.edges.sort_by_key(canonical_order);
        let mut rebuilt = Self {
            nodes: candidate.nodes.clone(),
            edges: Vec::new(),
        };
        for node in &candidate.nodes {
            let lane = candidate.read_chain(PlanEdgeKind::Next, Some(node.revision))?;
            check_lane(&lane)?;
            rebuilt.write_chain(PlanEdgeKind::Next, node.revision, &lane);
        }
        rebuilt.edges.extend(candidate.read_then_edges()?);
        rebuilt.edges.sort_by_key(canonical_order);
        if rebuilt.edges != candidate.edges {
            return Err(PlanGraphError::BrokenLane {
                kind: PlanEdgeKind::Next.as_wire_str(),
                revision: candidate.revision().get(),
                position: 0,
            });
        }
        Ok(rebuilt)
    }

    /// Author `r{n+1}`: the plan is being put up again, and this is why.
    ///
    /// The prior revision is retained in full — that is the point of the
    /// method. Returns the revision it authored, which is what a breadcrumb
    /// renders as `r{n+1}` (#4333).
    ///
    /// A revision whose task list is unchanged is legitimate and is not
    /// refused: it is what a re-proposal after a refusal looks like, where the
    /// plan is the same and the reason for re-asking is the driver's own. It
    /// contributes no [`Divergence`], because nothing about the plan moved,
    /// which keeps SPEC 7.3's drift count a count of changes rather than of
    /// conversations.
    pub fn revise(
        &mut self,
        tasks: Vec<TaskNode>,
        cause: DivergenceCause,
    ) -> Result<PlanRevision, PlanGraphError> {
        check_lane(&tasks)?;
        let revision = self.revision().next();
        self.nodes.push(PlanNode {
            revision,
            cause: Some(cause),
        });
        self.write_chain(PlanEdgeKind::Next, revision, &tasks);
        Ok(revision)
    }

    /// Record a `[:THEN]` edge: this task actually ran.
    ///
    /// Refused when the current revision does not contain `task_id` — see the
    /// module docs on why that refusal is the mechanism rather than a
    /// safety net.
    ///
    /// Idempotent per task. A board can report the same task starting more
    /// than once (a resumed turn re-reads its own snapshot), and an actual
    /// path that grew a duplicate row every time would report drift that
    /// never happened. Re-running a task on purpose is a real thing this does
    /// not model yet — #5037's residue names it.
    pub fn ran(&mut self, task_id: &str) -> Result<(), PlanGraphError> {
        let revision = self.revision();
        let Some(task) = self
            .planned(revision)
            .into_iter()
            .find(|task| task.id == task_id)
        else {
            return Err(PlanGraphError::UnplannedTask {
                id: task_id.to_owned(),
                revision: revision.get(),
            });
        };
        let actual = self.actual();
        if actual.iter().any(|ran| ran.id == task_id) {
            return Ok(());
        }
        let from = match actual.last() {
            Some(previous) => PlanEdgeSource::Task(previous.id.clone()),
            None => PlanEdgeSource::Plan,
        };
        self.edges.push(PlanEdge {
            kind: PlanEdgeKind::Then,
            revision,
            from,
            to: task,
            position: u32::try_from(actual.len()).unwrap_or(u32::MAX),
        });
        self.edges.sort_by_key(canonical_order);
        Ok(())
    }

    /// The revision the plan is on now — what a breadcrumb renders.
    #[must_use]
    pub fn revision(&self) -> PlanRevision {
        self.nodes
            .last()
            .map_or(PlanRevision::FIRST, |node| node.revision)
    }

    /// Every plan node, `r1` first. What a store persists.
    #[must_use]
    pub fn nodes(&self) -> &[PlanNode] {
        &self.nodes
    }

    /// Every edge, both lanes. What a store persists.
    #[must_use]
    pub fn edges(&self) -> &[PlanEdge] {
        &self.edges
    }

    /// The planned path at one revision, in order. Empty for a revision the
    /// graph does not have.
    #[must_use]
    pub fn planned(&self, revision: PlanRevision) -> Vec<TaskNode> {
        self.chain(PlanEdgeKind::Next, Some(revision))
    }

    /// The actual path, in the order the tasks ran.
    #[must_use]
    pub fn actual(&self) -> Vec<TaskNode> {
        self.chain(PlanEdgeKind::Then, None)
    }

    /// How many tasks the plan was approved with — SPEC 7.3's `planned 6`.
    #[must_use]
    pub fn planned_count(&self) -> usize {
        self.planned(PlanRevision::FIRST).len()
    }

    /// How many have actually run — SPEC 7.3's `actual 7`.
    #[must_use]
    pub fn actual_count(&self) -> usize {
        self.actual().len()
    }

    /// Every departure of the plan from the plan it was approved as, oldest
    /// revision first — SPEC 7.3's `⌥ 1 drift`.
    ///
    /// **Derived, never stored.** Each revision after the first is compared
    /// against its immediate predecessor; what it added is
    /// [`DivergenceKind::Inserted`], what it took out is
    /// [`DivergenceKind::Dropped`], and both carry that revision's own cause.
    /// Per revision the insertions come before the removals, so the order is a
    /// function of the graph and two readers never disagree about it.
    ///
    /// A revision that only reworded a task therefore contributes no
    /// divergence, which is right: the plan's *shape* did not change, and the
    /// footer counts steps rather than edits.
    #[must_use]
    pub fn divergences(&self) -> Vec<Divergence> {
        let mut out = Vec::new();
        for node in &self.nodes {
            let (Some(cause), Some(previous)) = (node.cause.as_ref(), node.revision.previous())
            else {
                continue;
            };
            let before = self.planned(previous);
            let after = self.planned(node.revision);
            let before_ids: HashSet<&str> = before.iter().map(|t| t.id.as_str()).collect();
            let after_ids: HashSet<&str> = after.iter().map(|t| t.id.as_str()).collect();
            for task in after.iter().filter(|t| !before_ids.contains(t.id.as_str())) {
                out.push(Divergence {
                    kind: DivergenceKind::Inserted,
                    task: task.clone(),
                    revision: node.revision,
                    cause: cause.clone(),
                });
            }
            for task in before.iter().filter(|t| !after_ids.contains(t.id.as_str())) {
                out.push(Divergence {
                    kind: DivergenceKind::Dropped,
                    task: task.clone(),
                    revision: node.revision,
                    cause: cause.clone(),
                });
            }
        }
        out
    }

    /// Append one lane as a chain: the plan node, then each task after the one
    /// before it.
    fn write_chain(&mut self, kind: PlanEdgeKind, revision: PlanRevision, tasks: &[TaskNode]) {
        for (position, task) in tasks.iter().enumerate() {
            let from = match position.checked_sub(1).and_then(|i| tasks.get(i)) {
                Some(previous) => PlanEdgeSource::Task(previous.id.clone()),
                None => PlanEdgeSource::Plan,
            };
            self.edges.push(PlanEdge {
                kind,
                revision,
                from,
                to: task.clone(),
                position: u32::try_from(position).unwrap_or(u32::MAX),
            });
        }
        self.edges.sort_by_key(canonical_order);
    }

    /// One lane's tasks in position order. `revision` narrows to a single
    /// `[:NEXT]` chain; `None` takes the whole lane, which is what the single
    /// `[:THEN]` chain wants.
    fn chain(&self, kind: PlanEdgeKind, revision: Option<PlanRevision>) -> Vec<TaskNode> {
        let mut lane: Vec<&PlanEdge> = self
            .edges
            .iter()
            .filter(|edge| edge.kind == kind && revision.is_none_or(|r| edge.revision == r))
            .collect();
        lane.sort_by_key(|edge| edge.position);
        lane.into_iter().map(|edge| edge.to.clone()).collect()
    }

    /// [`Self::chain`], but refusing a lane whose positions or links are not a
    /// chain. Used only on the way back in from a store.
    fn read_chain(
        &self,
        kind: PlanEdgeKind,
        revision: Option<PlanRevision>,
    ) -> Result<Vec<TaskNode>, PlanGraphError> {
        let mut lane: Vec<&PlanEdge> = self
            .edges
            .iter()
            .filter(|edge| edge.kind == kind && revision.is_none_or(|r| edge.revision == r))
            .collect();
        lane.sort_by_key(|edge| edge.position);
        for (position, edge) in lane.iter().enumerate() {
            let expected = u32::try_from(position).unwrap_or(u32::MAX);
            if edge.position != expected {
                return Err(PlanGraphError::BrokenLane {
                    kind: kind.as_wire_str(),
                    revision: edge.revision.get(),
                    position: edge.position,
                });
            }
        }
        Ok(lane.into_iter().map(|edge| edge.to.clone()).collect())
    }

    /// The `[:THEN]` lane's edges in position order, checked as a chain.
    ///
    /// Kept apart from the `[:NEXT]` rebuild because a `[:THEN]` edge carries
    /// the revision that was in force when its task ran, so the lane spans
    /// revisions and cannot be regenerated from any single one.
    fn read_then_edges(&self) -> Result<Vec<PlanEdge>, PlanGraphError> {
        self.read_chain(PlanEdgeKind::Then, None)?;
        let mut lane: Vec<PlanEdge> = self
            .edges
            .iter()
            .filter(|edge| edge.kind == PlanEdgeKind::Then)
            .cloned()
            .collect();
        lane.sort_by_key(|edge| edge.position);
        Ok(lane)
    }

    /// The revision ladder: contiguous from `r1`, and a cause on every rung
    /// but the first.
    fn check_revisions(&self) -> Result<(), PlanGraphError> {
        let mut expected = PlanRevision::FIRST;
        for node in &self.nodes {
            if node.revision != expected {
                return Err(PlanGraphError::MissingRevision {
                    revision: expected.get(),
                });
            }
            match (node.revision == PlanRevision::FIRST, node.cause.is_some()) {
                (true, true) => return Err(PlanGraphError::CausedApproval),
                (false, false) => {
                    return Err(PlanGraphError::CauselessRevision {
                        revision: node.revision.get(),
                    });
                }
                _ => {}
            }
            expected = expected.next();
        }
        if self.nodes.is_empty() {
            return Err(PlanGraphError::MissingRevision {
                revision: PlanRevision::FIRST.get(),
            });
        }
        Ok(())
    }
}

/// The one order [`PlanGraph::edges`] is ever in: the planned lanes by
/// revision, then the actual lane, each by position.
///
/// Held after every write rather than applied on read, so a graph rebuilt by
/// [`PlanGraph::restore`] compares equal to the live one it came from — which
/// is the whole of the replay claim, and would not hold if the order depended
/// on the sequence of calls that produced it.
fn canonical_order(edge: &PlanEdge) -> (u8, u32, u32) {
    let lane = match edge.kind {
        PlanEdgeKind::Next => 0,
        PlanEdgeKind::Then => 1,
    };
    (lane, edge.revision.get(), edge.position)
}

/// A lane is a non-empty order over distinct tasks.
fn check_lane(tasks: &[TaskNode]) -> Result<(), PlanGraphError> {
    if tasks.is_empty() {
        return Err(PlanGraphError::EmptyPlan);
    }
    let mut seen = HashSet::with_capacity(tasks.len());
    for task in tasks {
        if !seen.insert(task.id.as_str()) {
            return Err(PlanGraphError::DuplicateTask {
                id: task.id.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
