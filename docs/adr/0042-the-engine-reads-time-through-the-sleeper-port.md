---
id: adr/0042-the-engine-reads-time-through-the-sleeper-port
title: "ADR 0042: The engine reads time through the Sleeper port"
status: implemented
---

# ADR 0042: The engine reads time through the Sleeper port

- Status: accepted
- Date: 2026-09-10
- Decides: `#6486`, `#6484`
- Not part of the Phase 0 series.

## Context

`stella-core` has had a `Clock` port since the router's circuit breaker
needed one. It reads `now_ms() -> u64`. One module used it. The deadline
code read `std::time::Instant::now()` itself, in nineteen places, and nine
more read `.elapsed()`, which is the same call in disguise. Four timeouts
went to `tokio::time::timeout` directly. `#6482` counted the plain reads
into a down-only baseline instead of removing them.

A seam nobody routes through is a claim. A test that wanted to see a
deadline fire had to arm a real one and wait. A turn could not be replayed
from its record. And the prose about the crate's time source drifted from
the code twice.

Two sessions fixed this at once. One made every instant a `u64` reading of
`Clock`, with a hand-moved clock as the double. The other put `now` on
`Sleeper` and routed the timeouts through it. The second merged as
`#6486`. This record states that decision and why it holds.

The three hosts that build an engine each kept their own copy of the real
sources. A wall clock lived in `stella-cli`, `stella-runtime` and
`stella-serve`. A Tokio sleeper lived in two of them, and `stella-fleet`
had a sleeper trait of its own. About thirty test files each wrote the same
no-op sleeper. `stella-serve` may not link the other two, and `stella-core`
may not carry a timer, so the copies had nowhere to go.

## Decision

**`retry::Sleeper` is the engine's whole time port.** `now() -> Instant`
sits beside `sleep` and `jitter`. Every deadline the engine holds is an
`Instant` from that reading. Every elapsed time is the difference of two.
Every timeout is `retry::bounded`, a sleep racing a call through the same
port. Nothing in the crate calls `Instant::now()`, `.elapsed()` or
`tokio::time`; `make core-no-io` refuses all three, and its baseline is
empty.

**One port, not two, because a double cannot answer them apart.** A sleeper
that suspends virtually has moved its own `now`. A timeout is a sleep
racing a call. Tokio's paused runtime is the same shape: one virtual clock
behind `sleep` and `Instant::now`. A `u64` reading on a second port would
have made the test move two clocks and keep them in step by hand.

**The reading is an `Instant`, not a `u64`.** The deadline arithmetic
already spoke `Instant`, `tokio::time::Instant` converts to it, and a
paused runtime produces one. The cost is that a record cannot hold an
`Instant`. Replay against a host that answers `now` from a record is later
work either way.

**`ports::Clock` stays, for stamps.** The hook bus and the router read
milliseconds from an epoch the holder picks, and a hook script on the far
side of a socket compares those stamps with its own. That is a different
question from a deadline, and it keeps its own port.

**The real sources live in one crate, `stella-time`.** `TokioSleeper`
waits on the Tokio timer, reads `now` off the monotonic clock, and draws
its jitter from the OS entropy pool. `WallClock` counts from the Unix
epoch, for a stamp another process reads. `MonotonicClock` counts from the
first read, for a span compared as a number. AGENTS.md § "When a new crate
is justified" allows the crate on two counts. It holds the effects the
ports keep out of `stella-core`. And it sits below `stella-serve`, which
may not link `stella-cli` or `stella-runtime`. `stella-fleet`'s own
`Sleeper` trait is gone; its monitor takes the engine's.

**The test doubles ship from `stella-time` behind a `test-util` feature.**
`PausedSleeper` sleeps on tokio's clock and reads its `Instant`, for a test
that arms a timeout on a paused runtime. `NoopSleeper` never waits, for a
test that arms none. They live beside the real sources and not in
`stella-core` because a faithful sleeper needs the Tokio timer, which
`stella-core` may not link. `stella-core`'s integration tests and every
other crate take it as a dev-dependency, which cargo allows in a cycle.
That is the tokio / tokio-test shape. `stella-core`'s own unit tests keep
one copy in `src/tests.rs`, because a lib's unit tests are a second build
of the lib and a dev-dependency that links the lib implements the trait
for the first. The compiler forces that copy, and the witness names it.

## Consequences

`stella-core`'s test suite runs on a paused clock. Backoff sleeps that used
to cost real seconds cost nothing, and a timeout still lets a pending call
finish first.

Each host passes `stella_time::TokioSleeper` to `Engine::assemble`. Each
test passes `PausedSleeper` or `NoopSleeper`. A new double with a special
shape, such as a seeded jitter or a sleep that records its calls, stays
beside the one test that needs it.

`stella-time`'s `one_home` test reads the tree and fails on any other
`impl Sleeper for` in shipping code, so the copies cannot come back
unnoticed.
