# OCP protocol surface

This is the normative shape of the Open Context Protocol (OCP) as bound to
Rust types by [`ocp-types`](https://crates.io/crates/ocp-types). Every type
below lives in that crate, round-trips through `serde_json`, and *is* the
protocol — there is no separate IDL. Field-level doc comments in the crate
are the ultimate source of truth; this page is a guided tour.

Protocol version: `PROTOCOL_VERSION = "ocp/1.0-draft"` (`ocp-types/src/lib.rs`).
See [`stability.md`](./stability.md) for what "draft" means and when it
freezes.

## The three modules

`ocp-types` is organized into three modules, re-exported from the crate root:

- [`capability`](#handshake--capability) — what a provider is and does,
  negotiated at the handshake.
- [`query`](#context-query) — the retrieval request/response shape.
- [`frame`](#context-frame) — the unit of exchange a provider returns.

## Handshake / capability

A provider identifies itself and what it does with data before a host ever
sends it a query.

```rust
pub struct DataFlow {
    pub reads: bool,   // can see workspace content via query payloads
    pub writes: bool,  // persists context/upsert writes
    pub egress: bool,  // sends anything off the local machine
}

pub struct ProviderInfo {
    pub name: String,
    pub version: String,
    pub data_flow: DataFlow,
}

pub struct Capabilities {
    pub query: QueryCapability,
    pub upsert: bool,
    pub graph: bool,
    pub embeddings_fingerprint: Option<String>,
    pub subscribe: bool,
}

pub struct QueryCapability {
    pub kinds: Vec<String>,   // e.g. ["doc", "snippet"] — see FrameKind below
    pub filters: Vec<String>,
}
```

`DataFlow.egress` is the security-critical field. **A conforming host MUST
NOT auto-enable a provider that declares `egress: true`** — it must gate that
provider behind an explicit, one-time consent that names what leaves
(enforced by `ocp-host`'s `ConsentStore`; see
[Implementing a provider](./implementing-a-provider.md)).

## Context query

A request to a provider for context frames relevant to a goal. Every query
carries a token budget; a conforming provider never returns more than it and
never lies about the cost.

```rust
pub struct ContextQuery {
    pub goal: String,                    // the task/turn goal driving retrieval
    pub query_text: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub kinds: Vec<FrameKind>,           // empty = "give me your best frames of any kind"
    pub anchors: Vec<String>,            // open files / mentioned symbols, for proximity scoring
    pub max_frames: u32,
    pub max_tokens: u32,
    pub as_of: Option<String>,           // pin retrieval to a point in time (bi-temporal facts)
}

pub struct ContextQueryResult {
    pub frames: Vec<ContextFrame>,
    pub truncated: bool,                 // true if more candidates existed than fit the budget
    pub dropped_estimate: Option<u32>,
}
```

`ContextQueryResult` carries two helper methods any host can use:

- `total_token_cost() -> u64` — sum of `token_cost` across returned frames.
- `respects_budget(max_tokens: u32) -> bool` — whether that sum stayed within
  the query's budget. `ocp-host`'s fan-out router calls this on every
  response and drops (with a loud report) any provider whose frames fail it —
  a provider that returns more tokens than it claimed is exhibiting
  **budget dishonesty**, and its frames are never trusted into a prompt.

## Context frame

The unit of exchange returned from a query. Frames, never blobs, carry
relevance, cost, and provenance so a budgeting, citing host can compose
sources honestly.

```rust
pub enum FrameKind {
    Snippet, Symbol, Fact, Doc, Memory, Episode, Graph,
}

pub struct ContextFrame {
    pub id: String,                      // provider-scoped, stable for dedup across queries
    pub kind: FrameKind,
    pub title: String,                   // human label — never a bare uuid
    pub content: String,                 // untrusted data — host must delimit as quoted material
    pub uri: Option<String>,
    pub score: f32,                      // provider-normalized relevance, [0, 1]
    pub token_cost: u32,                 // honest, conformance-audited token cost
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub recorded_at: Option<String>,
    pub provenance: Vec<Provenance>,
    pub citation_label: Option<String>,
    pub embedding: Option<FrameEmbedding>,
    pub relations: Vec<Relation>,
}

pub struct Provenance {
    pub kind: String,           // e.g. "file", "derivation", "episode" (serialized as "type")
    pub uri: Option<String>,
    pub range: Option<String>,
    pub digest: Option<String>,
    pub method: Option<String>,
    pub by: Option<String>,
}

pub struct Relation {
    pub rel: String,
    pub target_uri: String,
    pub display_name: Option<String>,    // a graph edge is surfaced by human label, never a raw id
}

pub struct FrameEmbedding {
    pub fingerprint: String,             // the vector payload itself is elidable
    pub vector: Option<Vec<f32>>,
}
```

Two contract points worth calling out explicitly, because the conformance
suite checks both:

- **`score` must be in `[0, 1]`.** `ContextFrame::has_valid_score()` is the
  cheap self-check any provider or host can run; `ocp-conformance`'s
  `frame-validity` check enforces it against real providers.
- **`title` and `citation_label` must never be empty.** A host must be able
  to cite a frame by a human label — an empty or missing citation label is a
  conformance failure, not a cosmetic gap. (Whole-platform convention: raw
  ids are never the primary on-screen identifier.)

## Wire framing (defined in `ocp-host`, not `ocp-types`)

`ocp-types` defines the payload shapes above; `ocp-host::wire::Envelope`
defines how they're framed on the wire — newline-delimited JSON (NDJSON), one
`serde_json` value per line over stdio, or one JSON body per streamable-HTTP
request/response. See [Implementing a provider](./implementing-a-provider.md)
for the full envelope vocabulary (`handshake` / `handshake_ack` / `query` /
`frames` / `shutdown` / `error`) and the version-compatibility rule.
