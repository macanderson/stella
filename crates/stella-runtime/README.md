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
reads the environment for us, and for a long time one did: the tool registry
probed `gh auth status` and `LINEAR_API_KEY` from inside `new_detected`, so
building a `SessionRuntime` read ambient state no `RuntimeSpec` field
mentioned (#1596). That probe is now a port declared on
`RegistryOptions::issue_backend` and defaults to consulting nothing, which
makes it one more thing the caller fills in. `tests/registry_probe_is_declared.rs`
holds the behavioural half — it drives the real construction path and counts
how many times the host is asked — because the scan is structurally unable to.

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
| `src/parts.rs` | The individual construction steps: `build_provider`, `tool_registry`, `open_store`, `seed_calibration`, `budget_guard`. |
| `src/session.rs` | The composite: `RuntimeBuilder` assembling a `SessionRuntime`. Overridable per-step as hosts diverge (today: `with_provider`). |
| `src/error.rs` | `RuntimeError` — typed construction failures. Degradations are `Notice`s on the built runtime instead, per the warn-never-disable posture. |
| `tests/no_ambient_reads.rs` | The executable form of the invariant above. |
