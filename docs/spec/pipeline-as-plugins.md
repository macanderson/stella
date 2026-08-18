---
id: pipeline-as-plugins
title: "Every wrapper is a plugin: the full extraction plan"
status: living
---

# Every wrapper is a plugin: the full extraction plan

**Status:** living, updated 2026-08-18. First written 2026-08-17 as a
completion plan; each item below is tracked to landed/open individually as it
ships, which is the reason this reads as "proposed" in no single place — all
of Track A (A1–A10) and Track D's D1 have landed (#3479, building on
ce9c7cd/aabb8d4/eb1fe9e/ed18283/9672787/5c5c325), A3's live driver landed
separately (#3494/#3672, below), Track B has begun (`stella-research` is the
first extraction — see §7 and `plugins/stella-research`), and Track C, the
rest of D, and the remainder of B remain the plan as written. §2's "governing
gap" paragraph is left in place as history, with a correction above it,
because it is the clearest record of what A3–A5/A8–A10 actually closed.

**Update, 2026-08-18: the two moves §2 still called open have both shipped.**
`stella-cli` now drives a live turn through the wrapper socket
(`crates/stella-cli/src/wrapper_plugin.rs`, #3494) — §4 A3's "None of the
three drivers calls `TurnWrapper` yet" is no longer true of `stella run`, only
of `stella-serve` and an embedded `stella-engine` host — and the flag
inversion §7 named as a precondition has landed: the raw step-loop is the
default on every door, `--pipeline <variant>` is the sole opt-in, and
`--no-pipeline` is a deprecated no-op (#3381, PR #3694). Separately,
`stella-pipeline` itself now runs its stage order from the shipped
`[wrapper]` manifest instead of hardcoded branches (#3408, PR #3672) — a
different mechanism from the wrapper socket, not this crate becoming a socket
consumer. See §4 A3 and §7 for the corrected detail.

`doc:turn-loop-wrappers` established the direction — one loop, six doors,
wrappers are plugins — and sequenced its first three moves. This document is the
completion plan: what remains until **every part of the staged pipeline, and
self-driving, runs as a plugin outside core Stella**, with the plugin surface
proven in three languages rather than asserted in one.

Everything said here about today's code was read out of the tree, and the file
is named so it can be checked. Where a claim is an inference rather than an
observation, it says so.

---

## 0. The architectural north star

**Stella is a loop that drops into any application and runs over any surface —
embedded as a library, over HTTP, from a CLI.** Everything below is judged
against that sentence. Where speed and soundness conflict, speed wins on the
things a later commit can fix, and soundness wins on the seams — because a seam
is what a later commit cannot cheaply change.

Four consequences, and they are acceptance criteria, not aspirations:

1. **The wrapper socket must not assume a host.** A wrapper that only works when
   a terminal, a git workspace, or `stella-cli`'s process is present is not a
   socket, it is a CLI feature. The test: can `stella-serve` drive the same
   wrapper an embedded library does?
2. **The wire contract is the primary contract; in-process is an
   optimisation.** This is the same conclusion §5 reaches for Python and
   TypeScript, arrived at from a different direction — a loop driven over HTTP
   and a plugin spoken to over a pipe need the same thing: the loop's
   participation points expressed as data rather than as Rust borrows. Building
   the wire contract twice, once for hosts and once for plugins, is the failure
   to avoid.
3. **No ambient authority anywhere on the path.** `stella-serve` already proves
   the shape — its engine holds no credentials and remotes every model and tool
   call to its host. A wrapper must be handed its capabilities, never reach for
   them. This is what makes the loop embeddable in an application whose
   filesystem, credentials and lifecycle belong to somebody else.
4. **`stella-engine` and `stella-serve` are first-class consumers of this
   work**, not surfaces to update afterwards. If a wrapper lands that
   `stella-cli` can drive and they cannot, the seam is wrong and the fix is in
   the seam.

The failure mode this section exists to prevent is subtle and common: the
extraction succeeds, every plugin works, and the socket has quietly grown a
dependency on the CLI's process model — so the loop is excellent and only
embeddable in one thing.

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
| **D — Self-driving** | The autonomous loop leaves the binary | Its first step depends on nothing; its last depends only on A1 |

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
  a host-supplied `stop_hold_allowance`, clamped to `STOP_HOLD_CEILING` (8) and
  defaulting to `DEFAULT_STOP_HOLDS` (3) when no host asks
  (`crates/stella-core/src/driver/config.rs:241`, `:250`, `:258`) — this is A7,
  **landed** (aabb8d4; §4 below), which retired the `MAX_STOP_CONSULTS`
  constant this bullet used to cite — spent *before* the hooks run, and
  the final round announces itself (`STOP_FINAL_ROUND_NOTE`). It is
  fail-open by design (`:55-59`) — a hook that fails never blocks, because
  failing closed here means never completing.
- Subprocess hooks: JSON on stdin, a `HookDecision` on stdout, 60s default and a
  600s ceiling (`crates/stella-core/src/hooks.rs:75`). **This is the
  any-language plane, and it works now.**
- `TurnLane::Plugin(LaneId)` (`crates/stella-protocol/src/lane.rs:165`).
- The manifest grammar: participation ladder, `LoopGrant`, `[oracle]`,
  `[requirements]`, `[subloop]`, `[roles]`, and a closed condition grammar with
  a load-time stage-graph check (`crates/stella-plugin/src/{manifest,wrapper}.rs`).

**Update, 2026-08-17: Track A landed (#3479), and the paragraph below is what
it read like before that.** Kept for the record, because it names precisely
the gap A3–A5, A8–A10 closed. `stella-plugin` no longer has zero consumers —
`stella-runtime`, `stella-cli` and `stella-pipeline` all consume it now, per
`stella-plugin`'s own README "Consumers" section — and `.stella/plugins/`
resolution, `stella plugin install|list|remove`, and a `TurnWrapper` trait
with two working transports all exist.

**Update, 2026-08-18: a live turn is now driven through the socket.** The
gap the paragraph above named — "nothing drives a live turn through the
socket yet" — is closed for one of the socket's three intended drivers:
`stella_runtime::WrapperDispatch` (`crates/stella-runtime/src/wrapper/dispatch.rs`,
landed #3494) is the host sequence that calls `before_turn`/`after_turn`/
`judge`/`again?` in order, and `stella run --pipeline <variant>`
(`crates/stella-cli/src/wrapper_plugin.rs`) drives it — an installed
plugin's manifest is no longer only loaded and shown for consent, it
participates in a real turn and its id reaches `executions.pipeline_variant`.
`plugins/stella-research` is the first plugin exercising this path (§7).
What remains open, precisely: `stella-serve` over HTTP and a minimal embedded
`stella-engine` host — §0's other two acceptance-test drivers — do not call
`WrapperDispatch` yet (#3551), and a named `--pipeline <variant>` plugin is
refused on `stella goal`/`stella fleet`, which drive their own round loops
rather than `WrapperDispatch` (#3695).

> *Original text, now historical:* "`stella-plugin` has **zero consumers**.
> Nothing calls `PluginManifest::from_toml_str`, there is no `.stella/plugins/`
> resolver, no install command, no loader. `crates/stella-plugin/src/wrapper.rs:29-33`
> says it plainly — a `StageName` is 'a declared name that load-checks, not a
> dispatch target'. So the manifest describes a socket that does not exist.
> Track A builds the socket; everything else is downstream of it."

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

**LANDED (ce9c7cd).** `Principal` gained a fifth variant,
`Plugin(String)` — "an installed plugin, named by its manifest `name`"
(`crates/stella-core/src/ports/authz.rs:76-77`) — distinct in shape and
meaning from `Host(String)` exactly as this section required, plus a
render of its grant for install consent. What follows is the reasoning that
produced that addition, kept for the record.

**Before this landed, Stella had no name for a plugin.** The authority vocabulary landed —
`Principal` (`crates/stella-core/src/ports/authz.rs:62`), `AuthzGate` (`:244`),
`RiskLevel` and `ToolContract` (`crates/stella-protocol/src/contract.rs:68`,
`:197`) — and `Principal` has four variants: `User`, `Role`, `SubAgent`, `Host`.

**Correction, 2026-08-17.** An earlier draft of this section said "every call
site passes the constant `Principal::User`" and concluded that Stella "cannot
tell a plugin apart from the human". The narrow claim holds — the three
`crates/stella-cli/src/agent.rs` sites (`:362`, `:1657`, `:1672`) do pass the
constant — but the conclusion drawn from it does not, and the difference changes
what A1 has to build. Read out of the tree:

- `Principal::SubAgent` is constructed from a real dispatch id at
  `crates/stella-cli/src/subsession.rs:732` and `crates/stella-cli/src/fleet_cmd.rs:729`;
- `Principal::Role` is constructed at `crates/stella-cli/src/candidate_ws.rs:498`;
- `Principal::Host` is constructed from the serve wire at
  `crates/stella-serve/src/routes.rs:118`.

So the principal **is** threaded through dispatch already, and the gate already
sees a caller that is not the human. What is missing is narrower and sharper:
**there is no `Principal` variant that means "an installed plugin", so a loader
would have nothing honest to construct.** A1 is therefore an addition to the
vocabulary plus its consent surface, not the threading exercise the earlier
draft described — which makes it smaller than stated and no less blocking.

`Principal::Host(String)` (`authz.rs:74-76`) is the right *shape* to copy —
opaque, so core grows no opinion — but it is the wrong *meaning* to reuse: a
host is the process embedding Stella, and a plugin is something that process
installed. Collapsing them would make "the embedder" and "a thing the embedder
installed" indistinguishable to a gate, which is the same defect one level up.

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

**LANDED, both halves (#3479).** The four points are defined in
**`stella-runtime`** — not `stella-core`, because `before_turn` performs recall
and `after_turn` spawns processes, and invariant 2 bans I/O in the engine
(`doc:turn-loop-wrappers` §9.1).

Two halves, and they shipped together:

- **A3a — the in-process contract.** The `TurnWrapper` trait
  (`crates/stella-runtime/src/wrapper/mod.rs`), plus `[loop] points` — the
  manifest vocabulary for the four points `[wrapper]` was missing when this
  item was written — and `LoopGrant::permits_point` as its filter (folded into
  A4 below).
- **A3b — the wire contract.** `crates/stella-plugin/src/wire.rs`: the same
  points as serialized request/response, so a plugin can be a separate
  process, plus `docs/wire/wrapper.wire.json` as the generated, gate-checked
  corpus (`doc:wrapper-socket` §3 commitment 2).

**Update, 2026-08-18: the host sequence has landed too (#3494), and one of
the three drivers now calls it.** `stella_runtime::wrapper::WrapperDispatch`
(`crates/stella-runtime/src/wrapper/dispatch.rs`) is the `dispatch` function
this item said did not exist: it resolves the declared stage program, calls
`before_turn` per stage, hands a `TurnPrelude` to the host's `TurnDriver`,
calls `after_turn`, and settles with `judge` + `again`, looping while the
verdict asks for another turn. `stella-cli` is the first driver —
`crate::wrapper_plugin` (`crates/stella-cli/src/wrapper_plugin.rs`)
implements `TurnDriver` over one raw `stella run` turn, resolved by
`--pipeline <variant>` — so `TurnWrapper` is no longer only proven against a
test harness's own round loop
(`crates/stella-runtime/tests/wrapper_socket.rs`); it is proven again against
the shipped `WrapperDispatch` itself
(`crates/stella-runtime/tests/wrapper_dispatch.rs`) and driven end-to-end by
`stella-cli` (`crates/stella-runtime/tests/research_plugin_dispatch.rs`,
exercising `plugins/stella-research`).

**What "landed" still does not cover, stated so this is not read as more
finished than it is.** §6's acceptance test — the *same* plugin driven
unchanged by `stella-cli`, `stella-serve`, and an embedded `stella-engine`
host — is only one third proven: `stella-serve` and an embedded host neither
call `WrapperDispatch` yet (#3551). And on `stella-cli` itself, only `stella
run` is a real `TurnWrapper` driver — `stella goal` and `stella fleet` each
drive their own round loop (a judged goal round, a fleet worker's attempt),
and a named `--pipeline <variant>` plugin is refused on either door rather
than silently downgraded (#3695). Track B (§7) is where the first real
plugin — `plugins/stella-research` — actually runs against this driver.

### A4. A loader

**LANDED (#3479).** `.stella/plugins/` and `~/.stella/plugins/` resolvers,
`stella plugin install|list|remove`, and the install consent prompt all ship
in `crates/stella-cli/src/plugin_cmd.rs` + `plugin_cmd/{roster,process}.rs`,
gated on `project_code_execution_trusted()` for the project tier (#3509,
below). `LoopGrant::permits_hook` and `LoopGrant::permits_point` are both
consulted as routing filters.

**Update, 2026-08-18: the loader now also drives a turn, on `stella run`.**
`crate::wrapper_plugin` (`crates/stella-cli/src/wrapper_plugin.rs`, #3494)
resolves `--pipeline <variant>` against `PluginRoster`'s output and hands
what it resolved to `stella_runtime::WrapperDispatch`, so a manifest is no
longer resolved, shown for consent, and stopped there — an installed
plugin's declared points are dispatched into a live turn. What remains true
of the loader itself is narrower: `stella plugin install|list|remove` resolve
and show consent, and installation is deliberately decoupled from execution
— nothing about the install command itself drives a turn, and it should not,
since a plugin can be installed for `stella run --pipeline <variant>` without
that command ever running one.

**Uninstall must actually uninstall.** Hook settings currently *concatenate*
across scopes and no scope can remove another's entries
(`crates/stella-cli/src/settings/merge.rs:47`, `:171-172`). Correct for operator
hooks, wrong for plugins.

**The manifest file is `plugin.toml`, exactly** — one file, that name, at the
root of the plugin's directory; a directory without one is not a plugin and is
skipped rather than reported as broken. This was unwritten until #3501 while the
loader and every published example happened to agree on it, which is not
agreement: a third party's plugin discovers a disagreement here by silently not
loading, and "I installed it and nothing happened" is the failure this whole
crate exists to prevent. The loader's constant is
`crates/stella-cli/src/plugin_cmd/roster.rs::MANIFEST_FILE`, pinned by
`the_manifest_filename_is_the_specified_one`, and this paragraph is the
normative half — if the two ever disagree, this one is right and the loader is
the bug.

**An undeclared point is never dispatched**, the same way an undeclared hook is
never invoked. `[loop] points` names the wrapper socket points a plugin answers
and `LoopGrant::permits_point` is the filter (#3501 item 2); before it, a host
learned that a wrapper answers `after_turn` and refuses `before_turn` by getting
the refusal at run time. An `[oracle]` must declare `after_turn`, because that
is the one response its evidence rides on — a manifest that omits it buys the
most expensive grant in the ladder and can never reach a verdict.

### A5. A process declaration in the manifest

**LANDED (#3479).** `[runtime]` exists (`crates/stella-plugin/src/runtime.rs`):
`argv`, `timeout_secs`, and a **default-deny** environment allowlist, modelled
directly on `OracleCommand`. Deliberately **not** a `language` field — `argv`
already expresses `["python3", "${plugin_dir}/main.py"]` and
`["node", "${plugin_dir}/main.js"]` without Stella learning what a language is.

**`[oracle] command` is optional once `[runtime]` exists**, exactly as this
item asked (#3501 item 1): `PluginManifest::oracle_process()` resolves the two
declarations into one `OracleProcess` and reports which source it came from
(`OracleProcessSource::OracleCommand` or `::Runtime`), so a host never learns
which shape an author chose.

### A6. Structured verdicts

**LANDED (aabb8d4, eb1fe9e).** `HookDecision::Deny` used to carry only a
`String`. It now carries `Denial` (`crates/stella-protocol/src/denial.rs`):
`reason` plus an optional `DenialEvidence` naming the witness, the command,
`FlipOutcome`, and the artifact digest — exactly the four facts this item
asked for. The driver reads it structurally rather than by parsing prose
(`crates/stella-core/src/driver/user_hooks.rs:134-137`, `:379` emits the
evidence into the `HOOK_STOP_BLOCKED` journal payload rather than only the
rendered string). **Not independently verified here:** whether a trace fold
in `stella-cli` already consumes that structured evidence, or only the
journal payload exists so far — check before citing that half as done.

While here: `RequireApproval` is **still** surfaced as inapplicable at a turn
boundary (`user_hooks.rs:382-395`, renumbered from `:327-329`) — that half of
this item did not land. A paid plugin that must ask "verification budget
exhausted, continue?" still has no real answer; the code's own comment defers
it to the wrapper socket (#3380).

### A7. `max_holds` becomes real

**LANDED, engine side (aabb8d4).** The constant `MAX_STOP_CONSULTS` this
section named is gone from the tree entirely — `grep` for it now returns
nothing. In its place, `EngineConfig::stop_hold_allowance` is a host-supplied
`Option<u32>` read through `EngineConfig::stop_holds()`
(`crates/stella-core/src/driver/config.rs:241`, `:273-279`) and clamped to
`STOP_HOLD_CEILING` (8) by `clamp_stop_holds`, defaulting to
`DEFAULT_STOP_HOLDS` (3) when a host asks for nothing — `user_hooks.rs:353`
consults it directly. A verification plugin's four-round ask is representable
and consulted, exactly as this item asked.

**Not landed: the wiring from a manifest to that field.** Nothing in
`stella-cli` yet reads a `LoopGrant::max_holds` off an installed plugin's
manifest and sets `stop_hold_allowance` from it — because A4 (the loader) has
not landed, there is no installed manifest to read from. `config.rs`'s own doc
comment states the intended caller ("the *host* reads `LoopGrant::max_holds`
off a manifest, clamps it … and sets this field"); that caller does not exist
in the tree yet.

### A8. Signals and stages grow to cover the pipeline

**LANDED (#3479).** `StageName` is now all 12 (`Triage`, `Recall`, `Research`,
`Plan`, `Scope`, `Execute`, `Witness`, `Verify`, `Verdict`, `Reflect`,
`ContextWrite`, `Complete` — `crates/stella-plugin/src/wrapper.rs`), matching
`StageKind` in full. `Signal` grew from 5 to 15, and publication grew with it:
`Execute` publishes `MutatingActions`/`DiffLines`, `Witness` publishes
`WitnessAuthored`, `Verify` publishes `FlipAchieved`/`TestsRed`/`TestsGreen` —
so a condition can now read a diff size, a flip state and a test result, which
were exactly the gaps this item named. `BudgetMetered` is a host-published
signal rather than stage-published (`Signal::publisher` returns `None` for
it), which still closes the same gap from the other side. The closed grammar
held throughout: nothing here admits a Turing-complete condition, matching the
rule this item was written to protect.

### A9. Plugin events and the trace fold

**LANDED (ed18283).** `PLUGIN_NAMESPACE_PREFIX = "plugin."` is reserved in the
bus catalog (`crates/stella-core/src/bus/names.rs:284`, with
`plugin_event_name`/`plugin_id_of` validating and parsing it), and
`crates/stella-cli/src/trace.rs` gained the fold arm: `TraceRecord::plugin_facts:
Vec<PluginFact>` (`trace.rs:130-136`, `:195-207`), folded from `plugin.<id>.*`
journal events (`trace.rs:351-354`) — present and empty, never omitted, when
no plugin ran. Both halves this item asked for exist.

(Correction, 2026-08-17: an earlier draft said this namespace was "already
contemplated at `crates/stella-core/src/bus/names.rs:3-4`". It is not — the
string `plugin` does not appear in that file at all. What lines 3-4 actually
say is the weaker, general "Extensions may emit custom names — the catalog is
the contract for what the host emits, not a closed set", which permits the
namespace without contemplating it. The namespace is entirely net-new.)

**Plugins do not write traces.** They emit journal events; the trace is a fold
(`crates/stella-cli/src/trace.rs:8-18`). Contributed facts then inherit
replayability, `TRACE_SCHEMA_VERSION` skip-on-unknown, redaction, and the
guarantee that nothing reaches `store.db`. A plugin writing `traces.jsonl`
directly routes around all four.

### A10. A worktree handle that crosses a process

`CandidateWorkspacePort` + `CandidateWorkspace` are 19 methods returning
borrowed trait objects (`crates/stella-pipeline/src/ports/workspace.rs:94-335`
— 18 on `CandidateWorkspace`, one `create` on the port).
`after_turn` is defined as "author a witness, run the oracle, read the flip" —
all of which need the candidate worktree, and none of which has a serializable
handle. Custom tools currently pin a child's cwd to the workspace root
(`crates/stella-tools/src/custom.rs:41-42`), not a candidate.

Define the minimum serializable subset: create, root path, run-test, seal,
adopt, remove. The host fences filesystem access; tamper snapshotting stays
host-side, which `TamperPolicy::ArtifactIdentity` (`manifest.rs:187-192`)
already assumes.

**LANDED (9672787).** `CandidateOp::ALL` (`crates/stella-protocol/src/candidate.rs:121-141`)
is exactly the six operations named above — `Create`, `Root`, `RunTest`,
`Seal`, `Adopt`, `Remove` — and `crates/stella-pipeline/src/ports/handle.rs`'s
`CandidateHandles` is the registry that mints a handle and answers each one by
delegating to the existing `CandidateWorkspacePort`, fenced both lexically and
on disk as this item required, with tamper snapshotting staying host-side.
**Landed with a stated gap, not a silent one:** the module's own doc says
"nothing on the shipping path constructs a `CandidateHandles` today" — the
consumer is the wrapper socket's `after_turn` dispatch (A3), which does not
exist yet — and names the seam that will wire it up as #3485.

**Host-side now means the plugin cannot even say it (#3499).** "Snapshotting
stays host-side" was true of the *work* and false of the *wire*: the evidence a
plugin returned carried a `tamper` field, so an honest plugin declared a policy
it could not execute, answered `not-checked` because that was the only true
thing it could say, and every verdict abstained — which all three of Track C's
reference plugins hit identically, in three languages. The field is now split:
`ObservedEvidence` (`crates/stella-plugin/src/observed.rs`) is what a plugin
returns and has no tamper field in any language, and the host merges its own
finding through `EvidenceSet::from_observed` before `judge` runs. A plugin that
sends one anyway is refused by name rather than believed. Witness:
`crates/stella-runtime/tests/host_owned_tamper.rs`.

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

### 6.1 The falsifier, run

Run on 2026-08-17, before A3. **The decision held; the grammar did not, in two
places, and both were widened rather than reopened.**

**What was expressed: a performance budget.** Done means a named benchmark's
p50 did not regress past a recorded baseline — the check a team reaches for the
week after an all-green test suite shipped a 3x regression. It was picked over
the coverage-floor and schema-compatibility candidates because it is the one
whose verdict is a *relation between two measurements* rather than a property
of the post-state, so it stresses the grammar in three independent places at
once instead of one. The manifest is
`crates/stella-plugin/tests/fixtures/perf-budget.toml` and the exercise is
executable, not a memo: `crates/stella-plugin/tests/non_witness_done.rs`.

**What fit, unchanged.** The `[loop]` ladder carried it exactly: this is an
`arbiter`, it binds the Stop gate, it declares `max_holds = 2`.
`[requirements]` carried the definition of done as two named, enumerable
clauses. `[oracle]`'s command/timeout carried the evidence-gathering process —
a benchmark run is precisely the workload the in-process bus cannot host, which
is §6's own argument arriving intact from a completely different plugin. And
`tamper = "artifact-identity"` transferred with no change of meaning and turned
out to be load-bearing for a reason nobody designed it for: the recorded
baseline is the "before" half of every comparison, so a worker that rewrites
`benches/baseline.json` wins the budget without touching the code.

**What did not fit — the unflattering half.**

1. **Every oracle in the grammar was a witness oracle.** `flip` is a required
   field with a single variant, `"required"`, so the manifest could state
   exactly one definition of done: a red test went green. A benchmark passes
   before *and* after; what changes is a number. A performance plugin could
   only load by writing `flip = "required"` about a flip that does not exist.
2. **A threshold had nowhere to live.** `[requirements]` values are human
   prose, so "at most 5% slower" could only exist as a constant inside the
   oracle binary. That is precisely the arrangement §6 rejects, arriving by a
   side door: the plugin would be deciding done, the budget would be invisible
   at install, and a change to it would be invisible in review.

**What was widened** (both closed, both load-validated, both with tests):

- **`flip = "not-applicable"`** — this oracle's evidence is not a transition.
  It is *stricter*, not an escape hatch: with no flip to decide anything, every
  requirement must be decided by a declared check, or the manifest is refused
  (`UndecidableRequirement`). Vera's manifest is untouched and still says
  `required`.
- **`[oracle] measurements` + `[[oracle.checks]]`** — the oracle declares the
  names of the numbers it reports, and each check states one rule over one of
  them in the same closed comparison grammar `[wrapper]` conditions already
  use (`<measurement> <op> <integer>`). A check reading an undeclared
  measurement is a load error, and a number the oracle failed to report is an
  error at evaluation, never a satisfied budget. `Oracle::unmet` is the pure
  evaluator, and `consent_text` prints the budget under the requirement it
  decides — a rule a user cannot read before installing is not meaningfully
  declared.

The measurement namespace is the **plugin's**, not the host's, which is the one
deliberate asymmetry with `Signal`: the host cannot enumerate every benchmark
anyone will ever budget. What stays closed is what matters — the comparison
vocabulary, the shape of a rule, and the requirement that every name a rule
reads was declared in the same manifest a human consented to.

**Left as friction, not widened.** The literals are non-negative integers, so a
fractional or signed budget must be declared in an integer unit (the fixture
reports *percent of baseline*, which costs sub-percent resolution). A float in
a completion gate brings `NaN` — under which every comparison is false and a
broken oracle silently *passes* a `<=` budget — so the widening was not made on
the falsifier's say-so; #3488 carries the decision. Also unstated: the
provenance of a baseline (that it is the one from the merge base, not merely
unmodified since), and any quantifier over a set the host does not know, such
as "no changed file is at zero coverage". The general shape of that second
limit is worth naming, because it bounds what this grammar will ever do: **it
carries a verdict over an aggregate the oracle computes, not a quantifier the
host evaluates.** That is a real constraint and it is also the reason the
decision survives — the plugin chooses what to measure, and the manifest, which
a human reads and a reviewer diffs, decides what counts as done.

**What #3511 changes here, and what it does not.** The 2026-08-17 product
decision (#3511, settled as Option 2) settles that Vera — the paid,
Oxagen-owned verification plugin this document plans for — reports its own
evidence rather than being host-verified, and says so plainly in the manifest
and install consent text instead of leaving it implied. That is a distribution
and messaging correction, not a reopening of this section's argument: `judge`
still stays synchronous, I/O-free, and total, and a plugin still cannot grade
its own work by calling a model inside the verdict check, because that is a
property of the type signature this section fixes, not a promise the plugin
keeps. What was already true here — "a plugin supplies evidence
out-of-process" — is exactly what #3511 makes explicit: the oracle's evidence
was always plugin-produced, and a host that let a manifest suggest otherwise
(that the oracle's finding was host-run) was the gap #3511 closes, not
`judge`'s purity. This is the plugin path specifically — it does not withdraw
the guarantee from the built-in staged pipeline (§7, §8), which still runs its
own check and watches the flip; see `AGENTS.md`'s opening description for the
corrected, path-scoped wording.

---

## 7. Track B — extraction order

**The flag inversion of #3381 shipped (PR #3694), and so did the other half
of this paragraph's plan.** `--pipeline <variant>` replaced `--no-pipeline`
as the sole opt-in on every door: the raw loop is the default, and
`--no-pipeline` is a deprecated no-op. The wrapper id recorded on the
executions row so two variants can be compared shipped too: the column and
migration (`crates/stella-store/src/ddl.rs:115`, `migrations.rs:29`), the
`classic` path writing it (`crates/stella-cli/src/agent/persistence.rs:26`,
via `begin_pipeline_execution`), and — landed alongside the wrapper-plugin
driver (#3494) — the raw loop's named-plugin path writing it too:
`crates/stella-cli/src/wrapper_plugin.rs`'s `RawTurnDriver::run_turn` opens
its execution row through `TurnDoor::new("run").wrapped_by(self.variant)`,
where `self.variant` is the resolved plugin's declared `[wrapper] id`
(`bound.variant()`), so a `--pipeline <variant>` run is distinguishable from
an unwrapped raw run by a `GROUP BY pipeline_variant` today.

Order, easiest and least risky first:

1. **stella-research** — `before_turn` only, read-only, no worktree. The safest
   possible first real plugin. **Landed** — `plugins/stella-research`
   (`doc:pipeline-as-plugins-execution` Phase 4), driven end-to-end by
   `WrapperDispatch`, graded against committed vectors in
   `crates/stella-runtime/tests/research_plugin_*.rs`, and distinguishable in
   the store per the paragraph above.
2. **stella-plan** — `before_turn`, needs the triage signals A8 publishes.
3. **vera** — `after_turn` + `judge`. Needs A10 (worktrees) and A6 (structured
   verdicts). Ported, not copied: see §8.
4. **stella-candidates** — the heaviest, needs `again?` with different setup per
   round.
5. **stella-goal** — folded in last so it stops being a second copy.

**The bar for each:** a side-by-side benchmark holds before the built-in path is
deleted. The dependency cut — `stella-cli` no longer declaring
`stella-pipeline`, 187 references across 45 files as of 2026-08-18 (`grep -rn
stella_pipeline:: crates/stella-cli/src crates/stella-cli/tests`; it was 166/42
when this section was written and has grown with the flag-inversion and
schedule-manifest work, not shrunk) — is the **last** slice, never the first.

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

## 9. Track C — proving three languages in `stella-examples`

`macanderson/stella-examples` is public and already organised by capability —
`agents/`, `commands/`, `fleet/`, `hooks/`, `mcp/`, `memory/`, `rules/`,
`scripting/`, `settings/`, `skills/`, `tools/`. A `plugins/` directory slots in
beside them.

Today every executable example in that repo is a shell script
(`hooks/scripts/{guard-bash,log-tool-use,session-context}.sh`,
`scripting/{ci-autofix,nightly-goal}.sh`). So a Python plugin and a TypeScript
plugin are genuinely new artefacts, and that is the point: they are the proof,
not the documentation.

### The three reference plugins

All three implement the **same** plugin so they can be diffed against each
other — a verification plugin that runs a test command in `after_turn`, reports
the flip as evidence, and declares its verdict rule as data. Small enough to
read in one sitting, real enough to be non-trivial.

| Path | Language | Demonstrates |
|---|---|---|
| `plugins/verify-rs/` | Rust | the reference implementation, and the in-process fast path |
| `plugins/verify-py/` | Python | no SDK beyond stdlib; `argv = ["python3", "${plugin_dir}/main.py"]` |
| `plugins/verify-ts/` | TypeScript | `argv = ["node", "${plugin_dir}/dist/main.js"]`, with the build step shown |

Each directory carries its manifest, its entrypoint, a README explaining what
the plugin does and how to install it, and a test.

### The rules that keep it honest

1. **Identical manifests except `[runtime].argv`.** If the Python plugin needs a
   different manifest shape than the Rust one, the abstraction has leaked and
   that is a bug in Track A, discovered here.
2. **The Rust example uses the wire path in CI.** It may additionally have an
   in-process path, but if only Rust can reach a capability, the wire contract
   is second-class and will rot (§5.2).
3. **No SDK in the first cut.** Python and TypeScript should work with the
   standard library and a JSON parser. An SDK is a convenience added once the
   protocol is stable — if a plugin *cannot* be written without an SDK, the
   protocol is too complicated.
4. **CI runs all three on every PR**, in `stella-examples` and as a smoke check
   in `stella` itself, so a protocol change that breaks a non-Rust plugin fails
   the PR that made it rather than being discovered by a user.

### Why this is a hard requirement rather than a nice-to-have

A plugin surface exercised only by its authors' language is a library with
extra steps. The adoption case is explicit: developers who do not write Rust
will not adopt a Rust-only extension surface, and the marketplace has nothing to
sell if every listing must be compiled from Rust. Track C is the test that
Track A actually delivered a platform.

---

## 10. Track D — self-driving leaves core

Self-driving used to differ from the pipeline in one respect that mattered: it
was genuinely in `stella-core`. That is no longer true. D1 below — moving the
deterministic decision core to a new leaf crate, `stella-autonomy` — **landed
in 5c5c325**: `stella-core/src/lib.rs` no longer declares a `self_driving`
module, `crates/stella-core/src/self_driving.rs` is gone, and the pure logic
now lives at `crates/stella-autonomy/src/lib.rs` (958 lines) with its property
tests in `crates/stella-autonomy/src/tests.rs`. The pipeline left core long ago
in the same way (core declares no `stella-pipeline` and holds no witness,
ladder or flip code); self-driving's decision core has now joined it. The rest
of this section — D2 through D6 — is the plan as written; only D1 and the
framing above it are updated to match the tree.

### What is there now

The feature is split three ways, and the split is good:

| Layer | Where | What |
|---|---|---|
| Decision | `crates/stella-autonomy/src/lib.rs` (leaf crate, no workspace-crate dependencies) | The AIMD controller sizing a cycle to its machine, the aperture ladder, the dry-streak oracle, digest normalization for the dedup set, the ledger folds. Pure, synchronous, owned data, property-tested. |
| I/O | `crates/stella-cli/src/self_driving_cmd.rs` (+ `self_driving_cmd/probes.rs`, `self_driving_cmd/state.rs`) | Machine probes, state files, process spawning. Feeds results in, writes decisions out. Now depends on `stella-autonomy` rather than on `stella-core` for this logic. |
| Observation | `crates/stella-observatory/src/self_driving.rs` | Read-only fold over `~/.stella/self-driving/<slug>/` JSONL, now also sourced from `stella-autonomy` instead of a private copy. Never writes. |

There is also a shell driver (`scripts/self-driving.sh`) that delegates its
decisions to the CLI verbs rather than carrying a second copy (#1548), a gate
step `self-driving-test` (`scripts/test-self-driving.sh`) covering the control
logic hermetically, and a daemon path (`crates/stella-cli/src/daemon/`).
Two specs already exist: `doc:self-driving-missions` and
`doc:self-driving-foundry`.

### Why it was in core, and why that premise stopped holding

The module doc gave the reason plainly: the deterministic half lived in core so
"the model never has to re-derive it and cannot get it subtly wrong", and
keeping it free of I/O is what made it property-testable.

That reasoning was sound and the code was good. What changed was the premise —
core is meant to be a bare loop with minimal tools and one model, and an
opinionated perpetual-delivery policy is not part of a bare loop. The AIMD
controller and the aperture ladder are exactly the kind of policy Vera is
leaving for. The purity that justified its original placement was a property
of the code, not of the crate it sat in: it stays property-testable in
`stella-autonomy`, a leaf on the `stella-diff` / `stella-home` pattern, exactly
as D1 below specified.

### The honest problem: granularity

The wrapper contract is defined **per turn** — `before_turn`, `after_turn`,
`judge`, `again?`. Self-driving is an **outer** loop over whole runs: a cycle is
fix-a-batch → audit → file → benchmark → ship, and each of those is many turns.

So self-driving does not simply drop onto the four points, and this plan should
not pretend otherwise. Three options, and this needs a decision rather than a
default:

1. **`again?` at run granularity.** Treat a self-driving cycle as the unit and
   let the plugin request another one. Requires the socket to admit a second
   granularity, which is a real addition to #3380 rather than a use of it.
2. **A host-verb plugin.** Self-driving does not participate in a turn at all —
   it *drives* Stella from outside, the way `scripts/self-driving.sh` already
   does. Then it needs a plugin-callable command surface, not the wrapper
   socket, and it is closer to `stella-serve`'s shape than to Vera's.
3. **Both.** The cycle controller is option 2; anything it wants to enforce
   inside a single turn is a separate wrapper plugin.

**Recommendation: option 2, with the pure core moved as-is.** Self-driving's
relationship to the engine is that of a host, and the tree already has a proven
out-of-process host seam. Forcing it through a turn-granularity socket would
widen that socket for a single caller — the kind of generalisation that is
cheaper to add later, when a second caller exists to shape it.

### It already runs out-of-process — that is the feasibility proof

This is not a speculative port. `scripts/self-driving.sh` **already drives the
loop from outside the binary**, delegating every ported decision one-for-one to
`stella self-driving …` verbs (#1548). An external program calling a documented
command surface *is* the plugin shape. What is missing is packaging and
authority, not mechanism.

That also means **Track D does not depend on the wrapper socket at all.** It was
listed as depending on Track A; that is too coarse. D1 depends on nothing, and
the plugin depends only on A1 (identity), not on #3380.

### The work

- **D1. Move the pure core to a leaf crate — not into the plugin. DONE
  (5c5c325).** The module imported only `std`, `serde` and `sha2`, nothing in
  `stella-core` called it, and `lib.rs:44` was the sole reference — exactly as
  predicted, the move was mechanical: one `mod` line removed from
  `stella-core/src/lib.rs`, the file relocated to
  `crates/stella-autonomy/src/lib.rs` with no behaviour change and no renamed
  public items (verified: 25 `stella-autonomy` tests pass, and
  `scripts/test-self-driving.sh` reports 60/60 checks with every assertion
  intact — the behavioural golden D5 below names).

  It landed in a shared leaf crate, not inside a plugin binary, as required:
  `crates/stella-autonomy/Cargo.toml` declares zero workspace-crate
  dependencies, and its header comment carries the reason —
  `stella-observatory` links it deliberately because the observatory
  previously carried its own `fold_runs` and the two implementations drifted,
  so the dashboard and `stella self-driving metrics` disagreed about whether
  the loop was `NOISY` for every odd cycle count (#1613). A leaf crate on the
  `stella-diff` / `stella-home` pattern keeps one copy for both readers.

- **D2. The plugin is a host, not a wrapper.** Settled by §10's granularity
  argument and by the fact that the shell driver already works this way. It
  needs a stable command surface, not the four points.

- **D3. Finish the authority story before the plugin ships.** This, not core, is
  the real gate — see below.

- **D4.** Keep the observatory route working (`/api/self-driving`, plus its
  912-line reader). It reads JSONL from a fixed path and never writes, so if the
  plugin keeps writing the same shapes it needs no change. Verify rather than
  assume: a broken dashboard is how a monitor gets muted.

- **D5.** Keep `make self-driving-test` green. It is hermetic, drives the shell
  face end-to-end through the delegation map, and is the behavioural golden for
  the Rust port. It moves with the feature or the plugin ships untested. If an
  assertion has to be relaxed to make the move pass, the move changed behaviour
  and that is a bug in D1.

- **D6.** Carry the rest: `stella-home`'s four resolvers including the
  legacy-path migration, and the `/self-driving*` slash-command install path.

### The real blocker is authority, not core

Ejecting the decision core is mechanical. The I/O half is not, and the reason is
the size of the grant it needs: reading and filing GitHub issues, creating PRs,
starting and stopping a paid EC2 rig, `brew` installs from a tap, **rewriting a
line in `~/.zshrc`**, daemonising itself, and invoking a different agent product
through installed slash commands.

A plugin holding that is effectively root on the developer's machine. Today
Stella cannot tell a plugin from its operator (§A1), so installing this as a
plugin *right now* would grant all of it with no gate in front. That is a wider
grant than the tool-contract vocabulary currently expresses.

**Consequence for sequencing:** D1 is safe to land whenever. D3 must wait for
A1, and self-driving is the right forcing function for A1 precisely because it
is the most dangerous plugin anyone will install.

### What is not being ejected

Missions (`doc:self-driving-missions`, #2347 merged as a spec; #2345 closed
not-planned) are **spec only** — there is no `mission` or `campaign` code under
`crates/`. Ejecting today moves the stewardship loop, not the objective-driven
one. The missions design is greenfield and the plugin can adopt it without a
migration.

### Capabilities self-driving needs that the pipeline does not

Concretely, from the current implementation: machine probing (disk, memory,
compute — `self_driving_cmd/probes.rs`), durable cross-run state under
`~/.stella/self-driving/` (`state.rs`), process spawning for tools and
benchmarks, a daemon/scheduling path (`crates/stella-cli/src/daemon/`), and —
per the fullauto phases — GitHub issue filing and release installation.

None of these belong in the wrapper socket. All of them argue for option 2: they
are host powers, and a plugin holding them needs the authority story of §A1 to
be finished first, not approximated.

---

## 11. Definition of done for the whole plan

- **The embeddability test passes**: one wrapper plugin runs unchanged when
  driven by `stella-cli`, by `stella-serve` over HTTP, and by a minimal embedded
  host linking `stella-engine`. Prove it with a test that exercises all three,
  not with an argument that it should work (§0).
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

**Note on #3511 (2026-08-17).** The product decision that a plugin's
verification (Vera's) is its own self-reported evidence, not host-verified,
does not change this list — it is what the list has been describing all
along: `judge` stays pure because a plugin supplies evidence, not a verdict,
and that evidence was never host-verified in the first place. What #3511
changes is upstream of this section, in the claims Stella makes about itself:
the manifest and consent text stop implying the oracle's finding is host-run,
and orientation docs (`AGENTS.md`, `README.md`) stop stating "verified done,
not claimed done" as something true of every path uniformly — it now reads as
a property of the path (host-run in the built-in pipeline, self-reported by an
installed plugin), not of the binary.

---

## 12. Risks

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
  not finished. **The flip has shipped (#3381, PR #3694), and it shipped
  ungated by the side-by-side bench §9.6 of `doc:turn-loop-wrappers` asked
  for** — the maintainer's explicit call, not an oversight; both paths still
  coexist and the flip is one flag away from reversal, so this risk is open
  rather than retired.

---

## 13. A plugin is a package: what it *ships*, not only what it *does*

Landed 2026-08-17 (#3565, completing #3380's loader half). §4's A-items answer
"what say does a plugin have in the turn?". This section answers the other
half of the maintainer's directive — *"a plugin needs to be able to ship and
also be a packaged set of tools and skills too… plugins to the turn loop need
to be able to affect context records/recall, tools, and the loop bits we
shipped"* — and states the rule that keeps the two halves from disagreeing.

### 13.1 What a plugin may contribute, and why to existing surfaces

A package is `.stella`-shaped and that is the whole format:

```text
<plugin_dir>/
  plugin.toml              the say in the loop (§4), and the declaration below
  tools/*.toml             the same manifests .stella/tools/*.toml holds
  skills/<slug>/SKILL.md   the same files .stella/skills/ holds
  rules/*.toml             the same records .stella/rules/ holds
```

Three surfaces, no fourth. Each directory is handed to the loader that already
reads that surface as an additional source — a plugin's tool *is* a custom
tool (`stella_tools::custom`), its skill *is* a skill (`stella_core::skills`),
its record *is* a context record (`crate::context_records`).

**Contributing to an existing surface beats a parallel one**, and the argument
is not aesthetic. A parallel plane would be a second set of precedence rules, a
second set of diagnostics, a second thing to remember to gate, and a second
place a name can collide — and the failure mode of forgetting any one of them
is silent. Reusing the surface means a plugin's tool passes the same foundry
gate, is named in `stella tools` beside the user's own, collides under the same
"your own copy wins" rule, and is refused in an untrusted workspace by the same
`PluginRoster::load` check that refuses everything else the package carries.
Nothing here is exempt by construction rather than by policy.

Recall and the loop bits the directive also names are **not** contributions:
they are asks. A plugin reaches recall through the host-call channel
(`doc:wrapper-socket` §6b, #3540) under `[loop] calls`, and it reaches the loop
through `[loop] hooks` / `points`. The difference is that a contribution
persists across every session in the workspace, while an ask happens inside one
point of one turn and is answered by the host.

### 13.2 The provenance / consent / retraction triple

Every contribution has to answer all three, and each is answered structurally:

- **Provenance — from the directory, never the file.** A tool's contributing
  plugin is stamped by discovery from the directory that was read
  (`CustomTool::contributed_by`), which is what
  `custom::tests::a_manifest_cannot_claim_to_have_been_shipped_by_a_plugin`
  pins, and what `CustomTool::principal` turns into `Principal::Plugin`. The
  §13.3 declaration is deliberately *not* an input to this: a manifest names
  what it ships and can never claim to have shipped something it did not.
- **Consent — the crate's bytes, not a host's.** `stella_plugin::consent_text`
  names every contributed tool, skill and record, and it says the three things
  about a tool a user actually needs: it is executable code, the model calls it
  on its own initiative, and it runs as the plugin rather than as them. This was
  the defect #3565 was filed for — the inventory was rendered host-side by
  `plugin_cmd::package::Inventory::consent_addendum`, so `stella plugin
  install` was complete and every embedding host was not. That renderer is gone;
  there is one document.
- **Retraction — structural, by never copying.** Nothing is written into
  `.stella/tools`, `.stella/skills` or `.stella/rules`. Contributions are
  recomputed from the installed packages on every load, so `stella plugin
  remove` deletes a directory and there is nothing left to forget to clean up.
  The alternative was tried with hook matchers and produced a removed plugin
  whose process still ran on every `PreToolUse`: there is no expression that
  removes one owner's rows from a merged list nobody stamped with an owner.

### 13.3 Declaration vs. discovery — neither alone is sufficient

Discovery by directory convention is what makes provenance unforgeable, and it
is also why nothing could render the contributions: `stella-plugin` is pure, so
`consent_text` cannot see a directory. The manifest therefore declares what the
package ships, in the identity **each surface** uses:

```toml
[[tools]]
name = "vera_review"                       # the name the model calls
description = "review the working diff"

[[skills]]
slug = "house-style"
description = "how this shop writes code"

[[records]]
lineage = "ctx.acme.web.no-force-push"     # the lineage the registry merges
statement = "Never force-push to a shared branch."
```

Two rules govern the pair, and they are the whole design:

> **A declaration nobody checks is a promise. A directory nobody declared
> cannot be consented to.**

So `PluginManifest::reconcile` takes the host's own read as a `PackageListing`
and refuses **both** directions: an entry the directories hold that no
declaration names, and an entry declared that the directories do not hold.
`stella plugin install` calls it before it prints anything, which is what makes
the consent document provably complete rather than well-intentioned.

Three consequences worth stating:

- **Names, never paths**, so nothing here needs `${plugin_dir}` interpolation
  (contrast `[runtime]` and `[oracle] command`, where the host expands it).
- **A `[[tools]]` entry deliberately does not restate the tool's argv.** Author
  prose is labelled as the author's throughout the consent surface, but an argv
  reads as a *fact about what will run*, and this crate cannot check it against
  the file that decides. Printing an unverified command line is the "claimed
  limit rendered as an enforced one" mistake in new clothes.
- **The listing carries the effective identity, not the filename.** A package
  shipping `tools/fixer.toml` whose manifest inside says `name = "lint_fix"`
  must reconcile against `lint_fix`, or the agreement being enforced is between
  the manifest and a filename while the model is handed something else.

### 13.4 Governance: a contributed record steers, and may never enforce

`.stella/rules/` is tracked in Git, and in the `regulated` tier a record gains
the authority to *deny* a tool call only through the repository-visible,
hash-chained promotion ledger (`.stella/rules/promotions.jsonl`), where each
grant names its approver and its reason and is reviewed with the record it arms.
A package's records are outside that repository and outside that chain: they
arrive with an install, not a review, and `stella context validate` verifies the
chain against the repository's tree, which never contained them.

The loader half placed plugin records in the **project** trust tier and
**first** in merge order so a user's own copy wins. Both halves are correct and
together they are **not sufficient** — which was a live security finding when
this section was written:

> A promotion grant is keyed on a **lineage**, and `registry_from` armed
> `(Trust::Project, lineage)` for every live grant in the ledger. A plugin's
> records load into that same tier. So a package shipping a record whose
> `lineage_id` matched a live grant inherited the authority to deny tool calls
> with **no promotion entry naming it**. Merge order does not settle this: it
> only decides the collision when the workspace still *has* a record of that
> lineage, and a grant outlives the file it was written for — a record deleted,
> renamed into another set file, or moved out without a retirement event leaves
> the grant live and the lineage unclaimed.

Closed on both sides, because either alone leaves a gap:

1. **The declaration cannot ask for it.** `[[records]] enforcement` is a closed
   vocabulary and `blocking` is refused at load
   (`ManifestError::PluginRecordCannotEnforce`). The variant is spellable
   precisely so the refusal can explain the governance rule instead of
   degrading into a deserializer's "unknown variant".
2. **The ledger cannot grant it.** `context_records::repo_owned_lineages`
   restricts a grant to lineages the **repository's own** project files
   declare — every project rule file that did not arrive inside an installed
   package. A grant is the repository's statement about the repository's
   record; it now says nothing about anyone else's.

The witness is `a_promotion_grant_does_not_arm_a_record_a_plugin_shipped`, and
its second half is the anti-vacuity half: the repository's own record of the
very same lineage, under the very same grant, still arms.
