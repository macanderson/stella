# request-bench

An in-process allocator-churn + wall-clock A/B for the per-attempt model-request
build (#921): how many bytes and allocations does the engine spend cloning the
transcript and tool schemas into a `CompletionRequest`, before vs. after the
retry path switched to a borrowed `CompletionRequestRef`. Deterministic,
offline, no Docker, no provider key, no spend.

The full reasoning — why the two arms exist, what each one costs, how to read
"cumulative churn" vs. peak residency — is in the module doc at the top of
[`src/main.rs`](src/main.rs); this file only points there rather than
repeating it.

## Running it

```bash
cargo run -p request-bench --release
```

Run with `--release`. A debug build still counts allocations honestly, but the
wall-clock column is meaningless in debug — the allocator overhead dwarfs the
work being measured.

## Reading the output

Two rows, `owned-clone` (the pre-#921 shape) and `borrowed` (the shape after
it), each reporting bytes and allocations per step, cumulative MiB per turn,
and wall time. `borrowed` is asserted to allocate zero bytes — a single-attempt
model call built from slices and scalars performs no deep copy — so a red
assertion here is itself the regression signal, not just the printed numbers.

Not listed in [`../README.md`](../README.md)'s entry-point table: that table is
for running Stella against a public benchmark, and this is an in-process Rust
micro-benchmark of allocator behavior, not a benchmark run.
