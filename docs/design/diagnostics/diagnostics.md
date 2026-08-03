# Diagnostics for the workspace — the fourth plane, and why content in a log must not compile

**Status:** **Proposed.** Generalises the shipped, crate-scoped design in
[`serve-observability.md`](./serve-observability.md) to all 17 crates and the two
shipped binaries.
**Date:** 2026-08-01. **Owner:** Mac Anderson.
**Companion:** read `serve-observability.md` first. This document does not
restate its architecture; it adopts it, names the four places it does not scale
as written, and fixes those four.

---

## 1. The finding, measured

The prompt was "stella has no proper logging system". That is directionally
right and imprecise in a way that matters, because the imprecision is where the
design comes from. Measured against the tree at `20c5645d`:

| | |
|---|---|
| Crates in the workspace | 17 (+2 bench harnesses), across 19 manifests |
| Manifests declaring `tracing`, `log`, `env_logger`, `slog`, `fern`, or any logger | **0** |
| `tracing` in `Cargo.lock` | present, **transitively only**, via `reqwest`/`hyper` |
| Log file written to disk, ever, by either binary | **0** |
| `--verbose` / `--quiet` / `--log-level` flag on `stella` | **none**; eight global flags exist, no verbosity among them |
| `STELLA_LOG` env var | does not exist |
| `println!` in `stella-cli` | 682 |
| `eprintln!` in `stella-cli` | 71 |
| `let _ = …` in non-test `src/` | **625** |
| `.ok();` discarding a `Result` in non-test `src/` | **63** |

So: no framework, no file, no level, no filter, no flag. The claim is confirmed.

### 1.1 The part the claim gets wrong, which is the useful part

One crate has an excellent one. `stella-serve/src/observe/` is 2,495 lines
across six modules: 18 serde-first event variants, an `Observer` port, a JSONL
stderr sink, a metrics fanout that cannot disagree with the log, a bounded
per-turn tally, a sentinel redaction sweep, and an exactly-one property test —
with `serve-observability.md` behind it, a document that argues its own
dependency decision and states the trigger that would reverse it.

That is not a project without a logging design. It is a project with **one good
logging design at the wrong scope**. The correct move is therefore not to go
shopping for a framework; it is to promote what already works and fix the four
things that break when it goes from one crate to seventeen (§4).

### 1.2 What is actually lost today

Three concrete failures, each currently indistinguishable from success:

| Situation | Where it dies |
|---|---|
| A `stella run` that produced a wrong answer for an environmental reason (a hook that didn't fire, a settings file that didn't parse, a provider that silently dropped a pinned effort) | 625 `let _ =` sites and 63 `.ok()`s |
| A user hitting a bug we cannot reproduce | there is no artifact to ask them for. `stella` writes no log |
| A library embedder (`stella-serve`, the Oxagen sidecar) whose embedded engine misbehaves | `stella-core` emits `AgentEvent`, which is a *domain* stream; nothing carries operational cause |

The second is the one that decides whether this project is pleasant to
contribute to. Every mature OSS CLI can say "run it again with `-vv` and attach
the log". Stella cannot say that sentence. For a *coding agent* it is worse than
usual: runs are nondeterministic and expensive, so "reproduce it with verbose
on" is frequently not a request the user can fulfil.

---

## 2. The reframe: four planes, three built

The reason 682 `println!`s and 625 `let _ =`s coexist is not carelessness. It is
that stella has three observability planes, all good, and no home for the fourth
kind of statement — so it gets split between the plane that is easiest to reach
(stdout) and the one that costs nothing (the floor).

