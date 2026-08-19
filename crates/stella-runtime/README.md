# stella-runtime

**The construction sequence, once.** `RuntimeSpec` → `RuntimeBuilder` →
`SessionRuntime`: the engine-assembly bottom half — provider adapter, tool
registry, store, calibration, budget — built from explicit inputs into one
value any surface can drive: the CLI, the serve sidecar, or a test.

The crate exists because `stella-cli` is a bin-only crate — no `[lib]` target —
so nothing inside it is callable from anywhere else. That one manifest fact is
why the same engine setup got re-typed at seven call sites (`agent.rs` twice,
`agent/goal.rs` twice, `command_deck.rs`, `fleet_cmd.rs`, `subsession.rs`):
there was no shared home for it, and each new driver copied the nearest
existing one. A fix to the ordering had to be applied seven times, and had
been missed before — and `stella-serve` cannot link a binary at all. This
crate is that shared home.

## Boundary — does this change belong here?

The line `SessionRuntime` draws (`src/session.rs` states it in full): **below
it sit the resources**, which are pure functions of a `RuntimeSpec` and
identical whether a TUI, an SSE stream, or a test harness is driving. **Above
it sit the ports** — the approval gate is a terminal prompt in one host and a
reverse HTTP request in another; the event sink is a renderer thread in one
and an SSE pump in the other. Only the bottom half lives here; the top half
stays in each surface crate and can differ per surface without this crate
knowing.

So: a change to *how a resource is constructed* (provider from parts, registry
rooting, store opening, calibration seeding, budget mode) belongs here, in
`src/parts.rs`, surfaced as a `Notice` when it degrades rather than fails. A
change to *what the engine does* with those resources belongs in
[`stella-core`](../stella-core). A change to *how a host presents or approves
things* belongs in that host's crate. Construction only — no decision logic,
no rendering, no routes.

## Direction — the wrapper socket lives here

