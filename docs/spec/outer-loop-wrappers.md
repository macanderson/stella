---
id: outer-loop-wrappers
title: "One loop, one wrapper socket — the outer-loop interface and manifest-declared pipeline variants"
status: proposed
---

# One loop, one wrapper socket

**Status:** design for review. No code written. Deliverable 1 of the "refactor:
one loop, wrappers as plugins" directive (Mac, 2026-08-16).

**Companion docs:** `doc:turn-lane-assembly` (the lane seam this rides on —
subordinate to it wherever they touch), `doc:witness-protocol`,
`doc:verification-gate`. **Governing issues:** #3274 (one loop, seven lanes),
#3246 (the plugin authority plane), #3245 (CLOSED — plugins as turn-loop
participants; slice A shipped as `crates/stella-plugin`).

**Evidence basis.** `main` at `730f2286c` (0.9.57). Every claim about the tree
below was read this session at that commit and is cited by file and line; the
three inferences are labelled as such.

---

## 0. What this document decides, and what it inherits

The directive asks for four things: one generic outer-loop wrapper interface;
the raw turn loop as the default at every door; the staged pipeline ejected
into a plugin; and pipeline variants as declarative manifests.

Three of the four already have a design in this repository, and this document
**imports rather than restates** them:

| Piece | Where it is already decided | This doc's job |
|---|---|---|
| How a plugin declares a say in the turn loop | #3245 `[loop]` block, shipped and validated in `crates/stella-plugin/src/manifest.rs` | Add `[pipeline]` stages to the same manifest; change nothing about the ladder |
| How a non-builtin supervisor gets an engine with the right capabilities | `doc:turn-lane-assembly` §9 (plugin-owned lanes, two-level totality, `granted = requested ∩ authorized`) | Bind the wrapper socket to a lane; add no second vocabulary |
| When the pipeline may stop being linked into `stella-cli` | `doc:turn-lane-assembly` §10.4 + #3245 Test 2 — gated on a side-by-side bench, not a review | Respect the gate; sequence around it |

What is genuinely new here is **one interface, and the event-contract change
that makes it honest**: the four interception points, what crosses each
boundary, and the manifest schema for a variant.

Two places where the directive's plan collides with a standing invariant are
called out in §7 rather than silently resolved. They are the two things this
review most needs an answer on.

---

## 1. The problem, stated from the tree

Stella has exactly one turn loop — `crates/stella-core/src/driver/drive.rs:111`
is the only `loop {` over `Engine::run_step` in the workspace (#3274, re-read
this session). What is duplicated is the **supervisor**: the thing that decides
whether to run another turn and what to put in front of it.

There are two hand-rolled supervisors:

- **The staged pipeline** — `crates/stella-pipeline/src/pipeline.rs`, entry at
  `pipeline.rs:1021`. Stages triage → recall → research → plan → scope →
  execute → witness → verify → verdict, with revise back-edges onto execute
  (`stage_rank`, `crates/stella-pipeline/src/replay.rs`).
- **Goal mode's round loop** — `crates/stella-core/src/goal.rs`. One
  `Engine::run_turn` per round, then a verifier call, then `met`/`not met` with
  feedback threaded into the next round's first user message.

They are the same shape: *propose work → run a turn → judge from evidence →
continue or stop.* Goal mode's own module doc describes its rounds in exactly
the vocabulary the pipeline uses for its revise loop.

And they compose by embedding: `crates/stella-cli/src/agent/goal.rs:649`
(`run_goal_pipeline_turn`) runs a whole `Pipeline::run` inside each goal round,
so the default `stella goal` path is one supervisor nested in another, each
with its own budget threading, its own verifier resolution, and its own
terminal-event story.

### 1.1 The one thing that reaches into the loop

Everything else about the pipeline sits cleanly on top of `run_turn`. One thing
does not, and it is stated plainly in the pipeline's own module doc
(`crates/stella-pipeline/src/pipeline.rs:16-27`):

> The pipeline is the **single authority** for stage boundaries and the
> terminal event on an outcome-producing run: it gives each `run_turn` a
> private channel, then forwards every event to the consumer *except* the
> engine's `Stage`/`Complete` (which would otherwise falsely signal "done"
> after step one).

So: a private per-turn channel, a forwarding filter, and a suppressed engine
`Complete` replaced by a pipeline-authored one (`pipeline.rs:1467-1474`).

The reasoning that produced this is sound and should be preserved — a consumer
that treats the first turn's `Complete` as the run's ending is wrong. The
**mechanism** is what this document changes, because it costs three things:

