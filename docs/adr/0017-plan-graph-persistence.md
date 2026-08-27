---
id: adr/0017-plan-graph-persistence
title: "ADR 0017: The plan graph is persisted in the store, not the context plane"
status: proposed
---

# ADR 0017: The plan graph is persisted in the store, not the context plane

- Status: **Proposed** — awaiting ratification by the repository owner.
- Date: 2026-08-26
- Deciders: repository owner (pending)
- Tracking: [issue #5037](https://github.com/macanderson/stella/issues/5037)
- Scope note: not part of the Phase 0 adaptive-context series (ADRs 0001–0012).
  It is filed here because this is where Stella's numbered, ratifiable decision
  records live — see [README](README.md).

## Context

`design/tui-v2/SPEC.md` §7.4 specifies a **plan graph**: the planned path comes
from `[:NEXT]` edges, the actual path from `[:THEN]`, and each divergence
carries a cause. §1.3 makes it a product thesis — *drift is recorded, not
hidden* — and §7.3's plan-panel footer (`planned 6 · actual 7 · ⌥ 1 drift`)
reads three numbers straight off it. None of it existed: before #5037 the only
`NEXT`/`THEN` in the tree were SQL `CASE WHEN`.

Issue #5037 settles the type home and the decision home by naming them —
`stella-protocol` for the types (AGENTS.md rule 4) and `stella-core` for
the decision logic (rule 2) — and leaves one question open, in those
words: *persistence in `crates/stella-store` or `crates/stella-context`; decide
with the maintainer's routing table (AGENTS.md), and say so in the PR.*

The routing table gives each crate one sentence:

| Crate | Its row in AGENTS.md § "Workspace layout" | Its file |
|---|---|---|
| `stella-store` | Persistence: executions, events, telemetry (SQLite) | `.stella/private/store.db` |
| `stella-context` | Retrieval: graph, embeddings, episodic memory | `.stella/private/context.db` |

The word *graph* appears in the second row, which is the whole reason the
question is worth an ADR rather than a shrug.

## Decision

> **The plan graph is persisted in `stella-store`, in `store.db`, as two
> additive tables (`plan_revisions`, `plan_edges`) keyed on
> `executions.id`.**

`stella-context` is not extended, and no new database file is created.

### Why

**1. It is a record of a turn, not a thing to recall.** `stella-context`'s
graph is a *retrieval* index: nodes and embeddings that exist so a later
question can find relevant material by similarity. Nothing recalls a plan
graph. It is read back for exactly one reason — to reconstruct what one
execution planned and what it did — and that is the question every table in
`store.db` already answers about one execution.

**2. The join keys are all in `store.db`.** A plan graph is meaningful beside
the `executions` row it belongs to, the `tasks` rows its nodes name, and the
`events` journal that recorded the turn. Putting it in `context.db` would put a
join across two database files between rows that are only interpretable
together — and AGENTS.md is explicit that the two files were *separated* on
purpose (`context.db` vs `codegraph.db`, "don't revert this"), which is an
argument for keeping each file's contents coherent rather than for spreading a
record across them.

**3. Retention already knows how to drop it.** `Store::prune`'s unit of
retention is the execution, and `DEPENDENT_TABLES` is the list of tables that
cascade with one. A plan graph keyed on `executions.id` joins that list and is
reclaimed with the turn it describes. In `context.db` it would be outside every
retention policy either crate has, and a per-turn record that never expires is
a leak with a schedule.

**4. The counter-argument, answered.** The counter-argument is the word
*graph* in the routing table. It does not survive reading either crate: the
`stella-context` row means the *retrieval* graph, and `store.db` itself
carried a `graph_nodes`/`graph_edges` pair until schema v17 dropped them,
because they were a reserved seam for a context plane that shipped
its own stores instead. That deletion is a caution about **unwired** schema,
not about graph-shaped tables in the store — and it is why this decision ships
a live writer (the deck's plan gate, through
`stella_cli::command_deck::lead_turn`) and a live reader
(`Store::plan_graph`) in the same change rather than a table waiting for one.

### The shape, and why it is rows rather than a blob

Two tables, both additive, in one migration (v38 → v39):

- `plan_revisions` — one row per revision of one execution's plan: the plan
  node. `cause` is SQL NULL on revision 1 and only there.
- `plan_edges` — the two lanes. `kind` is `'next'` or `'then'`; `from_task` is
  NULL for the head of a lane (the edge that follows the plan node itself);
  `position` orders the chain.

The alternative was one row per execution holding the whole graph as JSON, the
way `tasks.contract` holds a `TaskContract`. That was rejected on the issue's own
labels: `pain:auditability` means somebody wants to ask *which turns diverged,
at which revision, and why*, and against a JSON column that is a full scan of
parsed text. The graph is edges; storing it as edges is the shape that still
reads as obviously correct in ten years (CLAUDE.md SD-2).

`plan_edges.to_subject` duplicates a column `tasks` already has — the one
place this design accepts denormalization. `/clear` deletes a session's `tasks`
rows on purpose, and an audit trail whose lanes go blank when somebody resets
their board is not an audit trail.

### What crosses the boundary

The store speaks `stella_protocol::plan_graph` values and nothing else.
`stella-store` does not depend on `stella-core`, so it cannot hold a
`PlanGraph`; it hands back nodes and edges, and
`stella_core::plan_graph::PlanGraph::restore` decides whether they compose into
a graph. One place knows what a lane is, and it is not SQL.

## Consequences

- `store.db` gains schema version 39. The step is purely additive with no
  backfill: nothing recorded either lane before it existed, so a workspace
  upgrading into it has no plan graphs, which is the truthful starting state.
- `DEPENDENT_TABLES` grows by two, so pruning an execution reclaims its plan
  graph with it.
- The plan graph is per **execution**, which makes it per turn. That matches
  the lifetime `ScopeProposal::revision` already has (the deck's gate resets
  per turn, "because that is where the deck also drops the plan it was
  holding"). A plan that spans turns is a larger question and is left open
  below rather than settled here.
- Nothing renders the two lanes yet. #4339's task-zoom scene is the consumer,
  and `Store::plan_graph` is the read path it will use.

## Open questions

1. **Should a plan graph span the turns of a session rather than one turn?**
   SPEC's breadcrumb reads as though a plan outlives the turn that proposed it.
   Keying on `executions.id` is what the deck's gate holds today; widening it
   to a session is a change to the gate's lifetime first and to this schema
   second.
2. **Is a re-ordering of the plan a divergence?** Today the derived kinds are
   `Inserted` and `Dropped`; a plan whose steps were re-ordered records
   neither. That is a narrowing chosen on purpose — an order diff has no
   unambiguous minimal answer, and SPEC only names insertion — not an
   oversight.
