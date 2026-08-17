---
id: pipeline-as-plugins
title: "Every wrapper is a plugin: the full extraction plan"
status: proposed
---

# Every wrapper is a plugin: the full extraction plan

**Status:** proposed, 2026-08-17.

`doc:turn-loop-wrappers` established the direction — one loop, six doors,
wrappers are plugins — and sequenced its first three moves. This document is the
completion plan: what remains until **every part of the staged pipeline, and
self-driving, runs as a plugin outside core Stella**, with the plugin surface
proven in three languages rather than asserted in one.

Everything said here about today's code was read out of the tree, and the file
is named so it can be checked. Where a claim is an inference rather than an
observation, it says so.

---

## 1. The short version

Nine plugins replace one pipeline and one built-in autonomous loop. Core keeps
one turn loop, one wrapper socket, and zero built-in wrappers.

The work splits into four tracks that must land roughly in order:

| Track | What it delivers | Why it is first |
|---|---|---|
| **A — Substrate** | The socket, a loader, a plugin identity, structured verdicts | Nothing below can dispatch without it |
| **B — Extraction** | The pipeline's stages become plugins, one at a time | Each is independently shippable once A exists |
| **C — Proof** | Python and TypeScript plugins in `stella-examples`, running in CI | A one-language plugin surface is a library, not a platform |
| **D — Self-driving** | The autonomous loop leaves the binary | Depends on A, and on capabilities B does not need |

The single most important constraint, stated once and enforced throughout:
**`judge` stays in-process, pure, and total.** Plugins declare their verdict
rule as data and supply evidence out-of-process; they do not ship verdict code.
§6 argues this rather than assuming it.

---

## 2. What is actually true today

The starting position is better than "nothing" and worse than "nearly there".

**Landed and working:**

- The engine owns its own ending (#3379). `crates/stella-pipeline/src/pipeline.rs:15-27`
  documents the one-directional connection: the pipeline no longer edits the
  engine's stream. This was the hard blocker — a plugin cannot hold a private
  channel into the engine.
- **The turn-completion gate is real, async, and already out-of-process.**
  `Engine::stop_hook_feedback` (`crates/stella-core/src/driver/user_hooks.rs:291`)
  runs at completion; a `Deny` holds the turn open and the engine keeps
  stepping (`crates/stella-core/src/driver/completion.rs:183`). It is bounded by
  `MAX_STOP_CONSULTS = 3` (`user_hooks.rs:99`), spent *before* the hooks run, and
  the final round announces itself (`STOP_FINAL_ROUND_NOTE`, `:105`). It is
  fail-open by design (`:55-59`) — a hook that fails never blocks, because
  failing closed here means never completing.
- Subprocess hooks: JSON on stdin, a `HookDecision` on stdout, 60s default and a
  600s ceiling (`crates/stella-core/src/hooks.rs:75`). **This is the
  any-language plane, and it works now.**
- `TurnLane::Plugin(LaneId)` (`crates/stella-protocol/src/lane.rs:165`).
- The manifest grammar: participation ladder, `LoopGrant`, `[oracle]`,
  `[requirements]`, `[subloop]`, `[roles]`, and a closed condition grammar with
  a load-time stage-graph check (`crates/stella-plugin/src/{manifest,wrapper}.rs`).

**The governing gap:** `stella-plugin` has **zero consumers**. Nothing calls
`PluginManifest::from_toml_str`, there is no `.stella/plugins/` resolver, no
install command, no loader. `crates/stella-plugin/src/wrapper.rs:29-33` says it
plainly — a `StageName` is "a declared name that load-checks, not a dispatch
target".

So the manifest describes a socket that does not exist. Track A builds the
socket; everything else is downstream of it.

---

## 3. The nine plugins

The staged pipeline is not one thing. Splitting it by *what would be installed
separately* rather than by current module boundaries:

| Plugin | Replaces | Points used | Ships |
|---|---|---|---|
| **vera** | witness authoring, flip oracle, verification ladder, reward labelling | `after_turn`, `judge` | Oxagen, private |
| **stella-plan** | triage, plan, scope | `before_turn` | first-party, open |
| **stella-research** | research sub-agents, recall | `before_turn` | first-party, open |
| **stella-candidates** | worktree candidates, best-of-N fan-out, steering mirror | `before_turn`, `again?` | first-party, open |
| **stella-selfdriving** | the autonomous delivery loop | `again?`, host verbs | first-party, open |
| **stella-goal** | goal mode, `stella monitor` | `judge`, `again?` | first-party, open |
| **example-py** | a working Python plugin | `after_turn` | `stella-examples`, public |
| **example-ts** | a working TypeScript plugin | `after_turn` | `stella-examples`, public |
| **example-rs** | the reference Rust plugin | all four | `stella-examples`, public |

Two notes on that table. `stella-goal` is included because #3380 already
observes goal mode and the pipeline are the same shape written twice —
extracting one and leaving the other rebuilds the drift. And the three examples
are listed as deliverables, not documentation, because a language is supported
when a plugin written in it runs in CI, and not before.

---

## 4. Track A — the substrate

Ordered by dependency. Each item names the files it touches.

### A1. A plugin is not the user

**Today Stella cannot tell a plugin apart from the human.** The authority
vocabulary landed — `Principal` (`crates/stella-core/src/ports/authz.rs:62`),
`AuthzGate` (`:244`), `RiskLevel` and `ToolContract`
(`crates/stella-protocol/src/contract.rs:68`, `:197`) — but every call site
passes the constant `Principal::User`
(`crates/stella-cli/src/agent.rs:362`, `:1657`, `:1672`).

`Principal::Host(String)` (`authz.rs:74-76`) is already the right shape and is
deliberately opaque so core grows no opinion about who hosts are. Thread a real
principal through dispatch and make plugin identity one.

This is **first** and not negotiable: a marketplace shipped on top of a
system that cannot distinguish an installed plugin from its operator grants
every plugin the operator's authority. Related: #2793 (MCP and custom tools
bypass the blocking policy chains) is the same hole one layer down and should
close alongside.