1. **The engine's ending is not observable.** A consumer downstream of a
   wrapper cannot see that turn 1 finished; it sees only the wrapper's
   synthesized ending. Replay of a wrapped run reconstructs the wrapper's
   story, not the engine's.
2. **Every wrapper must re-implement the filter.** Goal mode does not, which is
   why the two paths differ in what a consumer observes for the same work.
3. **It is unavailable to a plugin.** An out-of-process wrapper cannot be
   handed an in-process `EventSender` to filter. A plugin-authored wrapper
   under this mechanism would need a second, parallel ending story — which is
   the disease `doc:turn-lane-assembly` diagnoses, re-introduced at the seam
   meant to cure it.

---

## 2. The event contract, inverted

**Rule (new, one-directional).** The engine always finishes its turn and always
emits its own terminal events on the one shared consumer channel. A wrapper
never receives a private channel, never filters, never suppresses, and never
re-emits an engine event. A wrapper that wants more work **requests another
turn**.

The falsely-signals-done problem is then solved by making the events say what
they mean, instead of by hiding them:

- `AgentEvent::Complete` gains the turn's identity — it already rides in a
  stream stamped with `turn_instance` (`crates/stella-protocol/src/event.rs`);
  the wrapper-aware statement a consumer needs is *"turn N of this run
  completed"*, which is a fact and is true.
- The **run's** ending is a distinct event authored by whoever owns the run.
  For an unwrapped turn that is the engine and it fires once, so nothing
  changes for the default door. For a wrapped run the wrapper emits it after
  its `continue?` returns `Stop`.

Consequences that must land in the same change, because they are what makes
this enforceable rather than aspirational:

- **Invariant #10 applies in full.** Any new wire tag lands with its row in
  `crates/stella-protocol/src/event/consumers.rs` or it is an `E0004` build
  error. The run-ending event's `ConsumerPosture` must name the surfaces that
  select it (TUI ending, `stella run` exit code, the trace fold).
- **The `Stage` stream stays the wrapper's.** Stage boundaries are genuinely
  the wrapper's facts and it emits them additively. It never *replaces* an
  engine stage event; both are in the stream, distinguishable by origin.
- **The trace stays a fold.** Per #3246 O2 and
  `crates/stella-cli/src/trace.rs:8-18`, a wrapper emits journal events; nothing
  but the fold writes `traces.jsonl`. A wrapper is not granted a second capture
  path.

**Witness for this change:** a wrapped two-turn run's event stream contains two
engine `Complete` events and exactly one run-ending event, and the run-ending
event's `turn_instance` is the last turn's. This fails on `main` for a
structural reason — `main` contains at most one `Complete` per run on the
pipeline path, by construction of the filter.

---

## 3. The interface

### 3.1 Where it lives — and why not in `stella-core`

The directive's deliverable 2 says "core change: … add the wrapper socket."
**The socket cannot live in `stella-core`,** and this is the first of the two
things needing a decision.

`before_turn` does recall, research, and planning; `after_turn` runs a test
command and an oracle process. All of that is I/O, and invariant #2 forbids I/O
in the engine. `stella-core`'s dependency list today is `stella-protocol`,
serde, toml, thiserror, tokio, async-trait, futures-util, rand, sha2 — no
pipeline, no witness, no flip oracle (`doc:turn-lane-assembly` §9.6, re-read
this session).

So the trait is defined **above** core, in `stella-runtime`, which already owns
engine assembly and reads no ambient environment by contract
(`crates/stella-runtime/tests/no_ambient_reads.rs`). Core's share of the work is
strictly *subtractive*:

- delete the private-channel/forwarding affordance (§2);
- move goal mode's round loop out of `stella-core/src/goal.rs` and onto the
  trait, leaving core with the bare loop and no route-specific supervisor.

That second bullet is what actually delivers the directive's "no route-specific
loop logic survives in core" — and it makes core smaller, which the directive's
framing ("add a socket to core") would not.

### 3.2 The trait

Sketch, not final signatures. It is written over owned data and an injected
child-engine constructor, because §3.4 requires an out-of-process
implementation to satisfy the same trait.

