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

## Boundary — does this change belong here?

Almost nothing belongs here. This crate **re-exports and documents**; the step
loop, the checkpoint type, and every behavior it fronts live in
`stella-core` (`src/step.rs`, `src/driver.rs`), and a change to how a step
runs is a `stella-core` change. The only code that lives here is the thin
`encode_checkpoint` / `decode_checkpoint` pair — conveniences a host can hand
to a storage boundary — and the facade's own tests, which pin the re-exported
surface and the checkpoint round-trip so a `stella-core` refactor cannot
silently change what hosts see.

A new re-export is legitimate when a host genuinely cannot drive a turn
without it, and it arrives with doc prose explaining its place in the
host-driving story. Anything with logic, I/O, or state is wrong here by
construction: the crate inherits `stella-core`'s I/O-free posture (its tokio
dependency is `sync`-only, deliberately) and `#![forbid(unsafe_code)]`.

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
