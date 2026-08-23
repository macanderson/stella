# stella-mcp

An **MCP client**. It connects to external Model Context Protocol servers
(stdio child processes and streamable-HTTP endpoints), discovers their tools,
and merges them into the engine's tool registry so `stella-core::Engine` calls
them exactly like a built-in tool.

It is a client and nothing else. It drives `initialize` / `tools/list` /
`tools/call` — plus `resources/list` / `resources/read` for servers that
declare the `resources` capability (#2678) — and deliberately ignores every
server-initiated request or notification (sampling, roots, progress). It also does not decide *where*
anything lives on disk: [`config.rs`](src/config.rs) owns the shape of
`mcp.toml`, not its path, and [`TokenStore`](src/oauth.rs) is handed a path by
its caller. Path resolution, the `STELLA_TRUST_PROJECT` gate that decides
whether a cloned repo's `mcp.toml` may spawn processes at all, and every print
belong to `stella-cli` ([`mcp_cmd.rs`](../stella-cli/src/mcp_cmd.rs),
[`agent.rs`](../stella-cli/src/agent.rs)).

## Direction — the extension plane that already works

Stella's goal is an extensible turn loop whose new capabilities arrive from outside
the binary, and MCP is the half of that story already shipping: a server declares
tools, the host admits them under its own trust gate, and the engine calls them
exactly like a built-in. The other half is the turn-loop plugin — a participant
that gets a say in the loop itself rather than a tool in the registry
([`stella-plugin`](../stella-plugin) is its manifest, #3246 the sequencing).

Keep the two apart when planning: **a tool answers a call; a plugin shapes a turn.**
An MCP server that wants to gate completion, hold the loop, or judge a turn's work
is asking for the plugin plane, not a wider client here. What this crate should keep
inheriting from that plane is its posture, not its scope — the client stays a client,
ignores every server-initiated request, and leaves path resolution and the trust gate
to `stella-cli`.

## Where it sits

It depends on `stella-protocol` (`ToolOutput`, `ToolSchema`), `stella-core`
(the [`ports::ToolExecutor`](../stella-core/src/ports.rs) trait it implements
and the [`mcp_usage`](../stella-core/src/mcp_usage.rs) ledger it records calls
into), and `stella-store` (for the one shared atomic file-write helper,
`stella_store::durable::write_atomic`) — nothing else in the workspace.
`stella-cli` and `stella-runtime` depend on it. It also builds its own binary,
`mcp-fixture-server`, which exists purely for `tests/` and is excluded from
dist (`[package.metadata.dist] dist = false`).

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | The crate's authoritative design notes and the flat re-export surface. Read this first. |
| [`src/toolset.rs`](src/toolset.rs) (+ [`src/toolset/tests.rs`](src/toolset/tests.rs)) | `McpToolSet` — the `ToolExecutor` impl, tool namespacing, routing, native fall-through, disabled servers, and the Best-of-N `CandidateMcpView`. Open it for anything the engine sees. [`src/toolset/needs_auth.rs`](src/toolset/needs_auth.rs) holds the synthetic `login_required` tool an auth-suppressed server advertises; [`src/toolset/resources.rs`](src/toolset/resources.rs) the synthetic `list_resources`/`read_resource` pair a resources-capable server advertises (#2678). |
| [`src/client.rs`](src/client.rs) | `McpClient` — handshake, version negotiation, paginated discovery, `tools/call`, the reconnect/backoff state machine, and every ingest budget. The largest file and the one with the most invariants. |
| [`src/transport.rs`](src/transport.rs) | The `Transport` trait (framing only, no MCP methods) plus the `ScriptedTransport` test double used by the unit tests. |
| [`src/stdio.rs`](src/stdio.rs) / [`src/http.rs`](src/http.rs) / [`src/sse.rs`](src/sse.rs) | The two transports and the SSE decoder streamable-HTTP needs. `http.rs` also owns the shared `truncate` / `truncate_middle_out` helpers. |
| [`src/config.rs`](src/config.rs) | `mcp.toml`'s shape: `McpConfig` / `McpServerEntry` / `McpTransport`, the redacting `Debug`, and the `candidate_safe` opt-in. |
| [`src/oauth.rs`](src/oauth.rs) | OAuth 2.1 login (`login`), the on-disk `TokenStore`, and the runtime `OAuthManager` / `OAuthTokenSource` pair. |
| [`src/suppress.rs`](src/suppress.rs) | Auth-probe suppression (#2687): the on-disk 401 cache (`AuthProbeCache`, a sibling of the token store), the 15-minute TTL, and the pre-connect `connect_gate`. |
| [`src/registry.rs`](src/registry.rs) | The MCP Server Registry API client (`GET /v0.1/servers`) and the mapping from a registry entry to a writable `McpTransport` + the `AuthField`s still to fill. |
| [`src/error.rs`](src/error.rs) | `McpError` and `user_message()` — the length-bounded, credential-free rendering that reaches the model. |
| [`src/bin/mcp-fixture-server.rs`](src/bin/mcp-fixture-server.rs) | The canned MCP server the integration tests spawn, with its fault-injection flags. |

## God files — do not add lines

This crate has no god files: no file exceeds the gate's 1500-line ratchet
(`scripts/check-file-size.sh`), and none may appear — a new file crossing
1500 lines fails the gate outright, and `scripts/file-size-baseline.txt`
accepts no new entries. When a file here approaches the limit, split it before
it crosses.

## Key concepts

**One integration point, one namespace.** `McpToolSet` implements the same
`ToolExecutor` port `stella-tools`' `ToolRegistry` implements, so composing it
over the native set (`McpToolSet::connect(...).wrapping(native)`) makes MCP
tools indistinguishable from built-ins. Every MCP tool is advertised as
`mcp__<server>__<tool>` — composed by the one public `wire_name` function
(inverse: `split_wire_name`); a server name that is empty, contains the
reserved `__` separator, or starts/ends with `_` is skipped into
`failed_servers()` rather than producing an ambiguous wire name — those rules
make the encoding injective, and if two `(server, tool)` pairs collide anyway,
both routes are dropped and reported via `wire_name_collisions()` instead of
letting connect order pick a winner (#2675). A non-`mcp__` name falls through
to the native executor; an
`mcp__…` name that matches no route is a model-visible error and never falls
through. `stella-cli` builds the set once per session
(`load_mcp_plan` → `connect_mcp_servers` in
[`agent.rs`](../stella-cli/src/agent.rs)), which is why config changes take a
restart — but `schemas()` is re-read on every model call, so the shared
`DisabledServers` set toggles a server's tools in and out live.

**Everything a server can push at the model is capped at ingest** (#551).
`stella-core`'s compaction only trims on a *later* turn, so by the time it runs
the tokens are already paid for; the budgets therefore live in this crate.
[`client.rs`](src/client.rs) caps a rendered `tools/call` result at
`MAX_TOOL_RESULT_BYTES` (100 000, middle-out with a counted elision marker),
the tools accepted from one server at `MAX_TOOLS_PER_SERVER` (256), a
description at `MAX_TOOL_DESCRIPTION_CHARS` (2 000), and a serialized
`inputSchema` at `MAX_TOOL_SCHEMA_BYTES` (16 384). A schema cannot be
string-truncated and stay valid JSON Schema, so an over-budget one is *replaced*
with `{"type": "object"}` and a note appended to the description. A JSON-RPC
*error* response never reaches `decode_call_result`, so
[`McpError::user_message`](src/error.rs) bounds that second door on the same
budget.

**Connection failures are data, never an aborted turn.** `McpClient::call_tool`
classifies each bounded request into `RequestOutcome::{Ok, Dropped, Timeout,
Protocol}`. A *drop* reconnects and — only when the tool advertised
`readOnlyHint` or `idempotentHint` (`McpToolInfo::safe_to_retry`) — transparently
re-sends; a drop is ambiguous, so re-sending a mutating tool risks double
execution and instead surfaces a named error. A *timeout* already burned the
whole budget, so it tears down and defers the reconnect. A *protocol* error is
passed straight through: the server answered, it just answered badly.
`backoff_delay` retries the first failure immediately (a single blip self-heals
inside the turn) and then doubles from 1 s to a 30 s cap, so a long-dead server
is still probed forever. `HealthState` (`Live` / `Reconnecting` / `Down` /
`AuthRequired`) is surfaced per server through `McpToolSet::health` —
`AuthRequired` is never produced by the connection state machine; it is
synthesized for servers the set skipped before connect (below).

**Resources are tools, and embedded resources render** (#2678). A server that
declared the `resources` capability in its `initialize` result advertises two
extra synthetic tools, `mcp__<server>__list_resources` and
`mcp__<server>__read_resource` ([`src/toolset/resources.rs`](src/toolset/resources.rs))
— two single-purpose verbs per invariant #9, driving `resources/list` /
`resources/read` on the server's live client. A server whose *own* tool list
claims one of those wire names keeps it (the synthetic stands aside), a server
that never declared the capability is not probed for it, and — like
`login_required` — the pair never enters the routing map, so Best-of-N
candidates never see it. On the result side, an embedded **text** resource in
a `tools/call` result renders its text inline under a `[resource: <uri>]`
header instead of degrading to the bare placeholder; base64 `blob` payloads
are summarized, never inlined; and both doors are capped on the same
`MAX_TOOL_RESULT_BYTES` middle-out budget as every other result.

**A server that needs a login is skipped, not hammered** (#2687). A connect
answered `401 Unauthorized` is typed `McpError::Auth` (a redial cannot fix it;
`stella mcp login` can) and recorded in [`suppress.rs`](src/suppress.rs)'s
on-disk cache, a sibling file of the token store
(`.stella/private/mcp_auth_probes.json` for `stella-cli`). For the next
15 minutes (`AUTH_PROBE_TTL`) `McpToolSet::connect_with_auth` skips the server
entirely — no connection, no round trip — and the token store alone can rule a
dial out too: a stored login that is expired with no refresh token is
known-doomed before a byte is sent. Skipped servers land in
`auth_required_servers()` (deliberately not `failed_servers()`, which callers
render as "unavailable") and each advertises one synthetic
`mcp__<server>__login_required` tool
([`toolset/needs_auth.rs`](src/toolset/needs_auth.rs)) whose description and
invocation both carry the `stella mcp login <server>` instruction. Usable
stored tokens always win over a probe record — that is how a completed login
clears suppression with no back-channel — a successful connect retires the
record, TTL lapse restores the probe, and a fresh 401 re-arms it. The cache
fails open: corrupt or unreadable means "probe as before", never a wrongly
withheld server.

**A stdio server inherits no ambient credential.** `StdioTransport::spawn` uses
`Command::env_clear`; only the keys in the server's config `env` reach the
child, plus `PATH` — the one deliberate exception, because a bare runner
(`npx`, `uvx`, `docker`) cannot resolve without it, and a config `env` may still
override it. stderr is piped onto its **own** stream — never merged into stdout,
so a chatty server cannot corrupt the JSON-RPC framing — and continuously
drained into a small bounded ring of its newest lines (#638). That tail rides
along on every connection-death error, so a server that dies can say *why* (its
own last log lines) instead of just "closed the connection before responding";
nothing from stderr is ever parsed as JSON-RPC.

**OAuth is lazy and its state is secret.** `OAuthManager::source_for` hands
every HTTP transport a per-server `OAuthTokenSource` unconditionally; a source
with no stored tokens yields no header (static-header servers are untouched)
and re-checks the store until tokens appear, so a login completed mid-session
takes effect on the next tool call with no reconnect. Tokens live in the
caller-chosen JSON file — `stella-cli` puts it at
`.stella/private/mcp_oauth.json`, inside the gitignored `private/` directory.
Persistence is **hardened on Unix**: every open is `O_NOFOLLOW | O_CLOEXEC` at
`0600` under a `0700` parent and rejects a non-regular or multiply-linked file.
The write itself is the workspace's one durable-write contract
(`stella_store::durable::write_atomic`, #617) — temp + `fsync` + `rename` +
parent `fsync` — on every platform. Off Unix the hardening has no equivalent
and is skipped rather than made a precondition: refusing to persist does not
protect the token, it just leaves the user re-authenticating every session.

**Server annotations are claims, never facts (#3287).** A server's
`readOnlyHint` / `idempotentHint` / `destructiveHint` are ingested and ride
each tool's declared `ToolContract` (`McpToolSet::contracts()`) — visible to
display and policy at `Provenance::Declared`, with `destructiveHint` *raising*
the `High` grade to `Destructive`. They never touch the advertised
`ToolSchema`: `read_only` stays `false` there regardless of the hint, because
that bit admits a tool into concurrent dispatch and into read-only fences,
and a server's metadata is untrusted for the same reason its output is
(#2689). The one retry-path derivation (`safe_to_retry`) is unchanged.

## Gotchas

- **`PATH` is the only inherited variable, and it is required.** Without
  the pass-through, every registry-installed stdio server (`npx`/`uvx`/`docker`)
  failed to spawn. `stdio.rs`'s `a_bare_runner_command_resolves_via_inherited_path`
  is the witness.
- **`McpTransport`'s `Debug` is hand-written to redact values.** A plain derive
  prints an `Authorization` bearer or an API key verbatim into any log or panic
  message. Keys stay visible, values become `<redacted>`. Note the asymmetry:
  `McpConfig::to_toml_string` still writes those values to disk verbatim — the
  pre-existing `mcp.toml` convention.
- **The stdio transport never reconnects itself.** A dead child leaves it
  permanently closed and drains every waiter with `McpError::Closed`; respawning
  is `McpClient`'s job, so no `request` call hides a process restart. A client
  built with `McpClient::new` (the shape tests use) has no reconnector at all —
  dead stays dead.
- **Only a genuine response fulfills a waiter.** `JsonRpcMessage::is_response()`
  requires `result`/`error` and no `method`, because a server-initiated `ping`
  whose id collides with an in-flight client id would otherwise be handed to
  that caller as its answer.
- **An over-cap stdout line has its tail drained too.** Reading only the capped
  prefix and resuming would reparse the remainder as a fresh frame — a way to
  smuggle a response past `MAX_LINE_BYTES` (8 MiB). `sse.rs` bounds an
  unterminated event on the same reasoning.
- **`over_advertising_servers()` is deliberately not folded into
  `failed_servers()`.** Those servers are connected and their kept tools route
  normally; callers render `failed_servers()` as "server unavailable", which
  would be a lie. The dropped count is a floor — discovery stops at the cap.
- **`candidate_safe` is never inferred.** A server's own `read_only_hint` is
  untrusted and cannot distinguish "reads an external system" from "reads the
  local tree", so the Best-of-N allowlist is a human-set flag in `mcp.toml`, and
  `McpConfig::upsert` preserves it across a reinstall.
- **Every server-advertised MCP tool's `ToolSchema` is `read_only: false`.**
  External tools are unknown, so they are treated as mutating and never
  auto-parallelized. The exceptions are the *synthetic* tools this crate
  authors itself: `login_required` (#2687, invoking it touches nothing) and
  the `list_resources`/`read_resource` pair (#2678, the protocol's read
  surface) are `read_only: true` — none of them is `speculation_safe`, because
  a server's request budget is not ours to spend twice.
- **The token store is not locked across processes.** Two concurrent logins each
  rewrite from a pre-login snapshot and the later `rename` wins; one repeated
  login is cheaper than a stale-lock failure mode.

## Testing

```bash
cargo test -p stella-mcp
```

There is no `make` target for this crate — the root [`Makefile`](../../Makefile)
covers `core`/`model`/`tools`/`cli`/`protocol` only. Nothing needs a feature
flag, an env var, or the network. Unit tests live inline in each module and
drive the full protocol state machine over `transport::testkit::ScriptedTransport`
— no process, no socket. The integration tests are the interesting half:

- [`tests/stdio_integration.rs`](tests/stdio_integration.rs) spawns the real
  [`mcp-fixture-server`](src/bin/mcp-fixture-server.rs) binary, located via
  `env!("CARGO_BIN_EXE_mcp-fixture-server")` (cargo builds it automatically).
  Its flags drive the resilience matrix: `--hang` (call timeout),
  `--delay-call-ms` (a slow-but-working call — the connect/call timeout split),
  `--die-after` (mid-call death), `--garbage` (undecodable result),
  `--paginate` (cursor pagination), `--protocol-version` (negotiation, both
  accept and reject). The
  `env_probe` fixture tool is what makes the environment scrub testable from
  the outside.
- [`tests/http_integration.rs`](tests/http_integration.rs) and
  [`tests/oauth_integration.rs`](tests/oauth_integration.rs) use `wiremock`.
  The OAuth suite runs mock MCP *and* authorization servers and simulates the
  browser by GETting the loopback redirect with a code and the right state.
- [`tests/auth_probe_suppression.rs`](tests/auth_probe_suppression.rs) is the
  #2687 witness — a 401'd server's wiremock receives **zero** requests on the
  next startup within the TTL — kept alone in its file so it compiles against
  the pre-change surface; [`tests/auth_probe_integration.rs`](tests/auth_probe_integration.rs)
  covers the synthetic tool, login/TTL re-arm, and the skip-on-dead-tokens
  path.
- [`tests/resources_integration.rs`](tests/resources_integration.rs) holds the
  #2678 witnesses over the fixture server: embedded text resources render
  inline, the `list_resources`/`read_resource` pair round-trips and is
  `read_only`, and a `--no-resources` server advertises neither. Written
  against the pre-change public surface, so it compiles on the base commit and
  fails there on behavior.
- [`tests/registry_integration.rs`](tests/registry_integration.rs) replays
  recorded JSON in [`tests/fixtures/`](tests/fixtures) — deterministic and
  offline.

## Extending it

**Add a transport.** 1. Add a variant to `McpTransport` in
[`config.rs`](src/config.rs) (the enum is internally tagged on `transport`, so
the discriminant is the TOML key) and cover it in `kind_label`,
`credential_names`, `has_credentials`, `set_credential`, and the redacting
`Debug`. 2. Implement `Transport` in a new `src/<name>.rs`. 3. Wire it into
`build_transport` in [`client.rs`](src/client.rs) — that one function serves
both the first connect and every reconnect. 4. Re-export it from
[`lib.rs`](src/lib.rs) and add an integration test.

**Bump the protocol revision.** Add it to `SUPPORTED_PROTOCOL_VERSIONS` (and
move `PREFERRED_PROTOCOL_VERSION`) in [`protocol.rs`](src/protocol.rs). The
negotiation tests in [`tests/stdio_integration.rs`](tests/stdio_integration.rs)
drive both directions through the fixture server's `--protocol-version` flag,
whose own default is `2025-06-18` and must move with it.

**Add a fixture-server behavior.** New tools go in `all_tools()` /
`tools_call_response`, new resources in the `resources/list` arm +
`resources_read_response`, new fault flags in `Flags::parse` — then document
the flag in both the binary's `//!` header and the `[[bin]]` comment in
[`Cargo.toml`](Cargo.toml).

## See also

- [`../../AGENTS.md`](../../AGENTS.md) — "Architecture: ports, not direct dependencies"
  (why `ToolExecutor` is the only seam), "The `.stella/` directory" (where
  `mcp.toml` and `mcp_oauth.json` live), and "Testing approach".
- [`../../website/content/docs/agent-tools/mcp.mdx`](../../website/content/docs/agent-tools/mcp.mdx)
  — the user-facing `mcp.toml` reference.
- [`../../website/content/docs/commands/mcp.mdx`](../../website/content/docs/commands/mcp.mdx)
  — `stella mcp list|search|install|remove|login|logout|usage`.
- [`../stella-cli/src/mcp_cmd.rs`](../stella-cli/src/mcp_cmd.rs) — where
  `mcp.toml` and the token store actually live, and the atomic config write.
- [`../stella-cli/src/candidate_ws.rs`](../stella-cli/src/candidate_ws.rs) —
  the full rationale for `candidate_safe` and why every other server stays
  withheld from Best-of-N candidates.