```rust
/// One outer loop. The engine runs turns; this decides what goes in front of
/// them, what is measured after them, and whether there is another one.
#[async_trait]
pub trait OuterLoop: Send + Sync {
    /// Before a turn: triage, recall, research, plan, scope. Returns the
    /// prompt shape for the next turn, or asks to stop before spending one.
    async fn before_turn(&mut self, ctx: &RunCtx) -> Result<Proposal, OuterLoopError>;

    /// After the turn the engine just finished: author/run a witness, run the
    /// test command, snapshot the diff. Produces *evidence*, never a verdict.
    async fn after_turn(&mut self, ctx: &RunCtx, turn: &TurnRecord)
        -> Result<Evidence, OuterLoopError>;

    /// Evidence -> verdict. See §3.3: this is where "verification buys no
    /// model call" is either kept or knowingly spent.
    fn judge(&self, ctx: &RunCtx, ev: &Evidence) -> Verdict;

    /// Verdict -> continue or stop. Bounded by construction (§3.5).
    fn next(&mut self, ctx: &RunCtx, v: &Verdict) -> Continuation;
}

pub enum Continuation {
    /// Another turn, with this feedback threaded in as the next user message.
    Again { feedback: Feedback },
    /// Done. Carries the honest ending — never rounded up.
    Stop { outcome: RunOutcome },
}
```

What crosses each boundary, stated so a wire implementation is possible:

| Boundary | In | Out | Wire-representable? |
|---|---|---|---|
| `before_turn` | run goal, round index, prior verdicts, budget remaining, workspace digest | user message + volatile context block + scope hints | yes — JSON |
| `after_turn` | the turn's outcome, its diff, files touched, spend | `Evidence`: test/oracle exit statuses, flip observation, tamper check, diff stats | yes — JSON; the *running* is host-side (§3.4) |
| `judge` | `Evidence` only | `Verdict` (met / unmet+requirement / unverifiable / nothing-attempted) | yes, and **pure** |
| `next` | `Verdict`, round index, budget | `Again{feedback}` / `Stop{outcome}` | yes, and **pure** |

`judge` and `next` are deliberately synchronous, I/O-free functions over owned
data — the invariant #2 discipline applied to the wrapper. That is what makes
the ladder property-testable, and it is how `ladder_decision`
(`crates/stella-pipeline/src/verify.rs`) is already written. Porting it onto
`judge` is a re-home, not a rewrite.

### 3.3 `judge` and the "verification buys no model call" rule

The directive says `judge` is "measurements, not a model opinion; preserve the
'verification buys no model call' rule." That rule holds for the pipeline
(#2584 removed the model verdict *structurally* — `Roster::apply` rejects the
key as `NotAssignable`), and `judge`'s signature above enforces it: a
synchronous non-async function over owned evidence cannot make a model call.

**Goal mode does not satisfy that rule and never has.** Its judge *is* a model:
`crates/stella-core/src/goal.rs` runs one verifier call per round assessing the
transcript against the goal. That is not an oversight to be refactored away —
for an open-ended goal with no test surface, there is no measurement to take.

So the honest model is: **the model call belongs to `after_turn`, not to
`judge`.** A goal-mode wrapper's `after_turn` spends one verifier call and
returns its parsed assessment *as evidence*; `judge` then maps that evidence to
a verdict deterministically. The rule "verification buys no model call" is
preserved exactly where it was won — the `classic` pipeline variant's
`after_turn` spends only the witness *author* call, and its `judge` reads the
flip. A variant that spends a judging model call cannot hide it: the spend is
in `after_turn`, on the receipt, attributable to a declared role.

This is a real distinction between the two supervisors and the design surfaces
it rather than flattening it.

### 3.4 Child engines: the blessed constructor only

Constraint from the directive, and it is the one this design most depends on:
a wrapper obtains child engines **only** through the blessed sub-agent
constructor — gate, steering, hooks attached — and a hand-rolled engine must be
impossible from the plugin API.

Mechanism, which exists:

- `RunCtx` hands the wrapper a `ChildTurn` port, not a `Provider`, not an
  `Engine`, and not a credential. The wrapper names a **role intent**
  (`triage`, `planner`, `witness_author`); the host resolves it against the
  user's BYOK providers, carves the budget, attaches the capabilities, runs the
  turn, and settles once. This is #3245 slice C, and the sub-agent primitive it
  rides (`crates/stella-core/src/subagent.rs`) already guarantees carved budget,
  capped report, no parent transcript, depth cap.
- The lane the child runs in is a `TurnLane` row with a `TurnCapabilities`
  vector — `doc:turn-lane-assembly` Move 2. This is where "gate, steering,
  hooks attached" stops being a promise and becomes a matrix row with a witness
  test. Note `crates/stella-core/src/subagent.rs:61` documents that today's
  fork *drops* three capabilities by constructor signature; slice 2 of #3274 is
  the fix, and this design is **gated on it** (§6).
- For an out-of-process wrapper, `ChildTurn` is a JSON request on stdio and the
  host makes every model call. A plugin never sees a key — invariant #3 and the
  #3245 §3 process model, unchanged.

### 3.5 What a wrapper can never do

