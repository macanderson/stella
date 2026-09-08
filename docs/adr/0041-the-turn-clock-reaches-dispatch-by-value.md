---
id: adr/0041-the-turn-clock-reaches-dispatch-by-value
title: "ADR 0041: The turn clock reaches dispatch by value"
status: implemented
---

# ADR 0041: The turn clock reaches dispatch by value

- Status: accepted
- Date: 2026-09-08
- Decides: `#6460`
- Not part of the Phase 0 series.

## Context

A `bash` call carries its own time limit. The model writes it. `stella-tools`
cuts it to ten minutes at most, and the spawn arms it. Nothing ever weighed
that number against the clock the turn had left.

So a ten-minute call could start at t=350s of an 840-second budget and run to
t=950s. Harbor kills the trial at 900s. The process dies. Every edit the turn
made dies with it. The engine's own deadline never fires, because it is asked
between steps and the step never ends. On the 89-task Terminal-Bench 2.1 run
of 2026-09-07, 17 of 32 failures ended that way.

The engine already turns down work it cannot finish one step up.
`ContinuationBudget::affords_another` refuses to start a continuation that
costs more than the clock has left. `settlement::check_budget` refuses to open
a step it cannot pay for. Neither reaches a tool call, and AGENTS.md #6 means
nothing can: once the call runs, the engine may not stop it. The one moment
left is before it starts.

The clock and the named limit have to meet at that moment. Each sits on the
wrong side of a line.

The **clock** must be read in the driver, because AGENTS.md #2 keeps I/O out of
the engine's decisions. The dispatch site could not see it.
`Engine::execute_tool_calls` is called from `dispatch_completion`, which held
no turn state at all.

The **named limit** sits in the call's arguments, under a key only the tool
layer knows. `bash` reads `timeout_secs`. Nothing else in the tree means
anything by that name.

## Decision

**The clock is read once each step and passed down by value.**
`driver::turn_clock::TurnClock` is built in `run_step_inner` from the turn's
two bounds: `EngineConfig::turn_budget`, measured from the start of the turn,
and `BudgetGuard::task_deadline`. It keeps the earlier. It travels through
`dispatch_completion` into `execute_tool_calls`, which reads it again against
`Instant::now()` for each barrier group, because the groups before it have
spent clock.

That takes the place of the `Option<ContinuationBudget>` parameter already
threaded down the same path. One clock, read in one place. Both readers below
weigh the same value.

**The tool names its own limit, through the port.**
`ToolExecutor::declared_timeout(name, input)` is a defaulted method. It answers
`Option<Duration>`: how long this call may run, as this tool set would hold it.
`ToolRegistry` answers for `bash` and nothing else. Every decorator passes it
on. The default `None` means "cannot say", and the engine then starts the call
as it always has.

**A call it cannot finish is refused before it starts.** The limit must be
under the clock left, less one model call. That much is held back so the
refusal can reach the model and the model can answer. The refusal is a
`RefusedByPolicy` `ToolOutput` naming the seconds left, sent with no
`ToolStart`. It is the shape `close_open_tool_calls` and `HALTED_TOOL_RESULT`
already use. The turn goes on from it.

## Consequences

The turn stops racing a harness it cannot see. A model that asks for ten
minutes with four left is told so in seconds. It asks again for something that
fits, or writes down what it has.

`turn_budget` and `task_deadline` now bound the length continuation together,
rather than the first of them alone. Both are armed from `--turn-timeout` in
the shipped binary, so nothing there changes. A caller that arms only the
enforced deadline now gets the decline it always should have had.

`bash` is the only tool this can refuse today. It is the only built-in that
takes a time limit from the model. A tool that grows one writes
`declared_timeout` in the same change, or the clamp does not see it. That is
the safe way to be wrong, and the way `tool_origin`'s default is wrong too.

A refusal names a number, so two refusals in one turn do not match byte for
byte, and the loop rungs cannot read them as a run. That is fine here and
would not be for `HALTED_TOOL_RESULT`. The number is the fix, so a model that
reads it stops repeating. A turn in this state is one model call from its
deadline in any case.

## Alternatives considered

**Widen `ToolExecutor::execute` to carry a context.** The general answer, and
the one this refuses. Every carrier of the trait in the tree changes, plus
every test double and every outside one. The trait is public API, and
`stella-serve` and `stella-engine` hosts write their own. A defaulted method
costs nothing to anyone who does not want it. What is added is one question,
not a bag that will grow.

**Store the clock on `Engine`.** That struct holds borrowed ports and is shared
across the tool futures. The field would need a cell: written at the top of
`run_step_inner` and read four frames away, with nothing in either place
showing the other. Passing it by value makes the tie a signature, which is
what the borrow checker is for.

**Let the engine read `timeout_secs` out of the call's input.** One line, no
port change, and it writes one tool's key into the engine. That is the thing
AGENTS.md #1 exists to stop. It also lies the moment a second tool means
something else by that key. The engine would report a limit nothing holds, and
turn down a call that was never going to take that long.

**Cut the timeout down to fit, inside `stella-tools`.** Nothing about the wall
clock is visible there. Cutting is the wrong fix in any case. A ten-minute
test suite cut to four minutes fails at four minutes, having spent the four.
The model has to choose between smaller work and finishing, and only the model
can make that choice.

**Hold back a fixed margin.** Every neighbour on this path refuses to make one
up. The caller owns the deadline and can set a safe one. A margin here would
be a second rule the operator never saw. What is held back is the last model
call, measured, which is what one more round trip costs.