### A2. The blessed constructor (#3387)

#3380 lists this as its own precondition. The pipeline still builds its own
engine via `Engine::with_sleeper` (`crates/stella-pipeline/src/pipeline.rs:1530`)
and re-attaches `hooks`/`steering` by hand (`:1537`, `:1542`). A hand-rolled
child engine that silently drops `gate`, `steering` or `hooks` is the bug goal
mode already shipped. One constructor, or every wrapper re-authors the defect.

### A3. The wrapper socket (#3380)

The four points, defined in **`stella-runtime`** — not `stella-core`, because
`before_turn` performs recall and `after_turn` spawns processes, and invariant 2
bans I/O in the engine (`doc:turn-loop-wrappers` §9.1).

Two halves, and they are separable:

- **A3a — the in-process contract.** A Rust trait in `stella-runtime`, plus the
  manifest vocabulary for the four points (which `[wrapper]` does not yet have —
  it declares stages, not points).
- **A3b — the wire contract.** The same four points expressed as serialized
  request/response, so a plugin can be a separate process. This is the half that
  decides whether Python and TypeScript are first-class or bolted on, and §5
  argues it must be designed **with** A3a rather than after it.

### A4. A loader

`.stella/plugins/` and `~/.stella/plugins/` resolvers (`crates/stella-home/`),
`stella plugin install|list|remove` (`crates/stella-cli/`), a consent prompt on
install that shows the declared participation grade and hook grants, and
`LoopGrant::permits_hook` (`manifest.rs:134-139`) as the routing filter.

**Uninstall must actually uninstall.** Hook settings currently *concatenate*
across scopes and no scope can remove another's entries
(`crates/stella-cli/src/settings/merge.rs:47`, `:171-172`). Correct for operator
hooks, wrong for plugins.

### A5. A process declaration in the manifest

Today only the oracle may name an executable — `OracleCommand{argv,
timeout_secs}` (`crates/stella-plugin/src/manifest.rs:163-170`), "never a shell
string", with `${plugin_dir}` interpolation as the host's job. There is no
`runtime`, `language`, or `entrypoint` field on `PluginManifest`
(`:227-259`), and `deny_unknown_fields` on every table means one cannot be added
without editing the crate.

Add `[runtime]` modelled directly on `OracleCommand`: `argv`, `timeout_secs`, an
env allowlist. Deliberately **not** a `language` field — `argv` already
expresses `["python3", "${plugin_dir}/main.py"]` and
`["node", "${plugin_dir}/main.js"]` without Stella learning what a language is.

### A6. Structured verdicts