Inherited wholesale from #3245 §2's "what an arbiter can never hold hostage",
because a wrapper's `next` returning `Again` forever is exactly an unbounded
arbiter hold:

- `max_rounds` is **host-clamped**. A spent allowance stops the run with the
  unmet requirements **reported, not dropped**.
- A wrapper that fails, times out, or emits garbage never blocks: fail-open,
  the Stop-hook posture (`crates/stella-core/src/driver/user_hooks.rs:300-306`).
- Budget aborts (invariant #6), the user's soft stop, pause, and uninstall all
  outrank a wrapper. A wrapper may hold *done* open; it may never hold *stop*
  hostage.
- **Endings are honest.** A cap, an abort, or a budget stop ends as not-met and
  is never rounded up to success. `Continuation::Stop { outcome }` carries the
  reason; `RunOutcome` has no success variant reachable from a cap.

---

## 4. Variant manifests

A pipeline variant is a manifest. It extends the `[loop]`/`[oracle]`/`[subloop]`
blocks already parsed and validated by `crates/stella-plugin/src/manifest.rs`;
it does not introduce a second manifest format.

```toml
# --- already shipped, unchanged (crates/stella-plugin) ---
[loop]
participation = "arbiter"
hooks = ["Stop"]
max_holds = 3

[requirements]
tests-flip = "the declared oracle observed fail -> pass"
no-tamper  = "witness artifact identity unchanged at verify time"

[oracle]
command = { argv = ["${plugin_dir}/bin/oracle", "verify"], timeout_secs = 120 }
flip    = "required"
tamper  = "artifact-identity"

# --- new: the ordered stage list with typed I/O and skip conditions ---
[pipeline]
variant = "classic"

[[pipeline.stages]]
id       = "triage"
boundary = "before_turn"          # which of the four points this runs at
role     = "triage"               # a routing INTENT; never a model id or URL
input    = "goal"                 # typed, from a closed vocabulary
output   = "questions"
[[pipeline.stages]]
id       = "plan"
boundary = "before_turn"
role     = "planner"
input    = "questions"
output   = "plan"
skip_if  = "triage.questions == []"   # the conditional, in the manifest

[[pipeline.stages]]
id       = "witness"
boundary = "after_turn"
role     = "witness_author"
input    = "executed_diff"
output   = "witness_artifact"
skip_if  = "host.test_command_configured or diff.is_empty"
```

Rules, each inherited from a rule that already exists:

- **Unknown keys, unknown stage ids, unknown boundaries, unknown role intents,
  and unknown `skip_if` terms are load errors** — never ignored. #1400's
  manifest rule, which `stella-plugin` already enforces with
  `deny_unknown_fields` throughout.
- **`skip_if` is a closed predicate grammar, not an expression language.** A
  Turing-complete condition in a manifest is a second program with no gate on
  it. The grammar is a small closed set of named predicates over typed stage
  outputs and host facts, evaluated by a pure function — property-testable, and
  load-validated against the stage graph so a predicate naming an unreachable
  stage fails at load, not at round 3.
- **Typed input/output makes the stage graph checkable at load.** A stage whose
  `input` no prior stage produces is a load error. This is what buys "a new
  variant is a manifest file only" — the failure mode of a hand-written variant
  is a rejection with a reason, not a wedged run.
- **`participation` stays derived, never authored twice** —
  `doc:turn-lane-assembly` §9.3. A `[pipeline]` block with an `after_turn`
  stage and `[requirements]` *implies* `arbiter`; the manifest declares
  capability, the ladder is a rendering of it.

**Two variants ship, to prove the plural:**

- `classic` — today's staged pipeline, stage-for-stage, including the revise
  back-edge onto execute and the witness/flip/tamper rules.
- `plan-only` — `before_turn` runs triage + plan; `after_turn` is empty;
  `judge` returns met-if-turn-completed; `next` always stops. The minimal
  wrapper, and the regression test that the socket does not assume verification.

---

## 5. Scoring

Add `pipeline_variant TEXT` to `executions`
(`crates/stella-store/src/ddl.rs:77`), NULL for an unwrapped turn. Same
migration discipline as every other column: one `SCHEMA_VERSION` bump, DDL and
migration in the same PR.

The directive says "alongside the existing door tag" — **there is no door
column today.** `executions.kind` is the nearest thing (`ddl.rs:79`, written at
`crates/stella-store/src/lib.rs:830`); whether it already distinguishes all six
doors is a question for slice 4 and, if it does not, the door tag is a second
column, not a re-purposing of `kind`. (Labelled: this is an observation of the
DDL; I did not census `kind`'s live values.)

Sample query the column exists to serve:

```sql
SELECT COALESCE(pipeline_variant, 'none') AS variant,
       COUNT(*)                            AS runs,
       AVG(cost_usd)                       AS avg_cost,
       SUM(outcome = 'verified')           AS verified
FROM executions
WHERE finished_at IS NOT NULL
GROUP BY variant;
```

---

## 6. Sequencing — and what this is gated on

The directive's deliverable order is right in shape but understates two gates
that already exist in this repository and are not this document's to waive.

| # | Slice | Gate |
|---|---|---|
| 0 | This doc reviewed | — |
| 1 | Event contract inverted (§2): delete private channels + forwarding; run-ending event with its consumer row | none — independently shippable, witness in §2 |
| 2 | `OuterLoop` trait in `stella-runtime`; goal mode ported onto it; `stella-core/src/goal.rs`'s loop deleted | **needs #3274 slice 2** (`TurnCapabilities`), or the child-engine promise in §3.4 is unenforced |
| 3 | `classic` manifest + manifest loader + `plan-only`; pipeline runs *through* the socket while still linked | none |
| 4 | `pipeline_variant` column + query | none |
| 5 | Flip the default: `--no-pipeline` → `--pipeline <variant>` | **bench** (§7.2) |
| 6 | Cut `stella-cli`'s `stella-pipeline` dependency | **#2716** (authority) + **side-by-side bench**, per `doc:turn-lane-assembly` §10.4 and #3245 Test 2 |

Slices 1–4 deliver the whole interface and both variants without touching the
default and without the extraction. That is deliberate: it puts every
structural change in front of the two gates rather than behind them.

---

## 7. The two things this review must decide

### 7.1 The socket is not a core change

Stated in §3.1. If the reviewer wants the socket *in* `stella-core`, invariant
#2 has to be amended in the same PR and the "no I/O in the engine" property is
gone. The recommendation is the opposite: core loses `goal.rs`'s round loop and
gains nothing.

### 7.2 Flipping the default is a measured change, not a refactor

The directive makes the raw loop the default at every door. Today's default for
`stella run` and deck turns is the pipeline, and **every Stella benchmark number
this project has published was produced on the pipeline default.** Flipping it
changes the product's measured behavior, so it is gated on the same evidence any
other behavior change is: a side-by-side bench on the same panel, reported
honestly including if the raw loop is worse.

That gate is not invented here — #3245 Test 2 and `doc:turn-lane-assembly` §10.4
both already impose it on the *extraction*. This document extends it one step
earlier, to the *default flip*, because the flip is the change that reaches
users; the extraction is the change that reaches the dependency graph.

If the reviewer wants the flip ungated, that is a "now vs. right" call and it is
the maintainer's to make, not this document's — stated per CLAUDE.md rather than
decided silently.

### 7.3 The claim this design does NOT make

The directive's definition of done includes "the diff deletes more
loop-orchestration code than it adds." That is a plausible outcome and it is
**not** something this design can promise in advance. Slices 1–2 are strongly
net-negative (a filter, a channel, and a whole round loop deleted). Slice 3 adds
a manifest loader, a predicate grammar, and a stage-graph validator that do not
exist today. Whether the total is negative is a measurement to report when the
work lands, not a property to assert now.

---

## 8. Migration notes (drafted here, shipped with slice 5)

| Surface | Today | After |
|---|---|---|
| `stella run` | pipeline by default; `--no-pipeline` for the raw loop | raw loop by default; `--pipeline classic` restores today's behavior |
| Command Deck turn | pipeline | raw loop; `--pipeline` (or the session setting) restores |
| `stella goal` | pipeline per round (`agent/goal.rs:649`) | raw loop per round; `--pipeline classic` restores nested behavior |
| Monitor | goal mode with a pinned goal | unchanged — it is goal mode, and goal mode is now a wrapper |
| Fleet task | worker lane | unchanged by default; a variant is selectable per task |
| `stella serve` | pipeline where configured | variant named in the session request |

`--no-pipeline` is kept as a deprecated no-op alias for one release with a
notice, because it is in users' scripts; removing it is a separate PR.

---

## 9. Open questions

1. Does `executions.kind` already carry the door, or is a `door` column needed
   (§5)? Answerable by a census of live values; not answered here.
2. `skip_if`'s predicate set — the closed grammar's exact members are chosen
   when `classic` is transcribed, since `classic`'s existing branches are its
   requirements. Named here so it is not discovered as a surprise.
3. Whether `plan-only` should be first-party in-tree or a fixture. Recommendation:
   in-tree, so the second variant is gate-tested like the first.
