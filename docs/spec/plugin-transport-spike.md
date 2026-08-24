---
id: plugin-transport-spike
title: "D-2 transport spike: subprocess hook vs. MCP + lifecycle extension"
status: living
---

# D-2 transport spike: subprocess hook vs. MCP + lifecycle extension

Governed by `doc:pipeline-as-plugins-execution` §3 D-2 and `doc:pipeline-as-plugins`
§5 commitment 3. The question the spike answers: when a Track A wrapper plugin
is a separate process, what does it speak over the four turn-level points
(`before_turn`, `after_turn`, `judge`'s evidence supplier, `again?`) — the
existing subprocess hook plane, or an MCP server with a not-yet-built
"lifecycle extension"?

**Decision: the subprocess hook path.** §5 below states it with its evidence
and its falsifier. This section states the method first so the numbers can be
checked before the verdict is read.

**Scope note, stated up front:** this decision is about the wrapper socket's
four *turn-level* points only. It says nothing about `PreToolUse`/`PostToolUse`
operator hooks (unaffected — they stay shell hooks either way) or about
`stella-mcp` as a *tool*-provider client (unaffected — that is a completely
separate integration point, already shipped, already MCP). It also has no
bearing on Track D (self-driving): `doc:pipeline-as-plugins` §10 already
settled that self-driving is a host, not a wrapper, and does not touch this
socket at all.

---

## 1. Method

**What was measured, and against what.** Two things exist in the tree today
and were exercised directly, not re-implemented from a spec reading:

- **(a) the subprocess hook path** — `crates/stella-core/src/hooks.rs` (the
  matching/blocking logic) and `crates/stella-tools/src/hook_runner.rs`
  (`ShellHookRunner`, the real spawn). Production shape, confirmed by reading
  `hook_runner.rs:31-39`: `bash -c "<command>"`, the event payload piped on
  stdin, stdout/stderr captured, `setsid` into its own process group, killed
  on timeout (default 60s, `DEFAULT_HOOK_TIMEOUT_MS`; ceiling 600s,
  `MAX_HOOK_TIMEOUT_MS` — `hooks.rs:75,78`).
- **(b) MCP over stdio** — the project's own `mcp-fixture-server` binary
  (`crates/stella-mcp/src/bin/mcp-fixture-server.rs`), built with
  `cargo build -p stella-mcp --bin mcp-fixture-server` (a build, not a crate
  edit), driven by a hand-rolled minimal JSON-RPC client
  (`MinimalMcpClient` in `bench.py`, below) that speaks the exact wire shape
  `crates/stella-mcp/src/protocol.rs` and `src/stdio.rs` implement:
  newline-delimited JSON-RPC 2.0, `initialize` → `notifications/initialized`
  → `tools/call`.

**What this is not.** `MinimalMcpClient` is not `stella_mcp::McpClient`. It has
none of the real client's reconnect/backoff state machine, pending-request
map, stderr diagnostics ring, or byte caps — those ~9 modules
(see `crates/stella-mcp/README.md`) are exactly what a production
MCP transport costs, and are *not* charged against MCP's latency numbers
below, only implicitly against its axis-3 (author burden) numbers, where a
real plugin author would have to either write an equivalent or pull in an SDK.
A path dependency from an out-of-tree crate onto a workspace member
(`stella-mcp`) does not resolve cleanly against this repository's
`[workspace.dependencies]` without joining the workspace, and this run's scope
is documents only — no crate edits, no workspace-member additions — so the
harness is a standalone script rather than a Rust binary linking the real
client. This is a real scope limit, named rather than hidden: it measures the
**wire protocol and process model**, not the engineering-completeness gap
between "a client that speaks JSON-RPC" and "the client `stella-mcp` actually
ships." Axis 3 (below) prices that gap directly by writing a minimal server
against the same protocol and counting what it took.

**Environment.** Measured inside this run's container; absolute milliseconds
will differ on other hardware. The *ratios* between conditions, and their
sign, are what is essential, and the harness was run twice back to back to
confirm they reproduce (both runs included below).

**Harness.** `/tmp/claude-0/-home-user-stella/6b8efc55-944b-55a4-a6fd-b89df01fe544/scratchpad/transport-spike/`
(outside the repo tree, per this run's scope — not committed):

- `bench.py` — the latency harness (`N_ITERS=50` for spawn-shaped
  measurements, `N_STEADY_STATE=200` for the amortized-connection case),
  reporting median, mean, population stdev, min, p90, max per condition.
- `hook_stub.py` — the smallest working Python Stop-hook (8 lines of code by
  `grep -vE '^\s*(#|$)'`).
- `mcp_stub_server.py` — the smallest working Python MCP server answering
  `initialize` and one `tools/call` (42 lines of code by the same count),
  stdlib only, no SDK — exercised end-to-end against `MinimalMcpClient` and
  confirmed to round-trip real newline-delimited JSON-RPC before being used
  for the axis-3 count, not just written and assumed correct.

---

## 2. Axis 1 — latency (measured, milliseconds, real)

Five conditions, two independent runs:

| Condition | Run 1 median | Run 2 median | n |
|---|---|---|---|
| Control: single compiled binary, no shell (`/bin/cat` direct) | 1.192 ms | (not re-run; stable, see run 1 log) | 50 |
| Hook floor: `bash -c 'cat'` (matches `ShellHookRunner`'s exact spawn shape, no interpreter) | 3.448 ms | 3.311 ms | 50 |
| Hook realistic: `bash -c 'python3 hook_stub.py'` (a Python-authored Stop hook, same spawn shape) | 20.847 ms | 21.033 ms | 50 |
| MCP fresh: spawn `mcp-fixture-server` + `initialize` + one `tools/call`, then close | 1.566 ms | 1.593 ms | 50 |
| MCP steady state: one long-lived connection, `tools/call` repeated | 0.107 ms | 0.114 ms | 200 |

Full distributions (run 1; run 2 within noise of run 1 on every condition —
see `run1.log` in the harness directory for the raw output including stdev/p90/max):

| Condition | mean | stdev | p90 | max |
|---|---|---|---|---|
| bare compiled-binary spawn | 1.215 ms | 0.122 ms | 1.334 ms | 1.650 ms |
| hook floor (`bash -c cat`) | 3.387 ms | 0.338 ms | 3.820 ms | 4.338 ms |
| hook realistic (`bash -c python3`) | 22.732 ms | 5.959 ms | 27.593 ms | 59.419 ms |
| MCP fresh connect+call | 1.622 ms | 0.128 ms | 1.778 ms | 2.085 ms |
| MCP steady state | 0.120 ms | 0.015 ms | 0.142 ms | 0.188 ms |

**Reading it honestly:**

- **Process/interpreter startup dominates the subprocess hook's cost, not the
  IPC.** The bare-exec floor (1.2 ms) to `bash -c 'cat'` (3.3–3.4 ms) shows
  `bash -c` itself roughly triples the cost versus a single compiled-binary
  spawn — expected, since it is *two* process creations (`bash`, then the
  external `cat` it forks). The jump to a realistic Python hook (~21 ms, and
  a long tail to 59 ms) is almost entirely CPython interpreter startup, not
  anything protocol-shaped.
- **MCP's fresh-connect number is close to the bare-exec floor**, because the
  fixture server is a single compiled binary and the handshake is two small
  JSON-RPC round trips over an already-open pipe — cheap relative to process
  creation itself.
- **MCP's steady-state number is the structurally different one**: ~0.11 ms,
  roughly **190× faster** than a realistic Python subprocess hook and **~30×**
  faster than the bare `bash -c cat` floor. This is not a tuning artifact —
  it is the direct, expected consequence of amortizing one connect over many
  calls instead of paying full process creation on every call.
- **A Python-authored MCP server pays interpreter startup too — but only
  once, at connect, not on every wrapper-point firing.** This run does not
  measure a Python MCP server's connect cost directly (the fixture server is
  compiled Rust), but the honest extrapolation is that it is in the same
  ballpark as the Python hook's ~21 ms spawn cost — paid once per session
  instead of once per consultation.

**How much this matters at the socket's actual call frequency.** The wrapper
socket's four points fire **once or a small bounded number of times per
turn**, not per tool call — `before_turn` and `after_turn` once each,
`again?` once, and `judge` never leaves the process at all (`doc:pipeline-as-plugins`
§6: the plugin supplies evidence and declares a verdict rule as data; the
host evaluates it in-process). That is roughly 2–4 subprocess spawns per turn
for an active wrapper plugin. Over a 20-turn session that is on the order of
40–80 subprocess spawns; at the realistic Python-hook cost (~21 ms) that is
roughly **0.8–1.7 seconds of pure dispatch overhead accumulated across the
whole session** — real, reproducible, and worth stating precisely rather than
rounding to "MCP wins" or "it doesn't matter." Against MCP's steady-state
number the same volume of calls costs single-digit milliseconds. **This is
the axis that clearly favors MCP**, and the number is honestly reported even
though the final decision (§5) goes the other way.

---

## 3. Axis 2 — failure modes

| Failure | Subprocess hook (today, built) | MCP over stdio (partially built; the "lifecycle extension" gap) |
|---|---|---|
| Crash before responding | `HookExecError::SpawnFailed` — structurally distinct from a completed non-zero exit (`hooks.rs:301-309`). `PreToolUse` blocks (fail-closed); `PostToolUse`/`SessionStart`/`PreCompact` never block, failure lands in `diagnostics` (fail-open, `hooks.rs:461-483`). `Stop` never blocks — deliberately, because failing closed there means never completing (`user_hooks.rs:55-59`). | A dead child leaves `StdioTransport` permanently closed; every outstanding waiter gets `McpError::Closed` with the child's stderr tail attached (`stdio.rs:528-541`, `src/README.md` "Connection failures are data"). `McpClient` classifies it as `RequestOutcome::Dropped`, reconnects, and — only for a tool advertised read-only/idempotent — transparently retries once (`client.rs:273-294`). **No fail-open/fail-closed *direction* exists yet**, because no hook-event semantics exist on top of `tools/call` — this is exactly the "lifecycle extension" gap named by the spike. |
| Hang (never answers) | Bounded by `HookAction::effective_timeout_ms()` (default 60s, ceiling 600s, `hooks.rs:75,78,108-115`); on timeout the whole `setsid` process group is `SIGKILL`ed (`hook_runner.rs:99-109`). `PreToolUse` blocks with a `TimedOut` reason (fail-closed); `Stop`/others never block (fail-open), matching the crash row. | Bounded by `McpClient`'s per-call `call_timeout` (default 60s, `toolset.rs:78`, same order of magnitude as the hook default). `RequestOutcome::Timeout` tears the connection down (`client.rs:266-271`, `note_call_failure` sets `conn.transport = None` — `client/health.rs:205`), which drops the last `Arc<StdioTransport>` and fires `kill_on_drop` on the child (`stdio.rs:250`), so a hung MCP child is eventually `SIGKILL`ed too, functionally the same shape as the hook path. Still missing: which hook-equivalent event this maps to, and which direction it should fail. |
| Malformed output | A decision-aware `PreToolUse` hook whose stdout is not valid `HookDecision` JSON is treated as an evaluation failure and **denies unconditionally** through `resolve_precedence`, regardless of any softening flag (`user_hooks.rs:24-29`, OXA-2056). | `decode_call_result` maps an undecodable `result` to a typed `McpError` (`RequestOutcome::Protocol`), passed straight through without reconnecting — "the server answered, just badly" (`client.rs:310-312`). Again, there is no wrapper-point *decision* semantics layered on top yet to say what a malformed `after_turn`/`again?` payload should do to the turn. |
| Plugin that never exits | Bounded by the same timeout + process-group `SIGKILL` as the hang case — cannot outlive the timeout window (`hook_runner.rs`, `GroupKillGuard`). | Not a distinct failure mode for a *correctly* long-lived MCP server (that is the intended shape); a runaway child that should have exited is caught by the same timeout+`kill_on_drop` path as the hang row, plus `close()`'s `SHUTDOWN_GRACE` (500 ms) before a hard kill at session end (`stdio.rs:78,370-415`). |

**Reading it honestly:** the low-level failure *primitives* MCP would need —
bounded timeout, typed failure classification, and eventual `SIGKILL` of a
wedged child — already exist in `stella-mcp` and already behave correctly.
What does **not** exist, on either side of this table, is the
**event-semantics layer**: which of the four wrapper points is fail-open and
which is fail-closed, expressed as MCP methods/capabilities rather than as
`HookEvent` variants. The subprocess side already has this worked out and
shipped, with the direction chosen deliberately per event
(`user_hooks.rs`'s module doc comment, lines 7-29 for why `PreToolUse` is
fail-closed and lines 31-59 for why `Stop` is fail-open — a lesson paid for
once, not something the spike gets to
re-derive for free by picking MCP). Building it against MCP is additive
engineering on a sound lower layer, not a structural blocker — but it is real,
uncredited, not-yet-designed work, and it is a genuine cost this axis charges
against MCP that this axis does not charge against the subprocess path
(because the subprocess path already paid it). **Net: a tie on the
primitives, a real and currently-uncosted gap on the semantics MCP would
still have to invent.**

---

## 4. Axis 3 — how much a non-Rust author must understand

Both scripts below were written to be the smallest thing that actually works,
then **run**, not just read — `mcp_stub_server.py` was verified against
`MinimalMcpClient` before being counted (see `bench.py`'s standalone
verification block); this is not a hand-wave line count.

| | Subprocess hook | MCP (no SDK, per `doc:pipeline-as-plugins` §9 rule 3) |
|---|---|---|
| Lines of code (non-blank, non-comment) | **8** (`hook_stub.py`) | **42** (`mcp_stub_server.py`) — and this omits `tools/list` (which a real client calls before it will ever call `tools/call`), `resources/*`, and any error path beyond one `else` branch |
| Concepts a Python author must hold in their head | JSON on stdin; one JSON object on stdout (2) | JSON-RPC 2.0 envelope shape (`jsonrpc`/`id`/`method`/`params`/`result`/`error`); newline-delimited framing with an explicit per-line flush; the mandatory `initialize` handshake and its specific result shape (`protocolVersion`/`capabilities`/`serverInfo`); the request-vs-notification distinction (`notifications/initialized` has no `id` and gets no response — get this wrong and a real MCP client stalls); method-string routing; the nested `content: [{type, text}]` result shape; the JSON-RPC error-object shape (7) |
| Ratio | 1× | **~5.25× the lines, ~3.5× the concepts** |

`doc:pipeline-as-plugins` §9 states the bar for Track C directly: *"if a
plugin **cannot** be written without an SDK, the protocol is too
complicated."* The subprocess hook path clears that bar with room to spare —
an 8-line, 2-concept program is close to the floor of what a stateful
request/response protocol could ever require. The MCP path clears it too (42
lines is not an SDK), but 5× the code and 3.5× the concepts, **before adding
the lifecycle-extension vocabulary axis 2 says is still unbuilt**, is a
material distance from "read in one sitting" for the exact audience (a
developer who does not write Rust, and who Track C's own rationale says will
not adopt a Rust-only surface) this axis is measuring for.

---

## 5. Decision

**The subprocess hook path wins. Proceed on it for A3b** (the wire contract
for the wrapper socket's four points), reusing the shape `hooks.rs` and
`hook_runner.rs` already ship — JSON payload in, JSON decision out, per-point
fail-open/fail-closed direction preserved exactly as `user_hooks.rs` already
argues it — generalized from the five `HookEvent` variants to the wrapper
socket's four points, and spawned via `[runtime].argv` directly (no shell) as
A5 already specifies, which the control row in §2 shows is *faster* than
today's `bash -c` wrapper, not slower.

**Why, weighing all three axes rather than picking the flattering two:**

- **Axis 1 (latency) clearly favors MCP** — an honest order-of-magnitude win
  at steady state, reported in full in §2, including the session-aggregate
  estimate (roughly 0.8–1.7 s of pure dispatch overhead per 20-turn session
  for the subprocess path against low-single-digit milliseconds for MCP).
  This is real and is not being explained away.
- **Axis 3 (author burden) clearly favors the subprocess path** — a measured,
  reproducible 5× code / 3.5× concept gap, weighed against `doc:pipeline-as-plugins`
  §9's explicit acceptance bar for Track C ("no SDK in the first cut... if a
  plugin cannot be written without an SDK, the protocol is too complicated").
  MCP does not fail that bar outright, but it spends most of the margin the
  bar allows, and does so **before** paying for the lifecycle-extension
  vocabulary axis 2 identifies as still unbuilt — which would only widen this
  gap further, since none of that vocabulary is standard MCP either.
- **Axis 2 (failure modes) is a tie on primitives, and a real, currently
  uncosted liability for MCP on semantics.** The subprocess path's
  fail-open/fail-closed direction per event is shipped and reasoned about in
  `user_hooks.rs:31-59` today; MCP's equivalent does not exist and is not
  standard MCP, so choosing MCP means designing and shipping a bespoke
  extension that buys none of MCP's actual ecosystem value (no other MCP
  host or client understands "the Stop point is fail-open") while still
  paying MCP's protocol surface in full.
- **Deciding factor:** axis 1's win is real but *bounded* — the wrapper
  socket's four points fire a small, constant number of times per turn (not
  per tool call; `PreToolUse`/`PostToolUse` are unaffected by this decision),
  so the accumulated cost is on the order of a couple of seconds across a
  whole session, invisible next to the multi-second model calls that
  dominate session wall time. Axis 3's loss is not bounded the same way — it
  is a permanent, per-plugin-author tax that lands hardest on exactly the
  audience Track C exists to prove Stella can serve, and it compounds with
  axis 2's real, unbuilt engineering cost (a non-standard protocol extension
  that has to be designed, documented, and gotten right on the fail-open/
  fail-closed direction that `user_hooks.rs` already got right once). The
  subprocess path costs zero incremental engineering — it is shipped,
  tested, and its failure directions are already deliberately chosen per
  point. Given `doc:pipeline-as-plugins-execution` §0's bias-to-shipping
  standing rule, and that this is a live, reversible flag choice (`doc:pipeline-as-plugins`
  §7's `--pipeline <variant>` shape applies to wrapper transport exactly as it
  does to plugin extraction), the lower-cost, already-correct, easier-to-audit
  option is the better one to ship first.

