# stella-diag

The **diagnostic plane**: the stream that answers *why did the program behave
this way?* Design:
[`../../docs/spec/diagnostics.md`](../../docs/spec/diagnostics.md), which
generalises the shipped, crate-scoped
[`../../docs/spec/serve-observability.md`](../../docs/spec/serve-observability.md)
to the whole workspace.

Stella has four observability planes and, until this crate, had built three:

| Plane | Question it answers | Where it lives |
|---|---|---|
| **Domain** | *What did the agent do?* | `AgentEvent`, [`stella-protocol`](../stella-protocol) |
| **Ledger** | *What did it cost, and can I prove it?* | receipts, `crates/stella-cli/src/trace.rs` |
| **Presentation** | *What should the human see?* | `println!`, [`stella-tui`](../stella-tui) |
| **Diagnostic** | *Why did the program behave this way?* | **here** |

The missing fourth is why 682 `println!`s and 625 `let _ =`s coexisted: a
statement with no home gets split between the plane that is easiest to reach
(stdout) and the one that costs nothing (the floor).

## The routing rule

The one thing to remember, and the reason this crate is not a second event
stream:

> **A human reads it now → presentation. It should replay → domain. It explains
> a decision the program made → diagnostic. It is none of those → it is not a
> log line, it is a metric.**

## Where it sits

A **leaf**. It depends on `serde`/`serde_json` and **nothing else in the
workspace**, which is the property that lets all seventeen other crates depend
on it without a cycle. Adding a `stella-*` path dependency here forecloses that
for whichever crate it names — so don't.

## Boundary — does this change belong here?

This crate owns the diagnostic *envelope*, not the diagnostics themselves: the
record shape, `Cx`, `Dx`, the closed `Loggable` field vocabulary, the filter,
the sinks, and the crash ring. A change belongs here when it alters what a
record can safely carry or where records go — a new content-free field type in
the `ShortId`/`PathClass` family, a new sink, filter semantics. Emission sites
do not: a crate that wants to log takes a `&Dx` and implements `Facet` on its
own enum, so a new diagnostic in `stella-store` is a `stella-store` change and
this crate does not recompile.

Two things can never land here. A workspace dependency — the leaf property
stated above is the entire point, and one `stella-*` line in the manifest
forecloses it for the crate it names. And any widening of `Loggable`: an impl
for `String`, `Path`, `serde_json::Value`, or a `Display`/`Debug` blanket is
not a convenience, it is the removal of the compile-time guarantee this crate
exists to provide — the crash ring is attachable-by-a-stranger only because
such an impl does not exist. Weakening that is off the table, however
reasonable the individual field looks; the sanctioned outlets are `PathClass`,
`log_enum!`, and a justified `Redacted`.

