// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `plan_revisions` and `plan_edges` tables: one turn's plan graph — every
//! revision, the `[:NEXT]` chain each authored, and the `[:THEN]` chain of what
//! ran (`design/tui-v2/SPEC.md` §7.4). The DDL is `crate::ddl`; this is the
//! read/write API over it, in [`stella_protocol::plan_graph`] values, because
//! composing them back into a graph belongs to `stella_core::plan_graph`.
//! `doc:adr/0017-plan-graph-persistence` says why the record lives here.
//!
//! A plan graph only grows, so every write upserts on the row's natural key
//! and re-writing one is a no-op. Do not add a delete path: retention here is
//! `Store::prune`'s, keyed on the execution.

use rusqlite::{OptionalExtension, params};
use stella_protocol::plan_graph::{
    DivergenceCause, PlanEdge, PlanEdgeKind, PlanEdgeSource, PlanNode, PlanRevision, TaskNode,
};

use crate::{Result, Store, StoreError};

/// The `plan_edges.from_task` value meaning "the plan node itself" — the head
/// of a lane. Stored as SQL NULL, which is the shape a lane head has: there is
/// no task before the first one.
const LANE_HEAD: Option<String> = None;

impl Store {
    /// Write one execution's plan graph: its revisions and its edges.
    ///
    /// One transaction, like every sibling fan-out writer in [`crate`] (see
    /// [`Store::record_task_board`]): a graph is a whole record, and a write
    /// that failed partway would leave the table holding a plan whose lanes
    /// do not reach their own tasks — which
    /// `stella_core::plan_graph::PlanGraph::restore` would then refuse, losing
    /// the turn's whole plan rather than the half that failed.
    ///
    /// `nodes` and `edges` come off one graph and are not checked against each
    /// other here: coherence is the engine's rule, restored and enforced on the
    /// way back in, and re-deriving it in SQL would be a second opinion that can
    /// disagree with the first.
    pub fn record_plan_graph(
        &self,
        execution_id: i64,
        nodes: &[PlanNode],
        edges: &[PlanEdge],
        now_ms: u64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for node in nodes {
            tx.execute(
                "INSERT INTO plan_revisions (execution_id, revision, cause, recorded_at) \
                 VALUES (?, ?, ?, ?) \
                 ON CONFLICT (execution_id, revision) DO UPDATE SET \
                 cause = excluded.cause, recorded_at = excluded.recorded_at",
                params![
                    execution_id,
                    node.revision.get(),
                    node.cause.as_ref().map(DivergenceCause::as_str),
                    now_ms as i64,
                ],
            )?;
        }
        for edge in edges {
            let from_task = match &edge.from {
                PlanEdgeSource::Plan => LANE_HEAD,
                PlanEdgeSource::Task(id) => Some(id.clone()),
            };
            tx.execute(
                "INSERT INTO plan_edges \
                 (execution_id, revision, kind, position, from_task, to_task, to_subject) \
                 VALUES (?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (execution_id, kind, revision, position) DO UPDATE SET \
                 from_task = excluded.from_task, to_task = excluded.to_task, \
                 to_subject = excluded.to_subject",
                params![
                    execution_id,
                    edge.revision.get(),
                    edge.kind.as_wire_str(),
                    edge.position,
                    from_task,
                    edge.to.id,
                    edge.to.subject,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Read one execution's plan graph back: its revisions oldest first, then
    /// its edges in the lane order they were written.
    ///
    /// Returns two empty vectors for an execution that recorded no plan —
    /// every turn that ran without a plan gate, which is most of them. That is
    /// an answer, not a gap: nothing was approved, so there is no planned path
    /// to compare an actual one against.
    ///
    /// The rows are handed back as [`stella_protocol::plan_graph`] values and
    /// nothing more. Deciding whether they compose into a graph belongs to
    /// `stella_core::plan_graph::PlanGraph::restore`, which is the one place
    /// that knows what a lane is.
    pub fn plan_graph(&self, execution_id: i64) -> Result<(Vec<PlanNode>, Vec<PlanEdge>)> {
        let conn = self.lock();
        let mut revisions = conn.prepare(
            "SELECT revision, cause FROM plan_revisions \
             WHERE execution_id = ? ORDER BY revision",
        )?;
        let nodes = revisions
            .query_map(params![execution_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(revision, cause)| {
                Ok(PlanNode {
                    revision: plan_revision(revision)?,
                    cause: cause.map(divergence_cause).transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut lanes = conn.prepare(
            "SELECT revision, kind, position, from_task, to_task, to_subject FROM plan_edges \
             WHERE execution_id = ? ORDER BY kind, revision, position",
        )?;
        let edges = lanes
            .query_map(params![execution_id], |row| {
                Ok(EdgeRow {
                    revision: row.get(0)?,
                    kind: row.get(1)?,
                    position: row.get(2)?,
                    from_task: row.get(3)?,
                    to_task: row.get(4)?,
                    to_subject: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(EdgeRow::into_edge)
            .collect::<Result<Vec<_>>>()?;

        Ok((nodes, edges))
    }

    /// Whether an execution recorded a plan graph at all — one indexed lookup
    /// rather than reading a whole graph to find out.
    pub fn has_plan_graph(&self, execution_id: i64) -> Result<bool> {
        let conn = self.lock();
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM plan_revisions WHERE execution_id = ? LIMIT 1",
                params![execution_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

/// One `plan_edges` row as SQLite hands it over, before the protocol's rules
/// are applied to it.
struct EdgeRow {
    revision: i64,
    kind: String,
    position: i64,
    from_task: Option<String>,
    to_task: String,
    to_subject: String,
}

impl EdgeRow {
    fn into_edge(self) -> Result<PlanEdge> {
        Ok(PlanEdge {
            kind: PlanEdgeKind::from_wire_str(&self.kind).ok_or_else(|| {
                StoreError::Other(format!(
                    "plan_edges names the lane `{}`, which this build does not know; a plan \
                     graph has exactly two lanes, `next` and `then`",
                    self.kind
                ))
            })?,
            revision: plan_revision(self.revision)?,
            from: match self.from_task {
                Some(id) => PlanEdgeSource::Task(id),
                None => PlanEdgeSource::Plan,
            },
            to: TaskNode::new(self.to_task, self.to_subject),
            position: u32::try_from(self.position).map_err(|_| {
                StoreError::Other(format!(
                    "plan_edges carries the lane position {}, which is not a position",
                    self.position
                ))
            })?,
        })
    }
}

/// A stored revision number, refused rather than clamped: `r0` is not a
/// revision, and quietly promoting one to `r1` would let two different plans
/// claim the same identity.
fn plan_revision(stored: i64) -> Result<PlanRevision> {
    u32::try_from(stored)
        .ok()
        .and_then(PlanRevision::new)
        .ok_or_else(|| {
            StoreError::Other(format!(
                "a plan graph carries the revision {stored}, but revisions are one-based"
            ))
        })
}

/// A stored cause. SQL NULL is the approved plan's "nothing to explain" and is
/// handled by the caller; a stored empty string is a row nothing should have
/// written, and it is refused rather than rendered as a blank reason.
fn divergence_cause(stored: String) -> Result<DivergenceCause> {
    DivergenceCause::new(stored).ok_or_else(|| {
        StoreError::Other(
            "a plan revision carries a blank cause; every revision after the first says why the \
             plan left the path it was on"
                .to_owned(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str) -> TaskNode {
        TaskNode::new(id, format!("step {id}"))
    }

    fn chain(revision: PlanRevision, kind: PlanEdgeKind, ids: &[&str]) -> Vec<PlanEdge> {
        ids.iter()
            .enumerate()
            .map(|(position, id)| PlanEdge {
                kind,
                revision,
                from: match position.checked_sub(1).and_then(|i| ids.get(i)) {
                    Some(previous) => PlanEdgeSource::Task((*previous).to_owned()),
                    None => PlanEdgeSource::Plan,
                },
                to: task(id),
                position: position as u32,
            })
            .collect()
    }

    fn cause(text: &str) -> DivergenceCause {
        DivergenceCause::new(text).expect("a non-empty cause")
    }

    /// A plan graph written and read back is the plan graph that was written.
    /// Byte-exactness is the point: a replay that reads a *slightly* different
    /// plan disagrees with the run it claims to reproduce, and nothing notices.
    #[test]
    fn a_plan_graph_round_trips_through_the_store() {
        let store = Store::in_memory().expect("store");
        let exec = store
            .begin_execution("deck", "plan the work", "anthropic", "claude")
            .expect("exec");

        let nodes = vec![
            PlanNode {
                revision: PlanRevision::FIRST,
                cause: None,
            },
            PlanNode {
                revision: PlanRevision::FIRST.next(),
                cause: Some(cause("E0432")),
            },
        ];
        let mut edges = chain(PlanRevision::FIRST, PlanEdgeKind::Next, &["1", "2"]);
        edges.extend(chain(
            PlanRevision::FIRST.next(),
            PlanEdgeKind::Next,
            &["1", "3b", "2"],
        ));
        edges.extend(chain(
            PlanRevision::FIRST.next(),
            PlanEdgeKind::Then,
            &["1", "3b"],
        ));

        store
            .record_plan_graph(exec, &nodes, &edges, 1)
            .expect("record");
        let (back_nodes, back_edges) = store.plan_graph(exec).expect("read");
        assert_eq!(back_nodes, nodes);

        let mut expected = edges.clone();
        expected.sort_by_key(sort_key);
        let mut actual = back_edges;
        actual.sort_by_key(sort_key);
        assert_eq!(actual, expected);
        assert!(store.has_plan_graph(exec).expect("probe"));
    }

    fn sort_key(edge: &PlanEdge) -> (&'static str, u32, u32) {
        (edge.kind.as_wire_str(), edge.revision.get(), edge.position)
    }

    /// The writer is called at every turn boundary and on replay, so writing
    /// the same growing graph twice must add rows rather than duplicate them.
    #[test]
    fn re_recording_a_growing_graph_upserts_rather_than_duplicates() {
        let store = Store::in_memory().expect("store");
        let exec = store
            .begin_execution("deck", "plan the work", "anthropic", "claude")
            .expect("exec");
        let nodes = vec![PlanNode {
            revision: PlanRevision::FIRST,
            cause: None,
        }];
        let first = chain(PlanRevision::FIRST, PlanEdgeKind::Next, &["1", "2"]);

        store
            .record_plan_graph(exec, &nodes, &first, 1)
            .expect("record");
        let mut grown = first.clone();
        grown.extend(chain(PlanRevision::FIRST, PlanEdgeKind::Then, &["1"]));
        store
            .record_plan_graph(exec, &nodes, &grown, 2)
            .expect("re-record");

        let (back_nodes, back_edges) = store.plan_graph(exec).expect("read");
        assert_eq!(back_nodes.len(), 1);
        assert_eq!(back_edges.len(), 3, "{back_edges:?}");
    }

    /// A turn that ran without a plan gate recorded no plan, and that is an
    /// answer rather than a gap — there was nothing to compare an actual path
    /// against.
    #[test]
    fn an_execution_with_no_plan_reads_back_empty() {
        let store = Store::in_memory().expect("store");
        let exec = store
            .begin_execution("deck", "just do it", "anthropic", "claude")
            .expect("exec");
        assert_eq!(
            store.plan_graph(exec).expect("read"),
            (Vec::new(), Vec::new())
        );
        assert!(!store.has_plan_graph(exec).expect("probe"));
    }

    /// Two turns' graphs do not bleed into each other: the execution is the
    /// key, so one turn's revisions are invisible to the next.
    #[test]
    fn a_plan_graph_belongs_to_one_execution() {
        let store = Store::in_memory().expect("store");
        let first = store
            .begin_execution("deck", "turn one", "anthropic", "claude")
            .expect("exec");
        let second = store
            .begin_execution("deck", "turn two", "anthropic", "claude")
            .expect("exec");
        let nodes = vec![PlanNode {
            revision: PlanRevision::FIRST,
            cause: None,
        }];
        store
            .record_plan_graph(
                first,
                &nodes,
                &chain(PlanRevision::FIRST, PlanEdgeKind::Next, &["1"]),
                1,
            )
            .expect("record");

        assert!(store.has_plan_graph(first).expect("probe"));
        assert!(!store.has_plan_graph(second).expect("probe"));
    }

    /// A lane name this build does not know is refused, not guessed. Reading
    /// an unknown lane as `next` would put a task on the planned path that
    /// nobody planned.
    #[test]
    fn an_unknown_lane_is_refused_rather_than_guessed() {
        let store = Store::in_memory().expect("store");
        let exec = store
            .begin_execution("deck", "plan the work", "anthropic", "claude")
            .expect("exec");
        store
            .lock()
            .execute(
                "INSERT INTO plan_edges \
                 (execution_id, revision, kind, position, from_task, to_task, to_subject) \
                 VALUES (?, 1, 'maybe', 0, NULL, '1', 'step 1')",
                params![exec],
            )
            .expect("insert");

        let err = store
            .plan_graph(exec)
            .expect_err("an unknown lane is refused");
        assert!(err.to_string().contains("does not know"), "{err}");
    }

    /// `r0` is not a revision. Promoting one would let two plans claim the
    /// same identity, and the store is the last place that can still tell.
    #[test]
    fn a_zero_revision_is_refused() {
        let store = Store::in_memory().expect("store");
        let exec = store
            .begin_execution("deck", "plan the work", "anthropic", "claude")
            .expect("exec");
        store
            .lock()
            .execute(
                "INSERT INTO plan_revisions (execution_id, revision, cause, recorded_at) \
                 VALUES (?, 0, NULL, 1)",
                params![exec],
            )
            .expect("insert");

        let err = store.plan_graph(exec).expect_err("r0 is refused");
        assert!(err.to_string().contains("one-based"), "{err}");
    }
}
