---
id: adr/0026-context-editing-ships-off-until-measured
title: "ADR 0026: Context editing ships off until its trigger is measured"
status: implemented
---

# ADR 0026: Context editing ships off until its trigger is measured

- Status: accepted
- Date: 2026-09-03
- Decides: `#3756`

## Context

The Anthropic Messages API can drop old tool results from a chat. It can drop
old thinking blocks too. Stella can ask for that. The wire shape lives in
`crates/stella-model/src/anthropic/context_edit.rs`. A mock server proves it.

One number turns the feature on. It is the trigger. Below it, nothing is
cleared. Above it, each edit throws away the cached prefix from the point of
the edit.

That number has never been measured. A cache read bills at $0.30/MTok. A cache
write bills at $3.75/MTok. Stella runs near a 92% cache-hit rate. Set the
trigger too low and long turns cost more, with no sign a user would see. Set
it too high and nothing ever fires. Both ways the bill moves more than
tenfold.

Two counts on `#3756` bound the guess. Across 269,134 real model calls, only
0.47% went past the vendor default of 100,000 input tokens. And Stella already
trims old tool results itself, in compaction pass 0. That pass fires on a
ratio, not a token count. Turn the server-side feature on and both would trim
the same blocks. Each would spoil the prefix the other tries to keep.

So the choice is not on or off. It is measure, or say out loud that we have
not.

## Decision

Ship it off. Say so as a value, not as a gap.

`SHIPPED_TRIGGER_TOKENS` in `context_edit.rs` is `None`. A fresh
`AnthropicProvider` reads it. So no session sends `context_management`. A test
pins the constant. Its message names this record and the panel issue. To set a
number you must edit that test. That makes a guessed trigger a change a
reviewer sees.

`with_context_editing` stays. It is how one session opts in once its own shape
is known. It is also how the panel will run its arms.

There is no setting for this. The Anthropic page in the docs says so. A knob
added now would only move the guess to a user.

## Consequences

The bill does not change. That is the point. A feature that sends nothing
cannot make a session cost more.

The measurement is still owed. It is a rig launch, not a code change: four
arms over the same tasks, at triggers of 30k, 60k, 100k and off, on the direct
`anthropic` route. Compare billed cost and cached tokens. It is filed as
`#5796`. That issue carries the pass 0 question too.

Gateway routes are out of reach either way. `factory::build_provider` sends
every OpenAI-compatible provider to the shared chat-completions adapter.
OpenRouter is one of them. That adapter has no such field. So a bench run
through a gateway measures this feature as absent, whatever the gateway would
have done.

The panel may find no trigger that pays. Then this record is amended with the
numbers and the constant stays `None`. That is an answer, not a failure.
