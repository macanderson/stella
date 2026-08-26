---
id: adr/0015-the-task-tag-rides-the-event
title: "ADR 0015: The task tag rides the event"
status: implemented
---

# ADR 0015: The task tag rides the event

- Status: **Implemented** — landed with #5039. Not part of the Phase 0
  adaptive-context series.
- Date: 2026-08-26
- Tracking: [issue #5039](https://github.com/macanderson/stella/issues/5039)

## Context

`design/tui-v2/SPEC.md` §7.1 gives a board task three parts: a **contract**, an
**evidence ledger** (the events tagged with its id) and a **cost**
(`$ · tok · cache rd% · model calls · est remain`). The contract half shipped as
`stella_protocol::task_contract`. The other two could not be built, for one
reason: no event anywhere named a task. "What did task 3 edit?" and "what did
task 3 cost?" had no query behind them — only a reader's guess from timestamps,
which two concurrent lanes make unguessable.

Four decisions had to be made to close that, and each had an obvious cheaper
answer that would have been wrong later.

## Decision

### 1. The tag is a field on the event, not an envelope around it

`stella_protocol::journal::StampedEvent` makes the opposite call for `ts`, and
the two look alike enough that the difference has to be stated. A timestamp is a
fact about the **write**: one event reaches several sinks, and the engine that
produces it owns no clock, so a `ts` field would have meant a clock reachable
from `stella-core` and a stamp baked into every replay fixture.

A task id is a fact about the **work**. The engine dispatching a call is exactly
the thing that knows which task it is dispatching for, the answer is identical in
every sink the event reaches, and it must survive a replay that rewrites the
line. So it is a field: `task_id: Option<TaskId>` on the six cases that
represent work, `serde(default, skip_serializing_if)` so the wire stays additive
and an old stream re-serializes to the bytes it arrived as.

`stella_protocol::event::task_tag` holds the carrier table, listing the carrying
and the non-carrying cases separately so both are `E0004` — a new `AgentEvent`
case does not compile until someone classifies it. A wildcard `_ => None` arm
would have let a new case fall through to *untagged* and be wrong only at
runtime, as a task whose ledger silently misses a class of its own work.

### 2. Stamping happens at send, through a slot shared by a sender's clones

The tag has to be applied **synchronously, at send**. A drain is the obvious
place — a renderer or journal writer sees every event — but it sees them later,
and by the time it folds a `tool_result` the board may have moved on, so the
ledger would be misattributed. The emit sites are the other obvious place, and
there are dozens: threading the running task to each is a rule every future call
site has to remember, and the one that forgets is silent, which is the shape
AGENTS.md #10 exists to end.

`EventSender` therefore carries a late-attachable source
(`attach_running_task`) shared by every clone — the shape
`ToolRegistry::enable_task_delegation` and `attach_call_measure` already use, and
for the same reason: whether anything can answer "which task is running" is a
fact about the host, not about the sender, and it is not known where a sender is
built. Attaching to one clone reaches all of them, so a host wires the engine's
stream and the registry's stream by wiring the sender they already share — no
second sender to keep alive and no drop order to get right.

The source is a **closure over the board** (`stella_core::RunningTask`), not a
cached answer. A cached `Option<TaskId>` would be a second place the running task
is written down, refreshed by whoever remembered to, and the board is moved by
six tools, a plan seeding, a `/clear` and every assignment.

### 3. The store projects the tag into a column

The payload carries the tag, so a ledger *could* be answered by decoding every
row's JSON. `events` is the largest table the store keeps, one row per stream
position forever, so that would mean decoding a session's whole history to
answer a filter. `events.task_id` (schema v39) is a projection written in the
same transaction as the row it comes from, read through a partial index — the
same relationship `tool_calls` already has to `events`, with the payload
remaining the source of truth.

### 4. `est remain` has a source or it is absent, and `cache rd%` is not a field

SPEC 6.1's `det %` was specified once and dropped for having no source, because
a number nothing measures is worse on a receipt than an absent one
(`stella_protocol::task_contract` carries that argument). `est remain` is the
same hazard one field over. `TaskCost::estimated_remaining_usd` is an `Option`,
derived from **this session's own completed tasks** — the mean of what tasks that
reached `Completed` actually cost, minus what this task has spent, floored at
zero — and `None` when no sibling has finished. A terminal task reports
`Some(0.0)`, which is a fact rather than an estimate.

`cache rd%` is **not** a field on `TaskCost`. `stella-store` does
not depend on `stella-model`, where the one definition of that ratio lives
(`cache_economics::hit_rate`), and a second spelling of it in the store would be
one rule in two places. The two counts it is computed from ride the row instead.

## Consequences

- A task's evidence ledger and its cost are both selections
  (`Store::task_events`, `Store::session_task_costs`), so neither can drift from
  the stream it summarizes.
- An untagged event is in no task's ledger. That is the failure mode by design:
  a host that builds a second sender over the same channel, or a lane whose board
  is empty, under-reports rather than misattributes.
- A `task_assign` worker lane runs on its own, empty board and therefore emits
  untagged events today. Its source should be a constant rather than a board
  read; tracked in [#5158](https://github.com/macanderson/stella/issues/5158).
- `TaskItem::id` remains a `String` while the tag is a `TaskId`, which is a seam
  rather than a design; tracked in
  [#5159](https://github.com/macanderson/stella/issues/5159).
