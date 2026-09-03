---
id: adr/0024-release-builds-unwind
title: "ADR 0024: Release builds unwind on panic"
status: implemented
---

# ADR 0024: Release builds unwind on panic

- Status: accepted
- Date: 2026-09-02
- Decides: `#5493`

## Context

The release profile set `panic = "abort"`. A catch cannot catch an abort. So
every panic guard in the tree did nothing in the build a user runs. The code
that builds those guards is written and tested as if it works. It does work,
in a test build. That is where the tests run.

Two of those guards are promises to a person.

A panel that panics paints a small red card. The rest of the screen keeps
working. See `crates/stella-tui/src/panel_guard.rs`. Under an aborting profile
the whole program dies instead. It leaves the terminal in raw mode.

A panic in one `stella-serve` connection ends that connection. Under an
aborting profile it ends the server. Every other live turn dies with it.

Three more guards have the same shape. A child agent settles its spend on the
panic path, in `crates/stella-cli/src/subagent.rs`. A worker panic costs one
lane and not the session, in `crates/stella-cli/src/subsession.rs`. A hook that
panics is skipped and not fatal, in `crates/stella-core/src/bus.rs`.

## Decision

Set `panic = "unwind"` in `[profile.release]`. Keep `lto`, `codegen-units` and
`strip` as they are.

The other choice was to keep `abort` and delete the guards. That is cheaper.
It is the wrong trade here. A coding agent runs for hours on a user's
terminal. Living through one bad draw is worth more than the code size.
Deleting the guards would also drop the spend settle, and that one is money.

## Consequences

The binary grows. It loses a little speed. An unwinding build needs landing
pads that an aborting build leaves out, and the optimiser has more edges to
walk. Neither is measured here. Neither changes the answer.

Two paths read the strategy with `cfg!(panic = "abort")`. One is the terminal
restore in `crates/stella-tui/src/term.rs`. The other is the journal flush in
`crates/stella-cli/src/session_persist.rs`. Both already had an unwinding
branch. That branch is what a shipped build now takes. Neither `abort` branch
is dropped. A host that builds these crates with an aborting profile still
needs it.

Comments all over the tree said a panic ends the program in a release build.
They are fixed in the same change. A comment that claims more than the build
does is worse than no comment.

## How this is checked

`cargo test` always unwinds. So no test can see the strategy a release build
was made with. `crates/stella-tui/examples/panic_guard_probe.rs` can see it.
It panics inside the real panel guard. It exits 0 only if the catch ran. CI
runs it with `--release` next to the release smoke build. Put `abort` back and
that step dies on a signal.
