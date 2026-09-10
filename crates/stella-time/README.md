# stella-time

The real time sources behind `stella-core`'s two time ports, and the two
test doubles every test against the engine shares.

```rust
stella_time::TokioSleeper    // Sleeper: tokio::time::sleep, Instant::now, jitter from the OS
stella_time::WallClock       // Clock: milliseconds since the Unix epoch
stella_time::MonotonicClock  // Clock: milliseconds since this process's first read, never backwards

stella_time::test_util::PausedSleeper  // Sleeper on tokio's paused clock (feature `test-util`)
stella_time::test_util::NoopSleeper    // Sleeper that never waits (feature `test-util`)
```

`stella_core::retry::Sleeper` and `stella_core::ports::Clock` are the ports.
The engine reads time, waits and times out only through them (ADR 0042). A
host picks a source here and hands it to `Engine::assemble`.

## Which source

- **The engine, in a real run:** `TokioSleeper`. Its `now` is the monotonic
  clock, so a deadline armed from it is checked on the same timeline.
- **A stamp read by another process or a later run:** `WallClock`. A hook
  event's stamp, a fleet ledger row, a verdict stamp. Two such stamps must
  count from one start. The epoch is the only start every process agrees
  on.
- **A span measured inside one process and compared as a number:**
  `MonotonicClock`. A circuit breaker's cooling window, a CI monitor's wall
  cap. Every instance shares one origin, so two holders built at different
  moments read one timeline.
- **A test that arms a timeout:** `PausedSleeper`, on a paused tokio
  runtime. A sleep is free while the runtime is idle and still lets a
  pending call finish first. A sleeper that returned at once would fire
  every engine timeout the moment a provider future waited on another task.
- **A test that arms none:** `NoopSleeper`.

## Boundary — does this change belong here?

This crate implements the ports. It decides nothing. A change belongs here
only if it is a new way to read the real clock, wait on the real timer, or
stand in for either under test. What to do with a reading belongs in
`stella-core`, where it is a pure function over the reading. A double with
a special shape, such as a seeded jitter or a sleep that records its calls,
belongs beside the one test that needs it.

## Why it is a crate

AGENTS.md § "When a new crate is justified" names two of its three cases.
These items are the effects the ports keep out of `stella-core`. Its
manifest may not take the Tokio timer or an entropy source, and
`make core-no-io` holds it there. And they sit below every host.
`stella-cli`, `stella-runtime` and `stella-serve` each build an engine, and
`stella-serve` may not link the other two. Before this crate each host kept
its own copy, and about thirty test files each kept a double.

`test_util` lives here and not in `stella-core` for the same reason: a
faithful sleeper double needs the Tokio timer, and `stella-core` may not
link it. `stella-core`'s integration tests take this crate as a
dev-dependency, which cargo allows in a cycle. That is the tokio /
tokio-test shape. `stella-core`'s own unit tests keep one copy,
`tests`, because a lib's unit tests are a second build of the lib
and a dev-dependency that links the lib implements the trait for the
first. `tests/one_home.rs` names that copy and fails on any other.

## God files — do not add lines

This crate has no god files. No file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear. A new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here nears the limit, split it before
it crosses.

## Consumers

- `stella-cli` re-exports the three real sources from its `runtime` module.
- `stella-serve` builds its engines on `TokioSleeper` and stamps its
  extension bus from `WallClock`.
- `stella-runtime` stamps wrapper verdicts from `WallClock`.
- `stella-fleet` paces its CI monitor with `TokioSleeper`.
- `stella-core` and `stella-engine` test against `test_util`.
