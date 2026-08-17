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

## Direction — the wrapper socket lands here

Stella is becoming an embeddable turn loop with a plugin architecture around it:
one loop, several doors, and everything wrapped around it a plugin
(`doc:turn-loop-wrappers`). The socket those plugins plug into — the four-point
wrapper contract `before_turn` / `after_turn` / `judge` / `again?` (#3380) — is
slated for **this crate**, and the reason is the boundary above: `before_turn`
recalls and researches, `after_turn` runs a test command or an oracle process,
and invariant #2 forbids that I/O inside [`stella-core`](../stella-core). A socket
defined in core would either be a trait core never calls, or a trait core awaits
— which puts a process spawn inside the engine (`doc:turn-loop-wrappers` §9.1).
This crate already owns assembly and reads no ambient environment by contract,
which is the same property a plugin host needs.

Two consequences worth knowing before extending it. A wrapper will be handed a
child-turn **port** that names a role intent (`triage`, `planner`,
`witness_author`) — never a provider, an `Engine`, or a credential; the host
resolves the intent against the user's BYOK providers, carves the budget,
attaches gate/steering/hooks, and settles once. And "one blessed constructor"
only becomes enforceable when `TurnCapabilities` exists (#3274 slice 2): today
`Engine::with_sleeper` cannot carry `gate`/`steering`/`hooks`, so a hand-rolled
child engine silently drops all three. Until then, assembly here is the mitigation
rather than the guarantee.

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
| `tests/no_ambient_reads.rs` | The executable form of the invariant above. |
