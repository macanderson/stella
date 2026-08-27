---
id: adr/0019-graph-session-touch-tags
title: "ADR 0019: The Graph tab's session tags are stamped by the deck, not by the producer"
status: proposed
---

# ADR 0019: The Graph tab's session tags are stamped by the deck, not by the producer

- Status: **Proposed** — awaiting ratification by the repository owner.
- Date: 2026-08-26
- Deciders: repository owner (pending)
- Tracking: [issue #5045](https://github.com/macanderson/stella/issues/5045)
- Scope note: outside the Phase 0 adaptive-context series. It decides where one
  field of an existing read-model is written.

## Context

SPEC 9.1 asks the GRAPH tab's node card to tag an edge with what this session
did to the file at its far end: `← stella-cli::self_driving_cmd · edited turn
14`. The tag carries two values the code graph does not hold — a **turn
ordinal** and a **verb** — so something has to supply them.

Issue #5045 proposed supplying them in `crates/stella-cli`'s snapshot producer
(`agent::graph_view`), stamping each edge as the neighborhood is read out of
`codegraph.db`. Implementing it there ran into two facts about the tree:

1. **The producer cannot know a turn number.** A turn ordinal is
   `SessionModel::turns_completed + 1`, folded from the `AgentEvent` stream in
   `stella-tui`. `stella-cli` emits those events; it does not fold them, and it
   holds no session model. Reconstructing the ordinal from the store's
   `files_touched` rows would be a *second* answer to "what has this session
   touched", which is what the issue's own constraint — reuse the ledger the
   `● hot` marker already reads — exists to prevent. That ledger is the focused
   lane's `Vec<FileState>`, and it is deck-side.
2. **A value stamped at read time goes stale.** A neighborhood is queried once
   and then sits on screen for the rest of a session. The `● hot` mark beside
   the tag has always been derived live, per frame. A tag baked in at query
   time would still say `edited turn 3` after turn 9 edited the same file,
   while the mark next to it had moved on — two readings of one ledger
   disagreeing about the same row, in adjacent columns.

## Options

**A. `stella-cli` stamps the edges as it builds the snapshot** (the issue's
plan). Rejected: it needs a turn ordinal the producer cannot see, so it either
invents a second source of truth or ships a tag that is `None` in practice, and
the value it did stamp would be stale by the next turn.

**B. The deck stamps once, when the snapshot arrives** (`deck_ui::ingest_inner`
already has the model in hand). Rejected for staleness alone: the stamp is
correct for exactly as long as nobody edits anything.

**C. The deck stamps on every frame, from the ledger the `● hot` mark reads.**
Chosen.

A fourth question sat inside the third: whether the tag belongs on `GraphEdge`
or on `GraphNode`. An edge has two ends, the card always cites the end that is
*not* under the cursor, and which end that is depends on the cursor rather than
on the edge — so an edge-side tag has to say which endpoint it describes, and
still cannot answer for the other one. A node-side tag has no such ambiguity,
and it is also what SPEC 9.1's node list asks for ("`● hot` marks nodes touched
this session, tagged with the turn").

## Decision

`GraphNode` carries `touch: Option<SessionTouch>` — a turn (itself optional)
and the `FileChangeKind` that reached the file.
`GraphSnapshot::stamp_session_touches` writes it from a `&[FileTouch]` slice of
the session's file ledger, and `views::graph_tab::render` calls it on every
frame before drawing. Producers outside the deck leave the field unset; a
producer that *has* measured a session (a scenario fixture, a replayed
neighborhood) keeps its own value for any node the ledger does not name.

`FileState` gains `touched_turn: Option<u32>`, stamped in
`SessionModel::touch_file` from the same `turns_completed + 1` the SPEC 6.1
opening rule uses, so a node reading `edited turn 3` and the rule above that
edit in the transcript name one turn. `reset_conversation` clears it while
keeping the ledger: the turn counter restarts at `/clear` and the ledger does
not, so a stamp made under the old numbering would be compared against the new
one's. The touch survives — the node stays `● hot` — and only its turn is
dropped.

## Why this is the durable choice

- **One source of truth, structurally.** The mark and the tag are two renderings
  of one `FileState` row, read at one moment. Nothing can drift, because there
  is nothing to drift from.
- **The projection is idempotent and cheap.** Stamping is a pass over the
  snapshot's nodes against a ledger the deck already holds; it recomputes rather
  than caches, so no invalidation rule exists to get wrong later.
- **The seam stays where the crates already put it.** `stella-tui` renders data
  given to it and never reaches into a backend; `stella-cli` reads the index and
  never folds a session. Neither rule moves.
- **`None` keeps its meaning.** A snapshot with no turn says nothing rather than
  printing `turn 0`, the same rule `GraphSnapshot::query_ms` follows for a query
  nobody timed (#4335).

## Consequences

- The issue's file list named `GraphEdge`; the field landed on `GraphNode`. The
  rendered result is what the definition of done asks for — an edge whose file
  was edited this session renders `edited turn N` on the node card — and the
  node list gains the tag it was owed as well.
- `views::graph_tab::paint` no longer takes a `changed: &[String]` argument; the
  snapshot arrives already carrying the answer.
- A producer outside the deck can still stamp a node it measured, which is what
  lets a scenario fixture render the tag with no live session behind it.
