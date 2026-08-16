---
id: turn-loop-wrappers
title: "One loop, six doors, and wrappers are plugins"
status: proposed
---

# One loop, six doors, and wrappers are plugins

**Status:** proposed, 2026-08-16. Written from Mac's architecture review of the
"Stella Turn Loop — Step by Step" deck (`website/public/presentations/turn-loop/`,
landed in #3377).

Everything this document says about today's code was read out of the tree at
`main` `730f2286c`, not recalled. Where a claim comes from a file, the file is
named so you can go check it.

---

## 1. The short version

Stella has one step loop. Everything else is something wrapped around it.

Today one of those wrappers — the staged pipeline — is not just a caller of the
loop. It also reaches *into* the loop: it takes over the loop's own "I am done"
event, and it hands each turn a private event channel so it can filter what the
loop says. That is a two-way connection between the engine and one of its
callers, and two-way connections are where bugs live.

This document says what to do about that, in three moves:

1. **Turn the connection into a one-way one.** The engine always finishes its
   own turn and always says so. A wrapper that wants more work asks for another
   turn. It never edits or hides what the engine said.
2. **Give wrappers one shape, and move them out of the engine.** The staged
   pipeline and goal mode are the same idea written twice. Define that idea once
   as four points a wrapper can plug into. Then the pipeline is a plugin that
   uses those four points, and goal mode is a second one.
3. **Make a wrapper's steps a config file, not code.** A wrapper describes its
   stages in TOML. Every turn already writes a row to SQLite saying which door
   it came in by; add one column for which wrapper variant ran. Now comparing
   two pipeline designs is a `GROUP BY`, not a rebuild.

The end state, in one line: **one loop, six doors, and wrappers are plugins.**

---

## 2. Why this is worth doing

Three reasons, in order of how much they cost us today.

**It is where the flakiness is.** The pipeline swallows the engine's `Complete`
and emits its own (`crates/stella-pipeline/src/pipeline.rs`, the "Event
ownership" section of the module doc). That means a per-turn channel, a
forwarding loop, and a rule about which events get dropped. A dropped event, or
a revise round that closes a channel twice, produces a run that looks finished
and is not. Nothing in the engine can catch that, because the engine already did
its job correctly.

**It blocks the plugin platform.** #3246 and the closed-but-superseded #3245 both
want the pipeline to become a plugin so Stella does not depend on it. A plugin
cannot hold a private channel into the engine. So this coupling has to die
before the pipeline can leave, whatever else we decide.

**We are maintaining the same wrapper twice.** Route B (the pipeline) and Route D
(goal mode) both do: propose some work, run a turn, judge it from evidence,
decide whether to go again. `stella monitor` is Route D with the goal pinned. So
one idea, three surfaces. Every fix has to be made in each of them, and when
somebody forgets, the two drift.

---

## 3. Move one — the engine owns its ending

### What happens today

`Engine::run_turn` emits `Stage { Execute }`, a terminal `Stage { Complete }`,
and a `Complete`. That is right for one turn. A plan with a revise loop runs
several turns, so a `Complete` after the first one would falsely mean "done".

The pipeline's answer is to become the single authority for endings: it gives
each `run_turn` a private channel, forwards everything except the engine's
`Stage`/`Complete`, and emits its own `Complete` at the end.

### What should happen instead

The engine always finishes its turn and always emits `Complete`. That event
means exactly one thing and it never lies: **this turn is over**. It does not
mean "the work is over".

A wrapper that wants more work asks for another turn. The wrapper's own "the
whole job is over" signal is a *different* event with a different name, and both
events appear in the journal.

So:

- The engine gets no new knowledge of who is calling it. It does not know
  whether it is inside a wrapper.
- The wrapper stops filtering. It reads the engine's events, it does not edit
  them.
- Nobody has to hold a private channel, so nothing has to be forwarded, so
  nothing can be dropped in the forwarding.

### The thing this changes for readers

Anything reading the journal today sees one `Complete` per pipeline run. After
this, it sees one `Complete` per *turn* plus one wrapper-level ending. Consumers
have to be updated in the same change, and the signal-consumer ledger
(`crates/stella-protocol/src/event/consumers.rs`, epic #2701) is where the new
row is declared. This is not a free rename; it is the point of the change.

### How you would know it worked

A test that runs a two-round wrapper and asserts the journal holds two engine
`Complete` events and one wrapper ending, in that order, with no event
filtering anywhere in the wrapper's code path. It fails today, because today
there is exactly one `Complete` and the engine's two were swallowed.

---

## 4. Move two — one wrapper shape, and the pipeline is a plugin

### The four points

A wrapper is anything that wants to do work around a turn. It gets four places
to plug in, and nothing else:

| Point | When it runs | What it may do | What it may not do |
|---|---|---|---|
| `before_turn` | before the loop is asked for a turn | add context, pick a plan, narrow scope, choose a model role | run the loop itself |
| `after_turn` | once the turn's `Complete` lands | gather evidence — run a test, read a diff, author a witness | change the turn that just ran |
| `judge` | after `after_turn` | turn the evidence into a verdict | call a model |
| `again?` | after `judge` | say "another turn, here is the correction" or "stop, here is the outcome" | fake an ending the engine did not emit |

`judge` calls no model on purpose. That is already the rule in the pipeline
(`ladder_decision` in `crates/stella-pipeline/src/verify.rs` is terminal at all
five outcomes, and #2584 removed the model verdict structurally). The wrapper
contract keeps that rule instead of re-arguing it.

`before_turn` is where the steering plane (#3243) is asked its one question —
"what could help here?" — for a wrapped run. It is not a fifth plane.

### What each of today's wrappers becomes

- **The staged pipeline** is the first plugin. `triage`, `recall`, `research`,
  `plan`, `scope` are `before_turn`. `witness` is `after_turn`. `verify` is
  `judge`. `revise` is `again?`.
- **Goal mode** is the second, or the same one with a different `judge`. Its
  rounds are `again?`; its independent verifier is `judge`.
- **`stella monitor`** is goal mode with a pinned goal. It was never a separate
  route and it does not become one.

### What core keeps

One loop. One wrapper socket. Zero built-in wrappers.

The socket is exposed through the hook surface that already exists, which
already has the right words — `allow`, `modify`, `deny`, `require_approval`
(`crates/stella-core/src/bus.rs`, and the out-of-process hooks from #2684).
Wrappers are not a new plane; they are the existing plane given a turn-shaped
vocabulary.

### The bug class this deletes

Slide 22 of the deck names it: `Engine::with_sleeper` cannot carry `gate`,
`steering` or `hooks`, because those are private builder fields. So a
hand-rolled child engine silently drops all three, and goal mode shipped with
exactly that bug. If a plugin can only get an engine through one blessed
constructor, that whole class stops being possible. #3274 is the epic that makes
lane assembly a single place; this is the reason it has to cover plugin lanes
and not just the seven builtin ones.

---

## 5. Move three — stages are a manifest, and variants are measurable

### The manifest

A wrapper plugin describes its stages in TOML: an ordered list, each with a
condition. Something like:

```toml
[wrapper]
id = "staged-v1"

[[wrapper.stages]]
name = "triage"

[[wrapper.stages]]
name = "plan"
if   = "questions > 0"

[[wrapper.stages]]
name = "execute"

[[wrapper.stages]]
name = "witness"
if   = "no-test-command"

[[wrapper.stages]]
name = "verify"
```

The conditional skipping the deck already describes — "a cheap task should not
pay for ceremony it cannot use" — stops being branches buried in
`pipeline.rs` and becomes a line you can read and change. Each stage is its own
unit with a typed input and a typed output, so a stage can be tested on its own.

Trying a different pipeline design becomes editing a file. It does not become
editing a 2700-line Rust file that is already on the god-file list
(`scripts/file-size-baseline.txt` names `crates/stella-pipeline/src/pipeline.rs`,
and that file is closed to growth).

Two rules carried over from the manifest work already done in #3245 slice A
(`crates/stella-plugin/src/manifest.rs`): an unknown key is a load error, never
ignored; and a condition naming a signal the host does not publish is a load
error too. A manifest that quietly does nothing is worse than one that refuses
to load.

### The scoreboard you already paid for

Every turn writes an execution row tagged with the door it came in by — `run`,
`chat`, `deck`, `deck-sub`, `goal`, `pipeline`. Add one column: the wrapper
variant id from the manifest.

That is the whole A/B setup. Cost, outcome, and verified-versus-unverified rate
per variant is one `GROUP BY variant` away, against the SQLite database that
already exists at `.stella/private/store.db`. #2889 wants a full experiment
plane with first-class hypothesis and dataset objects; this column is the cheap
first cut of the same question, and it should land first so the plane has real
rows to model.

Two honesty rules, because this is a measurement surface and CLAUDE.md's bench
rules apply to it:

- A variant id is only written when the manifest was actually the thing that
  ran. A default or fallback path writes the default's id, never a blank.
- The column measures the wrapper, not the task. Two variants compared on
  different task sets are confounded and the query must not hide that.

### Flip the default

Today the pipeline is the default and `--no-pipeline` opts out. That is
backwards once wrappers are plugins: the raw loop is the ground truth and a
wrapper is the extra thing you asked for.

So: every door hits the raw loop by default, and `--pipeline <variant>` opts in.
`--no-pipeline` stays as a deprecated alias that does nothing, so no script
breaks the day it lands.

This is also a signal check. If inverting the flag feels wrong, one of the other
two moves is not finished — because a raw loop that is not safe to be the
default is a raw loop the wrappers are propping up.

---

## 6. Order of work

The moves depend on each other in one direction only.

1. **Move one first.** It is the precondition for everything: a plugin cannot
   hold a private channel, so the coupling dies whether or not the rest lands.
   It is also the smallest, and it is worth doing on its own even if the
   pipeline never leaves the workspace.
2. **`TurnLane` stays open from its first commit** (#3274 slice 1). That is the
   one decision in this whole plan that is expensive to defer — retrofitting an
   open lane enum after a parity matrix is written against a closed one is a
   matrix rewrite.
3. **Move two** — the four points, then the pipeline ported onto them, then goal
   mode.
4. **Move three** — the manifest, then the variant column, then the flag
   inversion. The flag goes last, because it is the claim that the first three
   worked.

---

## 7. What this does not change

Worth stating, so nobody reads this as bigger than it is.

- The twelve phases of a step. Untouched.
- The safe-boundary rule: turns end between steps, never mid-tool.
- Verification buys no model call. The one surviving verifier-tier call is the
  witness author, and it survives for the same reason as before — it builds the
  measuring stick instead of replacing it.
- The witness protocol itself: a different model writes the test, blind to the
  change, with three tools, and tampering voids the credit.
- Ports, not concretions. `stella-core` still imports no provider SDK, no
  filesystem API, and now also never learns that plugins exist.

---

## 8. Related issues

| Issue | Relationship |
|---|---|
| #3274 | Lane assembly epic. Move two's socket is a lane; slice 0 (#3280) and slice 1 stand as written. |
| #3246 | Plugin platform sequencing. This document names the specific wrapper shape that platform must carry. |
| #3243 | One steering plane. `before_turn` is where a wrapped run asks its question. |
| #2701 | Signal-consumer ledger. Move one adds and re-homes an ending signal; the ledger is where that is declared. |
| #2694 | Tool-first epic. Its "stages become tools" line and move two's "stages become manifest entries" are two different destinations; §4 of this document is the boundary. |
| #2889 | Experiment plane. Move three's variant column is the cheap first cut. |
| #2773, #2815 | Verification-ladder and candidate-workspace work. Both keep their outcomes; both land behind the wrapper contract rather than by growing `pipeline.rs`. |
