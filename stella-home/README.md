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

`stella-cli` layers its own seam on top (`stella-cli/src/paths.rs`): a
`UserPaths` value resolved once at startup and redirectable per-thread, so CLI
tests can point home and data-dir resolution somewhere else without touching
the process environment at all.