Stella is becoming an embeddable turn loop with a plugin architecture around it:
one loop, several doors, and everything wrapped around it a plugin
(`doc:turn-loop-wrappers`). The socket those plugins plug into — the four-point
wrapper contract `before_turn` / `after_turn` / `judge` / `again?` (#3380) —
**lives here** ([`src/wrapper/`](src/wrapper), landed #3479), and the reason is
the boundary above: `before_turn` recalls and researches, `after_turn` runs a
test command or an oracle process, and invariant #2 forbids that I/O inside
[`stella-core`](../stella-core). A socket defined in core would either be a
trait core never calls, or a trait core awaits — which puts a process spawn
inside the engine (`doc:turn-loop-wrappers` §9.1). This crate already owns
assembly and reads no ambient environment by contract, which is the same
property a plugin host needs — including inside `src/wrapper/` itself, which
reads neither.

The wire-contract *types* — `BeforeTurnRequest`/`Response`,
`AfterTurnRequest`/`Response`, `EvidenceSet`, `VerdictRule` — are one crate
down, in [`stella-plugin`](../stella-plugin), because a non-Rust plugin author
needs exactly that crate's JSON shapes and nothing else (`doc:wrapper-socket`
§2). `TurnWrapper` here is a typed Rust view of the identical shapes, never a
second design: [`SubprocessWrapper`](src/wrapper/subprocess.rs) speaks the wire
contract over stdio in whatever language the plugin is written in, and that is
the transport CI exercises; [`InProcessWrapper`](src/wrapper/in_process.rs) is
the in-process fast path §3 permits for Rust specifically, and it takes and
returns the identical owned request/response types so nothing is reachable
through it that is not reachable through the wire path too. `judge` and
`again` are free functions beside the trait, over the plugin's declared
verdict rule and the evidence its `after_turn` returned — synchronous, total
and I/O-free, so no plugin, in any language, can make a verdict a model call
(`doc:pipeline-as-plugins` §6). `admissible` is the check between a plugin's
`before_turn` answer and the turn: an undeclared role intent or a signal
published at the wrong type is refused, restating at the value level the same
rules `stella-plugin` already enforces at load time on the *declaration*.

**What has landed and what has not.** The trait, both transports, `admissible`,
and `judge`/`again` are proven end-to-end against a real (non-Rust, `/bin/sh`)
subprocess plugin (`tests/wrapper_socket.rs`) and against the evidence→verdict
mapping as a pure function (`tests/wrapper_verdict.rs`,
`tests/host_owned_tamper.rs` for the host-owned tamper split, #3499). The
*host sequence* has landed too (#3494): [`src/wrapper/dispatch.rs`](src/wrapper/dispatch.rs)
is `WrapperDispatch`, which resolves the declared stage program, calls
`before_turn` per stage, hands a `TurnPrelude` to the host's `TurnDriver`,
calls `after_turn`, and settles with `judge` + `again` — looping while the
verdict asks for another turn. `crates/stella-cli` drives it from **two**
doors, not one, each calling `WrapperDispatch::run` a different number of
times per invocation: `stella run --pipeline <variant>` was its first driver
and calls it exactly once per process, over the one raw turn the process
runs; `stella goal --pipeline <variant>` (#3695, goal half) calls it once
**per judged round** — the goal loop's own round loop, not
`WrapperDispatch`'s, decides how many rounds run, because the goal verifier
(`stella_core::Engine::assess`) stays outside `judge`/`again` entirely. That
per-round shape is why `stella goal` refuses an arbiter-grade wrapper before
ever calling `WrapperDispatch::run`
(`crates/stella-cli/src/wrapper_plugin.rs::reject_arbiter_wrapper_on_goal`,
#3832): only an arbiter grade can make `again` hold a round open past its
first internal turn, and holding one open *inside* an already-judged goal
round would be a second arbiter judging the round the goal loop's own
`Engine::assess` is already judging — a shape this door refuses outright
rather than let `WrapperDispatch`'s hold loop and the goal loop's hold loop
collide. An arbiter-grade wrapper's designed home is `stella run --pipeline
<variant>` instead, where `WrapperDispatch` is the only thing holding a turn
open. Either door, an installed wrapper plugin participates in a live turn and
its id reaches `executions.pipeline_variant`.

What has **not** landed: the other two drivers `doc:wrapper-socket` §6 makes an
acceptance criterion — `stella-serve` over HTTP and a minimal embedded host
linking `stella-engine` — neither of which calls `WrapperDispatch` yet (#3551).
The third item here used to be `crates/stella-pipeline`, which took its own
branches rather than being ported onto this socket (Track B,
`doc:pipeline-as-plugins` §7); it was deleted outright in #3865 instead, so
there is no longer a second in-tree wrapper mechanism to converge. It had
dispatched from its own `[wrapper]` manifest via
`Schedule`/`ProgressiveResolver` (#3408), a separate mechanism from this
crate's `TurnWrapper` socket, not this socket wearing a new caller.
Candidate-workspace grants on the CLI path landed (#3553,
`crates/stella-cli/src/wrapper_candidate.rs`): `RoundInput::candidate` now
carries a real grant over the shared work tree a `flip = "required"` oracle
can observe. A wrapper is also handed a child-turn **port** that
names a role intent (`triage`, `planner`, `witness_author`) — never a
provider, an `Engine`, or a credential; the host resolves the intent against
the user's BYOK providers, carves the budget, attaches gate/steering/hooks,
and settles once. That port is
[`ChildTurnPlane`](src/wrapper/child_turn.rs), implemented by
[`ChildTurns`](src/wrapper/child_turn.rs) over the host's own sub-agent
dispatcher, taking `ChildTurnArgs` on the wire. It is now attached at one live
call site of the two doors above (#3576): `stella run --pipeline <variant>`
builds its `HostCallGate` with `.with_child_turns(..)` over the session's own
`SubAgentDispatcher` — the one `task_assign` runs on — so an installed plugin
declaring `[loop] calls = ["child_turn"]` and a `[roles.<name>]` gets a real
turn, read-only, attributed to the seat its tier resolves to, with what it
spent printed beside what it was refused
(`crates/stella-cli/src/wrapper_plugin.rs`). `stella goal` builds no such
plane at all — its `WrapperHost` serves `recall` only — because the fixed
`turn_instance` slot `stella run`'s one-shot worker can afford would collide
with a goal round's own even/odd worker/verifier slots (#3833); a wrapper
that named `child_turn` there would be answered `Unavailable`, exactly like
naming the `verifier` tier is answered on `stella run`. Two more limits
stand on the door that does attach the plane: the `verifier`
tier is deliberately bound to no seat, so a plugin naming it is answered
`Unavailable` rather than having its call attributed to a role the host never
made (`ChildTurns::with_seat` is how a driver that wants it owns the claim);
and a plugin's points run *between* the parent's turns, where the tool
registry's event sender is a sink — so the child's `step_usage` reaches the
run's report and the session's budget guard, but not the store's receipt
(#3802).

**The driver channel now moves bytes, and still has no production caller.**
`SubprocessDriver` (#3634) is the transport between the wire
(`stella_plugin::driver`) and the gate (`src/wrapper/driver_call.rs`): it
spawns a driver, opens a session, relays every capability ask through the
grant a human consented to, and reads back the `next` that ends it —
`tests/driver_socket.rs` drives a `/bin/sh` driver through all of it. Two
things are deliberately still absent. Every capability answers `unsupported`,
because `NoDriverCapabilities` is the only implementation and B1-B6 (#3599)
are what give the verbs something to do; and nothing in `stella-cli` opens a
driver session yet, so `plugins/stella-selfdriving`'s `[driver]` grant remains
a declaration rather than a running program. Read the transport as "a
capability can now be held", not as "the loop drives".

One piece of that design is no longer the open risk `doc:turn-loop-wrappers`
§9.3 described it as, though it is not wired to this socket: `TurnCapabilities`
(#3274 slice 2, #3387) has landed in [`stella-core`](../stella-core), and
`Engine::assemble` is what
[`stella-core::subagent`](../stella-core/src/subagent.rs) builds its child
fork through today. Read `TurnCapabilities`'s own module doc before citing it
as "the constructor cannot carry those seams" — that framing turned out false
of this tree (`with_gate`/`with_steering`/`with_hooks` are `pub` and still
directly callable); the property it actually enforces is *totality*, a
decision recorded for every seam at every `assemble` call site. Whatever
implements the wrapper socket's future child-turn port earns the same
discipline only by calling `assemble` itself — nothing today connects the two.

## The invariant: no ambient reads

**Nothing in this crate reads the process environment or the current
directory.** Every ambient switch the CLI consults — `STELLA_NO_SETTINGS`, the
enterprise process-free authority flag, `HOME` — is a field on `RuntimeSpec`
instead, filled in by the caller. That is what makes N concurrent sessions
with N different workspace roots and N different trust postures sound in one
process — the thing a server needs and a CLI never did. The rule is enforced
executably by `tests/no_ambient_reads.rs`, not just asserted; extending this
crate means keeping that test green without an allowlist entry.

**The rule binds what this crate *builds*, not only what it spells.** A
textual scan of these sources cannot see a constructor one call below that
reads the environment for us (#1596 is the recorded instance). The registry
holds the line structurally: `stella_tools::ToolRegistry::new(root)` is its
only constructor and consults nothing beyond the root it is handed, so
building a `SessionRuntime` reads no ambient state that a `RuntimeSpec`
field does not name.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.

## Layout

| File | Owns |
|---|---|
| `src/spec.rs` | The inputs, as **values**: `RuntimeSpec`, `ProviderParts`, `Persistence`, `Notice`. Every field is something the CLI used to read ambiently. |
| `src/parts.rs` | The individual construction steps: `build_provider`, `open_store`, `seed_calibration`, `budget_guard`. (The tool registry needs no step: `ToolRegistry::new` takes only the workspace root, so the builder calls it directly.) |
| `src/session.rs` | The composite: `RuntimeBuilder` assembling a `SessionRuntime`. Overridable per-step as hosts diverge (today: `with_provider`). |
| `src/error.rs` | `RuntimeError` — typed construction failures. Degradations are `Notice`s on the built runtime instead, per the warn-never-disable posture. |
| `src/wrapper/mod.rs` | The `TurnWrapper` trait (`before_turn`/`after_turn`), plus `admissible` and the `AdmittedContribution` it answers with — the value-level restatement of the manifest's undeclared-role and mistyped-signal load-time rules, and the only value the dispatcher will apply. |
| `src/wrapper/dispatch.rs` | `WrapperDispatch` — the host sequence (#3494): stage program → `before_turn` per stage → the host's `TurnDriver` → `after_turn` → `judge` → `again`. Owns the loop; the host owns the turn. |
| `src/wrapper/subprocess.rs` | `SubprocessWrapper` — the transport CI exercises: spawns `[runtime].argv`, writes the request as one line of JSON on stdin, reads the response on stdout, and `refuses_env_name` — the default-deny env-allowlist check `stella-cli`'s consent prompt also renders. |
| `src/wrapper/in_process.rs` | `InProcessWrapper`/`WrapperHandler` — the fast path §3 permits for Rust, over the identical owned request/response types the subprocess transport uses. |
| `src/wrapper/verdict.rs` | `judge` and `again` — synchronous, total, I/O-free functions over `VerdictRule` and `EvidenceSet`; property-tested (`tests/wrapper_verdict.rs`) over the closed evidence vocabulary. |
| `src/wrapper/error.rs` | `WrapperError`, typed per failure mode (unreachable, over budget, unusable, undeclared role, mistyped signal), and `DriverError` beside it — the driver channel's own failures, minus every variant that is about standing inside a turn. |
| `src/wrapper/driver_call.rs` | The host half of the driver channel: the gate, the ceiling, and the report for a plugin that drives turns instead of sitting inside one (`doc:backlog-self-driving` §3.0). |
| `src/wrapper/driver_subprocess.rs` | `SubprocessDriver` — the driver channel's transport (#3634): spawns the driver, writes the `DriveRequest` on stdin, relays every capability ask through the gate, and reads back the `next` that ends the session. One budget covers the whole session, and `refuses_env_name` withholds model credentials here exactly as it does for a wrapper. |
| `src/wrapper/framing.rs` | Where one message ends on a child's stdout, and the one task that owns its stdin — shared by both transports, because a second framer would be quadratic and pass its tests. |
| `src/wrapper/host_call.rs` | The host half of the host-call channel: a plugin may ask the host for a capability, never reach for one itself — this module is the half that decides and applies the install-time grant (`doc:wrapper-socket` §6b). |
| `src/wrapper/child_turn.rs` | The `ChildTurn` port: the host spends a model call at a declared role intent (`triage`, `planner`, `witness_author`, …) so a plugin never holds a provider credential itself (`doc:turn-loop-wrappers` §9.3). |
| `tests/no_ambient_reads.rs` | The executable form of the invariant above. |
| `tests/wrapper_socket.rs` | The socket's end-to-end proof: a real `/bin/sh` subprocess plugin driven through `TurnWrapper` by the test's own round loop. It proves the socket *answers*; what it cannot prove is that anything in the workspace holds the same order, which is what `tests/wrapper_dispatch.rs` is for. `#![cfg(unix)]`; #3497 tracks the portable in-tree plugin binary a Windows-proof version needs. |
| `tests/wrapper_dispatch.rs` | The host sequence's witness: the same kind of `/bin/sh` plugin driven through the **shipped** `WrapperDispatch`, proving a contribution reaches the turn after the stable prefix, its evidence reaches `judge`, and the verdict is what decides whether another turn runs. `#![cfg(unix)]` for the same reason. |
| `tests/wrapper_verdict.rs` | The property tests behind `judge`/`again`'s totality claim. |
| `tests/wrapper_env_refusal.rs` | `refuses_env_name`'s refusal list, proven against the names #3512 named. |
| `tests/host_owned_tamper.rs` | #3499's split: `ObservedEvidence` carries no tamper field: `EvidenceSet::from_observed` is where the host's own finding is merged in. |
| `tests/no_pipeline_edge.rs` | Executable proof that this crate declares no dependency on the staged pipeline — a wrapper `stella-cli` can drive and `stella-serve` cannot is a CLI feature wearing a socket's name. The crate it names was deleted in #3865; the guard is kept because the edge it forbids is the one a re-home would most plausibly reintroduce. |
| `tests/wrapper_host_call.rs` | The witness for the host-call channel (#3540, `doc:wrapper-socket` §6b): a plugin asks the host for a capability mid-point and is handed real frames. |
| `tests/driver_socket.rs` | The witness for the driver channel's transport (#3634, #3599 B0): a `/bin/sh` driver is handed a session, is served a capability it declared and refused one it did not, and ends the session with each of the two terminal answers. Two of its twelve ask the **child** rather than the constructor — what environment the process actually received, and whether a driver that keeps writing after deciding wedges the session — because a constructor interrogated about itself only ever agrees. |
| `tests/wrapper_transport_limits.rs` | Witnesses for #3380's transport audit: the two resources a plugin process can spend that are not its own — the host's memory, and the machine after the turn ended. |
| `tests/wrapper_child_turn.rs` | The witness for `child_turn` (#3564, #3541, `doc:turn-loop-wrappers` §9.3): the host, not the plugin, spends the model call at a declared role intent. |
| `tests/wrapper_decided_flip.rs` | The witness for #3553: a plugin whose `[oracle]` declares `flip = "required"` reaches a decided verdict through the real dispatch. |
| `tests/research_plugin_conformance.rs` | The witness for Track B's first extraction: `plugins/stella-research` answers a real `before_turn` request over the wire in well-formed `stella_plugin::wire` shapes. |
| `tests/research_plugin_dispatch.rs` | Grades `plugins/stella-research` against `WrapperDispatch` itself — the declared stage program actually calling it, not just the wire shape being correct. |
| `tests/research_plugin_recall.rs` | The witness for the `recall` half of Track B's first extraction: the plugin answers `StageName::Recall` from frames it asked the host for, or an honest empty one when the host declines. |
