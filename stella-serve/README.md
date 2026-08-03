# stella-serve

The Stella engine as a headless, host-driven service. The host assembles a turn,
the engine here orchestrates it, and every governed side effect — model
completions and tool calls — is remoted back to the host over a wire protocol.
This is ADR-033 Option B, the Rust sidecar; the design is
[`../docs/design/serve-surface.md`](../docs/design/serve-surface.md).

The hard boundary is **no ambient authority**. The engine running inside this
server never calls a model and never executes a tool itself: `RemoteProvider` and
`RemoteToolExecutor` ([`src/remote.rs`](src/remote.rs)) satisfy the `Provider` and
`ToolExecutor` ports by emitting a request frame and parking on the host's answer,
so every effect re-enters the host's own governance. A local tool surface is not a
configuration option — `STELLA_SERVE_TOOLS` accepts only `remote`, and any other
value is refused at startup rather than silently ignored
([`src/main.rs`](src/main.rs)). Persistence is the host's as well: this crate does
not depend on `stella-store`.

## Where it sits

It depends on exactly two workspace crates — [`stella-protocol`](../stella-protocol)
for the wire types and [`stella-core`](../stella-core) for the engine — and
**nothing in the workspace depends on it**. It is a leaf that builds its own
binary from [`src/main.rs`](src/main.rs); `stella-cli` does not link it, and
`make build-release` builds `-p stella-cli` only, so a change here never reaches a
`stella` user. The corollary: `make smoke` runs the CLI and never exercises this
crate — `make gate`'s `cargo test --workspace` is the only thing that does.

The binary is meant to run containerized —
[`../packaging/docker/Dockerfile.serve`](../packaging/docker/Dockerfile.serve)
builds it with `--bin stella-serve`, runs it under a non-root numeric UID, binds
`0.0.0.0:8080`, and uses the binary's own `stella-serve healthcheck` subcommand as
the container HEALTHCHECK so the runtime image needs no `curl` or `wget`. Those
are claims a Dockerfile makes and cannot check, so
[`../.github/workflows/docker-serve.yml`](../.github/workflows/docker-serve.yml)
builds the image on every change to it and
[`../scripts/smoke-serve-image.sh`](../scripts/smoke-serve-image.sh) runs the
container until each one is answered — serving on 8080, the token gate refusing
and admitting, uid 10001 on PID 1, and Docker's own health verdict (#635).