| Plane | Question it answers | Where it lives | State |
|---|---|---|---|
| **Domain** | *What did the agent do?* | `AgentEvent`, 37 variants, `stella-protocol/src/event.rs` | built, excellent. Replayable, wire-stable, consumed by the TUI, observatory and journal |
| **Ledger** | *What did it cost, and can I prove it?* | receipts, `telemetry`, `stella-cli/src/trace.rs` (#1042) | built. A fold over settled state, not a live capture |
| **Presentation** | *What should the human see?* | 682 `println!` in `stella-cli`, the TUI | built |
| **Diagnostic** | *Why did the program behave this way?* | — | **missing** |

Two consequences follow, and they are the whole design brief:

1. **The new plane must not duplicate the first.** A second stream carrying what
   `AgentEvent` already carries would be a weaker parallel authority — the exact
   objection `serve-observability.md` §9.2 raises against putting `tracing` in
   `stella-core`, and it is correct. The diagnostic plane *references* domain
   events by `seq`; it never restates them (§8).
2. **The new plane must relieve the third.** 71 `eprintln!`s in `stella-cli` are
   diagnostics wearing presentation clothes. They are unfilterable, unroutable
   and untestable-except-by-scraping precisely because stdout was the only door
   open. §9 is the routing rule that closes this for good.

### 2.1 The one-sentence routing rule

Contributors need a rule they can apply without reading this document:

> **A human reads it now → presentation. It should replay → domain. It explains
> a decision the program made → diagnostic. It is none of those → it is not a
> log line, it is a metric.**

---

## 3. Constraints

Each has already rejected an obvious approach.

1. **Invariant 2 — no I/O in the engine.** Rules out any logger `stella-core`
   calls directly. Emission must be an injected port, and the *pure* decision
   functions must not log at all (§7.3).
2. **Invariant 3 — zero telemetry egress by default.** No sink opens a socket.
   But the sharper constraint is the one `serve-observability.md` §8 names:
   *operators ship logs*, so a log file is the easiest accidental egress there
   is. §5 is the answer, and it is stronger than the current one.
3. **Invariant 4 — serde-first.** The record crosses crate boundaries and lands
   on disk; it round-trips with a test.
4. **Invariant 5 — typed errors, no panics.** A sink that cannot write must
   degrade. Losing a log line must never lose a turn. Invariant 5 also *pays
   for* this design: every workspace error is already an enum, so structured
   error fields are nearly free here (§5.3).
5. **Invariant 7 — byte-stable prompts.** Nothing here may perturb prompt
   construction, including by timing.
6. **The file-size ratchet** (`scripts/check-file-size.sh`). New code goes in
   new modules.
7. **AGENTS.md dependency policy.** §10 is the justification, and its conclusion
   is again *not yet* — but for a different reason than last time, and with a
   trigger that has moved closer.
8. **The hot path stays free.** `AgentEvent::TextDelta` fires per token. The
   diagnostic plane emits **nothing** per token; the sanctioned alternative is
   the bounded tally `observe/tally.rs` already demonstrates.

---

## 4. What breaks when `observe/` scales from one crate to seventeen

The shipped design is right. Four things in it do not survive the jump, and this
is the entire delta:

| # | Holds at crate scope | Breaks at workspace scope | Fixed in |
|---|---|---|---|
| 1 | One closed `ServeEvent` enum, 18 variants | A shared god-enum across 17 crates: every new variant recompiles the world, and no crate can add an event without editing a crate it does not own | §5.1 — envelope + per-crate facets |
| 2 | Redaction proven by a sentinel sweep | A sweep tests the sentinel you wrote, not the property you meant — as §8 of that document *itself records*, after a working vacuity guard still missed a planted leak | §5.2 — make it a compile error |
| 3 | `STELLA_SERVE_LOG` is a level, not a filter | 17 crates need per-target filtering, or `debug` is unusable | §6 |
| 4 | Correlation by explicit `request_id` fields | Cross-crate, cross-process causality needs one carrier, or every crate invents its own | §7 |

---

## 5. The record: one envelope, many facets, and a closed field type

### 5.1 Envelope and facets

A **new leaf crate, `stella-diag`** — depending on `serde`/`serde_json` and
nothing else in the workspace, so all 17 crates may depend on it without a
cycle. (`stella-observatory` is taken and means something else: projections.)

The envelope is fixed and shared:

```json
{
  "ts": 1754006400123,
  "level": "warn",
  "code": "store.migration.retry",
  "target": "stella_store::migrate",
  "cx": { "session": "a1b2c3d4", "execution": "9f8e7d6c", "turn": 3, "step": 11 },
  "fields": { "attempt": 2, "backoff_ms": 250, "error": { "code": "Io", "kind": "PermissionDenied" } }
}
```

The **facets** are per-crate typed enums that render into it. `ServeEvent`
becomes one facet, unchanged in spirit; `stella-store` owns `StoreDiag`,
`stella-model` owns `ModelDiag`. Nobody edits a shared enum, nothing recompiles
the world, and emission stays typed — which is the property that made the serve
design testable in the first place (a test asserts on a value, never on scraped
stderr).

`code` is a **stable, documented public surface** — see §11.

### 5.2 The headline: content in a log does not compile

This is the part that is worth building rather than importing.

A field value is not `serde_json::Value` and not `impl Display`. It is a closed
type constructible only from things that *cannot carry runtime content*:

```rust
pub trait Loggable { fn to_field(&self) -> FieldValue; }
```

| Implemented for | Why it is safe |
|---|---|
| integers, `bool`, `f64`, `Duration` | no content channel |
| `&'static str` | lives in the binary; cannot hold a runtime string |
| enums declared through `log_enum!` | a closed variant set is a finite, reviewed vocabulary |
| `ShortId` | a truncated correlation handle, never a whole id (§7.2) |
| `PathClass` | §5.4 |
| `Redacted<T>` | an explicit, greppable, justified escape hatch (§5.5) |

Deliberately **not** implemented for `String`, non-`'static` `&str`, `Path`,
`PathBuf`, `serde_json::Value`, or any `Display` blanket.

The consequence:

```rust
diag!(warn, "tools.write.denied", path = user_path);   // ← does not compile
```

`tracing` will happily log that with `%user_path`. Here it is a type error.
Invariant 3's most dangerous failure mode stops being a thing review has to
catch and starts being a thing the compiler catches.

**Stated honestly, because overclaiming is worse than the gap:** `Box::leak`
turns a `String` into a `&'static str` and would defeat this. That is a
deliberate, single-token, greppable act — banned by a clippy `disallowed_methods`
entry in the same change. The claim is not "impossible"; it is *"impossible by
accident, and loud on purpose"*. That distinction is the one
`serve-observability.md` §8 learned the hard way.

### 5.3 Errors, which are the real leak

Error *messages* are where paths and content actually escape into logs —
`format!("{e}")` on an `io::Error` carries the filename. Invariant 5 has already
solved this without anyone noticing: **every library error in this workspace is
a typed enum**. So a facet derives from it:

```rust
log_error! {
    ModelError {
        HttpStatus { status },          // logged
        Decode { .. },                  // variant name only
        Io(source) => kind,             // io::ErrorKind, never the message
    }
}
```

The record gets `{"code":"HttpStatus","status":429}` — a stable code plus fields
explicitly marked loggable. The `Display` string is never emitted. In a codebase
where errors were `String`s this would be a large piece of work; here invariant 5
paid for it years ago.

### 5.4 Paths, which are the thing you most want and most cannot have

The single most-wanted log field in a coding agent, and the most dangerous.
`PathClass` is the compromise, and it is enough for nearly every real diagnosis:

```json
{ "class": "inside_workspace", "depth": 4, "ext": "rs" }
```

Classes: `inside_workspace` · `home` · `temp` · `absolute_other` · `relative`.
"The write was denied because the path was outside the workspace, four levels
deep, a `.rs` file" answers the operational question. The bytes of the path do
not.

### 5.5 The escape hatch, made expensive on purpose

```rust
Redacted::reviewed(branch_name, note!("git ref names are user-chosen but are not model content"))
```

`note!` accepts only a string literal, so the justification lives in the source,
is greppable, and shows up in the diff that introduced it. A gate script counts
`Redacted::reviewed` sites against a per-crate ceiling that can only fall —
the same ratchet idiom `check-file-size.sh` already uses. An escape hatch with a
budget is an escape hatch; one without is a loophole.

---

## 6. Filtering: a real filter, not a level

`STELLA_SERVE_LOG` is a level, and `serve-observability.md` §9 lists that as one
of the things knowingly given up. At 17 crates it stops being acceptable: `debug`
across the workspace is noise nobody can read.

```
STELLA_LOG=warn                                  # global
STELLA_LOG=warn,stella_store=debug,stella_model=trace
STELLA_LOG=off
```

Parsed into a `Filter` matching on `target` prefix, longest-prefix wins.
Unparseable values fall back to the default with one `warn` record naming the
bad clause — the posture `STELLA_SERVE_LOG` already takes, and it is right: a
typo in a log knob must not take a process down.

Surfaces:

| Surface | Meaning |
|---|---|
| `-v` / `-vv` / `-vvv` | `info` / `debug` / `trace`, global flags |
| `--log-level <spec>` | the full filter grammar |
| `--log-file <path>` | JSONL to a file, 0600 |
| `STELLA_LOG` | same grammar |
| `STELLA_SERVE_LOG` | kept as a documented alias; the shipped surface does not break |

Default: `warn` to stderr, human-readable one-line format when stderr is a TTY,
JSONL when it is not. Everything at every level, always, goes to the ring (§7.4)
regardless of filter — the filter governs *sinks*, never *emission*, which is
what keeps counters correct at any verbosity.

---

## 7. Correlation without spans

### 7.1 The context is already threaded

`tracing`'s substantive advantage over a hand-rolled logger is span propagation.
The usual reason hand-rolled loggers lose is that they force every call site to
re-thread ids by hand.

Stella does not have that problem, because **invariant 2 already forces it**.
"Plain synchronous functions over owned data" means there is no ambient state to
begin with and every decision function already receives its inputs explicitly.
So `Cx { session, execution, turn, step }` rides as an explicit parameter — not a
thread-local, not a task-local, both of which are wrong under `!Send` turn
futures (see `stella-cli`'s sub-agent work) and wrong under `tokio` task
migration.

The architecture stella already chose for testability hands correlation over for
free. That is the strongest argument in this document for not adopting a facade.

### 7.2 Ids are truncated

`ShortId` keeps 8 hex characters. `serve-observability.md` §8 argues this for
turn ids — a turn id is a second factor, and writing it verbatim into a file
operators ship spends that factor for nothing. The argument generalises
unchanged to session and execution ids.

### 7.3 Pure functions do not log

Compaction, eviction, loop detection, budget, skill selection, hook matching:
these must keep **returning** their rationale as typed values, which is what
makes them property-testable and is what makes `stella-core` the crate the audit
holds up as correct. Adding emission there would trade that for strictly less.
The caller — which is already at an I/O boundary — renders the returned rationale
into a diagnostic record. This is a rule, not a preference.

### 7.4 The crash ring

The distinctive sink, and the one with the most user-visible value.

A bounded in-memory ring (2,000 records or 1 MiB, whichever first) holds
**every** record at **every** level, filter notwithstanding. It is written to
disk only on a panic hook, or on a non-zero exit from `main`, landing at
`.stella/private/crash-<ts>.jsonl` — the 0700 directory `trace.rs` already
establishes, 0600 file.

This is what lets stella say the sentence it currently cannot:

> Attach `.stella/private/crash-*.jsonl` to the issue.

And here §5.2 pays off completely. Because content in a record does not compile,
that file is content-free *by construction* — so a maintainer can ask a stranger
on the internet to attach it without a privacy review, and the stranger can send
it without reading it first. A crash dump you can safely ask for is worth more
than a verbose flag nobody can rerun. `stella doctor --last-failure` prints it;
`stella bug-report` packages it with the version, target triple, and redacted
effective config.

---

## 8. Not a fourth stream: the domain bridge

The diagnostic plane must not restate `AgentEvent`. A `DomainBridge` adapter in
`stella-cli` subscribes to the existing event stream and emits diagnostic
records at `debug` that carry **only** the seq and a shape — never the payload:

- `AgentEvent::TextDelta` → **no record**. Counted into the turn tally, exactly
  as `observe/tally.rs` concluded after building the alternative and rejecting
  it for putting model output in a log.
- `AgentEvent::ToolCall` → `{code:"agent.tool.call", seq, tool:"bash", args_bytes:412}`.

One merged, ordered timeline for an operator; one authority for replay. The
domain plane stays the source of truth and the diagnostic plane points at it.

---

## 9. Retiring the 71 `eprintln!`s and ratcheting the 625 discards

Design without a migration path is a wish. Both halves are ratchets, because a
625-site cleanup as one heroic PR is not reviewable and will not land.

**The discards.** An extension trait makes naming the loss the same length as
dropping it:

```rust
let _ = tx.send(answer);                                    // before
tx.send(answer).or_diag(&dx, "serve.reverse.answer_dropped"); // after
```

`scripts/check-silent-discards.sh` counts `let _ =` and `.ok();` in non-test
`src/` per crate against checked-in ceilings, joins `make gate`, and
`make discards-update` may only lower them. Identical in shape to
`check-file-size.sh`, which contributors already understand.

**The prints.** §2.1's rule sorts them. The 682 `println!`s are overwhelmingly
presentation and stay (routed through the render layer, not touched here). The
71 `eprintln!`s split: operator-facing warnings become `warn` records;
user-facing notices stay presentation but move behind the render layer so they
are testable. The ratchet counts `eprintln!` in non-test `src/`, ceiling-only-down.

---

## 10. The dependency decision: still not `tracing` — and the trigger has moved

`serve-observability.md` §9 recommended deferring, and named two triggers.
Re-examined at workspace scope, honestly:

**Trigger 1 — "a second Stella binary needs correlated cross-process traces" — is
arguably live.** There are two shipped binaries (`stella`, `stella-serve`) and
they do talk, over reverse-RPC. The propagation value is real.

**The recommendation is nonetheless to defer, for reasons that changed:**

1. The four arguments in §9 of that document mostly still hold, but the
   load-bearing one is now different: §7.1. Correlation is free here because
   invariant 2 already banned ambient state. `tracing`'s spans exist to
   reconstruct context that ordinary code loses; this codebase does not lose it.
2. §5.2 is not available through `tracing`. `%value` / `?value` log anything
   `Display`/`Debug` can render, which is the precise hole invariant 3 cannot
   afford. Adopting the facade would trade a compile-time privacy guarantee for
   a review-time one. That is the wrong direction, and it is the strongest single
   reason in this document.
3. Two binaries and a leaf crate is a supply-chain surface an AGPL CLI can
   defend. `tracing-subscriber` + `tracing-opentelemetry` + the OTLP exporter is
   a large transitive tree to make unconditional.

**So: `tracing` arrives as an off-by-default `otel` cargo feature**, at slice 7,
implementing the same `Diag` port as one additional sink file — no emit site
touched, which is what the port has been for since the serve design drew it.
`Cx` serialises to a W3C `traceparent`, so the ids are already correct on the
day that feature is switched on. Enterprise and host integrations turn it on;
the default build stays a leaf crate and `serde`.

**What is still given up, plainly:** off-the-shelf OTel export in the default
build, and ecosystem sinks. Nothing else — §6 recovers the filtering that §9
listed as lost, and §7 recovers the correlation.

---

## 11. Diagnostic codes as documented public surface

`code` is stable and public, in the way `rustc`'s `E0308` is: operators alert on
it, docs link it, and it outlives the message text.

`docs/reference/diagnostics.md` is **generated** from the facet registry by a
`make` target, one section per code: what it means, what fields it carries, what
to do about it. `scripts/check-diagnostic-codes.sh` fails the gate on an
undocumented code, a duplicated code, or a documented code no longer emitted.

Very few projects treat log codes as a versioned surface. It is the difference
between a log an operator greps and a log an operator can build on, and it is
most of what "reference grade" means here.

---

## 12. Proving it

Per AGENTS.md, each slice needs a test that fails on the old code.

| # | Property | Test |
|---|---|---|
| 1 | Content cannot enter a record | a `trybuild` compile-fail case: `diag!(info, "x", p = String::from("secret"))` **must not compile**. This is the witness for §5.2, and it is a stronger artifact than a sentinel sweep because it fails at the only moment that matters |
| 2 | The sentinel sweep still runs | belt and braces. Retained from serve, extended workspace-wide, with the planted-leak discipline §8 of that doc learned |
| 3 | Filter never gates emission | property: for any filter and any record sequence, counters and the ring are identical; only sink output differs |
| 4 | A failing sink never fails a caller | property: a sink returning `Err` on every write leaves `emit` infallible and the turn intact |
| 5 | The ring survives a panic | integration: panic mid-turn, assert the crash file exists, is 0600, parses as JSONL, and contains the last pre-panic record |
| 6 | No per-token record | property over a synthetic 10k-`TextDelta` turn: diagnostic record count is `O(steps)`, not `O(tokens)` |
| 7 | Codes are documented | gate script, §11 |
| 8 | Envelope round-trips | invariant 4 |

---

## 13. Shipping it

Each slice is independently reviewable and independently valuable. Slices 1–3
are the ones that change a user's life.

| # | Contents | Why this order |
|---|---|---|
| **1** | `stella-diag`: envelope, `Level`, `Filter`, `Loggable`, `log_enum!`, `Diag` port, `JsonlSink`, `Capture`, `NullSink`, ring. Properties 3, 4, 8 and the `trybuild` case | the leaf crate everything else needs |
| **2** | CLI wiring: `-v`/`--log-level`/`--log-file`, `STELLA_LOG`, panic hook, crash ring on disk, `stella doctor --last-failure`. Property 5 | the moment the project can say "attach the log" |
| **3** | `stella-serve/observe` re-based on `stella-diag`; `ServeEvent` becomes a facet; `STELLA_SERVE_LOG` kept as an alias | proves generality against the one real existing consumer, and retires the duplicate |
| **4** | `or_diag`, `check-silent-discards.sh`, ceilings, and the first crate cleaned (`stella-store`) | starts the ratchet falling |
| **5** | `log_error!`, `PathClass`, `Redacted` ceiling script | the field vocabulary the remaining crates need |
| **6** | `DomainBridge`; property 6; `stella-model`, `stella-tools`, `stella-cli` facets | the merged timeline |
| **7** | `docs/reference/diagnostics.md` generator + gate (§11); the off-by-default `otel` feature (§10) | the public surface, and the deferred dependency on its own terms |

---

## 14. What this deliberately does not do

- **No logging in `stella-core`'s pure functions.** §7.3. They return rationale;
  callers record it. This is the invariant most likely to erode, and the one
  most responsible for `stella-core` being the crate the audit scores at 100.
- **No egress, at any level, in any build.** No sink opens a socket. The `otel`
  feature of §10 is opt-in at compile time *and* requires an endpoint at
  runtime; absent both, nothing leaves the machine.
- **No per-token records.** §3.8, property 6.
- **No `Display`/`Debug` blanket impl for fields**, however convenient — that
  single blanket impl is the entire hole §5.2 exists to close, and it will be
  proposed by someone every six months.
- **No replacement of `AgentEvent`.** The domain plane is the authority for
  replay and stays that way. §8.
- **No touching the 682 presentation `println!`s** beyond routing them. They are
  not logs and should not become logs.
