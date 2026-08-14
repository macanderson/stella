---
id: turn-lane-assembly
title: "One loop, seven lanes — making turn-loop capability assembly a single place"
status: proposal
---

# One loop, seven lanes

**Status:** proposal, awaiting a decision on §6 (`ResumeAuthority`). Everything
in §1–§3 is descriptive of `origin/main` at `a41de7be8` and was read out of the
tree, not recalled.

**Companion docs:** `doc:engine-embedding` (the CLI↔API parity matrix this
generalises), `doc:serve-surface`.

---

## 1. The complaint, and why the diagnosis has to be inverted

The complaint is: *"Stella should only have 1 loop. I do not understand why
fleet would have checkpointing and repl wouldn't. When we add a feature to the
turn loop I should only have to go to one and only one place."*

The second and third sentences are exactly right. The first one is already
true, and that matters, because it changes what has to be built.

**There is exactly one loop.** `crates/stella-core/src/driver/drive.rs:111` is
the only `loop {` over `Engine::run_step` in the workspace. `run_turn` and
`run_turn_with_sender` are `drive` over an adopted transcript. Nothing in
`stella-fleet`, `stella-pipeline`, `stella-cli`, `stella-tui` or `stella-serve`
implements a second one — a sweep for `run_step(` outside `driver/` returns one
hit, and it is a doc comment in `stella-engine/src/lib.rs:37`.

So the fleet does not have "its own loop with checkpointing." It has *the*
loop, assembled differently. What is duplicated is not the loop; it is the act
of deciding **which of the loop's optional capabilities this particular run
gets**. That decision is made by hand, from scratch, at seven production sites,
in three unrelated vocabularies, with nothing checking the result.

That is the actual defect, and it is a better defect to have than the one
feared: the hard, expensive part (a single correct step loop) is already
finished and does not need to be touched. What needs building is a seam above
it.

---

## 2. The seven lanes

A **lane** is one place a turn runs. Today there are seven, and none of them is
named anywhere in the codebase — which is the root of everything below. You
cannot write a table whose rows have no names.

| Lane | Assembly site |
|---|---|
| Lead (deck / REPL) | `stella-cli/src/command_deck.rs:4244` |
| Resume | `stella-cli/src/agent/resume.rs:311` |
| Sub-session | `stella-cli/src/subsession.rs:729` |
| Subagent fork | `stella-core/src/subagent.rs:705` |
| Fleet worker (raw) | `stella-cli/src/fleet_cmd.rs:914` |
| Pipeline stage | `stella-pipeline/src/pipeline/execute_stage.rs:363`, `witness_stage.rs:361` |
| Serve session | `stella-serve/src/session.rs:675` |

Plus `stella-core/src/goal.rs:240`, which is a lane-shaped *derivation*
(`with_turn_instance`) rather than a fresh assembly, and is the one site that
already does the right thing.

---

## 3. Three vocabularies, twelve slots, no table

The engine's optional capabilities are bound through three mechanisms that do
not know about each other.

**Vocabulary A — `EngineConfig` fields** (owned, `Arc`-carried):
`checkpoint_sink`, `turn_halt`, `completion_gate`, `turn_budget`.

**Vocabulary B — `Engine::with_*` builders** (borrowed `&'a`): `hooks`,
`hook_approvals`, `calibration`, `gate`, `steering`, `bus`, `outcomes`,
`fallback`.

**Vocabulary C — `TurnControls` published to the `ToolRegistry`**: the pause
gate, *again*, so that sub-agents dispatched by this turn inherit it.

Twelve optional slots, three spellings, and one capability (the gate) that must
be bound twice through two different mechanisms at every site that wants it —
`registry.attach_turn_controls(...)` **and** `.with_gate(...)`. Both appear
side by side at `fleet_cmd.rs:882-887`, `subsession.rs:749-758` and
`command_deck.rs:4244-4252`. A site that does one and forgets the other is
silently half-wired.

### 3.1 The constructor cannot carry three of them