`HookDecision::Deny` carries only a `String` (`crates/stella-core/src/bus.rs:144`).
A verification plugin must return which witness, which command, whether the flip
was achieved, and the digest. This is invariant 5's own test — the driver and the
trace fold both branch on it (#3246 §O1.2).

While here: `RequireApproval` is surfaced as inapplicable at a turn boundary
(`user_hooks.rs:~322`). A paid plugin that must ask "verification budget
exhausted, continue?" needs a real answer.

### A7. `max_holds` becomes real

`LoopGrant::max_holds` is declared and read by nothing
(`manifest.rs:120-124`); the live bound is the constant `MAX_STOP_CONSULTS`
(`user_hooks.rs:99`). Clamp the declared value to a host ceiling. A verification
plugin needing four rounds cannot currently ask for them.

### A8. Signals and stages grow to cover the pipeline

`StageName` is 8 of 12 — `verdict`, `reflect`, `contextwrite`, `complete` are
undeclarable (`wrapper.rs:114-131` against `StageKind` in
`crates/stella-protocol/src/event.rs:137+`). `Signal` is 5 values and only
`Triage` publishes any (`wrapper.rs:140-156`), so no condition can read a diff
size, a flip state, a test result, or a budget — which are precisely the
pipeline's live skip conditions (`pipeline.rs:1141-1159`, `:1198`).

Grow both to cover what the pipeline actually branches on. Keep the closed
grammar: `wrapper.rs:16-21` is right that "a Turing-complete condition in a
manifest is a second program with no gate on it."

### A9. Plugin events and the trace fold

A `plugin.<id>.*` namespace in the bus catalog — already contemplated at
`crates/stella-core/src/bus/names.rs:3-4` — plus the fold arm that reads it.

**Plugins do not write traces.** They emit journal events; the trace is a fold
(`crates/stella-cli/src/trace.rs:8-18`). Contributed facts then inherit
replayability, `TRACE_SCHEMA_VERSION` skip-on-unknown, redaction, and the
guarantee that nothing reaches `store.db`. A plugin writing `traces.jsonl`
directly routes around all four.

### A10. A worktree handle that crosses a process

`CandidateWorkspacePort` + `CandidateWorkspace` are 21 methods returning
borrowed trait objects (`crates/stella-pipeline/src/ports/workspace.rs:94-334`).
`after_turn` is defined as "author a witness, run the oracle, read the flip" —
all of which need the candidate worktree, and none of which has a serializable
handle. Custom tools currently pin a child's cwd to the workspace root
(`crates/stella-tools/src/custom.rs:41-42`), not a candidate.

Define the minimum serializable subset: create, root path, run-test, seal,
adopt, remove. The host fences filesystem access; tamper snapshotting stays
host-side, which `TamperPolicy::ArtifactIdentity` (`manifest.rs:187-192`)
already assumes.

---

## 5. Multi-language is a design input, not a later port

The hard requirement is Python and TypeScript plugins. The risk is not that they
are impossible — the subprocess plane already proves they are not — it is that
A3a ships first as a Rust trait, A3b is deferred, and the trait's shape then
dictates a wire protocol it was never designed for.

Three commitments prevent that:

1. **The wire contract is authored in the same PR as the trait.** Not
   implemented in the same PR — authored. If a point cannot be expressed as
   serialized request/response, that is discovered while the trait is still
   editable.
2. **The reference Rust plugin is a client of the wire contract too.** If Rust
   gets a private in-process path that Python cannot use, the wire contract
   becomes a second-class citizen and rots. Rust may *additionally* have an
   in-process fast path, but the wire path must be the one CI exercises.
3. **The transport is chosen by spike, not by argument.** #3246 §O5 asks for the
   Stop-hook path implemented twice — once as a shell hook, once as an MCP
   server with a lifecycle extension — and compared. That spike is a Track A
   deliverable, scheduled before A3.

What is already settled and should not be relitigated: **do not embed CPython**
(#3246 §O5 — the GIL on an async runtime, a per-platform packaging story, and a
concretion inside crates invariant 1 keeps free of them). The SDK is a thin
client over whatever the spike selects.

---

## 6. Why `judge` stays in-process

`doc:turn-loop-wrappers` §9.2 sharpens "`judge` may not call a model" into a
property of the signature: `judge` is synchronous and I/O-free over owned data,
so the rule is enforced by the compiler rather than by review.

An out-of-process `judge` is I/O by construction and destroys that property.
Since Python plugins are a hard requirement, this needs an explicit resolution
rather than a silent one. **The resolution is: plugins declare the verdict rule;
the host evaluates it.**

- A plugin supplies **evidence** out-of-process, in any language, with the
  600s subprocess budget — running a test suite is exactly the workload the
  in-process bus cannot host, and #3246 §O3 is right that out-of-process is the
  *better* substrate for it.
- A plugin supplies its **verdict rule as data** — the closed condition grammar,
  `[requirements]`, and the `[oracle]` flip/tamper policy. Data has no
  programming language, so a Python author and a Rust author write the identical
  artifact.

Three reasons this is the better decision, not merely the conservative one:

1. **The failure it forecloses is the one the project exists to prevent.** A
   verification plugin that quietly calls a model to decide "done" is the worker
   grading its own work, and a passing verdict looks identical either way. Today
   that is impossible by construction.
2. **A compiler-enforced guarantee is free forever; a policed one needs a
   policeman**, who needs tests, and drifts.
3. **The costs are asymmetric and one direction is one-way.** Widening a closed
   grammar later is additive. Re-imposing the guarantee after plugins depend on
   arbitrary verdict code is not possible.

The honest cost: a plugin author cannot write a verdict as a loop in Python.
Given `ladder_decision` is already deterministic and terminal at six outcomes,
the interesting variation lives in what counts as evidence and what done means —
both of which stay open.

**The falsifier, and it should be run before A3 freezes:** express one genuinely
different definition of done — not Vera's — as declarative data. What will not
fit is the evidence for widening the grammar, and it is far cheaper to find
before the socket exists than after.

---

## 7. Track B — extraction order

Each plugin ships behind the flag inversion of #3381 (`--pipeline <variant>`
replacing `--no-pipeline`), with the wrapper id recorded on the executions row
so two variants can be compared. That column and migration already shipped
(`crates/stella-store/src/ddl.rs:115`, `migrations.rs:29`) but the only writer
passes the constant `PIPELINE_VARIANT_CLASSIC`
(`crates/stella-cli/src/agent/persistence.rs:22`, `:45`) — wiring `Wrapper::id`
to it is a Track A tail item, because without it the A/B comparison this whole
plan is justified by cannot distinguish two variants.

