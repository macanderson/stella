---
id: adr/0021-a-memory-event-names-one-memory
title: "ADR 0021: A memory event names one memory"
status: implemented
---

# ADR 0021: A memory event names one memory

- Status: **Implemented** — landed with #5032.
- Date: 2026-08-27
- Tracking: [issue #5032](https://github.com/macanderson/stella/issues/5032)

## Context

`design/tui-v2/SPEC.md` §6.3 gives memory two transcript events: a **log**,
carrying the lesson's text, its rung on an `OBSERVATION ▸ RULE ▸ FACT` ladder,
its confidence, its decay and the threshold it promotes at; and a **promote**,
one line saying which rung it moved to, on what confidence, and which
governance record makes the move auditable.

What the wire had was `AgentEvent::ContextWrite`: `provider`, `upserts`,
`superseded`. Three numbers about a batch. It can say that a turn wrote two
memories; it cannot say which two, so nothing downstream can act on one of
them — and a reader who disagrees with a lesson the loop just learned has
nothing to point at.

It also had no producer. Every construction of it in the tree is a test
fixture or a wire-contract sample (#5249), which meant two things nobody had
noticed: the `✎ memory` transcript row was unreachable on every live path, and
`Receipt::memories` — the `· n memories` cell on a turn's closing receipt —
had never once rendered a number, because that count came only from
`ContextWrite::upserts`.

So the question was not "how do we render the aggregate better". It was: what
shape does a memory event take.

## Decision

### 1. One event per memory, not one per batch

`AgentEvent::MemoryLogged` carries a single memory's id, text, class,
confidence, kind, decay and promotion threshold. A turn that writes three
lessons emits three.

The cheaper option was to widen `ContextWrite` — add a `memories: Vec<…>`
field beside the counts. It was rejected on the property that decides this
class of question: **can a consumer act on one item without re-deriving which
item it is?** A batch event forces every reader to index into a list and carry
the index alongside — the transcript's `x reject` would have to resolve a row
to `(event, position)`, and any later consumer would have to agree with it
about the ordering. One event per memory makes the identity the event's own
subject, and a row that renders one memory is a row that can act on one memory.

It is also what makes the two SPEC rows expressible at all: a log row states a
confidence and a threshold, and a batch has neither.

### 2. The aggregate is superseded, not fed

`ContextWrite` keeps its case, its transcript arm and its ledger row. Nothing
new emits it.

Retiring it outright was tempting — nothing produces it, so nothing would
break today. It stays because a stream recorded by another binary may carry
the tag, and a transcript that dropped an event it can decode is worse than
one that renders the older shape. Whether it gains a producer or is retired is
#5249's question, and the two are mutually exclusive: if both fired for one
write, the turn's receipt would count that write twice.

That constraint is stated where it can be enforced rather than in prose alone.
`stella-tui`'s `model/memory.rs` folds all three memory events — the two new
ones and the aggregate — in one function, so the counter they share is visible
in one place. The alternative, three arms in `SessionModel::apply`, put the
rule in the reader's head.

### 3. The identity is minted by the store, never derived by a caller

`stella_context::UpsertReceipt` gained `memory_node_ids`: the `nod_…` public
id of each memory's mirror node, in delta order.

The caller could compute it. The id is `sha256(kind \0 natural_key)`, the
natural key is the node's uri, and the uri is `memory://{lineage}` where the
lineage of a fresh memory is seeded from its content hash. All of that is
knowable from `stella-cli`.

Computing it there would be a second copy of an identity derivation, wrong the
day any of those three parts changes and silent when it does — the reject
affordance would tombstone an id no memory has, and report success. The store
mints the id; the receipt reports it. `ContextStore::upsert`'s anchor loop
already states this reasoning for why it mints the mirror node itself rather
than letting a caller name it, and this is the same rule pointed at the id.

### 4. Every number on the row is derived from evidence that already exists

A log row states `conf 0.62 · kind domain · decays` and
`promotes to RULE at 0.85`. None of those is a constant chosen to make the row
look like the mock:

- **confidence** comes from `confidence_from_score` over the evidence the
  lesson actually has — one occurrence, one distinct task — which is the same
  function the rule miner scores a proposal with. The number on the screen and
  the number the promotion gate reads are one derivation, so a row cannot
  promise a promotion the gate would refuse.
- **promotes_at** is the workspace's own
  `context.promotion.inferred_directive.auto_activate_at_confidence`, carried
  on the event rather than assumed by the renderer, because it is configurable
  per workspace and a row printing a constant would be wrong wherever it was
  tuned.
- **kind** is the lesson's own `LessonKind` (`domain` / `process`), the axis
  that varies. The store's `memory.kind` is `reflection` for every one of these
  and would be a column that never changes.
- **decays** is that same distinction read for what it means: a process note is
  true of the turn that produced it, a domain lesson is still true on a task
  the agent has not seen. `LessonKind::recall_tier` already spends the recall
  budget on exactly this.

A field with no such source does not go on the event. That is the rule #4180
established for the read head's line count — a column that is expressible,
unreachable, and reached by nothing but a fixture — restated for a new event.

### 5. Auto-activation records its own governance event

`AgentEvent::MemoryPromoted` is emitted when a mined rule clears the
confidence bar and a rule file lands in `.stella/rules/`, and it cites a
`promotion_event` record by id.

That record did not exist. Auto-activation wrote the rule file and nothing
else, so the lifecycle's own contract — replaying `promotion_event` records in
order reproduces the loop's governance state — did not hold for anything the
loop activated on its own: a replay rebuilt a workspace with every user-kept
rule and none of the automatic ones. The review surface's `keep` has always
recorded one.

Writing the record here is therefore a repair, and the event's `audit event
<id>` is what makes the repair falsifiable: the row cites a record, so a
missing record is a broken link somebody sees rather than a gap in a ledger
nobody reads.

`PromotionActor::System` is what keeps it safe. `PromotionEventRecord::new`
refuses a system actor blocking enforcement outright, so this path cannot mint
a directive that denies a tool call even if a later edit passes the wrong
argument.

## Why this is durable

The shape generalises. Every other "the loop did something to its own state"
event faces the same fork — count the batch or name the item — and the answer
is the same one for the same reason: a consumer that can act on one item is
worth more than a consumer that can total them, and totals are recoverable
from items while items are not recoverable from totals.

The identity rule generalises further. Wherever a store derives an id, the
receipt reports it; a caller that recomputes one is a second definition of the
same thing, and the failure mode is silence.

## Consequences

- `context_write` now has a documented producer gap (#5249) as well as its
  consumer gap (#4501), and the two must be settled together.
- `MemoryPromoted` ships `ConsumerPosture::RecordedOnly` — nothing branches on
  it (#5230).
- SPEC 6.3's `e edit` affordance is not rendered, because it is not routed
  (#5231). The footer lists the affordances that work.
