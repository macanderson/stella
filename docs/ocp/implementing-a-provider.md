# Implementing an OCP provider

There are two ways to implement an OCP provider, depending on whether you're
writing Rust that runs inside the same process as the host, or a standalone
program (in any language) that the host talks to as a child process or a
remote HTTP endpoint.

## Option A: in-process, via the `ContextProvider` trait (Rust only)

If your provider runs in the same process as an `ocp-host`-based host,
implement the one trait every source implements
(`ocp-host::provider::ContextProvider`):

```rust
use async_trait::async_trait;
use ocp_host::HostError;
use ocp_types::{Capabilities, ContextQuery, ContextQueryResult, ProviderInfo};

#[async_trait]
pub trait ContextProvider: Send + Sync {
    /// The provider's host-facing id — its routing key and its consent key.
    fn id(&self) -> &str;

    /// Identity + declared data-flow direction, surfaced at consent time.
    fn info(&self) -> &ProviderInfo;

    /// Capabilities: which frame kinds and filters this provider serves,
    /// whether it upserts, does graph, is an embedder, or supports
    /// subscriptions.
    fn capabilities(&self) -> &Capabilities;

    /// Answer a context query with budgeted, provenance-carrying frames.
    async fn query(&self, query: &ContextQuery) -> Result<ContextQueryResult, HostError>;

    /// Shut the provider down cleanly. Defaults to a no-op.
    async fn shutdown(&self) -> Result<(), HostError> { Ok(()) }
}
```

`info()` and `capabilities()` are cheap synchronous getters — cache them at
construction time rather than recomputing per call. Register your provider
with `host.register(Box::new(my_provider))` and it participates in
`Host::query_all`'s fan-out like any other provider.

## Option B: out-of-process, via the wire protocol (any language)

A provider written in any language — the common case for a third-party
integration — implements the OCP wire protocol directly. `ocp-host` speaks
this protocol over two transports; you only need to implement one:

- **stdio** — the host spawns your program as a child process and exchanges
  newline-delimited JSON (NDJSON) over its stdin/stdout: exactly one
  `serde_json`-shaped value per line.
- **streamable HTTP** — the host POSTs one JSON envelope per exchange to your
  URL and expects one JSON envelope back as the response body.

Both transports carry the same message vocabulary, `ocp-host::wire::Envelope`
(a `serde` externally-tagged enum, `#[serde(tag = "type", rename_all =
"snake_case")]`):

| `type` | direction | payload |
|---|---|---|
| `handshake` | host → provider | `{ protocol_version }` |
| `handshake_ack` | provider → host | `{ protocol_version, provider: ProviderInfo, capabilities: Capabilities }` |
| `query` | host → provider | `{ query: ContextQuery }` |
| `frames` | provider → host | `{ result: ContextQueryResult }` |
| `shutdown` | host → provider | *(no payload)* |
| `error` | provider → host | `{ message: String }` |

`ProviderInfo`, `Capabilities`, `ContextQuery`, `ContextQueryResult`, and
`ContextFrame` are the `ocp-types` shapes documented in
[protocol-surface.md](./protocol-surface.md) — the wire payload is exactly
their `serde_json` serialization, field names and all.

### The exchange

1. **Handshake.** The host sends `handshake` with the protocol version it
   speaks. Your provider replies `handshake_ack` with:
   - `protocol_version` — see [Version compatibility](#version-compatibility) below.
   - `provider` — your `ProviderInfo` (name, version, and **honest**
     `data_flow`).
   - `capabilities` — what you can do (`Capabilities`).
2. **Zero or more queries.** The host sends `query`; you reply `frames` with
   a `ContextQueryResult`, or `error` if the request itself was bad (an
   `error` reply lets you report a problem without dying — a provider that
   exits on a bad request fails the `malformed-input-tolerance` conformance
   check).
3. **Shutdown.** The host sends `shutdown`; a well-behaved provider exits
   cleanly (stdio: exit the process; HTTP: nothing further to do — the host
   doesn't expect a reply).

A malformed line (bad JSON, wrong envelope shape) should be **ignored or
answered with `error` — never crash the process.** The host bounds every
exchange with a timeout on its side, so a slow reply is a timeout, not a
hang; but only your provider can guarantee it survives garbage input.

### Version compatibility

Two protocol version strings interoperate when they share a **major
family** — the substring up to the first `.`. So `ocp/1.0-draft` and
`ocp/1.0` interoperate (both family `ocp/1`), while `ocp/2.0` does not
(`ocp-host::wire::versions_compatible`). This is what lets the eventual
public `ocp/1.0` freeze drop the `-draft` suffix without a flag day — ack
whatever `1.x` family you actually implement; don't hardcode the exact
string. A version-family mismatch is reported to the host as a named error,
never left to hang.

### The data-flow / consent contract

`ProviderInfo.data_flow` is not decorative — it changes what the host will
do:

- `reads: true` — you can see workspace content via query payloads.
- `writes: true` — you persist `context/upsert`-style writes (not yet part
  of the query/frames exchange in this crate; reserved for a future OCP
  method).
- `egress: true` — **anything you do sends data off the local machine.**

**Declare `egress: true` honestly if your provider calls out to a remote
service, even indirectly.** A conforming host (`ocp-host::consent`) refuses
to query an `egress` provider until the user has recorded explicit, one-time
consent naming what leaves — the query payload is never transmitted before
that. This is enforced host-side and cannot be opted out of by a provider
that under-declares its own egress; note that `ocp-host`'s own HTTP transport
goes further and treats *every* remote provider as egress regardless of what
it claims in the handshake, precisely so a remote can't lie its way out of
the consent gate.

### The budget-honesty contract

Every `ContextQuery` carries `max_tokens`. **The frames you return must sum
`token_cost` to at most that budget.** A host built on `ocp-host::Host`
checks `ContextQueryResult::respects_budget` on every response and drops
(with a loud report, not a silent discard) the frames of any provider that
exceeds it — the `budget-honesty` conformance check enforces the same rule.
If you have more relevant material than fits, return your best frames within
budget, set `truncated: true`, and optionally `dropped_estimate`.

### The citation contract

Every frame needs a non-empty `title` and a non-empty `citation_label` — a
host must be able to cite what it used without falling back to a bare id.
This is checked by the `frame-validity` conformance check and is a
platform-wide convention, not an OCP-specific quirk.

### A complete minimal example

The `ocp-example-docs` binary bundled with `ocp-conformance`
(`ocp-conformance/src/bin/ocp-example-docs.rs`) is a real, runnable ~150-line
stdio provider that implements this whole exchange: it reads NDJSON lines
from stdin, replies to `handshake` with a `handshake_ack`, replies to `query`
with two canned `doc` frames, and exits cleanly on `shutdown`. Read it end to
end as the reference implementation; it deliberately reuses `ocp-host`'s
`Envelope` type for convenience (both crates live in the same workspace), but
an out-of-tree provider in any language only needs a JSON codec and the wire
table above — no dependency on `ocp-host` itself.

### Probing your provider interactively

Once you have something that speaks the handshake, point `ocp-inspect` (from
`ocp-conformance`) at it before running the full suite:

```bash
cargo install ocp-conformance
ocp-inspect stdio --query "how do I configure it" -- ./my-provider
# or:
ocp-inspect http --query "how do I configure it" https://my-provider.example.com/ocp
```

It prints your negotiated identity, capabilities, and data-flow, fires the
optional test query, and shows you the frames it got back with their scores
and token costs — a fast human-readable feedback loop before you run the
scripted conformance suite. See
[running-conformance.md](./running-conformance.md) for that next step.
