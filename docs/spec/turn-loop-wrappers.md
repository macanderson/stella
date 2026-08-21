---
id: turn-loop-wrappers
title: "One loop, six doors, and wrappers are plugins"
status: living
---

# One loop, six doors, and wrappers are plugins

**Status:** living, updated 2026-08-18. Written 2026-08-16 from Mac's
architecture review of the "Stella Turn Loop — Step by Step" deck
(`website/public/presentations/turn-loop/`, landed in #3377); §9 was added the
same week to resolve four places the vision below hit something already
decided in this repository, and both are now overtaken in part by what has
actually shipped. **Move one (§3, the engine owns its ending) is landed.** Of
move two (§4): the socket itself — the `TurnWrapper` trait, both transports,
`judge`/`again`, and the wire contract — landed in `stella-runtime` and
`stella-plugin` (#3479, `doc:wrapper-socket`, `doc:pipeline-as-plugins` Track
A), and the host sequence that drives a live turn through it has landed too
(#3494, `stella_runtime::WrapperDispatch`, driven by `stella run --pipeline
<variant>`) — but neither the staged pipeline nor `goal.rs` has moved *onto*
this socket (§9.1's subtraction from `stella-core` has not happened; the
staged pipeline instead grew its own, separate manifest-driven dispatch, §5
below). Move three (§5) landed its manifest half — `[wrapper]` stages, the
closed condition grammar, the `pipeline_variant` column — **and its flag
inversion has now shipped too** (§5 "Flip the default", #3381, PR #3694): the
raw loop is the default on every door and `--pipeline <variant>` is the sole
opt-in. `doc:pipeline-as-plugins` is the completion plan and the current
source of truth for exactly what has landed; this document is the vision it
completes and the place §9's architecture-review corrections live.

**Status, 2026-08-19: the built-in path this document describes is removed.**
`crates/stella-pipeline` — the staged pipeline itself — has been deleted from
the workspace (#3865, `doc:pipeline-as-plugins` §7's "last slice", landed on
this branch). `stella run --pipeline classic` is refused outright; a
verification-carrying wrapper is now only ever an installed plugin, never a
crate this repository ships. This document's file:line citations into
`crates/stella-pipeline/` below are read out of the tree as it stood *before*
that deletion — kept because they are the clearest surviving record of the
mechanism §8 of `doc:pipeline-as-plugins` asks Vera (Oxagen's private
reference verification plugin) to port, not because the paths still resolve.

Everything this document says about today's code was read out of the tree at
`main` `730f2286c` unless marked otherwise by a dated update note; a few
claims below have been corrected against the tree at a later commit and are
labelled with the date they were re-verified. Where a claim comes from a
file, the file is named so you can go check it.

---

## 1. The short version

Stella has one step loop. Everything else is something wrapped around it.

When this was written, one of those wrappers — the staged pipeline — was not
just a caller of the loop. It also reached *into* the loop: it took over the
loop's own "I am done" event, and it handed each turn a private event channel so
it could filter what the loop said. That is a two-way connection between the
engine and one of its callers, and two-way connections are where bugs live.

That particular wrapper is gone (#3865, see the status note above), but the
problem it exemplified is not: **the diagnosis is about the shape, not about
that crate.** Any future wrapper that reaches into the loop instead of sitting
around it re-creates it, which is exactly why the socket that replaced it makes
the engine own its own ending (move one, landed) and gives a wrapper four
declared points rather than a channel it can filter.

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

`judge` calls no model on purpose. That was already the rule in the pipeline:
`ladder_decision` in `crates/stella-pipeline/src/verify.rs` was terminal at every
arm of `LadderDecision` — that enum was the enumeration, and this document
deliberately does not restate it or its count, because a number copied into a
second file drifts (#3473) — and #2584 removed the model verdict structurally.
The wrapper contract **keeps that rule** instead of re-arguing it, and since the
crate that used to demonstrate it is deleted (#3865), the socket is now the only
place the rule is enforced: `judge` is a synchronous, I/O-free, total function,
so a plugin cannot buy a model call to grade its own work even if it wanted to.

The arm a plugin author most needs from that enum is `WitnessUnsatisfiable`: the
witness was authored and its red does not discriminate, so `judge` must be able
to say "the instrument is broken", not only "the work failed".

`before_turn` is where the steering plane (#3243) is asked its one question —
"what could help here?" — for a wrapped run. It is not a fifth plane.

### What each of the wrappers becomes

- **The staged pipeline** was to be the first plugin, with `triage`, `recall`,
  `research`, `plan`, `scope` as `before_turn`, `witness` as `after_turn`,
  `verify` as `judge` and `revise` as `again?`. It was **deleted rather than
  ported** (#3865), so this mapping is now the porting instruction handed to a
  verification plugin (`doc:pipeline-as-plugins` §8) rather than a description
  of a migration in flight. The mapping itself is unchanged and is the useful
  part: it is how a stage graph lands on four points.
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

When this was written the pipeline was the default and `--no-pipeline` opted
out. That is backwards once wrappers are plugins: the raw loop is the ground
truth and a wrapper is the extra thing you asked for.

So: every door hits the raw loop by default, and `--pipeline <variant>` opts in.
`--no-pipeline` stays as a deprecated alias that does nothing, so no script
breaks the day it lands.

This is also a signal check. If inverting the flag feels wrong, one of the other
two moves is not finished — because a raw loop that is not safe to be the
default is a raw loop the wrappers are propping up.

**Status, 2026-08-18: landed exactly as written (#3381, PR #3694).** Every
door — `run`, `arena`, `goal`, `fleet`, `deck` — hits the raw step-loop by
default; `--pipeline <variant>` is the sole opt-in (`classic` names the
built-in staged pipeline, any other name resolves an installed plugin); and
`--no-pipeline` is a deprecated no-op kept parseable so no script breaks,
implemented as `PipelineChoice::resolve` in
`crates/stella-cli/src/wrapper_plugin.rs`. It shipped as the signal check
this section calls it: per the maintainer's explicit call (§9.6 below), the
flip went out ungated by a side-by-side bench, on the grounds that both paths
still coexist and the flip is one flag away from reversal — not because the
other two moves were judged finished by a benchmark.

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
- Ports, not direct dependencies. `stella-core` still imports no provider SDK, no
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

---

## 9. Design resolution

§1–§8 are the vision and stand as written. This section resolves the four
places where turning them into code hits something already decided in this
repository, and answers §5's open question about the door tag. Written against
`main` at `730f2286c` (the same commit §2 was verified at); every "today" claim
below was read out of the tree, and the two inferences are labelled.

### 9.1 The socket cannot live in `stella-core`

§4 says core keeps "one loop, one wrapper socket, zero built-in wrappers", and
that the socket is the existing hook surface. The first and third clauses hold.
The middle one needs a location, and it cannot be `stella-core`:

`before_turn` does recall and research; `after_turn` runs a test command or an
oracle process. That is I/O, and invariant #2 forbids I/O in the engine. A
socket defined in core is either a trait core never calls — which is fine but
is not a socket — or a trait core awaits, which puts the process spawn inside
the engine.

So the trait is defined **above** core, in `stella-runtime`, which already owns
engine assembly and reads no ambient environment by contract
(`crates/stella-runtime/tests/no_ambient_reads.rs`). Core's share of move two is
strictly **subtractive**:

- delete the private-channel affordance move one already removes;
- delete goal mode's round loop from `crates/stella-core/src/goal.rs`, which is
  a route-specific supervisor sitting inside the engine crate today.

That second deletion is what actually delivers "zero built-in wrappers". It
also means move two makes `stella-core` smaller, which is a better claim than
the one §4 makes and is worth stating in those terms.

The hook surface stays exactly what §4 says it is — the *dispatch* mechanism,
with its existing `allow`/`modify`/`deny`/`require_approval` vocabulary. What
moves out of core is the definition of the wrapper contract and the code that
sequences it.

### 9.2 `judge` may not call a model — and goal mode's judge is a model call

This is the one internal contradiction in §4, and it is load-bearing enough to
resolve rather than paper over.

The table at §4 says `judge` "may not call a model". Four lines later, "Goal
mode is the second [plugin] … its independent verifier is `judge`". But goal
mode's independent verifier **is** a model call, once per round, assessing the
transcript against the goal — that is what
`crates/stella-core/src/goal.rs` does today and the only thing it could do. For
an open-ended goal with no test surface there is no measurement to take.

Both halves are right; they are describing different stages. The resolution:

> **The model call belongs to `after_turn`, never to `judge`.**

A goal-mode wrapper spends its verifier call in `after_turn` and returns the
parsed assessment *as evidence*. `judge` then maps evidence to verdict
deterministically, and is a synchronous, I/O-free function over owned data —
which is what makes "judge calls no model" a property of the signature instead
of a rule someone has to remember. The pipeline's `ladder_decision` is already
written that way, so porting it is a re-home, not a rewrite.

What this preserves, exactly: "verification buys no model call" holds where
#2584 won it — the `classic` variant's only verifier-tier spend is the witness
author, and its `judge` reads the flip. What it stops pretending: that goal mode
already satisfies that rule. It does not, it never did, and the spend is now
visible on the receipt against a declared role instead of being described as a
`judge`.

### 9.3 The child-engine constructor is the whole security story

§4's "bug class this deletes" is right that one blessed constructor kills it.
Two things make that enforceable rather than aspirational:

- **A wrapper is handed a `ChildTurn` port, not a provider, not an `Engine`, and
  not a credential.** It names a *role intent* (`triage`, `planner`,
  `witness_author`); the host resolves the intent against the user's BYOK
  providers, carves the budget, attaches gate/steering/hooks, runs the turn, and
  settles once. For an out-of-process wrapper this is a JSON request on stdio
  and every model call is made by the host — invariant #3 and #3245 §3, intact.
- **This was gated on #3274 slice 2 — it has since landed, with a correction
  worth carrying forward.** `TurnCapabilities` (#3387) now exists in
  `crates/stella-core/src/driver/capabilities.rs`, and
  `crates/stella-core/src/subagent.rs`'s child fork is built through
  `Engine::assemble`, which takes one. But read `TurnCapabilities`'s own module
  doc before citing it the way this section originally did: "it was tempting to
  state this as 'the constructor *cannot* carry those seams' … That is not
  true of this tree" — `with_gate`/`with_steering`/`with_hooks` are `pub` and
  directly callable today, so the fix is not that the old path became
  impossible. What `TurnCapabilities` actually enforces is *totality*: no
  `Default`, so a struct-literal assembly site must answer every seam by name,
  and forgetting one is a compile error rather than a silently unset builder
  call. This is real progress against the bug class, and it is *not yet* wired
  to the wrapper socket — `stella-runtime`'s `TurnWrapper` has no child-turn
  port at all today (§9.1's `ChildTurn` is still design, not code; see
  `doc:wrapper-socket` and `doc:pipeline-as-plugins` §4 A3's landed note), so
  whatever eventually implements one earns `TurnCapabilities`'s guarantee only
  by calling `Engine::assemble` itself.
- **Landed (#3564), and the last clause above is exactly how.** The port is the
  `child_turn` host call, not a new `TurnWrapper` method:
  `stella_runtime::wrapper::ChildTurns` resolves the declared role intent to a
  `ModelCallRole` seat, clamps the count against the host's own ceiling, and
  spends through the host's `SubAgentDispatcher` — which builds its child
  through `Engine::assemble`, so the guarantee is inherited rather than
  re-argued. Two clauses of this section are now enforced rather than described:
  a plugin holds no provider and no credential because `ChildTurnArgs` has no
  field that could carry one, and a role intent resolving to the **worker's**
  seat is refused outright (`HostCallRefusal::Forbidden`), so a plugin cannot
  nominate the model it is judging. What remains a host's own claim, as this
  section always said: that its dispatcher attaches gate, steering and hooks —
  `SubAgentDispatcher`'s contract requires it, and nothing here can check it.

### 9.4 The manifest needs two properties the sketch does not yet have

**Landed (#3380/#3408), re-verified 2026-08-18.** Both additions below exist
in `crates/stella-plugin/src/{manifest,wrapper,program}.rs`: the stage graph is
load-checked on both axes (a condition reading a signal only a later stage
publishes, or one only a conditionally-run earlier stage publishes, is a load
error — `stella-plugin`'s README §"the stage graph is load-checked" states
the rule this subsection asked for), and `if` is the closed grammar this
subsection specified — `[no-]<boolean-signal>` or `<count-signal> <op>
<number>` over a published `Signal`, never an expression language. The
heading is kept as written, describing the gap at the commit this document
was verified against; §5's TOML sketch is the right shape, and the two
additions below are what closed it:

- **Typed stage input/output, checked as a graph at load.** A stage whose input
  no prior stage produces is a load error. §5 already says each stage has a
  typed input and output; making the *graph* load-checked is what turns that
  from documentation into a gate.
- **`if` is a closed predicate grammar, not an expression language.** §5's
  `"questions > 0"` and `"no-test-command"` are two different languages in one
  field — one an arithmetic comparison over a stage output, one a host fact. A
  Turing-complete condition in a manifest is a second program with no gate on
  it. The grammar should be a small closed set of named predicates over typed
  stage outputs and host facts, evaluated by a pure function, and validated at
  load against the stage graph. §5's existing rule — a condition naming a signal
  the host does not publish is a load error — is exactly this rule; it just
  needs the grammar to be closed for it to be checkable.

Both fall out of the format `crates/stella-plugin/src/manifest.rs` already
enforces, so the variant block extends that manifest rather than introducing a
second one. `participation` stays derived from declared capability, never
authored twice (`doc:turn-lane-assembly` §9.3).

### 9.5 The door tag exists — and it already encodes the wrapper

**Landed (#3388), re-verified 2026-08-18.** The precondition this subsection
asks for is done: `crates/stella-store/src/ddl.rs`'s own module doc now states
the rule this section derived — "`kind` is the door… nothing else" — as the
schema's contract, and neither write site below still exists.
`crates/stella-cli/src/agent.rs:282` calls
`persistence::begin_pipeline_execution`, which opens a `TurnDoor::new("run")`
`.wrapped_by(PIPELINE_VARIANT_CLASSIC)` — door `"run"`, variant `"classic"` —
and `crates/stella-cli/src/command_deck.rs:1520` always writes door `"deck"`,
passing `pipeline_on.then_some(PIPELINE_VARIANT_CLASSIC)` as the variant.
Neither site writes `"pipeline"` or `"deck-pipeline"` any more. The rest of
this subsection is kept for the record — it is the derivation that produced
the fix, and its backfill note (point 3 below) still describes how to read a
row written before #3388.

*Original text, describing the state at `730f2286c`, before #3388:*

§5 says every execution row is "tagged with the door it came in by". That is
true: `executions.kind` (`crates/stella-store/src/ddl.rs:79`) carries `run`,
`deck`, `deck-sub`, `goal`, `fleet`, and `pipeline`.

But two of those values are not doors:

```
crates/stella-cli/src/agent.rs:281          begin_execution(&store, "pipeline", …)
crates/stella-cli/src/command_deck.rs:1514  if pipeline_on { "deck-pipeline" } else { "deck" }
```

`pipeline` and `deck-pipeline` name **the wrapper**, in the column that is
supposed to name the door. A deck turn writes a different door depending on
whether a wrapper ran.

This matters for move three specifically, because it is the measurement surface:
adding `pipeline_variant` beside a `kind` that already encodes the wrapper gives
two columns answering one question, disagreeing whenever the mapping is
imperfect — and `GROUP BY` over either one silently double-counts the deck. The
column addition therefore has a precondition:

1. `kind` becomes the door and only the door — `pipeline` → `run`,
   `deck-pipeline` → `deck`;
2. `pipeline_variant` becomes the sole home for which wrapper ran, NULL for an
   unwrapped turn;
3. the backfill is stated rather than assumed: historical `pipeline` rows are
   `kind='run', pipeline_variant='classic'`, and any query comparing across the
   migration boundary must say so, per §5's own confounding rule.

(Inference, labelled: I read the six values at their write sites and did not
census live values in a `store.db`, so a legacy value not written by current
code would not appear above.)

The sample query move three exists to serve, after the disentangling:

```sql
SELECT kind AS door,
       COALESCE(pipeline_variant, 'none') AS variant,
       COUNT(*)                           AS runs,
       AVG(cost_usd)                      AS avg_cost
FROM executions
WHERE finished_at IS NOT NULL
GROUP BY door, variant;
```

### 9.6 Two gates, and one claim not made

**The flag inversion is a measured change, not a refactor.** §5 frames flipping
the default as a signal check, and as a signal check it is a good one. It is
also the change that reaches users: every Stella benchmark number this project
has published was produced with the pipeline as the default. #3245 Test 2 and
`doc:turn-lane-assembly` §10.4 already gate the *extraction* on a side-by-side
bench on the same panel; the same gate belongs on the *default flip*, one step
earlier, and the result gets reported even when the raw loop is worse. Whether
to spend that bench, or to flip ungated and accept the unknown, is the
maintainer's call — recorded here rather than decided.

**Decided, 2026-08-18: flipped ungated.** The flag inversion (§5, #3381, PR
#3694) shipped without the side-by-side bench this paragraph asked for,
per the maintainer's explicit direction rather than an oversight — both paths
still coexist behind `--pipeline classic`, so the flip is one flag away from
reversal, and the maintainer judged that reversibility sufficient to ship
ahead of a bench result. No bench number has been published against this
flip as of this update; the gap this paragraph named is still open, and
reporting it — flattering or not — remains CLAUDE.md's bench-honesty rule
whenever that run happens.

**The dependency cut stays gated on #2716, and it has not moved.** Cutting
`stella-cli`'s `stella-pipeline` dependency needs the authority vocabulary for
the `granted` half of a plugin lane, per `doc:turn-lane-assembly` §9.4 and
§10.4. Moves one through three all landed without it, exactly as predicted —
the flag inversion (§5) shipped and the dependency count grew rather than
shrank in the same work (174 references across 44 files as of 2026-08-18,
`grep -rn stella_pipeline:: crates/stella-cli/src crates/stella-cli/tests`,
up from 169/41 when this paragraph was written), because the schedule-manifest
wiring (`doc:pipeline-as-plugins` §7, #3408/#3672) and the wrapper-plugin
driver (#3494) both added call sites into `stella-pipeline` rather than
removing any.

**Update, 2026-08-19: the dependency cut landed (#3865), and with it the
"both paths still coexist" reversibility this section relied on is gone.**
The reference count did not shrink gradually — it went from growing (above)
to zero in the removal itself: `crates/stella-pipeline` is deleted, `stella
run --pipeline classic` is refused, and there is no flag that restores the
built-in path. The authority-vocabulary precondition this paragraph named
(#2716) was not what unblocked the cut in the end; see this branch's own
removal notes and #3865 for what the deletion actually depended on. Reversal
now means restoring the crate from git history and reintroducing it to the
workspace, not flipping a flag.

**One claim this document should not make.** "The diff deletes more
loop-orchestration code than it adds" is a plausible outcome and not a property
assertable in advance. Move one and the `goal.rs` deletion are strongly
net-negative; move three adds a manifest loader, a predicate grammar, and a
stage-graph validator that do not exist today. It is a number to measure and
report when the work lands, not a design property to promise now.
