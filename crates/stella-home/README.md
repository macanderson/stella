# stella-home

Where the user-global stella home resolves. Everything user-global lives
directly under `~/.stella` on every platform — no OS-specific data dir — and
this crate is the one place that decides what `~/.stella` means for a given
process.

```rust
stella_home::home_dir();    // $HOME, else %USERPROFILE%
stella_home::stella_home(); // $STELLA_HOME, else ~/.stella
stella_home::data_dir();    // $STELLA_DATA_DIR, else the stella home, else "."
```

## Boundary — does this change belong here?

This crate owns one decision: what path a process should treat as the stella
home, the user-tier data dir, and the config dir, given `$HOME` /
`%USERPROFILE%` and the three `STELLA_*` overrides. If a planned change alters
that answer — a new override variable, a new tier, a change to precedence — it
belongs here, and it arrives as a pure `resolve_*` half plus a thin
environment-reading wrapper, like every resolver already present.

Everything else is out. The crate has **no dependencies and must keep none**
(the next section is the full argument), so any change that wants `serde`, a
workspace sibling, or any registry crate is wrong here by construction. It
also answers "where", never "make it so": no directory creation, no permission
hardening, no legacy-layout migration — those are I/O with failure modes, and
they live in [`stella-store`](../stella-store)
(`crates/stella-store/src/private.rs`, `crates/stella-store/src/home.rs`).
Per-process redirection seams are likewise the consumer's job, like `UserPaths`
in `crates/stella-cli/src/paths.rs`. Even a new resolver has a bar to clear:
both sides of the store/observatory divide must need the same answer — a
helper only one consumer wants belongs in that consumer.

`self_driving_root` / `legacy_self_driving_roots` are the worked example of a
**feature-specific** path clearing that bar (#1755). They look like they belong
in `stella-cli`, and would, but for the same reason the crate exists at all:
`stella-cli` is the single writer of that directory and `stella-observatory`
reads it back read-only, the two must not know each other, and a root spelled
differently in one of them shows an operator an empty dashboard for a machine
holding a full ledger. That is one answer needed on both sides of the divide,
which is the test — not "is it about the home directory". They still answer
"where" and never "make it so": the lazy migration those legacy roots exist for
is I/O with failure modes, and it stays in the CLI.

This crate is also the workspace's worked example of when a new crate is
justified. The rule: a new crate is warranted only when functionality (a) sits
behind a port/trait and would otherwise drag heavy dependencies into a crate
that is deliberately light, (b) needs a dependency direction the current graph
forbids, or (c) is a genuinely separate deliverable with its own binary and
release cadence. This crate is case (b) — [`stella-store`](../stella-store)
and [`stella-observatory`](../stella-observatory) must not know each other and
still needed one shared answer. Absent all three, extend an existing crate: a
new one costs a workspace-table row, an impacted-crates scope, CI time, and a
README, and a wrong split is harder to undo than a wrong merge. Adding one
means updating AGENTS.md's workspace table and the root `Cargo.toml` members
in the same PR.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.

## Why it is a crate and not a module

There were two implementations. `stella_store::usage::data_dir` was canonical;
`stella_observatory::global::data_dir` was a deliberate copy carrying a comment
asking future readers to keep it in sync by hand. The copy existed because the
observatory must **not** link [`stella-store`](../stella-store): `Store::open`
runs migrations, migrations are writes, and an observer must never mutate what
it observes.

Hand-synced duplicates of a resolution rule are a divergence waiting to happen
(#1139) — two crates answering differently for the same environment means the
dashboard reads a `usage.db` the CLI never writes. A leaf crate that pulls in
*nothing* is the one shape both sides can depend on without touching that
isolation.

**Therefore: this crate has no dependencies, and must keep none.** Not
`serde`, not a workspace sibling. Adding one takes the property away from
whichever crate the new dependency names.

## Pure resolvers, and the environment

Every resolver comes in two halves:

| Half | Reads `std::env` | Use it when |
|---|---|---|
| `resolve_data_dir(data_dir, stella_home)` | no | you already hold the anchors — a test, an injected port, a host told where its home is |
| `data_dir()` | yes | you are the process, at the point where reading ambient configuration is the intended behaviour |

The split is what lets this crate's own tests assert the precedence order
(`STELLA_DATA_DIR` → `STELLA_HOME` → `~/.stella` → `.`) without a single
`set_var`. Concurrent `getenv`/`setenv` is undefined behaviour on POSIX, the
test runner is multi-threaded, and the process environment is the one piece of
global state a test cannot own — so a resolution rule that can only be tested
by mutating it is a rule that gets tested badly.

`stella-cli` layers its own seam on top (`crates/stella-cli/src/paths.rs`): a
`UserPaths` value resolved once at startup and redirectable per-thread, so CLI
tests can point home and data-dir resolution somewhere else without touching
the process environment at all.
