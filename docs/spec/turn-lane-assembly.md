---
id: turn-lane-assembly
title: "One loop, seven lanes — making turn-loop capability assembly a single place"
status: proposed
---

# One loop, seven lanes

**Status:** §6 (`ResumeAuthority`) approved 2026-08-14; §9 and §10 answer the
review. Everything in §1–§3 is descriptive of `origin/main` at `a41de7be8` and
was read out of the tree, not recalled; the measurements in §9.5, §9.6 and
§10.2 were read at `a4b87649a`.

**Companion docs:** `doc:engine-embedding` (the CLI↔API parity matrix this
generalises), `doc:serve-surface`. **Subordinate to #3246** (the plugin
authority plane) wherever the two touch — see §9.

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

`TurnLane` in `stella-protocol` (types only, per invariant #1). **It is open
from the first commit** — see §9 for why, and why this is the one decision here
that is expensive to defer:

```rust
pub enum TurnLane {
    Builtin(BuiltinLane),
    /// A lane contributed by a plugin manifest. Not known at compile time.
    Plugin(LaneId),
}

pub enum BuiltinLane {
    Lead, Resume, SubSession, SubagentFork, FleetWorker, PipelineStage, ServeSession,
}
```

`BuiltinLane` is closed, which is what preserves Move 2's compile-error
property for everything in this workspace. `TurnLane` is not, which is what
lets a manifest contribute a row. Both halves are load-bearing and neither is
a compromise of the other.

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

**Decided (Mac, 2026-08-14): approved as written.** The three-way split is the
model. Move 5 is a small PR per lane, and `#3232`/`#3233` close on it. A plugin
lane declares its own authority in its manifest (§9) rather than inheriting one
by default — an undeclared authority is a load-time rejection, not a silent
`Redispatch`.

---

## 7. What this deliberately does not do

**It does not merge the pipeline into the loop.** The pipeline is a
*supervisor* that runs several turns through the one loop, in a stage graph. It
is a different layer and should stay one. "Re-implement the pipeline" remains a
change to a stage graph — the matrix makes that boundary explicit rather than
moving it. If the goal is that re-implementing the pipeline touches one place,
that is a separate (and mostly already-satisfied) property: `stage_rank` in
`stella-pipeline/src/replay.rs` is the canonical ordering.

It also does not move the pipeline *out*. That is the opposite direction and a
live question — §10.

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
| 6 | Drop `stella-runtime`'s dead `stella-pipeline` dependency (§10.2) | — | XS |
| 7 | Manifest-declared lanes: `LaneId`, load-time totality, `stella plugins doctor` (§9) | #3246 | L |
| 8 | Pipeline registers as a lane; cut `stella-cli`'s pipeline dependency (§10.4) | #3246 | XL |

PRs 1 and 2 are independent and can run in parallel. PR 3 is the one that must
not be split across two sessions — it is a single mechanical sweep, and half a
sweep is worse than none.

---

## 9. Plugin lanes — why `TurnLane` is open from the first commit

This section exists because of a standing directive that predates this
document: **plugins become turn-loop participants by manifest** (Mac,
2026-08-13), and **the verification engine leaves `stella-core` for a plugin
called Vera** — `stella-core` is to be the bare, near-immutable agent loop, and
the witness protocol, verification ladder, flip oracle and staged pipeline are
an opinionated policy layer that moves out. The live authority is **#3246**;
**#3245** is closed as completed, and **#3243** is the companion steering
plane. Anything here that contradicts #3246 loses.

That directive changes exactly one thing in §5, and it is cheap now and
expensive later.

### 9.1 A closed `TurnLane` would have been a one-way door

If a plugin can contribute a lane, then a closed seven-variant enum is wrong
from the first commit — every plugin lane would need a `stella-protocol`
release to exist, which is the precise thing a manifest-only plugin system is
for. Hence the two-level shape in Move 1:

- **`BuiltinLane` stays closed.** This is what preserves Move 2's
  compile-error property: a new capability still fails to build at all seven
  in-tree lanes, because those are exhaustively known.
- **`TurnLane` is open.** A manifest contributes a `LaneId` and the matrix
  gains a row without a core change.

Neither half is a compromise of the other, and reversing this later means
rewriting every `TurnLane` match in the tree. That asymmetry — free today,
costly in six months — is why it lands in PR 1 rather than being deferred to
the plugin work.

### 9.2 Resolution order: builtin-first, never port-first

`#2456` (opening the pipeline roster's agent vocabulary) already spec'd this
exact shape and recorded the finding that matters here, which this design
adopts rather than re-derives:

> **Builtin-first, not port-first.** The issue proposed consulting the new port
> before the router. Wrong direction — a port answering a builtin name creates
> two paths for one name.

So lane resolution consults `BuiltinLane` first and the plugin registry only on
a miss. A manifest naming `lead` is a load-time rejection ("that lane exists
and is not yours"), not a silent override of the deck's own lane. The second
`#2456` finding applies too: the custom set nests (`[lanes.custom.<id>]`), it
does not `#[serde(flatten)]`, because a flattened map is silently skipped by
scope-merge.

### 9.3 The matrix becomes two-tier, with one schema

Move 3's matrix is a *schema*; a manifest is an *instance* of it. Same fields,
two enforcement mechanisms:

| | Core lanes | Plugin lanes |
|---|---|---|
| Declared in | `stella-parity`'s `LANES` | the plugin manifest |
| Enforced at | compile + `cargo test` | manifest load |
| Missing posture | build error | load-time rejection |
| Postures | `Bound` / `Declined` / `Deferred` | the same three |

The load-time validator and the compile-time completeness test read **one**
list of capability names — derived from `TurnCapabilities`' fields — so a
capability added in core cannot be one a manifest is unable to speak about.
That is the "drive it from manifests/registries" ask, and the matrix is what
makes it expressible: without Move 2 there is no single list to validate a
manifest against, because the capabilities are spread across three
vocabularies.

### 9.4 `TurnCapabilities` is the plugin ABI

This is the strongest argument for landing Moves 2–4 **before** plugin work
rather than alongside it. A manifest-driven plugin surface has to name what a
plugin may change about the loop. Today that surface would have to be spelled
three ways at once — `EngineConfig` fields, `Engine::with_*` builders, and
`TurnControls` published to the registry — and a plugin author would have to
know which capability lives in which vocabulary. After Move 2 there is exactly
one struct, and the manifest schema is a projection of its field names.

Concretely, #3246's stated ownership — *"change the definition of done — the
exit condition of the turn loop"* — is `TurnCapabilities::halt` plus the Stop
gate. That capability already exists (`turn_halt`, today bound only by the
pipeline). Move 2 does not build it; it makes it **nameable**, which is what a
manifest needs.

### 9.5 What this section does not decide

Participation levels (`none | observer | steering | arbiter`), `max_holds`,
fail-open semantics, the witness/oracle wire contract, deck presence, and the
authority plane are all **#3246's**, not this document's. A lane is *where a
turn runs and what it is allowed to bind*. Participation is *what a plugin may
do to a running turn*. They meet at `TurnCapabilities` and are otherwise
separate; this document deliberately stops at the seam.

---

## 10. Moving the pipeline out of core: now, later, or never

The question raised in review: *do we refactor the verification engine /
pipeline out of core Stella now, later, or never?*

**Never is already off the table** — the Vera directive settled it, and 22
issues were closed `wontfix` on the strength of it precisely because they were
defects in a subsystem that is leaving. So the live question is now vs later.

**Recommendation: later, and this PR is what makes later cheap.** Specifically:
after PR 3, before PR 5.

### The argument for *not* now

Extraction needs a typed surface to extract **onto**. Today the pipeline's
relationship to the loop is not expressible as data: it binds `turn_halt`
through `with_turn_halt`, sets `checkpoint_sink = None` at
`witness_stage.rs:380`, attaches gate and steering per stage, and reaches the
engine through `PipelineConfig::engine` — four different mechanisms, none of
which a manifest can name. Extracting first means designing the plugin ABI and
performing the dependency cut in one motion, against a moving target, with no
compile-time check that a lane kept its capabilities across the move.

Extracting *after* Moves 2–4 turns the same work into a **dependency cut**:
the pipeline's lanes already declare their postures in the matrix, so the
extraction has an explicit before/after to hold itself to, and a row that
changes posture during the move is a failing test rather than a silent
regression. That is the difference between a refactor and a redesign.

This also matches the sequencing already recorded for the plugin work: *the
dependency cut is the LAST slice, only after a side-by-side bench holds.*

### The argument against waiting longer than that

Every capability added to the loop between now and extraction gets bound at the
pipeline's stage sites by hand, which is more surface to cut later. Moves 2–4
stop that accumulation — after them, new capabilities arrive as matrix rows,
and the pipeline's rows move with it. So the window is not "whenever"; it is
"as soon as the matrix exists".

### What would change this answer

If #3246 needs a working plugin lane sooner than PR 3 can land, invert it: do
PR 1 (open `TurnLane`) and PR 2 (`TurnCapabilities`) only, ship a plugin lane
against the new ABI with the in-tree lanes still on the deprecated shims, and
take PR 3's sweep afterwards. That costs one extra migration of the plugin lane
and keeps the shims alive longer, but it does not require extraction to precede
the matrix — which is the ordering this section is actually arguing for. PR 6 is free and can go first. PRs 7 and 8 are the
plugin half and are specified in §9 and §10; 8 is gated on a bench, not on a
review.

---

## 9. Plugin-owned lanes

> "Plugins should be able to create their own lane. Ideally pipeline is moved
> out of core stella and core stella remains the bare loop with a plugin
> surface that can change the behaviors of the loop — in other words we need
> some way to drive this from manifests/registries."
> — Mac, on this PR

This is the right requirement and it is compatible with §5, but only if one
decision in Move 1 is made now rather than later. That decision is the enum.

### 9.1 What a plugin lane breaks, stated exactly

Move 2's entire value is a **compile error**: `TurnCapabilities` has no
`Default`, so adding a capability forces the author to visit every lane. That
guarantee is available only for lanes the compiler can see. A lane contributed
by a manifest is, by construction, not one of those. So a plugin lane cannot
inherit the property that makes this design worth building — and pretending
otherwise is how the guarantee quietly becomes decoration.

The answer is not to weaken the in-tree guarantee. It is to be explicit that
there are **two totality regimes**, and to make the weaker one loud.

### 9.2 Two-level totality

| Lane origin | Totality is enforced | By what | When a new capability is added |
|---|---|---|---|
| `TurnLane::Builtin(_)` | at compile time | no `Default` on `TurnCapabilities`, exhaustive destructuring in the parity test | the workspace does not build until every builtin lane declares |
| `TurnLane::Plugin(_)` | at **load** time | the manifest is validated against a capability registry generated from the same table `TurnCapabilities` is generated from | the plugin loads with the new slot forced to `Declined`, and `stella plugins doctor` reports every lane sitting on a defaulted slot |

Two rules keep the load-time half from rotting into a silence:

- **An unknown capability name in a manifest is a load error, never ignored.**
  This is #1400's existing manifest rule (unknown top-level keys are a load
  error), applied one level down.
- **A defaulted slot is reported, not assumed.** A plugin lane that has never
  been told about a capability added after it was written is in exactly the
  position `subagent.rs:705` is in today — a `None` nobody decided. The
  registry knows the plugin's declared engine-compat range, so it can say so.

This is strictly weaker than the compile error, and that is not a flaw to be
argued away: **you cannot give an out-of-tree file a build failure.** The
honest move is to name the weaker guarantee in the matrix itself, so a reader
of a `Plugin` row knows what it is worth. A `LanePosture` for a plugin row
therefore carries its origin.

### 9.3 The participation ladder is this matrix, at a coarser grain

#3245's manifest already proposes the registration:

```toml
[loop]
participation = "arbiter"   # none | observer | steering | arbiter
```

That ladder and the capability vector are **the same statement at two
resolutions**:

| `participation` | Capability slots it implies |
|---|---|
| `none` | — |
| `observer` | `bus` |
| `steering` | `+ steering`, `hooks` |
| `arbiter` | `+ gate`, `halt`, `completion_gate` |

They must not both be authored. A repo that has already lost one limit to
having a number in two places should not invent a second vocabulary for lane
capability on purpose. **The manifest declares capabilities; `participation` is
derived from them and displayed** — in `stella plugins doctor`, in the Command
Deck's active-plugin chip, and in the lane matrix. One source, two renderings.

### 9.4 A manifest *requests*; the host *grants*

The lane matrix as specified in Move 3 records what a lane **has**. For a
builtin lane those are the same thing. For a plugin lane they are not, and
conflating them turns a design document into a security claim it has not
earned:

```text
granted = requested ∩ authorized(principal, capability)
```

`requested` comes from the manifest. `authorized` is the authority vocabulary
of #2716 — `ToolContract` / `AuthzGate` / `Principal` / `RiskLevel`. That
issue is closed `NOT_PLANNED` and has **zero code in the tree** (verified in
#3246 §2: `rg -n "struct ToolContract|trait AuthzGate|enum RiskLevel|struct
Principal" crates/` → 0 hits).

So: a plugin lane is designable today, and **safely grantable only after
#2716**. Until that vocabulary exists, a paid plugin and a hostile one hold
identical authority over the turn loop, and a `Plugin` row in the lane matrix
is a record of an intention rather than of a permission. The matrix should
carry both columns from the start — `requested` and `granted` — so that the
day #2716 lands, the enforcement point already has a place to write its answer
instead of needing a schema change.

### 9.5 The borrow question, and why `stella-serve` already answers it

`Engine<'a>` holds `&'a dyn Provider`, `&'a dyn ToolExecutor`, `&'a dyn
Sleeper` (`driver.rs:480-484`), and every vocabulary-B slot is `&'a` as well.
An out-of-process plugin cannot hand the engine a borrow.

It does not have to. The host owns an adapter and lends *that*, which is
precisely the shape `stella-serve` already ships: its engine holds no ambient
authority and every model and tool call is remoted back to a host process.
**A plugin-driven lane is `stella-serve` pointed the other way** — the same
seam, with the out-of-process participant supplying capability rather than
consuming it. That is an existence proof in the tree, not an analogy, and it
is the single largest reason to treat this as feasible.

Two constraints follow, and both bite Move 2 *now* rather than later:

- **`TurnCapabilities` must admit owned slots, not borrows only.** A design
  that is `&'a dyn`-only forces plugin lanes into a second, parallel
  vocabulary — which is the exact disease §3 diagnoses. Either the struct is
  generic over ownership or the owned form is defined in the same PR.
- **A blocking plugin cannot live on the `HookBus`.** Its handlers are sync
  closures (`Fn(&HookEvent) -> HookDecision`, `bus.rs:190`) and cannot await.
  An arbiter lane whose definition of done involves running a test suite
  belongs on the async out-of-process hook plane (60s default, 10min ceiling),
  which is the *better* substrate for this work, not the fallback.

### 9.6 What "core remains the bare loop" already means

Worth stating because it changes the size of the ask: **`stella-core` is
already the bare loop.** Its `[dependencies]` are `stella-protocol`, serde,
toml, thiserror, tokio, async-trait, futures-util, rand, sha2 — no
`stella-pipeline`, no verification machinery, no witness or flip-oracle module.
The only `Verdict` identifiers in the crate are `LoopVerdict` (loop detection)
and `GoalVerifierVerdict` (goal assessment), neither of which is the
verification engine.

The work is therefore not carving policy *out of* the loop. It is giving the
loop a registry so that policy can be plugged *into* it from outside — which
is a smaller and much less dangerous job.

---

## 10. Does the pipeline leave — now, later, or never?

### 10.1 The premise, corrected

It has already left `stella-core` (§9.6). What has not left is `stella-cli`.

### 10.2 What is actually coupled, measured

| Crate | Declares `stella-pipeline` | Source references |
|---|---|---|
| `stella-cli` | yes | **169 across 41 files** |
| `stella-serve` | yes | 4 files |
| `stella-runtime` | yes | **zero — the dependency is dead** |

`stella-runtime`'s dependency is declared in its `Cargo.toml` and referenced
nowhere in its `src/`. That is a free deletion and PR 6 in §8.

### 10.3 The recommendation

| | |
|---|---|
| **Now** | The lane seam — Moves 1–4, with `TurnLane` open (§5 Move 1) and `TurnCapabilities` ownership-generic (§9.5). Plus the dead-dependency deletion. |
| **Later** | The extraction: the pipeline registers as a plugin lane, and `stella-cli`'s dependency is cut. Gated on #2716 for authority and on a side-by-side bench for equivalence. |
| **Never** | Merging the pipeline into the loop (§7). It is a supervisor; it stays a layer. |

The reason "now" applies to the seam and not the extraction is that the two
decisions have opposite costs of delay. Making `TurnLane` open costs one extra
enum variant today and is unaffordable to retrofit after seven lanes and a
parity matrix are written against a closed one. The extraction costs 169 call
sites whenever it is done, and doing it before the seam exists means doing it
twice.

### 10.4 Definition of done for the cut

So it is decidable rather than debatable:

- `rg 'stella-pipeline' crates/stella-cli/Cargo.toml` returns nothing;
- `stella run` still delivers today's pipeline mode, through a lane registered
  from a manifest;
- a side-by-side bench on the same panel holds — this is the gate #3245 already
  imposes, and it is a measurement, not a review.

### 10.5 What the extraction buys, and what it does not

It buys modularity, a customer-owned definition of done, and the paid-plugin
story. It does **not** buy the property that started this document. "When I add
a feature to the turn loop I should only have to go to one place" is delivered
by Moves 2 and 3, whether or not the pipeline ever moves. Keeping those two
claims apart is what stops the extraction from being sold on a benefit it does
not deliver.
