# stella-engine

The **step-scoped facade** over [`stella-core`](../stella-core)'s turn loop
(#971). [`Engine::run_turn`] drives a whole turn and returns when it is over —
the right shape for a CLI and the wrong one for a durable host, which has to
persist progress between steps, stop a turn on a deadline or a shutdown, and
resume it in a different process. This crate exposes the same engine one step
at a time: `Engine::run_step` advances exactly one committed step,
`TurnState` owns what the step loop mutates, and `Checkpoint` round-trips that
state through serde so a host can pause, cancel and resume between steps.

Nothing here re-implements the loop. `run_turn` is itself a loop over
`run_step`, so a host driving steps and a CLI driving turns run the identical
per-step code and emit the identical per-step `AgentEvent` sequence. What
`run_turn` adds is turn *framing* the host owns for itself in step mode: the
`Stage(Execute)` event before the first step, the step-cap gate, and — on that
exit — a non-retryable `Error` carrying `step_cap_reason`.

Consumed by [`stella-serve`](../stella-serve) and external hosts.
`stella-cli` deliberately does **not** link it — the CLI drives turns through
`stella-core` directly.

## Direction — this is the in-process embedding door

Stella's goal is to be the AI engine inside somebody else's application: a
durable, composable turn loop reached either **in process through these Rust
ports** or **over the wire** through [`stella-serve`](../stella-serve)
(`doc:engine-embedding`, `doc:serve-surface`). This crate is the first of those
two doors, and step mode is what makes it durable — a host that owns a queue, a
deadline, and a crash-restart story drives `run_step`, persists the
`Checkpoint`, and resumes in a different process.

This door deliberately does not carry:

- **No verification.** The staged pipeline is a *wrapper* around the loop, not
  part of it, and it is leaving the workspace to become a plugin (#3246,
  `doc:turn-loop-wrappers`). A host driving steps through this facade gets the
  loop and the loop's own ending — since #3379 `run_turn` always emits its
  terminal completion and no caller may filter it — and gets nothing that
  adjudicates whether the work was correct. That is the plugin's job, and the
  host opts into it.
- **No wrapper socket.** The four-point wrapper contract (#3380) is assembled in
  [`stella-runtime`](../stella-runtime), above this facade, because two of its
  four points do I/O. Nothing about it belongs in a crate that inherits
  `stella-core`'s I/O-free posture.

## Boundary — does this change belong here?

Almost nothing belongs here. This crate **re-exports and documents**; the step
loop, the checkpoint type, and every behavior it fronts live in
`stella-core` (`src/step.rs`, `src/driver.rs`), and a change to how a step
runs is a `stella-core` change. The only code that lives here is the thin
`encode_checkpoint` / `decode_checkpoint` pair — conveniences a host can hand
to a storage boundary — and the facade's own tests, which pin the re-exported
surface and the checkpoint round-trip so a `stella-core` refactor cannot
silently change what hosts see.

Anything with logic, I/O, or state is wrong here by construction: the crate
inherits `stella-core`'s I/O-free posture (its tokio dependency is `sync`-only,
deliberately) and `#![forbid(unsafe_code)]`.

### What earns a re-export: the closure rule

This section and the `# What earns a re-export` section of `src/lib.rs` state
one rule in the same words, deliberately. They used to state two: this file
said a re-export is earned only when a host "genuinely cannot drive a turn
without it", while `src/lib.rs` stated a strictly wider per-port closure. The
gap between them was not theoretical — `Engine::with_requery` was reachable
through the facade and callable by nobody, because neither the
`SteeringRequery` it takes nor the `TurnSignal` that port's one method names
could be spelled from this crate (#3715).

**The rule.** A host must be able to write, naming nothing but
`stella_engine::` paths:

1. an `impl` of every port this facade's engine accepts,
2. a construction of every value it accepts — every `EngineConfig` field and
   every builder argument, and
3. a `match` on every value it hands back that the host must branch on.

Closure is transitive through those three obligations and stops where they
stop. `GenerationParams` is re-exported because a host fills that config
field, so `Verbosity` and `ServiceTier` come with it; `AgentEvent` is
re-exported because a host receives one, and its payload types are not,
because a host forwards or serializes an event rather than constructing one. A
facade closed over mere reachability would be `stella-protocol` with extra
steps.

The rule is a coherence property of what is *already* exported, not a licence
to widen the facade for an imagined caller — the distinction that separates it
from #2481, which asked for turn-boundary payload shapers no exported
signature names and was closed as speculative. A genuinely new capability is
still a design question; making an already-reachable one writable is not.
Every entry arrives with doc prose explaining its place in the host-driving
story.

**The hook plane is the wrong layer, by design.** `Engine::with_hooks`
(`Hooks`, `HookRunner`) and `Engine::with_bus` (`HookBus`) are not closed
over. Their closure is the shell-command hook plane, an extension surface
whose purpose is to *execute* things, fronted here by a crate that inherits
`stella-core`'s I/O-free posture; the supported host-extension door is
[`stella-runtime`](../stella-runtime)'s wrapper socket (#3380,
`wrapper::WrapperDispatch::bind`), which lives one layer above this facade
precisely because two of its four points do I/O.

#3768 asked whether to close over both, over the observer half alone, or
neither. **Neither**, permanently: closing over either method would let a host
reach the engine's shell-execution authority by naming `stella_engine::` paths
alone. Nothing is stranded by it — [`stella-serve`](../stella-serve), the
embedded host that would have been the observer half's first caller, already
links `stella-core` directly and mints its bus from `stella_core::bus::HookBus`.
`tests/embedding.rs` enforces the boundary: it stops compiling if any of the
five hook-plane names becomes reachable through `stella_engine::*`.

Two contracts every consumer must know, documented at length in `src/lib.rs`:

- **Cancel, do not drop.** `CancelToken` is read at the top of every step, so
  the in-flight step finishes and commits first. Dropping the turn future
  stops it immediately, and the cost is an unpaired `tool_use` in the borrowed
  history, a billed-but-lost model call, and everything since the last
  checkpoint.
- **The turn future is `!Send`.** Neither `run_step` nor `run_turn` can be
  `tokio::spawn`ed onto a multi-thread runtime; drive them on a
  current-thread runtime, one OS thread per session (the pattern
  `stella-cli`'s fleet worker uses).

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.

## Testing

`src/tests.rs` exercises the facade the way a host would: driving a turn to
done step by step, proving `run_turn` emits the identical event sequence to a
hand-driven step loop, resuming from a checkpoint and getting the same
downstream events, cancelling mid-turn at a safe boundary with a valid
transcript, cancelling before the first step for free, and round-tripping a
between-steps checkpoint through `encode_checkpoint`/`decode_checkpoint`. The
deeper step-loop properties (loop detection, budget, retry) and the
`CHECKPOINT_VERSION` refusal are `stella-core`'s tests
(`crates/stella-core/src/step.rs`); do not duplicate them here.