`Engine::with_sleeper` is documented as "the only constructor" and takes
`(provider, tools, config, sleeper)`. Everything in vocabulary B is attached
afterwards by consuming builders. That is fine for a caller holding all the
borrows — and structurally impossible for a caller that is *forking*.
`stella-core/src/subagent.rs:61` states the consequence in its own module doc:

> `Engine::with_sleeper` cannot carry `gate`/`steering`/`hooks`

So a subagent fork drops three capabilities, not by decision but by signature,
and vocabulary C exists largely as the workaround for the one of the three
(`gate`) that could not be allowed to drop.

### 3.2 What the matrix actually is today

Reconstructed by reading all seven sites. Nothing in the tree states it; this
table did not exist before this document, and constructing it required tracing
`bind_session` callers × three `engine_config_for*` variants × seven
`with_sleeper` sites.

| Lane | gate | steering | hooks | calibration | bus | checkpoint_sink | turn_halt |
|---|---|---|---|---|---|---|---|
| Lead | ✓ | ✓ | ✓ | ✓ | — | ✓ | — |
| Resume | — | — | ✓ | ✓ | — | ✓ | — |
| Sub-session | ✓ | — | — | ✓ | — | **✗ #3233** | — |
| Subagent fork | — | — | — | — | — | ✗ hard-coded | — |
| Fleet worker | ✓ | — | ✓ | ✓ | — | **✗ #3232** | — |
| Pipeline stage | ✓ | ✓ | ✓ | ✓ | — | ✗ explicit `None` | ✓ |
| Serve session | ✓ | ✓ | — | ✓ | ✓ | ✓ conditional | — |

Some of those cells are correct decisions. A fleet worker genuinely has no
concurrent input channel, so `steering: None` is right and the code says so in
a comment. A witness author's workspace is discarded, so its `checkpoint_sink =
None` is right and `witness_stage.rs:380` says so.

The problem is that **a correct decision and a forgotten one look identical.**
Nothing distinguishes `subsession`'s deliberate `checkpoint_sink: None` (added
by #3242 to stop a lane destroying the lead's resume point, and defended by a
named test) from `subagent.rs:705`'s hard-coded `None`, which is a default
nobody revisited. Reading the code cannot tell you which is which; you have to
go find the PR.

### 3.3 The concrete cost, measured on this week's issues

`#3232` (fleet has no step-level durability) and `#3233` (sub-sessions have
none) are not two bugs. They are two cells of the table above, discovered one
at a time, months apart, each by someone tripping over it. `#3242` is a third
cell, discovered as *damage* — a sub-session was inheriting the lead's sink and
destroying the lead's resume point — and its fix was to strip the cell rather
than decide it, because deciding it required a design answer nobody had.

`stella fleet` gets `None` for a reason that is invisible at every one of the
sites involved: `SessionDurability::sink()` returns `None` while unbound, and
`durability::bind_session` is called from `command_deck.rs`, `agent/presence.rs`
and `agent/resume.rs` — never from `fleet_cmd.rs`. Nothing declares that. The
fleet worker's raw path calls `agent::engine_config_for(&cfg)`, which *does*
read `cfg.durability.sink()`, so the site looks wired. It is unwired three
files away.

**That is the whole complaint, precisely located:** adding a capability to the
turn loop today means (a) adding a slot in `stella-core`, then (b) finding the
seven sites, with (c) no list of what the seven are, and (d) no test that fails
if you miss one. Every miss is silent and stays silent until a run pays for it.

---

## 4. The design in one line

**Make forgetting a lane a build error.**

Everything below is machinery for that sentence. The repository has already
proved this exact instrument twice — invariant #8's provider parity matrix and
invariant #10's event-consumer ledger — and both use the same three parts: a
declared row per subject, a posture that makes an absence legal only when
written down, and enforcement from both sides so the declaration cannot rot.
This proposal points that discipline at lane assembly. It invents no new
pattern.

---

## 5. Four moves

Each is independently shippable and independently useful. None is a big-bang.

### Move 1 — Name the lane

