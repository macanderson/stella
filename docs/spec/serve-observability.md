---
id: serve-observability
title: "Observability for `stella-serve` — a typed event at every boundary"
status: implemented
---

# Observability for `stella-serve` — a typed event at every boundary

**Status:** **Built** — `crates/stella-serve/src/observe/`, all four slices of §11 in
one change. Corrects audit dimension 30 (Observability, 62/100) and closes
[#930](https://github.com/macanderson/stella/issues/930). Two things below were
changed *by* building them, and are marked **[amended]** where they appear:
the per-frame debug record in §7 was dropped (it would have put model output in
a log), and the handlers moved to a new `routes.rs` (§11). Everything else
shipped as designed.
**Date:** 2026-07-30. **Owner:** Mac Anderson.
**Companion:** [`serve-surface.md`](./serve-surface.md) — the route table and
turn lifecycle this document instruments. Read that first; everything here
attaches to surfaces described there.

---

## 1. The finding, measured

> `stella-core` emits a typed event at every boundary and reports discarded work
> explicitly. `stella-serve` emits two `println!`s and nothing else (#930), and
> the workspace has no logging framework at all.

All three clauses check out, and the middle one is understated. The precise
shape of the gap:

| | |
|---|---|
| Print statements in `stella-serve` | 10 — **all** in `src/main.rs` (2 `println!`, 8 `eprintln!`) |
| Print statements in the `stella-serve` *library* | **0**, across 3,423 lines and 9 modules |
| Direct dependency on `tracing` / `log` / any logger, any crate | **none**. `tracing` and `log` appear in `Cargo.lock` purely transitively, via `reqwest`/`hyper` |
| `stella-core` typed event variants | 34 (`AgentEvent`, `crates/stella-protocol/src/event.rs`) |

So the server's entire observable surface is its startup banner and its exit
code. Once `serve()` is running, the process is mute for the rest of its life.

### The work it currently throws away without saying so

This is the specific clause the audit contrasts against `stella-core`, so it is
worth enumerating. Every one of these is a real discard, verified in the code:

| Site | What is lost |
|---|---|
| `server.rs:500` — `let _ = handle_conn(stream, state).await` | **every** per-connection error, for every connection |
| `server.rs:495` — `let _ = stream.set_nodelay(true)` | a latency regression on every subsequent frame |
| `server.rs:481` — `AcceptAction::Backoff` | the accept loop degrading under fd exhaustion |
| `server.rs:483` — backoff returns `None` | the accept loop *giving up permanently* |
| `server.rs:866` — `encode_or_abort` | a frame the server could not encode; the turn dies and the server has no record it happened |
| `server.rs:324` — `reclaim_finished_unstreamed` | an evicted turn *and* every frame it had buffered |
| `pending.rs:87,98` — `let _ = tx.send(..)` | a host answer delivered into a dropped receiver |
| `pending.rs:107` — `clear()` | every in-flight reverse request, at teardown |
| `pending.rs:142` — `abandon()` | a reverse request whose deadline expired — **the wedge signal** |

The last row is the one that matters most. `abandon()` is called exactly when a
turn has waited out its full five-minute deadline because the host never
answered. That is the single most diagnostic event this service can produce, and
today it is a silent `HashMap::remove`.

### The four situations #930 says are indistinguishable

They are indistinguishable, and the table above says why each is invisible:

| Situation | Where it dies |
|---|---|
| turn wedged on a reverse request nobody will answer | `pending.rs:142` `abandon()` |
| host answering with the wrong `request_id` | `ServeError::UnknownRequest` → a 409 nobody counts |
| 429 storm from a misbehaving client | `register_turn` returning `None` |
| bearer-token brute force | `unauthorized_delay()` → a held 401 nobody counts |

---

## 2. The reframe: emission and sink are separate decisions

#930 poses one question — adopt `tracing`, or hand-roll a JSON logger — and
notes the blocker is that adopting `tracing` is a workspace-wide dependency
decision with a contested edge at `stella-core`.

That framing conflates two things that should be decided separately:

- **What is emitted.** A typed vocabulary of boundary events. This is the part
  the audit actually rewards — it is what `stella-core` does — and it is the
  part that is *testable*, because a test can assert on a typed value rather
  than scraping stderr.
- **Where it goes.** A sink. This is the part that costs a dependency.

Decide them separately and the dependency question stops being a blocker:

> Make emission typed and the sink a trait. Ship a zero-dependency JSONL sink
> today. Adopting `tracing` later becomes one new file implementing the same
> trait — **no emit site changes**.

That is the whole architecture. Everything below is detail.

---

## 3. Constraints this design has to satisfy

These are not preferences; each one has already rejected an obvious approach.

1. **Invariant 3 — zero telemetry egress by default** (AGENTS.md). Nothing here
   leaves the machine. Records go to stderr or an operator-chosen file; metrics
   are **pull**, over the same authenticated socket, never push. There is no
   sink that opens a connection. See §8 for the redaction discipline that keeps
   *content* out of records even locally.
2. **Invariant 2 — no I/O in the engine.** Rules out any design that reaches
   into `stella-core`. It does not have to: see §7.
3. **Invariant 4 — serde-first.** The event type crosses a crate boundary
   (`stella-serve` → its binary, and eventually the host), so it round-trips
   through `serde_json` with a test.
4. **Invariant 5 — typed errors, no panics.** A sink that cannot write must
   degrade, never panic. Losing a log line must not lose a turn.
5. **The file-size ratchet.** `server.rs` is 1,257 lines against a `LIMIT` of
   1,500 (`scripts/check-file-size.sh`). ~240 lines of headroom, and rustfmt's
   reflow makes that budget smaller than it looks. **New code goes in new
   modules**, not into `server.rs`.
6. **AGENTS.md dependency policy** — "No new dependencies casually. Every new
   crate gets a justification." §9 is that justification, and its conclusion is
   *not yet*.
7. **The engine hot path stays free.** `AgentEvent::TextDelta` fires per token.
   Any design that writes a log line per event makes the server slower than the
   model. §7 addresses this directly.

---

## 4. Layer 1 — `ServeEvent`, the typed vocabulary

A new module, `crates/stella-serve/src/observe/event.rs`. This mirrors `AgentEvent`
deliberately: same `#[serde(tag = ...)]` shape, same one-variant-per-boundary
discipline, same habit of naming discarded work rather than letting it vanish.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ServeEvent {
    // ---- process lifecycle ----
    Listening { addr: String },
    ShuttingDown { reason: ShutdownReason, live_turns: usize },

    // ---- the request fold: exactly one per accepted connection (§6) ----
    RequestCompleted {
        request_id: RequestId,
        method: Method,
        /// The route TEMPLATE — `/v1/turns/{id}/tool-result`, never the raw
        /// path. See §8.
        route: Route,
        /// `None` when the peer hung up before we answered. Absence is a
        /// terminal state here, not a missing field.
        status: Option<u16>,
        duration_ms: u64,
        bytes_in: u64,
        bytes_out: u64,
        turn: Option<TurnRef>,
    },
    /// Split out of `RequestCompleted` so a brute force is one grep, and so the
    /// throttle's own state is visible without changing what the guesser sees.
    Unauthorized { request_id: RequestId, route: Route, held_ms: u64 },

    // ---- turn registry ----
    TurnCreated  { turn: TurnRef, live_turns: usize },
    TurnRefused  { reason: RefusalReason, live_turns: usize },
    TurnSettled  { turn: TurnRef, outcome: SettledOutcome, tally: TurnTally },
    StreamEnded  { turn: TurnRef, frames_sent: u64, reason: StreamEndReason },

    // ---- reverse RPC: the wedge axis ----
    ReverseDispatched { turn: TurnRef, request_id: RequestId, kind: ReverseKind },
    ReverseAnswered   { turn: TurnRef, request_id: RequestId, kind: ReverseKind, waited_ms: u64 },

    // ---- work the server threw away (the clause the audit rewards) ----
    /// The host never answered; the port gave up at its deadline. THE wedge
    /// signal — `pending.rs:142` today.
    ReverseTimedOut  { turn: TurnRef, request_id: RequestId, kind: ReverseKind, waited_ms: u64 },
    /// A host answer that matched nothing, or matched the wrong kind.
    ReverseMisrouted { request_id: RequestId, fault: MisrouteFault },
    /// In-flight requests dropped wholesale — cancel, or session teardown.
    ReverseAbandoned { turn: TurnRef, in_flight: usize, reason: AbandonReason },
    /// A finished-but-unstreamed turn evicted to make room, with the frames
    /// nobody ever read.
    TurnReclaimed    { turn: TurnRef, buffered_frames: usize, age_ms: u64 },
    /// `encode_or_abort` substituted a terminal frame. The turn died of a bug
    /// in our own serialization and said nothing.
    FrameUnencodable { turn: TurnRef, error: String },
    /// The `let _ =` at `server.rs:500`, given a name.
    ConnectionFailed { request_id: RequestId, error: String },
    AcceptBackoff    { kind: AcceptKind, delay_ms: u64, streak: u32 },
    AcceptGaveUp     { kind: AcceptKind, streak: u32 },
}
```

Eighteen variants against `stella-core`'s 34, for a crate a tenth the size
(3,885 lines against 40,448) — proportionally denser coverage, which is what a
process boundary deserves.

Two shape decisions worth stating:

- **Severity is derived, not stored.** `ServeEvent::level()` is a match, so a
  record's level can never disagree with its variant, and adding a variant
  forces the author to classify it (`-D` non-exhaustive match).
- **Every payload field is an enum, an integer, or a bounded id.** The only
  `String`s are `addr`, and the two `error` fields — which carry *our* error
  types (`ServeError`, `io::Error`), never host data. That is what makes the
  redaction property in §8 provable rather than aspirational.

---

## 5. Layer 2 — the `Observer` port

Invariant 1's grain, applied to logging: ports, not concretions.

```rust
pub trait Observer: Send + Sync + 'static {
    fn emit(&self, event: &ServeEvent);
}
```

Four implementations, none of which need a dependency:

| Impl | Purpose |
|---|---|
| `JsonlSink` | one JSON object per line to stderr (or a file). Timestamps as epoch millis via `SystemTime` — the existing workspace convention (`crates/stella-store/src/sessions.rs`), so no time crate |
| `Metrics` | folds events into `AtomicU64` counters (§6) |
| `Fanout` | `Vec<Arc<dyn Observer>>`; the server holds one of these |
| `Capture` | test-only; a `Mutex<Vec<ServeEvent>>`. **This is what makes the acceptance criteria assertable** — tests match on typed variants, not on scraped stderr |

`JsonlSink` holds a `Mutex<Stderr>` so concurrent connections cannot interleave
half-lines, and a write failure is dropped on the floor rather than propagated:
per constraint 4, losing a log line must never lose a turn.

Level filtering is a single env var, matching the existing `STELLA_SERVE_*`
family:

```
STELLA_SERVE_LOG = off | error | info (default) | debug
```

`debug` is the only level that logs per-`AgentEvent` records; see §7 for why
that must not be the default.

### Why a trait rather than calling `eprintln!` directly

Because it makes the `tracing` question reversible. `TracingObserver` is one
file. Nothing above this line — not one emit site, not one test — changes if
and when the workspace adopts a facade. That is what converts §9 from a
blocking decision into a deferred one.

---

## 6. Layer 3 — the request record, in the fold

`handle_conn` has sixteen exits — fifteen `return`s plus a tail expression.
Adding an `emit()` before each is the obvious implementation and it is the wrong
one, for a reason this project has already been burned by and written down: a
rule distributed across N exits is one refactor from being silently false. The
PROOF rail (#901, "the proof rail resolves on every path, not just the happy
one") was exactly this shape — a surface that reported only on the paths someone
remembered, and hung on `pending` forever on the ones they did not.

So the record goes in the fold, and the emit sites lose the ability to forget.

**Mechanism.** A `Responder` owns the write half and is the only thing that
writes to it:

```rust
pub(crate) struct Responder<'a> {
    stream: &'a mut TcpStream,
    /// What was actually written. `None` until the first write — and `None` at
    /// the end is a *reportable terminal state*, not a missing record.
    wrote: Option<Wrote>,
}
```

Every `write_json` / `write_json_with_headers` / `write_sse_head` takes
`&mut Responder` instead of `&mut TcpStream`, so status and `bytes_out` are
captured by construction. Then `handle_conn` becomes a wrapper whose entire body
is the fold:

```rust
async fn handle_conn(mut stream: TcpStream, state: Arc<ServerState>) -> io::Result<()> {
    let started = Instant::now();
    let mut responder = Responder::new(&mut stream);
    let mut record = RequestRecord::opening(&state);   // request id assigned here

    let io = route(&mut responder, &mut record, &state).await;

    // The ONLY construction site of `ServeEvent::RequestCompleted`.
    state.observer.emit(&record.close(responder.wrote(), started.elapsed(), &io));
    io
}
```

The existing body becomes `route(..)`, unchanged in structure — its fifteen
exits stay fifteen exits, and none of them knows a record exists.

Four properties this buys, in the order the memory on this pattern prescribes:

1. **Declare before anything can fail.** `RequestRecord::opening` assigns the
   request id and start time before the first byte is parsed, so a request that
   dies in `read_head` still has an identity.
2. **"Unreported" is a terminal state, in the fold.** `close()` converts "no
   response written" into `status: None`, and an `Err` return into a
   `ConnectionFailed` alongside it. The `let _ = handle_conn(..)` at
   `server.rs:500` stops discarding anything because there is nothing left to
   discard.
3. **Delete the escape hatch.** `RequestRecord::close` consumes `self` and is
   the only place in the crate that constructs
   `ServeEvent::RequestCompleted`. **[amended]** The original wording claimed
   `-D dead-code` would *prove* there is no second path; it does not — a public
   enum variant is constructible anywhere, and sealing it would mean a private
   witness field that `Deserialize` then has to fabricate, which buys less than
   it costs. What `-D dead-code` did do is real and worth keeping: it found two
   helpers nothing reached (`RequestRecord::route`, `request_id_for`) and
   refused to compile until they were deleted, so the module has no
   almost-used second path lying around for a future edit to reach for. The
   *single-record* guarantee rests on rule 4, which is where it belongs.
4. **Prove it with a property, not examples.** §10 — and the property earned
   its keep: injecting the classic bug (skip the emit when nothing was written,
   i.e. forget the hangup path) fails it with `input of 0 byte(s) produced 0
   records, not exactly 1`.

### `X-Request-Id`

Echoed on every response; generated when absent, reusing the `rand` already in
the crate. Host-supplied values are **sanitized before they are stored**:
capped at 64 characters, restricted to `[A-Za-z0-9._-]`, replaced wholesale if
they fail. An unsanitized header echoed into a JSONL log is a log-forging
vector — `serde_json` escaping stops newline injection, but a caller could
still bloat every line it touches.

---

## 7. Layer 4 — counters, and the frame tap

### Counters derive from events

`Metrics` is an `Observer`. It does not have its own instrumentation points; it
folds the same `ServeEvent` stream the log sees:

```rust
impl Observer for Metrics {
    fn emit(&self, event: &ServeEvent) {
        match event {
            ServeEvent::TurnCreated { live_turns, .. } => {
                self.turns_created.fetch_add(1, Relaxed);
                self.turns_live.store(*live_turns as u64, Relaxed);
            }
            ServeEvent::ReverseTimedOut { .. } => { self.reverse_timed_out.fetch_add(1, Relaxed); }
            // ...
        }
    }
}
```

This is not a stylistic choice. It means **a counter cannot disagree with the
log**, because there is exactly one emission and the counter is downstream of
it. The usual failure of hand-rolled metrics — a code path that increments but
does not log, or logs but does not increment — is unrepresentable.

Exposed at `GET /v1/metrics`, behind the same bearer token as everything else
(#930 is explicit: authenticated, not open). JSON, matching the rest of the API.
Prometheus text exposition under `Accept: text/plain` is a natural follow-on and
about thirty dependency-free lines, but it is not in the first slice.

### The frame tap — what `stella-core` gives us for free

`ServerFrame::Event { event: AgentEvent }` already flows through this crate on
every turn. The engine's whole typed stream — `Stage`, `Retry`,
`ToolStart`/`ToolResult`, `LoopDetected`, `SpeculationDiscarded` — passes
through `stella-serve` and is forwarded to the host without the server ever
looking at it.

So #930's suggestion that "`stella-core`'s step loop and `stella-model`'s retry
paths are both places where a span would pay for itself" is already satisfied,
better than a span would: those events exist, are typed, and are wire-stable.
`stella-serve` simply is not listening.

Two details make the tap correct rather than merely appealing:

**Tap where frames are produced, not where they are streamed.** The obvious
place is the SSE loop in `handle_events`. That is wrong: a turn nobody streams
buffers its frames and the tap never runs — and unstreamed turns are precisely
the abandoned ones worth observing. The tap belongs in
`session.rs::run_session`, on the producing side, with the `Observer` reaching
it via a new `SessionSpec` field.

**Fold, do not log.** `AgentEvent::TextDelta` fires per token. A log line per
frame would make the server slower than the model it is waiting on. So the tap
increments a per-turn `TurnTally` — plain atomics, no I/O — and emits exactly
one `ServeEvent::TurnSettled` carrying the aggregate:

```rust
pub struct TurnTally {
    steps: u32, model_calls: u32, retries: u32,
    tool_calls: u32, tools_failed: u32,
    speculation_discarded: u32, loop_detections: u32,
    duration_ms: u64,
}
```

**[amended — not built, and deliberately so.]** This section originally also
planned individual `AgentEvent` records behind `STELLA_SERVE_LOG=debug`.
Building it showed that to be a mistake: `AgentEvent::Text`, `TextDelta` and
`Reasoning` carry model output verbatim, so those records would put
prompt-adjacent content in a log file — and would make §8's no-content property
*conditional on a verbosity flag*, which is the "safe unless you turn the knob"
shape of invariant that fails in production. The tally supersedes it: aggregates
answer the diagnostic questions (is this turn progressing? how many retries?)
with no payload reaching a record, so the property holds unconditionally.

That tally is also what distinguishes *wedged* from *slow* — the fourth
situation #930 lists — because a turn whose `steps` has not advanced while its
`waited_ms` climbs is a wedge, and a turn advancing steadily is just long.

---

## 8. Redaction — content stays out, and it is proven

Invariant 3's enforcement machinery (`crates/stella-store/src/content_free.rs`) guards
*egress*. Records here never egress, so that harness does not automatically
apply. But "it stays on the box" is not a reason to put prompts in a log file:
operators ship logs, and a log is the easiest accidental egress there is.

**Never recorded, at any level:** prompt text, tool arguments, tool results,
model output, reasoning, filesystem paths, the bearer token, raw request paths,
full turn ids.

Two of those need their reasoning stated.

**Route templates, not raw paths.** A record carries
`route: "/v1/turns/{id}/tool-result"`. The raw path is never logged, because it
contains the turn id.

**Turn ids are truncated.** `TurnRef` holds the first 8 of the id's 32 hex
characters. A turn id is a *second factor*: acting on a turn requires the bearer
token **and** the id, and `new_turn_id` spends 128 random bits precisely so the
id cannot be guessed. Writing it verbatim into a file that gets shipped to a log
aggregator spends that second factor for nothing. Thirty-two bits is ample to
correlate records within one process's lifetime and useless for forging a
request.

**How it is proven.** Reuse the project's own idiom rather than inventing one:
the sentinel harness in `content_free.rs`. Run a turn whose prompt, tool
arguments, tool result and working path are all poisoned with
`CONTENT_SENTINEL`-style markers, capture every `ServeEvent` through
`Capture`, serialize them, and substring-search for each sentinel. A hit is a
privacy incident, not a test failure.

And, per that module's own lesson (`content_free.rs:125` — "without it, an
encoder could pass every sentinel check vacuously"), the harness carries a
**vacuity guard**: assert the capture is non-empty and that at least one record
of each expected variant was produced. A test that observes nothing passes every
redaction check for the wrong reason.

**[amended] A sweep can also be non-vacuous and still blind.** The first version
of this harness had a working vacuity guard and still failed to catch a planted
leak: it asserted `!rendered.contains(&turn_id)` where `turn_id` is the
`turn-`-prefixed form, while a record holds the *bare hex* — so a `TurnRef` that
had been broken to emit all 32 characters sailed through. The assertion now
compares the hex suffix, and separately asserts the first 8 characters *are*
present, so "leaked the whole id" and "recorded no handle at all" are both
failures. The general lesson: a sentinel sweep tests the sentinel you wrote, not
the property you meant, unless you plant the leak and watch it fail.

---

## 9. The dependency decision: not `tracing`, not yet

**Recommendation: do not add `tracing` in this change.** The reasoning, since
AGENTS.md requires a new dependency be argued rather than assumed:

1. **The audit's own benchmark scores 100 without one.** `stella-core` is the
   crate this dimension holds up as correct, and it has no logger. What it has
   is a typed event at every boundary. Copying the pattern is copying the thing
   that scored.
2. **It would create a second, weaker channel in the one crate where it is
   contested.** `stella-core` already has `AgentEvent` — typed, wire-stable,
   round-tripped, consumed by the TUI, the observatory and the journal. Adding
   `tracing` there yields a *parallel* stream that is less structured, is not
   serializable, and that no existing consumer reads. That is worse than the
   status quo, not better, and it spends invariant 2's clean edge to get there.
3. **The port makes it reversible for one file.** Deferring costs nothing later,
   because `TracingObserver` is additive by construction (§5).
4. **The scale does not warrant it.** Eighteen event variants and one binary.
   `tracing`'s value is in filtering and span propagation across a large surface,
   and this surface is small and flat.

**What is genuinely given up, stated plainly:** `RUST_LOG`-style per-module
filtering; spans and async context propagation; and any off-the-shelf OTel
export. `STELLA_SERVE_LOG` is a level, not a filter language, and correlation is
by explicit `request_id`/`turn` fields rather than by ambient context.

**The trigger to revisit — a deferral with an expiry, not an omission.** Adopt
`tracing` + `tracing-opentelemetry` when either becomes true:

- a **second** Stella binary needs correlated cross-process traces; or
- the host (Oxagen) wants engine spans stitched into its own OTel traces across
  the host↔engine boundary — at which point the value is in the *propagation*,
  which is exactly what a hand-rolled sink cannot provide and what a facade
  exists for.

Until then the `Observer` port is the abstraction, and it is the seam the
adoption would land on.

---

## 10. Proving it

#930's acceptance criterion is behavioural, so the tests are too. Each asserts
on captured typed events via `Capture` — never on scraped stderr.

| # | Scenario | Assertion |
|---|---|---|
| 1 | Host is answered nothing; the reverse deadline expires (paused clock — the crate's dev-deps already enable `tokio/test-util`) | a `ReverseTimedOut` naming the turn, kind and `waited_ms`, and a `TurnSettled` whose `steps` never advanced |
| 2 | Host POSTs a fabricated `request_id`, then answers a `provider_request` with a tool result | two `ReverseMisrouted`, with `fault` distinguishing `UnknownRequest` from `KindMismatch` |
| 3 | N+1 wrong bearer tokens past the burst allowance | N+1 `Unauthorized`, with `held_ms` non-zero after the bucket empties — and identical response bodies throughout, so the throttle stays invisible to the guesser |
| 4 | A turn created and never streamed, evicted by the cap | `TurnReclaimed` carrying `buffered_frames` |

Plus the two properties, which are what stop this from decaying:

- **Exactly-one.** For any byte sequence written to a connection — malformed,
  truncated, oversized, unauthorized, well-formed, hung-up — `Capture` holds
  exactly one terminal record for that connection. `proptest` is already in
  `[workspace.dependencies]`, so this costs a `proptest.workspace = true` in
  `[dev-dependencies]` and no new crate in the graph. This is the property that
  would have caught the original distributed-emit design, because the bug is
  always a path nobody enumerated.
- **No-content.** §8's sentinel sweep, with its vacuity guard.

Verify both are non-vacuous by disabling the fold and watching them fail — the
fourth rule of the pattern this design follows.

---

## 11. Slicing

Four PRs. Each is independently useful and independently revertible; the first
alone closes most of #930.

| PR | Contents | New files | Touches |
|---|---|---|---|
| **1** | `ServeEvent`, `Observer`, `JsonlSink`, `Capture`, the `Responder` fold, `X-Request-Id`, `STELLA_SERVE_LOG`. Scenarios 2–3 + exactly-one property | `observe/{mod,event,sink,record}.rs` | `server.rs` (fold + `Responder` threading), `http.rs` (signatures) |
| **2** | Reverse-RPC events and the registry events — `ReverseDispatched`/`Answered`/`TimedOut`/`Misrouted`/`Abandoned`, `TurnCreated`/`Refused`/`Reclaimed`. Scenarios 1 + 4 | — | `pending.rs`, `remote.rs`, `server.rs` |
| **3** | `Metrics` + `GET /v1/metrics`. Route table in `serve-surface.md` updated | `observe/metrics.rs` | `server.rs`, `docs/spec/serve-surface.md` |
| **4** | The frame tap, `TurnTally`, `TurnSettled`. The sentinel harness | `observe/tally.rs` | `session.rs` |

**[amended] What actually shipped.** All four slices landed as one change, and
the file-size prediction below was wrong in a useful direction.

**File-size note (constraint 5).** `server.rs` had ~240 lines of headroom and
PRs 1–3 all touch it. The prediction was that extracting `route()` would be
roughly size-neutral and the ratchet might still trip.

What happened instead: the fold is small, but *threading a `Responder` through
every handler* is not, so the handlers moved to a new `routes.rs` — the
endpoints and their wire types on one side, the transport (listener, fold, turn
registry, throttle) on the other. That cut follows the concern rather than the
line count, and it took `server.rs` from 1,257 lines to **1,072** while adding
the fold, the route classifier and the metrics route. Both files now sit
comfortably under the ratchet with room for the next change. The guidance holds
and is worth restating: when the ratchet threatens, split along a seam that
already exists — never `make file-size-update`.

---

## 12. What this deliberately does not do

- **No engine changes.** `stella-core` gains nothing and loses nothing;
  invariant 2 stays clean and the `tracing`-in-the-engine question stays
  unasked.
- **No egress.** No sink dials out. `/v1/metrics` is pull, authenticated,
  same-socket, same token.
- **No sampling, no log rotation.** stderr is the interface; the supervisor
  (systemd, Docker, Oxagen's runner) owns rotation. Reimplementing that inside
  the process would be the first thing a real logging framework does better,
  and is a reason to adopt one rather than to hand-roll further.
- **No workspace rollout.** Other crates are untouched. If the pattern proves
  itself here, `Observer` is portable — but that is a later decision with its
  own evidence, not a presumption baked into this one.