```
host  ──POST /v1/turns──►  stella-serve  ──►  Session (dedicated OS thread)
  ▲                                                   │
  └──GET .../events: SSE frames (agent events + reverse-RPC requests)──┘
  └──POST .../tool-result, .../provider-result──► Pending ──unparks the engine step
  └──POST .../cancel───────────────────────────► Pending ──unwinds the turn
```

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The crate `//!` overview and the whole public surface: `Session`, `SessionSpec`, `ServerFrame`, `ServeConfig`, `serve`. Read this first. |
| [`src/session.rs`](src/session.rs) | `Session::start` — one turn on a dedicated OS thread with its own current-thread runtime. Open it to change the turn lifecycle or frame ordering. |
| [`src/remote.rs`](src/remote.rs) | The two remoted port impls, plus `TokioSleeper` for the engine's retry backoff. |
| [`src/pending.rs`](src/pending.rs) | `Pending`, the `request_id` → one-shot registry shared across the two runtimes. Open it when a resolve POST returns 409. |
| [`src/frame.rs`](src/frame.rs) | The wire vocabulary: `ServerFrame`, `TurnOutcomeWire`, `ToolResultIn`, `ProviderResultIn`, `ProviderErrorWire`. Every wire-shape change starts here. |
| [`src/goal.rs`](src/goal.rs) | Judged multi-round runs (#1297): `GoalRun`, and the round loop driven over the same step driver a single turn uses. |
| [`src/subagents.rs`](src/subagents.rs) | Sub-agents (#1297): the operator's `SubAgentPolicy`, the dispatcher that runs a child on the same remoted ports, and the `task` tool layered over the host's stack. |
| [`src/server.rs`](src/server.rs) | `serve` — accept loop, bearer auth, the connection fold (one record per connection), route classification, the turn registry, and a rustdoc list of the operational limits the deployment must supply. |
| [`src/routes.rs`](src/routes.rs) | The endpoint handlers and the wire types they parse — the five `/v1/turns` routes (including `cancel`), `/healthz`, `/v1/metrics`, and the host-supplied ceilings (`max_steps`, `reverse_request_timeout_ms`). |
| [`src/observe/`](src/observe/) | Observability: `ServeEvent` (18 typed boundary events), the `Observer` port and its sinks, the request fold, the counters behind `/v1/metrics`, and the per-turn tally folded from the engine's own `AgentEvent` stream. Start at [`mod.rs`](src/observe/mod.rs); the design is [`docs/design/serve-observability.md`](../docs/design/serve-observability.md). |
| [`src/accept.rs`](src/accept.rs) | The written `accept()` classification policy — transient vs fatal, the backoff, and the give-up streak. Byte-identical to [`stella-observatory/src/accept.rs`](../stella-observatory/src/accept.rs) (the observatory takes no `stella-*` dependency, so there is no shared crate to hold it); a drift-guard test in both crates fails if the two copies differ. Change one, change the other. |
| [`src/http.rs`](src/http.rs) | A hand-rolled HTTP/1.1 + SSE layer following [`stella-observatory`](../stella-observatory)'s no-framework idiom, extended with request bodies, bearer auth, and long-lived responses. |
| [`src/error.rs`](src/error.rs) | `ServeError` — the named failures at the boundary. |
| [`src/main.rs`](src/main.rs) | The binary: env config (`STELLA_SERVE_BIND` / `_TOKEN_FILE` / `_TOKEN` / `_TOOLS` / `_LOG`; the file supply wins, and a token under 32 chars warns rather than refusing to start) and the `healthcheck` subcommand. |

### Reading the output

The server writes one JSON object per line to **stderr**, and exposes the same
information as counters at `GET /v1/metrics` (authenticated, same bearer token,
pull-only — nothing is ever sent anywhere).

```
STELLA_SERVE_LOG = off | error | warn | info (default) | debug
```

An unrecognised value falls back to `info` rather than failing startup: a typo
in a log knob must not take a service down. Rotation, retention and shipping are
the supervisor's job (systemd, Docker, the host's runner) — this process only
writes lines.

Records never carry prompt text, tool payloads, model output, filesystem paths,
raw request paths, the bearer token, or a whole turn id (a turn id is a *second
factor*: acting on a turn needs the token **and** the id, so records carry only
its first 8 hex characters — enough to correlate, useless for forging). That is
enforced by a sentinel sweep in the test suite, not by convention.

The four things that used to be indistinguishable from silence, and what to grep
for now:

| Situation | Record |
|---|---|
| a turn wedged on a reverse request nobody will answer | `"event":"reverse_timed_out"` — carries `waited_ms` and the kind |
| a host answering with the wrong `request_id` | `"event":"reverse_misrouted"` — `fault` separates a stale id from a wrong-kind answer |
| a 429 storm | `"event":"turn_refused"` with `reason":"at_capacity"`, plus `turns_live` |
| a bearer-token brute force | `"event":"unauthorized"` — a non-zero `held_ms` is the throttle engaging, which is a sustained guess rather than a misconfigured client |

To tell *wedged* from merely *slow*: a turn whose `turn_settled` tally shows no
advancing `stages` while a reverse request's `waited_ms` climbs is wedged; one
whose stages keep advancing is just long.

## Key concepts

**The reverse tool-call protocol.** Two deliberately asymmetric directions.
Outbound is one stream of `ServerFrame`s: mostly `Event` (agent events for the
UI), plus `ToolRequest` / `ProviderRequest` carrying a `request_id`, terminated by
exactly one `TurnComplete`. Inbound is the host POSTing `ToolResultIn` /
`ProviderResultIn` keyed by that same `request_id`, which fires the one-shot the
engine step is parked on. Over HTTP the outbound stream is the SSE endpoint and
the inbound direction is the two result POSTs; that transport is a thin layer, and
`Session` is usable directly without it.

**One turn, one OS thread.** The engine's `run_turn` future is deliberately
`!Send` (it holds provider futures and the retry-jitter RNG across awaits — see
[`../stella-cli/src/fleet_cmd.rs`](../stella-cli/src/fleet_cmd.rs)), so it cannot
be `tokio::spawn`ed onto the server's multi-thread runtime. Each session instead
gets a named OS thread running a *current-thread* runtime that `block_on`s the
turn, and the server talks to it only through `Send` channels. `tokio::sync::oneshot`
is runtime-agnostic, so a sender fired on the server runtime cleanly wakes a
receiver awaited on the session runtime. This is the fleet's bridge, reused for a
long-lived server; [`tests/bridge.rs`](tests/bridge.rs) exists to prove it in
isolation, with no socket involved.

**`Pending` register-before-emit, and its single-lock take.** A port registers its
reply channel *before* sending the request frame, so the entry always exists by
the time the host could answer. `take_tool` / `take_provider` do the kind check
and the removal under one lock on purpose: removing first and re-inserting on a
mismatch leaves a window where the id is absent, so a correctly kinded resolve
racing a mis-kinded one is rejected as unknown — the host gets a 409 for a result
it will not send twice, and the engine step stays parked forever.

**A turn is not always one agent (#1297).** A `goal` block runs judged rounds
until an independent judge says the goal is met; a `sub_agents` block lets the
model delegate research to a read-only child. Both put several agents behind
one turn id, which is why `ProviderRequest` carries `provider_id` and `role`:
a host reads them to route the judge — or the children — to a different model
than the worker. Both fields are additive, so a single-model host answers
every request with its one model exactly as before. The caller's knobs are
bounded by the server (`MAX_SERVED_GOAL_ROUNDS`, `ServeConfig::sub_agents`),
and sub-agents are **off by default** because children spend money on the
host's account. Shapes and a worked example:
[`docs/design/serve-surface.md`](../docs/design/serve-surface.md#judged-multi-round-runs-and-sub-agents-1297).

**Provider failures are classified by the host, not re-derived here.**
`ProviderErrorWire` mirrors `ProviderError`'s taxonomy on the wire; the engine
rebuilds a real `ProviderError` from it so retry behaves exactly as with a local
provider. Sending `terminal` where the host meant `rate_limited` silently disables
the engine's backoff.

## Gotchas

- **Event frames and reverse-RPC frames are not mutually ordered.** Agent events
  reach the frame channel through a forwarder task; the ports write request frames
  straight to it (so a starved forwarder cannot stall the turn). A `tool_request`
  can therefore overtake the `ToolStart` event that logically precedes it. The one
  guarantee `run_session` does enforce: the event channel is closed and the
  forwarder awaited before `TurnComplete` is sent, so every event frame precedes
  the terminal frame.
- **Several tool requests can be outstanding at once.** The engine runs
  consecutive `read_only` tool calls as a concurrent group (`stella-core`'s
  `execute_tool_calls`), and the host's advertised `read_only` flags drive that
  partitioning. Answer by `request_id`, in any order — a host that assumes one
  outstanding request at a time will stall the group.
- **`request_id`s are per-turn counters** (`prov-0`, `tool-0`), unique only within
  a turn. They are safe because the resolve routes are scoped by turn id — do not
  treat them as global handles.
- **A turn's event stream is exclusive and one-shot.** The second
  `GET /v1/turns/{id}/events` gets 409, and the registry entry is removed when a
  stream ends — or when the turn is cancelled. A *live* turn created and never
  streamed keeps its entry and its thread, so `cancel` is how a caller reclaims
  one it decided not to stream. Once such a turn finishes on its own (deadline,
  budget, completion) it is retained for a late stream, but it is no longer
  protected: when the registry is full, finished-unstreamed turns are reclaimed
  oldest-first to admit a new one, and a reclaimed id answers 404. That is what
  keeps the 32-turn cap a queue rather than a one-way latch — a host that
  abandons turns cannot wedge the server into refusing every later create.
- **`max_steps` from the wire is validated, not trusted.** `0` is a 400 (it would
  produce a zero-iteration turn that aborts with the misleading "reached the step
  cap (0)"), and anything above `MAX_SERVED_STEPS` (10 000, fifty times
  `EngineConfig::default`'s 200) is clamped — the host also supplies the budget
  mode, which may be `Off`, so an unclamped cap would remove the last bound on a
  turn holding an OS thread.
- **Chunked request bodies are not decoded.** A chunked POST parses as an empty
  body and fails validation with a 400. That is safe rather than a smuggling hole
  only because this layer serves one request per connection and then closes.
- **Requests are capped separately at the head (64 KiB) and the body (8 MiB),
  and going over either one is answered `413`** ([`src/http.rs`](src/http.rs)).
  The head and body caps are split because they are abused differently: no
  legitimate client sends 64 KiB of headers, while a body legitimately carries an
  assembled conversation or one tool's whole output (which `stella-tools` caps at
  100 KB per call). Over-cap used to get no response at all, which meant a
  `tool-result` one byte too large was indistinguishable from a crashed peer and
  left the engine step it would have answered parked until teardown.
- **Reads are bounded by a 30-second deadline**, answered `408`. It applies to
  the request head and body only — never to the SSE response, which is
  long-lived by design and would be killed mid-turn by any such deadline.
- **At most 32 turns may be live at once**; the 33rd `POST /v1/turns` is a `429`
  with `Retry-After: 5`. Accepted connections are deliberately *not* bounded: an
  SSE stream would hold a connection permit for the whole turn and starve the
  result POSTs that same turn must deliver on other connections, deadlocking the
  reverse-RPC protocol. Turns are the resource that accumulates, so turns are
  what is capped.
- **Turn ids are 128 random bits**, not a sequence, so seeing one id in a log or
  a proxy trace does not make every other live turn addressable.
- **The SSE stream must not be buffered by anything in front of it.** The
  response carries `X-Accel-Buffering: no` for nginx-family proxies; a proxy that
  buffers anyway deadlocks the protocol rather than merely delaying it, because
  the host cannot answer a `provider_request` it has not received.
- **The token comparison must stay constant-time.** `constant_time_eq` in
  [`src/server.rs`](src/server.rs) exists because `==` on `&str` stops at the first
  differing byte and leaks the shared secret to a caller timing its own 401s. A
  missing `Authorization` header is a hard `false`, so an empty configured token
  cannot authorize an anonymous request. **401s are also rate-limited** by a
  per-process token bucket (burst 8, refill 2/s, 500 ms penalty once empty) —
  the response body and status never change, only the latency, so the throttle
  leaks nothing about its own state and a correctly-configured host never
  reaches it.
- **`ToolExecutor::execute` never returns an error.** A tool failure is
  model-visible data, so a host disconnect mid-tool becomes `ToolOutput::Error`,
  not an engine error. The provider port is the opposite: a disconnect there is a
  `ProviderError::Transport`.
- **The server has no admission control and no read timeout** — see the
  `# Operational limits` rustdoc on `serve`. Front it with a proxy on a private
  network; it is a sidecar for one trusted host, not an internet-facing server.
- **A turn that stops making progress is bounded from two sides.** Every reverse
  request carries a deadline (`SessionSpec::reverse_request_timeout`, five
  minutes by default, overridable per turn as `reverse_request_timeout_ms` on
  `POST /v1/turns`), so a host that never answers fails the turn in minutes
  instead of parking its thread forever. `POST /v1/turns/{id}/cancel` is the
  manual side: the parked step wakes at once and the turn unwinds to an `aborted`
  outcome, so its settled cost still reaches a host that is streaming `/events`.
- **A transient `accept()` failure does not stop the server.** `accept()` errors
  are classified — see [`src/accept.rs`](src/accept.rs), which also explains why
  that file is duplicated verbatim in
  [`stella-observatory`](../stella-observatory/src/accept.rs) and must stay in
  sync. A peer that hangs up before its connection is accepted is retried; fd
  exhaustion backs off; only a structurally unusable listener (or one that has
  accepted nothing for the whole give-up streak) ends the loop. `serve`'s
  contract is therefore "serve until a **fatal** accept error", not "until the
  accept loop errors".

## Testing

```bash
cargo test -p stella-serve
```

There is no `make test-serve` target — the Makefile only has per-crate targets for
`core`, `model`, `tools`, `cli`, and `protocol`. No fixtures, env vars, or network
access are needed; the suites either bind `127.0.0.1:0` or use no socket at all.

- [`tests/bridge.rs`](tests/bridge.rs) drives a live `Session` from a mock host
  with **no HTTP**, answering reverse-RPC requests in-process. It covers the full
  model → tool → model loop, the no-tool path, a classified provider failure
  aborting cleanly, both reverse-request deadline paths, and cancelling a parked
  turn. Because the bridge is the risky part, prove a change here first.
- [`tests/http.rs`](tests/http.rs) runs the same protocol end-to-end over a real
  socket: `POST /v1/turns`, SSE, the two result POSTs, and `cancel`, plus the
  auth, `max_steps` and deadline rejections.
- Unit tests live beside the code in [`src/frame.rs`](src/frame.rs) (wire
  round-trips, including that a legacy `aborted` payload without `cost_usd` still
  deserializes), [`src/server.rs`](src/server.rs) (the step-cap and deadline
  clamps) and [`src/accept.rs`](src/accept.rs) (the `accept()` classification
  table against synthesised `io::Error` kinds, plus the drift guard that keeps
  this file byte-identical to the observatory's copy).

Deadline tests inject a short `reverse_request_timeout` — never the five-minute
default — so the suite stays fast. A test that waits out a real deadline is a bug.

Both integration suites need `#[tokio::test(flavor = "multi_thread")]`: a
current-thread test runtime cannot both drive the socket and let the session
thread's replies land.

## Extending it

To add another remoted port (an approval gate, a command runner):

1. Implement the port in [`src/remote.rs`](src/remote.rs), following
   `RemoteToolExecutor`: allocate a `request_id`, **register the one-shot before**
   sending the frame, and map a send failure onto whatever "the host is gone"
   means for that port's contract.
2. Add the outbound `ServerFrame` variant and the inbound `…In` type in
   [`src/frame.rs`](src/frame.rs).
3. Add a `PendingReply` variant plus its `register_*` / `resolve_*` / `take_*`
   trio in [`src/pending.rs`](src/pending.rs), keeping the kind check and removal
   under one lock.
4. Construct the port in `run_session` in [`src/session.rs`](src/session.rs) and
   pass it to the engine.
5. Add the POST route to `handle_conn` in [`src/server.rs`](src/server.rs) and a
   `ServeError` arm if the failure is new.
6. Extend [`tests/bridge.rs`](tests/bridge.rs) first, then
   [`tests/http.rs`](tests/http.rs).

Adding a `ProviderError` variant in `stella-protocol` is smaller but easy to miss
from the other crate: both `From` impls in [`src/frame.rs`](src/frame.rs) match
exhaustively, so this crate fails to compile until `ProviderErrorWire` grows the
matching arm — which is the intended forcing function, since a variant absent from
the wire would be silently reclassified.

## See also

- [`../AGENTS.md`](../AGENTS.md) — "Architecture: ports, not concretions" (why the
  remoted ports are adapters, not a rewrite) and the "Workspace layout" row for
  this crate, which states the own-binary/not-linked rule.
- [`../docs/design/serve-surface.md`](../docs/design/serve-surface.md) — the full
  design. Its status line now flags the gaps itself: it describes a larger
  target surface than what is implemented (sessions rather than turns,
  steering, pause, an approval gate, SSE replay from `?after=<seq>`, a
  `Host`-header guard, and a SIGTERM drain); the code today serves one turn per
  registered id with no resume. Cancellation is no longer on that list — it ships
  as `POST /v1/turns/{id}/cancel`. Treat the doc as the destination, `src/` as
  the state.
- [`../packaging/docker/Dockerfile.serve`](../packaging/docker/Dockerfile.serve) —
  how the binary is actually deployed, and the constraints that shape `main.rs`.