**What would falsify this decision** — re-run the spike, not re-argue it, if
any of the following becomes true:

1. **A wrapper point starts firing at tool-call frequency instead of
   turn frequency.** If a future point is added that fires many times inside
   one turn (the shape `PreToolUse` has today, deliberately excluded from
   this socket), axis 1's bounded-impact argument collapses and MCP's
   steady-state advantage becomes the dominant cost, not a secondary one.
2. **A first-party SDK for the subprocess wire shape turns out to need real
   engineering** (state machines, retries, partial-read handling) that erodes
   axis 3's advantage — i.e., if "JSON in, JSON out" stops being as simple in
   practice as it measures on paper once real plugins are written against it
   in Track C.
3. **The lifecycle-extension vocabulary for MCP turns out to be small** —
   if A3b's authoring step (`doc:pipeline-as-plugins` §5 commitment 1: the
   wire contract is authored, not implemented, before the trait is frozen)
   finds that mapping four points onto MCP methods with per-point
   fail-open/fail-closed direction is a handful of well-understood additions
   rather than a new protocol dialect, axis 2's liability shrinks and the
   decision should be revisited with that cost re-measured, not assumed.

None of these is true today, on the evidence gathered above. If any of them
becomes true, the numbers in this document are the baseline to re-run
against, not to trust from memory.

---

## Reproducing this spike

```bash
cargo build -p stella-mcp --bin mcp-fixture-server
python3 /tmp/claude-0/-home-user-stella/6b8efc55-944b-55a4-a6fd-b89df01fe544/scratchpad/transport-spike/bench.py
```

The harness path is a session scratchpad and is not committed (per
`AGENTS.md`'s `no-scratch` guard and this run's scope, which restricted
changes to this document and `docs/manifest.json`). A future session that
wants to keep this harness for repeat runs should promote it deliberately —
as a `#[ignore]`d integration test in `crates/stella-mcp/tests/` linking the
*real* `McpClient`, not this hand-rolled one — rather than reviving it from
scratch.
