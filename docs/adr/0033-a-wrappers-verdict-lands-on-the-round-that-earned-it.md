---
id: adr/0033-a-wrappers-verdict-lands-on-the-round-that-earned-it
title: "ADR 0033: A wrapper's verdict lands on the round that earned it"
status: implemented
---

# ADR 0033: A wrapper's verdict lands on the round that earned it

- Status: accepted
- Date: 2026-09-07
- Decides: `#6001`
- Not part of the Phase 0 series.

## Context

A wrapper plugin judges a turn. What it decided is two events. One is
`AgentEvent::Verdict`. The other is the `AgentEvent::GateBoard` drawn from it.
Three doors drive a wrapper. Only one of them wrote those events down.

`stella goal --pipeline` wrote them. It opens one row for the whole goal run.
It holds one channel open across every round. A send at a round boundary
reaches both.

`stella run --pipeline` did not. Each of its rounds is a
`crate::agent::run_turn`. That call opens a row of its own. It closes the
channel that feeds the row before it returns. The verdict comes after the last
round has done both. So the send `run_wrapped` made through the registry found
an empty slot. It dropped both events and said nothing.

`stella fleet --pipeline` did not either. Its reason is a different one. Its
channel and its row both outlive the dispatch. It never sent the two events at
all.

This is a real loss. `stella dataset export` builds a training label from one
row's journal. `crate::dataset_cmd`'s `fold_journal` reads
`Store::execution_events`. It wants the verdict and that turn's own
`FileChange` events in one fold. A verdict on any other row is one that fold
cannot see. A shared session id does not help it.

The issue offered two directions.

**Widen the wire.** `TurnDriver::run_turn` returns `DrivenTurn`. A new field
could name the row the round used. But that type is the wrapper socket's
contract. It is meant for hosts with no local store at all. A host's own
bookkeeping does not belong in it.

**Fold across the session.** Give the events to some other row. Then teach the
export to read a whole session. That changes the one-row model a plain `stella
run` still leans on, where `session_id` is NULL. It would change it to fix a
problem only the wrapped path has.

## Decision

A third. The row travels back to the host that asked for the turn. The wire is
untouched.

`crate::turn_row::TurnRow` is a shared handle. `run_turn` writes the row it
just opened into it. It does that through the `TurnDoor` the caller already
threads in (`recording_to`). `RawTurnDriver` carries one across the dispatch.
Each round replaces the last, so what survives is the final round's row. That
is the round `DispatchReport::verdict` is about. `run_wrapped` appends the two
events there.

`Store::append_event` is the write. The renderer that owned the sequence is
gone, so the caller has no number to hand in. This reads `max(seq) + 1` inside
the transaction that writes the row. Two appenders block on the same lock.
Neither can pick the other's number.

The fleet door needs none of that. Its channel is open. Its row is the one the
rounds already wrote to. So `AttemptWrapper::settle` sends the same two events
on it. The events come from `wrapper_plugin::run_events`, the vocabulary the
other doors use, so no door invents its own spelling.

`TurnDriver` and `DrivenTurn` do not change. `stella-serve` has nothing to
absorb. Its own `DrivenTurn` is a different type in a different crate. It never
implemented this trait.

## Consequences

- A wrapped `stella run` writes the verdict and the board on the round's own
  row. The witness is
  `run_wrapped_records_the_verdict_on_the_rounds_own_execution_row`. It asserts
  the row and not just the count. A verdict anywhere else would not reach the
  fold that needs it.
- A wrapped `stella fleet` attempt writes them on the attempt's row. The
  witness is `a_fleet_attempt_records_the_verdict_on_its_own_execution_row`.
  Both fail without the change. The store holds zero verdicts.
- A wrapper that holds several rounds open writes one verdict, on the last
  round. An earlier row would date the call to a turn that was later revised.
- The events land after the row is finished. Nothing in the schema closes a
  journal. Every reader orders by sequence. So they read as the last thing
  that happened to that row, which they are.
- A run with persistence off writes neither event. Every other event on this
  path degrades the same way.
- The two events reach the store, not a live screen. On the `run` door there
  is no live screen left at that moment. The round's renderer has already
  finished. What a person sees is unchanged: the report's own lines, which
  `report_to` still prints.