For a planned record that fails [the routing rule](#the-routing-rule), the fix
is to move the statement, not to grow this crate a new plane: something that
should replay is an `AgentEvent` and belongs in
[`stella-protocol`](../stella-protocol); text a human reads now is
presentation and belongs in the caller or [`stella-tui`](../stella-tui); cost
and proof belong to the ledger plane.

A new crate is the wrong answer for almost anything diagnostic-shaped. The
workspace rule: a new crate is justified only when functionality (a) sits
behind a port/trait and would otherwise drag heavy dependencies into a crate
that is deliberately light, (b) needs a dependency direction the current graph
forbids, or (c) is a genuinely separate deliverable with its own binary and
release cadence. This crate is itself the (b) shape — one serde-only home
every crate can reach without a cycle — and per-crate vocabularies already
scale through `Facet` without new crates. Otherwise extend the existing crate:
a new one costs a workspace-table row, an impacted-crates scope, CI time, and
a README, and a wrong split is harder to undo than a wrong merge. Adding one
means updating AGENTS.md's workspace table and the root `Cargo.toml` members
in the same PR.

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.

## Content in a log does not compile

The part worth building rather than importing. A field value is not
`serde_json::Value` and not `impl Display`; it is a closed type constructible
only from things that cannot carry runtime content.

```rust
use stella_diag::{diag, Cx, Dx};

let (dx, records) = Dx::capturing();
let cx = Cx::new().session("session-a1b2c3d4").turn(3);

diag!(&dx, warn, "store.migration.retry", cx: cx, attempt = 2_u32, backoff_ms = 250_u32);
```

```json
{"ts":1754006400123,"level":"warn","code":"store.migration.retry",
 "target":"stella_store::migrate","cx":{"session":"a1b2c3d4","turn":3},
 "fields":{"attempt":2,"backoff_ms":250}}
```

`Loggable` is implemented for integers, `bool`, `f64`, `Duration`,
`&'static str`, closed enums declared through `log_enum!`, `ShortId`,
`PathClass` and `Redacted` — and deliberately **not** for `String`, a
non-`'static` `&str`, `Path`, `PathBuf`, `serde_json::Value`, or any `Display`
blanket. So this is a type error:

```rust,compile_fail
diag!(&dx, warn, "tools.write.denied", path = user_path);   // ← String
```

`tracing` would log that happily with `%user_path`. That is the strongest
single reason the design defers adopting the facade: it would trade a
compile-time privacy guarantee for a review-time one.

**Stated honestly:** `Box::leak` would defeat this. That is a deliberate,
single-token, greppable act, disallowed by the workspace
[`../clippy.toml`](../clippy.toml). The claim is not "impossible"; it is
*"impossible by accident, and loud on purpose"*.

The witness is a compile-fail test, not a runtime one —
[`tests/ui/`](tests/ui/) has five cases a reasonable contributor would actually
write, and each must fail to build. Regenerate the expected output after an
intentional change (including a toolchain bump) with:

```bash
TRYBUILD=overwrite cargo test -p stella-diag --test compile_fail
```

## Fields that carry the useful part without the dangerous part

| Type | Records as | Why it is safe |
|---|---|---|
| `ShortId` | `"a1b2c3d4"` | eight hex characters, never a whole id — enough to correlate, not enough to act |
| `PathClass` | `{"class":"inside_workspace","depth":4,"ext":"rs"}` | answers "outside the workspace, four deep, a `.rs` file" without the bytes of the path |
| `Redacted` | `{"reviewed":"main","note":"…"}` | the explicit hatch — see below |

`PathClass`'s `ext` is a **closed vocabulary**, which is one place this crate is
stricter than the design sketch: a raw extension is still attacker-chosen text
(`secrets.API_KEY_sk_live_…` is a legal filename), so anything unrecognised
records as `other`.

## The escape hatch, made expensive on purpose

Some runtime strings are genuinely safe and genuinely useful. `note!` accepts
**only a string literal**, so the justification lives in the source, is
greppable, shows up in the diff that introduced it, and travels into the record
where a reader of a crash file can see it:

```rust
Redacted::reviewed(branch_name, note!("git ref names are user-chosen but are not model content"))
```

An escape hatch with a budget is an escape hatch; one without is a loophole.

## Filtering

A real filter, not a level. Longest-prefix wins, on module-path boundaries:

```
STELLA_LOG=warn
STELLA_LOG=warn,stella_store=debug,stella_model=trace
STELLA_LOG=off
```

The filter governs **sinks**, never emission. Everything at every level always
reaches the counters and the crash ring, which is what keeps a number from
changing when you raise verbosity — and what keeps a crash dump complete for a
user who ran at the default `warn`.

A clause that does not parse is skipped and reported, never fatal: a typo in a
log knob must not take a process down.

## The crash ring

A bounded in-memory ring (2,000 records or 1 MiB, whichever binds first) holding
every record at every level. It reaches disk only on a panic or a non-zero exit,
landing at `.stella/private/crash-*.jsonl` — 0700 directory, 0600 file.

This is what lets stella say the sentence it could not before:

> Attach `.stella/private/crash-*.jsonl` to the issue.

And this is where the compile-time guarantee pays off completely: because
content cannot enter a record, that file is content-free **by construction**, so
a maintainer can ask a stranger on the internet for it without a privacy review
and the stranger can send it without reading it first. `stella doctor
--last-failure` prints the newest one.

## Correlation without spans

`Cx { session, execution, turn, step }` rides as an **explicit parameter** — not
a thread-local, not a task-local, both of which are wrong under `!Send` turn
futures and wrong under `tokio` task migration. `Dx` is a handle, not a global,
for the same reason.

This is nearly free here because invariant 2 already banned ambient state:
`tracing`'s spans exist to reconstruct context that ordinary code loses, and
this codebase does not lose it. The architecture stella chose for testability
hands correlation over for nothing.

## What this deliberately does not do

- **No logging in `stella-core`'s pure functions.** Compaction, eviction, loop
  detection, budget, skill selection and hook matching keep *returning* their
  rationale as typed values; the caller — already at an I/O boundary — records
  it. This is the invariant most likely to erode, and the one most responsible
  for `stella-core` being the crate the audit scores at 100.
- **No egress, at any level, in any build.** No sink opens a socket.
- **No per-token records.** `AgentEvent::TextDelta` fires per token and produces
  nothing here; the sanctioned alternative is a bounded tally.
- **No `Display`/`Debug` blanket impl for fields**, however convenient. That
  single blanket impl is the entire hole this crate exists to close, and it will
  be proposed by someone every six months.
- **No replacement of `AgentEvent`.** The domain plane stays the authority for
  replay; the diagnostic plane references it and never restates it.

## Adding a diagnostic to your crate

1. Depend on `stella-diag` (it is a leaf; there is no cycle to worry about).
2. Take a `&Dx` where you need one. Do not reach for a global — there isn't one.
3. `diag!(dx, warn, "yourcrate.thing.happened", cx: cx, count = n)`. The `code`
   is dotted, stable, and a public surface: operators alert on it and it
   outlives any message text.
4. If a value will not compile as a field, that is the design working. Reach for
   `PathClass`, a `log_enum!`, or — with a written justification — `Redacted`.

For a whole vocabulary rather than a call site, implement `Facet` on a per-crate
enum: the crate that owns the events owns their rendering, so nobody edits a
shared enum and no new variant recompiles the world.
