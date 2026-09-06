---
id: adr/0030-a-turn-has-no-step-cap-by-default
title: "ADR 0030: A turn has no step cap by default"
status: implemented
---

# ADR 0030: A turn has no step cap by default

- Status: accepted
- Date: 2026-09-06
- Decides: `#6237`
- Not part of the Phase 0 series.

## Context

Before this, every turn stopped at 200 steps. `EngineConfig::default()` set
`max_steps: 200`. The step loop in `crates/stella-core/src/driver/drive.rs`
ended the turn there with a `DeliberateStop`. Its own doc called it a backstop
behind loop detection.

A count has a flaw the other bounds do not. It cannot tell real work from
wandering. A prompt that asks for a whole feature can take thousands of steps.
So can a run that keeps going until its tests pass. Every one of those steps
is real work. The cap ended those runs. It ended them at the point where the
engine was doing its job. A wandering turn was ended at the same point, after
all 200 steps had been paid for.

The engine has bounds that read evidence instead of a count:

- **Loop detection** (`crates/stella-core/src/loop_detect.rs`) reads what each
  call returned. It has five rungs. They fire on the same output twice, on a
  short cycle, on arguments that change with no new output, on a repeat with
  other work between, and on a sweep that wrapped. Each fires within a few
  calls. The steer-then-abort ladder in `driver/loop_escalation.rs` ends the
  turn if the model does not change course.
- **The stall rung** (`driver/loop_escalation.rs`) reads how long the turn has
  slept.
- **The budget guard** (`crates/stella-core/src/budget.rs`) reads what the turn
  has cost, when the caller sets a mode that enforces.
- **The turn budget** (`EngineConfig::turn_budget`) reads how long the turn has
  run.
- **The turn halt** (`EngineConfig::turn_halt`) reads whether the goal is met.
- **The cancel token** and the soft stop are a person's hand on the switch.

One case is left for a count. A model makes new but useless calls. The budget
mode is `Off`. There is no deadline. Nobody is watching. The record says the
count did not catch that case either. `crates/stella-tools/src/read.rs` records
a turn that spent $7.83 and 18.8M input tokens paging through one file. It ran
off the end, wrapped, and started over. No loop verdict fired. The step cap
"had not been reached when the user killed it by hand". The fix was a new
detector rung and a ceiling inside the tool. Both read evidence. The cap did
nothing.

## Decision

`EngineConfig::max_steps` is `Option<usize>`. The default is `None`.

A turn ends when the model stops asking for tools. It ends when loop detection
or the stall rung says it is stuck. It ends when the budget or the deadline is
spent. It ends when the host's halt predicate says the goal is met. It ends
when a person stops it. It does not end on a count.

The mechanism stays for a host that means a count. A test must end. A served
turn's caller may ask for a bound. A research sub-agent is a short search by
design. Each sets `Some(n)`. Each gets the same `DeliberateStop` it always got,
with the same reason string. `SubAgentSpec::max_steps` is `Option<usize>` for
the same reason. A best-of-N candidate carries the parent's cap, which is none
by default. A research child keeps its sixteen.

`stella-serve` passes a caller's `max_steps` through as asked. Before this it
clamped the value to 10,000. The clamp kept a caller from removing "the last
bound" on a turn that holds an OS thread. With no cap by default, that clamp
would bound only the caller who asked for a bound. The caller who asked for
none would stay unbounded. A served turn is bounded by the reverse-request
deadline on every call, by the caller's budget, and by loop detection. That
was always true.

## Consequences

- A long run completes. The witness is `driver::tests::unbounded_by_default`.
  A thousand new steps under the default config end in `Completed`. A cap of
  200 refused that at step 200.
- A stuck turn still ends within a few calls, on the same rungs as before.
  Nothing here touches loop detection.
- Take a turn with budget mode `Off`, no deadline, and no halt predicate. Give
  it a model that keeps making new but useless calls. It now runs until a
  person stops it. That is the tradeoff. Anyone running with no human watching
  should set a budget or a deadline. Those bounds measure the thing they care
  about. A new stuck shape gets a new detector rung, never a count.
- The `agent.turn.started` payload carries `max_steps: null` for a turn with no
  cap. It is a `serde_json::Value`, so no wire schema changes.
- A host that drives `run_step` itself reads `Engine::max_steps()` as an
  `Option<usize>`. It gates only when the value is `Some`. The `stella-engine`
  crate docs carry the loop.
