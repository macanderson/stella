# Stella serve surface — the headless engine for Oxagen

**Status:** Partly implemented as the `stella-serve` crate (`Session`,
`ServerFrame`, the HTTP/SSE transport, bearer auth, remoted tools) — ADR-033
Option B, the Rust sidecar. It builds its own binary and nothing in
`stella-cli` links it, so a change here never reaches a `stella` user.
**This document describes the target surface, not all of it is built:** the
code today has no approval gate. Turn cancellation, a reverse-request
deadline, — as of #1130 — the **DNS-rebinding `Host` guard**, — as of #1131 —
the **SIGTERM drain and `GET /readyz`**, — as of #971 phase 2 —
**resumable SSE streams** (`seq`, retained history, `?after=` /
`Last-Event-ID`), — as of #932 — **mid-turn steering and pause/resume, and an
observable hold** (`turn_held` / `turn_released` frames, `held` on the session
view), and — as of #931 — **server-owned sessions** (`/v1/sessions`, retained
history, per-session budget) *have* shipped. Sections below flag the gaps
individually; treat `stella-serve/src/` as the state and this doc as the
destination.

**The two crates this document assumed did not exist now do (#971):**
`stella-runtime` (phase 0 — the construction sequence, extracted from
`stella-cli` so a server can assemble the same stack without linking a binary)
and `stella-engine` (phase 1 — `run_step`, a serde `Checkpoint`, and a
step-boundary `CancelToken`, with `run_turn` re-implemented as a loop over
`run_step` so there is one code path). As of #1129 `stella-serve` **does**
drive `run_step`: `session.rs` owns a `TurnState` carrying a `CancelToken`
and loops over steps, so `POST /v1/turns/{id}/cancel` unwinds at the next
step boundary — interrupting CPU-bound work *between* reverse requests
(compaction, loop detection), which the previous cancel could not reach. The
difference is visible in the abort reason: it now names a step-boundary
cancel rather than a failed model call, because the turn ends by being asked
to rather than by having its transport pulled. `StepOutcome::Continue` is
also the seam where a durable runner persists `state.to_checkpoint()` — and
as of #1198 this server does, when the embedder gives it somewhere to put
one. See "Durability" below for what that means and, just as importantly,
what it deliberately does not.

**Date:** 2026-07-20, revised 2026-07-30. **Owner:** Mac Anderson.
**Companion:** `oxagen-platform/docs/specs/agent-engine-v2/` (ADR-033 + spec) —
the host side. ADR-033 lives in that repository, not in this one: Stella's own
`docs/adr/` is a separate 0001-0009 series scoped to Phase 0 adaptive-context,
so every bare "ADR-033" reference here and in `stella-serve/src/` means the
Oxagen ADR. That repository is private to Oxagen; this document is the
*Stella* side of the same integration and is self-contained without it.

---

## As built today — the only routes that exist

**Read this table before writing a client.** Everything after it describes the
*destination*; this is the surface a request actually reaches, verified end to
end against a running binary by Oxagen's sidecar smoke test (oxagen #1132) —
the `/v1/sessions` and steer/pause rows landed after that smoke test and are
verified by `stella-serve/tests/sessions.rs` and `tests/control.rs`. Any path
not in this table is a 404.

| Method | Path | Notes |
|---|---|---|
| `GET` | `/healthz` | Unauthenticated. Liveness — is the process alive. `{"status":"ok"}` |
| `GET` | `/readyz` | Unauthenticated. Readiness — is it safe to send new work. `200 {"state":"ready"}`, or `503` with `"starting"` / `"draining"` (#1131) |
| `GET` | `/v1/metrics` | Counters as a flat JSON object of integers. Authenticated like every other route, and **pull-only** — see [serve-observability.md](./serve-observability.md) |
| `POST` | `/v1/turns` | Body is `TurnRequest`; returns `{"turn_id":"turn-<32 hex>"}`. Optional `engine` object (#1167) carries per-turn caller-policy knobs (`max_output_tokens`, `temperature`, `effort`, `reasoning`, `params`, `compaction_budget_tokens`, `summarize_overflow`, `summarize_keep_recent`) lowered onto the defaults — omitted/empty is a no-op; unusable values are 400s naming the knob; values past an operator ceiling are clamped and reported in the response's `clamped` array (`{knob, requested, effective}`, absent when nothing was lowered). `retry_policy`/`loop_detection` are operator policy and not settable |
| `GET` | `/v1/turns/{id}/events` | SSE `id: <seq>` + `data: <ServerFrame>`. Resumable via `?after=<seq>` or `Last-Event-ID`. One concurrent subscriber; a second gets 409 |
| `POST` | `/v1/turns/{id}/provider-result` | Answers a `provider_request` |
| `POST` | `/v1/turns/{id}/provider-delta` | Optional (#1165): a batch of streamed fragments (`{"request_id", "deltas": [{"kind":"text"\|"reasoning","text":"…"}]}`) for an in-flight `provider_request`, ahead of its `provider-result`. Fragments surface on `/events` as `text_delta` / `reasoning` frames and each batch resets the reverse-request deadline. Empty batches are refused (400); fragments after the result get a 409 |
| `POST` | `/v1/turns/{id}/tool-result` | Answers a `tool_request` |
| `POST` | `/v1/turns/{id}/cancel` | Step-boundary stop (#1129): the turn unwinds at its next step, keeping completed steps, and still emits `turn_complete`. Deregistered immediately, so a second cancel is a 404. There is no `DELETE` |
| `POST` | `/v1/turns/{id}/steer` | `{"message": "…"}` — injected at the next step boundary, echoed as a `steered` event (#932) |
| `POST` | `/v1/turns/{id}/pause` | Hold at the next step boundary. Idempotent; never mid-tool (#932). Body optional: `{"reason": "…"}` rides onto the `turn_held` frame. An empty body holds without a reason; a non-empty body that is not JSON is a 400 |
| `POST` | `/v1/turns/{id}/resume` | Release a held turn. Idempotent (#932) |
| `POST` | `/v1/sessions` | `{"system_prompt": "…", "budget": …}` → `{"session_id":"session-<32 hex>"}` (#931) |
| `GET` | `/v1/sessions/{id}` | History, cost to date, live turn id, and `held` — whether that live turn has been asked to hold (#932). Always answers from the last settled state |
| `POST` | `/v1/sessions/{id}/turns` | `TurnRequest` minus `messages`/`budget`, plus `input` (this turn's new messages). Accepts the same optional `engine` object as `/v1/turns` (safe for the prompt-cache contract: no engine knob touches the transcript). Returns `{"turn_id", "session_id"}` plus the same `clamped` reporting; the turn is then an ordinary `/v1/turns/{id}` member. One live turn per session (else 409) |
| `DELETE` | `/v1/sessions/{id}` | Ends the session and cancels its live turn. Also drops its resume point |
| `GET` | `/v1/sessions/{id}/checkpoint` | The session's resume point, verbatim (a `stella_core::step::Checkpoint`). Read from the **store**, not the session registry — so a session this process never created still answers `200` if its bytes are there. `404` when there is none, when the id is not a legal key, or when no store is configured (#1198) |
| `DELETE` | `/v1/sessions/{id}/checkpoint` | Reclaim a resume point the host has finished recovering. Idempotent |
| `GET` | `/v1/turns/{id}/checkpoint` | As above, for a stateless `POST /v1/turns` turn, which keys on its own turn id |
| `DELETE` | `/v1/turns/{id}/checkpoint` | As above |

Session semantics a client must know: the transcript lives on the server and
the host sends only `input` per turn; the system prompt is minted once at
session create and held byte-identical across turns (the prompt-cache
contract); an **aborted turn does not write back** — the session history stays
as it was, while the aborted turn's spend still joins `cost_usd`; sessions are
capped (64) and one idle past `ServeConfig::session_idle_ttl` (default 1 h)
may be reclaimed under pressure, so a long-lived host should expect `404` on a
session it abandoned and `429` when it hoards.

Corrections to prose further down that reads as present tense but describes the
destination, and which a client written from it gets wrong:

- **The turn resource remains first-class.** A stateless `POST /v1/turns`
  drives exactly one turn with host-supplied messages and retains nothing.
  Sessions are additive (#931), not a replacement — and note the shape is
  `POST /v1/sessions/{id}/turns` (plural), not the singular `/turn` some
  diagrams below sketch. `/readyz` **now exists** (#1131) and, like `/healthz`,
  is unauthenticated — a kubelet has no bearer token. `/v1/metrics` **does**
  exist and is authenticated.
- **Reverse RPC is keyed by `request_id` on its own frame types**, not by a
  `call_id` on a `tool_start` / `scope_review` / `ask_user` `AgentEvent`. The
  engine emits dedicated `tool_request` / `provider_request` `ServerFrame`
  variants whose ids are per-turn counters (`prov-0`, `tool-0`, …). This is the
  single most dangerous drift in this document for anyone mirroring it in
  another language: it names the wrong field on the wrong frame.

**Resolved as of #971 phase 2** — the previous edition of this table said no
frame carried a `seq`, no history was retained, and `?after=` was split off and
discarded. All three are now built:

- Every frame carries a monotonic `seq` (`#[serde(flatten)]`, so it is an extra
  key on the frame object a client already parses, not a new envelope), emitted
  additionally as the SSE `id:` line.
- Frames are retained per turn in a bounded ring (4096). A resume point that has
  aged out is answered with an explicit `replay_truncated` frame — never a
  silent jump to the oldest retained frame.
- A reconnect names its resume point with `?after=<seq>` or, for a browser
  `EventSource`, the `Last-Event-ID` header the platform sends automatically.
  `?after=` wins if both are present.
- A disconnect **parks** the turn for `ServeConfig::resume_grace` (default 30s,
  clamped to 5 min, `Duration::ZERO` to restore cancel-on-disconnect) rather
  than cancelling it, so there is a live turn to resume into.

One contract point a client author must not miss: **an outstanding reverse
request is not re-announced on resume.** Asking for `after=N` asserts you
received everything through `N`, obligations included. A client that persisted
its `seq` but not its in-flight `request_id`s must resume from `after=0` and
replay the retained stream to rediscover what it owes.

Also: the tool surface is selected by the `STELLA_SERVE_TOOLS=remote`
environment variable, not a `--tools remote` flag — the binary parses no flags
beyond `healthcheck`, `--version` and `--help`.

## Durability — what a served turn survives (#1198)

`drive_turn` has always called `Engine::persist_checkpoint` at the
`StepOutcome::Continue` seam and `discard_checkpoint` on every terminal path,
at the same boundaries the CLI driver uses. What was missing was everything on
the other side: nothing set `EngineConfig::checkpoint_sink`, and `SessionSpec`
carried no durable identity to key storage on. A served turn therefore died
with the process while a local turn did not.

The seam is now complete, and the shape it takes is decided by
[ADR 0013](../adr/0013-session-artifact-boundary.md):

- **The server owns a port, not a location.** `CheckpointStore` is a three-verb
  trait (`put` / `get` / `remove`); `ServeConfig::checkpoints` is `None` by
  default and this crate never names a path. That is ADR 0013's line held
  exactly: the workspace stays the host's, so serve does not get a filesystem —
  what it gets is the ability to hand an embedder a defined artifact to persist.
- **`FileCheckpointStore` and `MemoryCheckpointStore` are library types**, not
  a server default. The `stella-serve` binary exposes no `--checkpoint-dir`
  flag, because choosing the location is the act that ADR 0013 lists under what
  it does not commit us to. Adding the flag is one line if that ADR is ratified
  the other way.
- **Which id keys the store is a routing decision.** A turn inside a session
  keys on the **session id** — the name the host still has after a crash, and
  one with a single writer, since a session admits one live turn. A stateless
  turn keys on its own turn id.
- **Read-back consults the store, not the registry.** `200` on
  `GET …/checkpoint` means "this id has a resume point", *not* "this session
  exists" — deliberately different questions, because the interesting caller is
  one whose session object died with the process.
- **The body is the stored bytes, verbatim.** `Checkpoint`'s field order is its
  wire order and this workspace's `serde_json` has no `preserve_order`, so
  parsing and re-encoding would return it alphabetized. Passing bytes through
  is both cheaper and the only version that cannot corrupt a resume point.

### What this does not do

- **It does not resume a turn.** A resumed turn's first act is a reverse
  request that only a host can answer, so a server that re-drove one on startup
  would park a thread on a request nobody is listening for. The recoverable
  unit is the checkpoint; the host decides whether to continue.
- **It does not survive a graceful drain's tail.** Every terminal path
  discards, cancellation included, because `drive_turn` and the CLI driver are
  deliberately the same in this respect — "an HTTP turn and a local turn are
  equally recoverable rather than merely similar" is a promise `session.rs`
  makes in prose. So what survives is the turn that died *without* unwinding: a
  `SIGKILL`, a panic, a lost machine. A turn the shutdown drain has to cancel
  discards on its way out. Retaining a cancelled turn's resume point is
  arguable and is a change to what a checkpoint *means*, so it belongs in a
  decision of its own rather than in this one, on one surface only.
- **It does not garbage-collect.** The steady-state contents of a store are
  exactly the turns that died without ending — bounded by crash frequency, not
  by traffic, but not self-emptying, because nothing can tell an abandoned
  resume point from one whose host is about to come back for it. That judgment
  is the host's, which is why `DELETE …/checkpoint` exists.

A failing store is reported once per turn as `ServeEvent::CheckpointFailed`
and counted by `checkpoints_failed_total`, never propagated into the turn: a
checkpoint that cannot be written leaves the turn exactly as recoverable as it
was before the store existed, so failing live work to report a durability
*improvement* would be strictly worse than not offering one.

## One sentence

Expose the Stella engine as a long-lived, multi-session **service** — a
step-scoped `stella-engine` facade wrapped in a `stella-serve` HTTP/SSE sidecar —
so Oxagen's web app drives the Rust core over a wire protocol whose payload is
already Stella's serialized `AgentEvent` stream, while every side effect the
engine requests round-trips back to Oxagen's kernel over the same connection.

This is **Option B** of ADR-033 (the Rust sidecar), which that ADR keeps as the
documented fallback with "identical port surface, transport swappable." The user
has now elected the sidecar model — "Oxagen's web app uses the rust app under the
hood, with infra provisioned to support it." Option A (the napi embed) and Option
B share the *same* upstream Stella work; this doc scopes that shared work plus the
serve/transport layer B additionally needs.

## Why the engine is already 90% ready (evidence from the line-by-line sweep)

A full read of `stella-protocol`, `stella-core`, `stella-cli`, `stella-model`,
`stella-store`, `stella-context`, `stella-graph`, `stella-tools`, `stella-mcp`,
`stella-pipeline`, `stella-fleet`, and `stella-observatory` (2026-07-20) confirms
the engine is structured as a headless library, not a terminal program:

1. **The core is I/O-free by construction.** `stella-core` depends on tokio with
   `features = ["sync"]` only — no `rt`/`io`/`net`/`fs`/`process`/`time`. No
   `println!`, no `std::env`, no `current_dir`, no `process::exit`, no globals,
   no `unsafe`. `Engine::run_turn(&mut messages, &mut budget, &events)`
   (`stella-core/src/driver.rs:329`) holds no conversation state — the caller
   owns history, budget, and calibration. The engine is driven entirely through
   **10 injected trait ports** (`Provider`, `ToolExecutor`, `Clock`, `TurnGate`,
   `TurnSteering`, `Sleeper`, `HookRunner`, `RuleSource`, `SkillSource`,
   `ToolCallObserver`) with zero process-global state.

2. **`AgentEvent` is already the wire format.** `stella-protocol/src/event.rs`
   defines a ~30-variant, `#[serde(tag = "type", rename_all = "snake_case")]`
   enum. The `--output-format stream-json` mode is literally
   `serde_json::to_string(&event)` per line, additive-only, with round-trip
   tests. The TUI consumes *only* `AgentEvent` (`stella-tui` depends on nothing
   but `stella-protocol`). **A web client is a drop-in peer of the TUI**: attach
   an SSE pump to the same `UnboundedReceiver<AgentEvent>`.

3. **The pipeline is headless-first.** `stella-pipeline` has no TTY coupling in
   its core; `PipelineConfig.headless` and `headless_bypass_scope_review` are
   first-class, and a headless scope-review over threshold returns the named
   error `ScopeReviewRequiredHeadless` — **never a silent auto-approve**.
   `AutoApproveGate` / `AlwaysAbortGate` are the headless approval ports.

   **Correction (#932 approval half).** This paragraph is the premise behind
   "surface `ScopeReviewRequiredHeadless` to the host as a decision", and the
   premise does not hold for `stella-serve` as built: **a served turn never
   reaches a scope review at all.** `ApprovalGate::review` has exactly one
   call site — `stella-pipeline/src/pipeline/scope_stage.rs` — and
   `stella-serve` does not depend on `stella-pipeline`. It drives the
   *engine* (`stella_engine::run_step`), which has no plan or scope stage.
   Only `stella-cli` and `stella-runtime` link the pipeline.

   So `POST /v1/turns/{id}/approve` is not merely unbuilt, it is currently
   **unreachable**: a route no turn could trigger, and a row in the table
   above that a client could code against and never exercise. Building it
   is blocked on a prior decision — whether `stella-serve` grows a
   pipeline-driving surface (`POST /v1/runs`, distinct from `/v1/turns`)
   or whether the approval boundary moves down into the engine. That is a
   design question, not an increment, which is why the steering and
   pause/resume halves of #932 shipped on their own (#1056) and this one
   did not. It stays out of the table until that decision is made.

   **What *did* ship in its place — the hold, made observable.** The half of
   the approval story that does not depend on that decision is the part the
   issue's own sequencing note is about: "a client which drops during an
   approval wait can come back and find the turn still holding". That works
   now, for the pause gate rather than for a scope review. `wait_if_paused`
   emits `turn_held` when it actually parks (later than the `POST /pause`
   response, which only promises to hold at the *next* boundary) and
   `turn_released` when it lets go — including on the release paths that
   never touch `POST /resume`, namely a cancel and a torn-down entry. Both are
   ordinary numbered frames, so they are retained and replayed like any other:
   a subscriber that reconnects with `?after=` re-learns the hold.

   Note the asymmetry against the reverse-request rule, because a client that
   assumes the rules are the same parks forever: an outstanding
   `tool_request` / `provider_request` is **not** re-announced on resume, but
   a hold **is**. An obligation is something `?after=N` asserts you already
   received; a hold is a state the turn is still in. A `turn_held` with no
   `turn_released` after it in the replayed tail means the turn is waiting on
   you.

   `GET /v1/sessions/{id}` answers the same question without a stream, for a
   host that restarted or that manages many sessions and subscribes to none:
   `held` is `true` while the live turn is asked to hold.

4. **Multi-workspace in one process already works.** `stella-fleet` runs N
   workers concurrently in one process, each with `cfg.workspace_root` overridden
   per task; nothing below `Config::load` reads `current_dir()`. The three
   SQLite stores (`store.db`, `context.db`, `codegraph.db`) are all path-injected
   (`Store::open(root)`, `ContextStore::open(path)`, `CodeGraph::open(root,
   db_path)`) with WAL + `busy_timeout=5000`.

5. **The Observatory is the in-repo HTTP precedent.** `stella-observatory`
   serves a loopback-only, read-only dashboard over a hand-rolled HTTP/1.1
   responder on `tokio::net::TcpListener` (no axum/hyper). `respond(root, path)`
   is a pure function unit-tested with no sockets. `stella-serve` mirrors these
   idioms and adds the write path.

### The three real gaps (what "prepare it to be the engine" actually means)

| Gap | Evidence | Fixed by |
|---|---|---|
| ~~**Bin-only crate.**~~ **CLOSED (#971 phase 0).** `stella-cli` is still bin-only, but the wiring it duplicated across seven call sites now lives in **`stella-runtime`**, which any surface can link. Its invariant — no `std::env`, no `current_dir()`, every ambient switch an explicit `RuntimeSpec` field — is what makes N sessions with N roots and N trust postures sound in one process, and is enforced executably by `stella-runtime/tests/no_ambient_reads.rs`. | was: `agent.rs`, `agent/goal.rs`, `command_deck.rs`, `fleet_cmd.rs`, `subsession.rs` each re-assembling the stack. | Done. |
| ~~**Whole-loop API only.**~~ **CLOSED (#971 phase 1).** **`stella-engine`** exposes `run_step(&mut TurnState) -> StepOutcome`, a versioned serde `Checkpoint`, and a `CancelToken` read at the same safe boundary as the pause gate and budget enforcer. `run_turn` is now a loop over `run_step`, so there is one code path — `driver.rs` shrank rather than grew. **Not yet consumed by `stella-serve`.** | was: `run_turn` owning the whole `for step in 0..max_steps` loop. | Done; wiring into serve is the next increment. |
| **No transport / no server.** The event channel and control ports exist but are only wired to stdin/TUI. No process hosts them over a socket; no graceful shutdown. | Observatory is read-only; `run_turn` future is `!Send`. | **`stella-serve`** — HTTP/SSE + reverse-tool-RPC sidecar, thread-per-session. |

## The `!Send` constraint drives the server shape

Three independent sweeps flagged it: the engine turn future is **deliberately
`!Send`** (it holds provider futures and the retry-jitter RNG across awaits) —
documented at `stella-cli/src/fleet_cmd.rs:375-380`. **A server cannot
`tokio::spawn(engine.run_turn())`** on a multi-thread runtime. The fleet already
solved this: each worker gets a **dedicated OS thread running a current-thread
tokio runtime**, bridged to the async side by a `Send` oneshot
(`fleet_cmd.rs:388-405`). `stella-serve` adopts the same pattern: **one OS thread
+ current-thread runtime per session**, the accept loop and SSE pumps on the main
multi-thread runtime, sessions addressed by id.

## Architecture

**The route table in this diagram is the target, not the code.** See
[As built today](#as-built-today--the-only-routes-that-exist).

```
┌────────────────────────── stella-serve (new crate, the sidecar) ──────────────────────────┐
│  tokio multi-thread runtime: TcpListener accept loop (Observatory idiom + write path)      │
│  bearer-token auth · bind 0.0.0.0:PORT (containerized) or 127.0.0.1 (local)                 │
│                                                                                            │
│  POST /v1/sessions            → create Session { id, workspace_root, provider cfg, ... }    │
│  POST /v1/sessions/:id/turn   → drive one turn/pipeline; returns run id                     │
│  GET  /v1/sessions/:id/events → SSE stream of AgentEvent (no ?after=<seq> replay yet)       │
│  POST /v1/sessions/:id/steer  → TurnSteering::drain_steering  (mid-turn message)            │
│  POST /v1/sessions/:id/pause  → TurnGate::wait_if_paused                                     │
│  POST /v1/sessions/:id/cancel → soft-stop (keep work) | hard-cancel (drop future)           │
│  POST /v1/sessions/:id/tool-result → reverse-RPC: host returns a ToolResult by call_id      │
│  POST /v1/sessions/:id/approval    → reverse-RPC: host resolves a scope/approval by id      │
│  DELETE /v1/sessions/:id      → tear down thread + runtime + stores                          │
│  GET  /healthz  /readyz  /metrics                                                           │
│                                                                                            │
│  per session:  std::thread + current-thread runtime  ── drives ──►  stella-engine.run_step  │
└──────────────────────────────────────────────┬─────────────────────────────────────────────┘
                                               │  10 trait ports, but remoted:
                     ┌─────────────────────────┼──────────────────────────────┐
                     ▼                         ▼                              ▼
             RemoteProvider            RemoteToolExecutor            RemoteApprovalGate
        (Provider port → the host    (ToolExecutor port → each      (ApprovalGate →
         streams model deltas back    tool call becomes a           a scope-review
         over the events channel;     `tool_call` AgentEvent; the   AgentEvent; the host
         the host owns @oxagen/ai)    host runs kernel.invoke()      resolves via approval
                                      and POSTs the ToolResult)      rows + POSTs back)
```

The engine keeps its full local port set when run as the CLI. In the serve
sidecar, the ports that must stay governed by Oxagen (model calls, tool
execution, approvals, recall, command runs) become **remote ports**: the engine
emits a request as an `AgentEvent`, blocks that step's tool/model future on a
oneshot, and the host fulfills it over a reverse endpoint keyed by `call_id`.
This is exactly the "reverse tool-call protocol" ADR-033 Option B names, and it
is why the sovereignty rule holds: **the engine never gains ambient authority —
every effect re-enters `kernel.invoke()` on the host.**

### Cancellation and deadlines

A turn that stops making progress is bounded from two sides. Unlike most of this
document, **both of these ship today** (#641).

**Reverse-request deadline.** Every reverse request — a model call or a tool
call — carries a deadline, defaulting to **five minutes**
(`SessionSpec::reverse_request_timeout`; on the wire, an optional
`reverse_request_timeout_ms` on `POST /v1/turns`, refused at `0` and clamped to
one hour so a caller cannot restore the unbounded wait). The default is sized to
clear the slowest legitimate reverse request — an extended-thinking completion,
or a tool shelling out to a test suite — while turning "wedged forever" into
"fails in minutes". An expired **provider** request fails the turn with a
*terminal* error, deliberately not a retryable transport one: retrying would
hand an unresponsive host the same full wait once per attempt, multiplying the
window the deadline exists to close. An expired **tool** request becomes a
`ToolOutput::Error` the model can react to, because the `ToolExecutor` port's
contract is that `execute` never returns `Err`.

**`POST /v1/turns/{id}/cancel`** ends an in-flight turn. It takes the
action-suffixed shape of its sibling routes rather than a bare `POST
/v1/turns/{id}`, because every verb in this API is already the last path
segment; a bare `POST` to a collection member would be the one route whose
meaning came from its method. Semantics:

- `200 {"status":"cancelled"}` once the turn is **signalled**, not once it has
  unwound — blocking on the unwind would deadlock a single-connection client.
- Any parked reverse request wakes at once, and no new one may park, so the turn
  unwinds via a non-retryable `Cancelled` error within one engine step.
- The turn is **unwound, not killed**: a host streaming `/events` still receives
  its terminal frame with an `aborted` outcome, so a cancelled turn reports its
  settled cost like any other.
- The id leaves the registry immediately, so a second `cancel` — or a late
  `tool-result` / `provider-result` — is a `404`.
- Cancelling a turn nobody streamed is valid, and is how a caller reclaims its
  OS thread.

Still absent: a step-scoped cancellation token threaded through `run_step`.
Cancellation is enforced at the reverse-RPC boundary, which is where every long
wait actually happens; it does not interrupt CPU-bound work *between* reverse
requests.

**Accept-loop lifetime.** `serve` runs until a **fatal** accept error, not the
first one (#637). `accept()` failures are classified in
`stella-serve/src/accept.rs` — duplicated byte-identically in
`stella-observatory`, with a test enforcing that: a peer that hung up before we
accepted it is retried at once, resource exhaustion backs off 10ms→1s, and only
a structurally unusable listener — or one that has accepted nothing across 64
consecutive backoffs — ends the loop.

### The step-scoped facade (`stella-engine`)

```rust
// stella-engine/src/lib.rs (facade over stella-core + stella-pipeline)
pub struct TurnState { /* messages, budget, oracle_state, calibration, seq */ }

impl Engine {
    pub fn new_turn(&self, spec: TurnSpec, resume: Option<Checkpoint>) -> TurnState;
    pub async fn run_step(&self, state: &mut TurnState) -> Result<StepOutcome, EngineError>;
    //                                                     ^ one committed step, then return
}

pub enum StepOutcome { Continue, Done { text, cost_usd }, Aborted { reason } }
```

`run_step` is an **extraction** of the body of `driver.rs`'s `for step in
0..max_steps` loop — the phase functions (compaction, loop-detect, budget check,
model call, dispatch) are already separate. After each `run_step` the host
persists `(messages_digest, budget_state, oracle_state, calibration_state)` +
the event seq in one transaction, giving Oxagen's durable runner its
per-step checkpoint and crash-resume. This is ADR-033 §6 item 1 and §4.3.

### Wire protocol

- **Events (engine → host):** `AgentEvent` JSON in SSE `data:` frames, the
  payload identical to `stream-json`. **Planned, not built:** a monotonic `seq`
  on each frame, and replay from `?after=<seq>` so a reconnect resumes
  losslessly (mirroring the Observatory's read model and Oxagen's `agent_events`
  log discipline). At this baseline **no frame carries a `seq` at all**, no
  `?after=` parameter is parsed, and no event history is retained — so a dropped
  connection loses whatever was streamed while it was down.
- **Reverse RPC (host → engine):** the engine emits a dedicated
  `ServerFrame::ToolRequest` / `ServerFrame::ProviderRequest` carrying a
  `request_id` (a per-turn counter — `tool-0`, `prov-0`, …); the host runs the
  governed work and POSTs the result back to
  `/v1/turns/{id}/{tool-result,provider-result}` with that `request_id`. The
  engine's `RemoteToolExecutor::execute` awaits a per-`request_id` oneshot.
  **Not the same thing as** the `call_id` on a `tool_start` `AgentEvent`: that
  id is the model's, addresses the tool call inside the transcript, and is not
  what a resolve POST is keyed by. `scope_review` / `ask_user` have no reverse
  endpoint at all yet.
- **Provider deltas (built, #1165):** a host that streams its model call POSTs
  fragment batches to `/v1/turns/{id}/provider-delta`, keyed by the same
  `request_id` its eventual `provider-result` answers.
  `RemoteProvider::complete_observed_ref` forwards each fragment to the
  engine's observer, so `AgentEvent::TextDelta` / `Reasoning` fire under serve
  exactly as with a local streaming adapter — through the same forwarder,
  `FrameHistory`, `seq`, and replay. Strictly optional (a non-streaming host
  keeps POSTing the whole result) and strictly advisory (the terminating
  `CompletionResult` is authoritative; a retried call re-streams from the
  start with no reset marker). Retention note: delta frames share the turn's
  bounded ring (`history.rs`, 4096 frames), so a token-streaming turn evicts
  its early frames sooner — the ring was already sized for per-token
  `TextDelta` traffic, and a resume point older than the window is reported
  as `replay_truncated`, never silently skipped.

## Containment posture (why the sidecar runs *inside* Oxagen's sandbox)

The tools sweep found that turning `tools.bash: off` does **not** remove
arbitrary shell execution — `build_project`, `run_tests`, `verify_done`, and
`run_script` all shell out via `bash -c`, and nothing confines any of them
in-process. (The built-in OS sandbox, `STELLA_BASH_SANDBOX`, covered the
`bash` tool alone and was removed in #1300 for exactly the reason this
paragraph gives: it bounded one spawn path out of many. This section's
conclusion is unchanged by that — it never rested on the sandbox.) The web
tools are an unguarded SSRF primitive when enabled. Several credentials/config
knobs are process-global (web auth, provider keys), so **multi-tenant in one
process is a non-starter.**

Therefore the serve model is **one engine process per trust boundary, run inside
Oxagen's existing Firecracker/Modal sandbox** (the same isolation
`agent.code.execute` and durable sandbox sessions already use). The engine's
`CommandRunner`/`ToolExecutor` ports resolve to the host's governed sandbox exec
— the engine does not spawn its own shells server-side. `stella-serve` in server
mode:

- **binds to a token-gated port only** (no ambient trust), behind a
  DNS-rebinding `Host`-header guard (`stella-serve/src/hostguard.rs`, #1130)
  that runs before route dispatch on every route — `/healthz` included, since
  that is the one route the bearer token does not cover. The policy is derived
  from the bind rather than hardcoded, because the Observatory's
  `host_is_local` was written for a loopback-only surface and this one also
  binds `0.0.0.0` in a container:
  - **loopback bind** → only loopback names/addresses are accepted;
  - **non-loopback bind + `STELLA_SERVE_ALLOWED_HOSTS`** → that list, plus
    loopback (a container's own probe) and the bound literal;
  - **non-loopback bind, no allow-list** → the guard is *inert*, since nothing
    can know what the deployment is legitimately called. That arm is named on
    the `listening` record (`host_guard`) rather than left silent, and refusals
    are counted (`host_rejected_total`) and reported (`host_rejected`) so a
    missing allow-list entry is distinguishable from an attack.

  A request with no `Host` at all is permitted, matching the Observatory: a
  browser `fetch` always sends one, so its absence is not the attack.
- **disables the local shell/web tool surface by default** (`--tools remote`),
  delegating all execution to the host's `RemoteToolExecutor`.
- **does not use `stella-store` or `stella-cli` shell hooks server-side** (per
  ADR-033 §4.1) — persistence and policy are the platform's.
- **drains on SIGTERM** rather than hard-killing in-flight turns (#1131).
  `SIGTERM`/`SIGINT` flips `GET /readyz` to `503`, refuses `POST /v1/turns` and
  both session-creating routes with `503` + `Retry-After`, and asks every live
  turn to stop at its next step boundary — so an in-flight model call *lands*
  and its result is kept, which cancelling would have thrown away. Turns that
  do not reach a boundary inside `STELLA_SERVE_SHUTDOWN_GRACE_SECS` (25s by
  default, under Kubernetes' 30s `terminationGracePeriodSeconds`) are then
  cancelled — still a step-boundary unwind that emits `turn_complete`, never a
  dropped future. The outcome is reported as `drain_completed
  {drained, cancelled, timed_out}`.

  The **listener stays open for the whole drain**, which is the
  counter-intuitive part and load-bearing: reverse RPC means a parked turn is
  finished *by the host* over new connections, so closing the port would strand
  every turn the drain is waiting for — and would take `/readyz` with it.
  New work is refused by status code; the port is released when the process
  exits.

## Metering parity

The host owns metering (it implements the `Provider` port over
`@oxagen/ai::streamAgentReply`, which writes `token_usage` rows to ClickHouse).
The engine still emits `StepUsage` / `BudgetTick` / `Complete` events carrying
`{input_tokens, output_tokens, cached_input_tokens, cache_write_tokens,
cost_usd, model, duration_ms}` for cross-check. Because tool/command execution
runs on the host sandbox, the host also emits the sandbox-runtime cost event —
so both the per-step model cost and the compute cost are priced (the gap
ADR-033 §7 names).

## Upstream Stella work items (shared by Option A and B; ADR-033 §6)

1. ✅ **DONE (#971 phase 0).** `stella-runtime` — the CLI's engine wiring as one
   reusable builder, taking every ambient switch as an explicit `RuntimeSpec`
   field.
2. ✅ **DONE (#971 phase 1).** `stella-engine` facade + `run_step` + a versioned
   serializable `Checkpoint`. `run_turn` is a loop over `run_step`.
3. ✅ **DONE.** `stella-serve` sidecar crate — HTTP/SSE + reverse-tool-RPC,
   thread-per-session, bearer auth. Graceful SIGTERM shutdown is still open.
4. ✅ **DONE (#971 phase 3).** `AgentEvent` → JSON Schema + TypeScript, behind
   `stella-protocol`'s optional `schema` feature, into committed artifacts under
   `docs/wire/`. `scripts/check-wire-schema.sh` fails the gate on drift, and
   `validate_stream` runs as a conformance check over recorded fixtures and
   deliberate corruptions.
   **Correction:** `validate_stream` lives in `stella-pipeline/src/replay.rs`,
   not `stella-protocol` as earlier editions of this document said.
5. ✅ **DONE (#1133).** Bus lifecycle events at the turn, step and model-call
   boundaries — closes "the bus is only emitted from the tool registry."
   `Engine::with_bus` attaches a `HookBus`; the engine then emits
   `agent.turn.started`/`.completed`, `agent.step.started`/`.completed`, and
   `model.request.started`/`.completed`/`.failed`. `HookBus::session_started`
   emits `session.started` and is called by whoever *mints* the bus (today
   `stella-cli`'s rule guards), because construction is the only place the
   session boundary actually is — and it fires after subscribing, or the one
   event that opens the stream would be the one nobody sees.
   **Correction:** earlier editions implied the catalog already declared step
   names. It did not — `agent.step.started`/`.completed` were added with the
   emitters, since a catalog name nothing emits is a promise rather than a
   contract. Turn-level `agent.turn.*` did already exist.
   These are **observer-only**: none is on the blocking allowlist, so an
   extension can watch a step begin but not veto one. Payloads carry counts
   and identifiers (message count, tokens, cost, model id, step index) and
   never transcript content — the one free-text field is an abort `reason`,
   which already reaches `AgentEvent::Error`. An engine with no bus attached
   pays nothing: every emit site is behind one `if let Some(bus)`, and the
   payload closure is not called without one.
6. ✅ **DONE (#971 phase 1).** A real `CancelToken` threaded through `run_step`,
   read at the step boundary, closing any open `tool_use` with synthetic error
   `tool_result`s so the transcript stays valid; hard-drop semantics documented
   on the facade.
   **Correction:** `ProviderError::Cancelled` is **not** dead — `stella-serve`'s
   `RemoteProvider` produces it on a live path. That crate landed after the
   sentence claiming otherwise was written. The token deliberately does not
   produce it: the token stops at a step boundary, whereas `Cancelled` belongs
   to a call already in flight.
7. ⬜ `complete_observed` overrides for OpenAI/Gemini/Vertex/Bedrock adapters so
   token streaming is uniform across providers.
8. ⬜ Per-session isolation audit of `stella-tools` process-global state
   (file-touch mutex, `STELLA_*` env reads) — server mode must inject these, not
   read the environment.

## Non-goals

- Not re-implementing isolation inside Stella — reuse Oxagen's sandbox.
- Not persisting turns in `stella-store` server-side — Oxagen owns durability.
- Not exposing the local shell/web tools to tenants — delegate to `RemoteToolExecutor`.
- Not gating the payoff on Rust: the host-side durable runner (ADR-033 Track 1)
  lands independently and this sidecar swaps in at the `executeTurn` seam.

## Build decomposition

See `serve-surface.fleet.toml` (this directory) — a `stella fleet --plan` file
that fans the eight upstream items into gate-verified tasks, each of which passes
`make gate` (no-scratch + doc-citations + fmt `--check` + clippy `-D warnings`
+ `cargo test --workspace`) before its commit lands.