Order, easiest and least risky first:

1. **stella-research** — `before_turn` only, read-only, no worktree. The safest
   possible first real plugin.
2. **stella-plan** — `before_turn`, needs the triage signals A8 publishes.
3. **vera** — `after_turn` + `judge`. Needs A10 (worktrees) and A6 (structured
   verdicts). Ported, not copied: see §8.
4. **stella-candidates** — the heaviest, needs `again?` with different setup per
   round.
5. **stella-goal** — folded in last so it stops being a second copy.

**The bar for each:** a side-by-side benchmark holds before the built-in path is
deleted. The dependency cut — `stella-cli` no longer declaring
`stella-pipeline`, today 166 references across 41 files — is the **last** slice,
never the first.

---

## 8. Vera specifically

Vera is a port, not a lift. It takes the verification nucleus and leaves the
orchestration.

**Ports** (pure functions over owned data, which is what makes this realistic):
`crates/stella-pipeline/src/verify.rs` (`FlipOracle`, `FlipState`,
`ladder_decision`, the evidence builders, `strip_witness_hunks`),
`witness.rs` (prompt construction, `parse_test_invocation`, `runner_probe`, the
three acceptance validators), `witness/airlock.rs`, `flip_halt.rs`, and
`reward.rs` for the training labels.

**Carry over first:** the property test
`flip_requires_a_prior_failing_observation`. A flip with no prior failing
observation is not a flip.

**Lift, do not copy:** tamper exclusion currently lives inside
`Pipeline::verify_candidate` in the god file `src/pipeline.rs` rather than beside
the oracle.

**Vera contributes model roles** to the `/models` table — the worker whose output
is judged, and the independent verifier that authors the witness. Verifier
independence becomes Vera's invariant to enforce; the roster already refuses a
responsibility whose agent is the worker's (`roster.rs:656-660`). Blocked on
#3472 (the role table must be plugin-populated; `EngineRole` is a closed
six-variant enum at `crates/stella-tui/src/envelope.rs:993`).

**Net-new, not ported:** the durable flip record. Today a `LadderSnapshot` rides
inside `AgentEvent::Verdict`, but there is no dedicated flip-transition event and
the `verdict` tag's consumer posture is `Unclassified` (#2703) — nothing is
declared to read it. Vera owns a declared flip record whose named consumer is
the fine-tuning corpus. A verification signal nothing reads is the exact failure
mode this project exists to end.

---

## 10. Definition of done for the whole plan

- `stella-cli/Cargo.toml` does not declare `stella-pipeline`.
- Core ships zero built-in wrappers and one role (`default`); every other role
  in `/models` is contributed by an installed plugin and disappears when it is
  removed.
- `stella plugin install` works for a plugin written in each of Rust, Python and
  TypeScript, and CI runs all three on every PR.
- A side-by-side benchmark holds for the plugin path against the built-in path
  before each built-in is deleted.
- `judge` remains synchronous and I/O-free; no configuration restores a model
  verdict.
- Plugin-contributed facts appear in traces via the journal fold, and a
  fine-tuning export can join trace to oracle flip.
- `make gate` green throughout; no baseline entry added to
  `scripts/file-size-baseline.txt` to accommodate this work.

---

## 11. Risks

- **The socket ships Rust-shaped.** Mitigated by §5's three commitments. This is
  the risk most likely to be discovered too late.
- **`judge` gets relitigated per-plugin** rather than settled once. §6 is meant
  to be cited, not re-argued.
- **The pipeline is deleted before the plugin is as good.** The benchmark bar in
  §7 exists for this; a five-task solve rate cannot resolve it, so comparisons
  need repeats per task.
- **Extraction stalls half-done**, leaving two code paths and the drift #3380
  already identifies between goal mode and the pipeline. The flag inversion is
  the forcing function: if inverting the default feels unsafe, the extraction is
  not finished.
