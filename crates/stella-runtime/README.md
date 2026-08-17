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
verdict asks for another turn. `stella run --pipeline <variant>` is its first
driver, so an installed wrapper plugin now participates in a live turn and its
id reaches `executions.pipeline_variant`.

What has **not** landed: the other two drivers `doc:wrapper-socket` §6 makes an
acceptance criterion — `stella-serve` over HTTP and a minimal embedded host
linking `stella-engine` — neither of which calls `WrapperDispatch` yet (#3551);
candidate-workspace grants on the CLI path, so `RoundInput::candidate` is
`None` there and a `flip = "required"` oracle abstains (#3553); and
[`stella-pipeline`](../stella-pipeline), which still takes its own branches
rather than being ported onto this socket (Track B, `doc:pipeline-as-plugins`
§7). A wrapper is meant to be handed a child-turn **port** that names a role
intent (`triage`, `planner`, `witness_author`) — never a provider, an
`Engine`, or a credential; the host would resolve the intent against the
user's BYOK providers, carve the budget, attach gate/steering/hooks, and
settle once. That port has no name and no type in this crate yet — it is
design (`doc:pipeline-as-plugins` §9.3), not code.

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
| `src/wrapper/error.rs` | `WrapperError`, typed per failure mode (unreachable, over budget, unusable, undeclared role, mistyped signal). |
| `tests/no_ambient_reads.rs` | The executable form of the invariant above. |
| `tests/wrapper_socket.rs` | The socket's end-to-end proof: a real `/bin/sh` subprocess plugin driven through `TurnWrapper` by the test's own round loop. It proves the socket *answers*; what it cannot prove is that anything in the workspace holds the same order, which is what `tests/wrapper_dispatch.rs` is for. `#![cfg(unix)]`; #3497 tracks the portable in-tree plugin binary a Windows-proof version needs. |
| `tests/wrapper_dispatch.rs` | The host sequence's witness: the same kind of `/bin/sh` plugin driven through the **shipped** `WrapperDispatch`, proving a contribution reaches the turn after the stable prefix, its evidence reaches `judge`, and the verdict is what decides whether another turn runs. `#![cfg(unix)]` for the same reason. |
| `tests/wrapper_verdict.rs` | The property tests behind `judge`/`again`'s totality claim. |
| `tests/wrapper_env_refusal.rs` | `refuses_env_name`'s refusal list, proven against the names #3512 named. |
| `tests/host_owned_tamper.rs` | #3499's split: `ObservedEvidence` carries no tamper field: `EvidenceSet::from_observed` is where the host's own finding is merged in. |
| `tests/no_pipeline_edge.rs` | Executable proof that this crate declares no dependency on `stella-pipeline` — a wrapper `stella-cli` can drive and `stella-serve` cannot is a CLI feature wearing a socket's name. |