`TurnLane` in `stella-protocol` (types only, per invariant #1):

```rust
pub enum TurnLane {
    Lead, Resume, SubSession, SubagentFork, FleetWorker, PipelineStage, ServeSession,
}
```

Nothing branches on it yet. It exists so the matrix in Move 3 has row keys, and
so `agent.turn.started` can say which lane a turn came from — which no surface
can answer today, including the Observatory.

*Witness:* a lane-tagged `TurnStarted` round-trips through `serde_json`
(invariant #4) and the Observatory renders the lane column.

*Cost:* small. One enum, one event field, one snapshot re-bless.

### Move 2 — Collapse three vocabularies into one

`TurnCapabilities<'a>` in `stella-core`: one struct holding every optional slot
from vocabularies A, B and C.

```rust
#[non_exhaustive]
pub struct TurnCapabilities<'a> {
    pub gate: Option<&'a dyn TurnGate>,
    pub steering: Option<&'a dyn TurnSteering>,
    pub hooks: Option<HooksHandle<'a>>,
    pub hook_approvals: Option<&'a dyn HookApprovalRoute>,
    pub calibration: Option<&'a CalibrationMap>,
    pub bus: Option<&'a HookBus>,
    pub outcomes: Option<&'a dyn ProviderOutcomes>,
    pub fallback: Option<&'a dyn FallbackResolver>,
    pub checkpoint: Option<Arc<dyn CheckpointSink>>,
    pub halt: Option<Arc<dyn TurnHalt>>,
    // ...
}
```

`Engine::assemble(provider, tools, sleeper, config, capabilities)` replaces
`with_sleeper` + eight consuming builders. Two properties follow, and they are
the point:

**It is one value, so a fork can carry it.** The hole
`subagent.rs:61` documents closes structurally rather than by a workaround, and
vocabulary C (`TurnControls` via the registry) becomes *derived* from the
capabilities rather than a parallel truth a site can forget to write.

**It has no `Default` and is `#[non_exhaustive]` outside the crate.**
Constructing one inside the workspace requires naming every field. So **adding a
capability is a compile error at all seven lanes**, and the compiler walks the
author to each one in the PR that adds it. That is the user's ask, delivered by
the type system rather than by discipline.

*Witness:* a subagent fork carries `gate`, `steering` and `hooks` — the three
capabilities `subagent.rs:61` currently documents as undeliverable. This
witness fails on `main` for a structural reason, which is the strongest kind.

*Cost:* the largest of the four. Every test that builds an `Engine` changes —
`goal.rs` alone has ~12, `driver/tests.rs` many more. Mechanical, but wide.
`driver.rs` is a grandfathered god file at 1917 lines, so `TurnCapabilities`
lands in a sibling module (`driver/capabilities.rs`), following
`driver/settlement.rs`.

### Move 3 — Declare the matrix

`LANES: &[LaneCapability]` in `stella-parity`, one row per (capability × lane),
with the same posture vocabulary the crate already uses:

```rust
pub enum LanePosture {
    Bound { how: &'static str, witness: &'static str },
    Declined { reason: &'static str },
    Deferred { issue: &'static str, waiting_on: &'static str },
}
```

Enforced from both sides, exactly as `CAPABILITIES` is today:

- every field of `TurnCapabilities` must be claimed by a row — checked by
  exhaustively destructuring the struct in the test, so a new field that skips
  the matrix fails `cargo test --workspace` in its own PR;
- every `Bound` row's named witness test must still exist in the lane's sources.

`#3232` and `#3233` become two `Deferred` rows citing their issues. They stop
being invisible and start being *scheduled*. `subagent.rs:705`'s hard-coded
`None` has to declare itself as `Declined` with a reason or `Deferred` with an
issue — the PR author cannot leave it ambiguous, which is the half that has
never been true before.

*Witness:* a capability field added without a matrix row fails the completeness
test.

### Move 4 — Resolve lanes in `stella-runtime`

`stella-runtime` already owns "the engine-assembly bottom half (provider,
registry, store, budget)" and reads no ambient environment by contract.
`TurnCapabilities` is the top half, and the lane→capabilities resolution
belongs there: `RuntimeSpec { lane, .. } → SessionRuntime::capabilities()`.

This keeps `stella-core` I/O-free (invariant #2) and gives every lane one door.
`stella-cli`'s three `engine_config_for*` variants collapse into
`runtime.capabilities(lane)`, and the fleet's silent-`None` — a `bind_session`
that is never called, three files from the site that reads it — becomes a
`Deferred` row someone has to look at.

---

## 6. The open question this forces (and it is yours)

Moves 1–3 make the gap visible and typed. They do not by themselves decide
`#3232`/`#3233`, because those are blocked on a genuine design question that
was correctly declined rather than settled in a background job:

> **Nothing reads a fleet worker's or a sub-session's checkpoint.** Binding the
> write side alone buys a whole-transcript serialization per step with no
> reader.

That is right, and the matrix makes it answerable *once* instead of seven
times. Proposal: model it as a property of the lane.

```rust
pub enum ResumeAuthority {
    /// The lane resumes itself. Lead, Resume, ServeSession.
    Own,
    /// The lane's checkpoint is read by its parent for *reporting*, not
    /// resumption: a killed sub-session hands its transcript to the lead
    /// rather than restarting itself. SubSession, SubagentFork.
    Parent,
    /// The supervisor re-runs the unit. The checkpoint is evidence, not a
    /// resume point. FleetWorker, PipelineStage.
    Redispatch,
}
```

The reason this unblocks rather than restates: **`Parent` and `Redispatch`
lanes do not need a per-step resume point at all.** They need a *terminal
frame* — last committed step plus transcript, written once when the lane dies
or ends. That is far cheaper than the per-step serialization the earlier fix
declined, and it has a reader by construction: the parent's report, and the
supervisor's re-dispatch decision. A fleet worker killed at step 40 stops
losing 40 steps of transcript, without anyone paying for a resume path that
nothing would ever call.

**The decision needed from you is whether that three-way split is the right
model of "resume" for a non-primary lane.** If it is, Move 5 is a small PR per
lane and both issues close. If you want `Parent` lanes to genuinely resume,
that is a larger build and the matrix rows should say `Deferred` until it is
scheduled.

---

## 7. What this deliberately does not do

**It does not merge the pipeline into the loop.** The pipeline is a
*supervisor* that runs several turns through the one loop, in a stage graph. It
is a different layer and should stay one. "Re-implement the pipeline" remains a
change to a stage graph — the matrix makes that boundary explicit rather than
moving it. If the goal is that re-implementing the pipeline touches one place,
that is a separate (and mostly already-satisfied) property: `stage_rank` in
`stella-pipeline/src/replay.rs` is the canonical ordering.

**It does not, on its own, carry a behavioural witness for Moves 2–3.** They
are refactors. Move 2's fork witness is real and load-bearing; the rest of
Moves 2–3 are structural, and per this repository's own contract that should be
declared in the PR rather than dressed up as a behaviour change.

**It does not reduce the number of lanes.** Seven is arguably right — they
genuinely differ, and should. The claim is not that a fleet worker and the REPL
should be configured identically. It is that *every difference between them
should be a written, tested, reviewable decision* instead of the residue of
whichever site was edited last.

---

## 8. Sequencing

| # | PR | Closes | Size |
|---|---|---|---|
| 1 | `TurnLane` + lane-tagged turn events | — | S |
| 2 | `TurnCapabilities` + `Engine::assemble`, `with_*` as deprecated shims | — | L |
| 3 | Migrate all seven sites; delete shims; no `Default` | — | M |
| 4 | Lane matrix in `stella-parity`, both-sides enforced | — | M |
| 5 | `ResumeAuthority` + terminal-frame write side | #3232, #3233 | M |

PRs 1 and 2 are independent and can run in parallel. PR 3 is the one that must
not be split across two sessions — it is a single mechanical sweep, and half a
sweep is worse than none.
