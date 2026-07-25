# Context reuse: identity, accounting, consent, and verification

<!-- NORMATIVE-HOME: macanderson/context-graph-protocol @ 6f8d7ef (contextgraph/1.0-draft) -->

<!--
  VENDORED — do not edit this copy.

  Source: macanderson/context-graph-protocol, docs/context-reuse.md
  Rev:    6f8d7ef13b2528c26913c6472405408ba2584a85

  This is the normative contract cited by 23 rustdoc comments across five
  crates (stella-cli, stella-context, stella-graph, stella-pipeline,
  stella-protocol). It lived only upstream, so every one of those citations
  dangled: `cargo doc` rendered them as plain text and nothing could check
  them. Vendoring makes them resolve offline and lets
  scripts/check-doc-citations.sh verify both the path and the cited section.

  The NORMATIVE-HOME pin above is enforced by scripts/check-normative-home.sh
  against the contextgraph-* git rev in stella-cli/Cargo.toml, so this copy
  cannot silently drift from the revision the workspace compiles against. To
  re-sync when that pin moves, bump the manifests and re-fetch:

      gh api "repos/macanderson/context-graph-protocol/contents/docs/context-reuse.md?ref=<rev>" \
        --jq .content | base64 -d

  Upstream is dual-licensed MIT OR Apache-2.0 (LICENSE-MIT / LICENSE-APACHE in
  that repository). That licensing carries with this copy; it is not changed by
  this workspace's AGPL-3.0-only terms.
-->

This is a companion to the [protocol surface](./protocol-surface.md). Where
that page defines the wire shapes for a *single* retrieval, this one defines
the four interlocking guarantees that make **reusing** retrieved context
across turns safe, cheap, and auditable:

1. **[Deterministic composition](#1-deterministic-composition)** — a stable
   frame identity and a canonical ordering, so an unchanged context set renders
   byte-identically and stays friendly to provider prompt caches.
2. **[Usage reports](#2-usage-reports)** — a per-request roll-up of frame costs
   an auditor can walk from an invoice line back to the exact frames.
3. **[Consent scopes and receipts](#3-consent-scopes-and-receipts)** — a closed
   vocabulary of egress classes and a durable receipt, so "did it leave the
   machine, to whom, and who agreed?" is answerable years later.
4. **[Context verification](#4-context-verification)** — a pull-based
   `context/verify` request that revalidates held frames cheaply, without
   re-sending any frame body.

They share one spine: the **frame identity** defined in §1. A usage report
references served frames by it; a verify request carries it. Build order and
dependency run top to bottom.

> **Conformance language.** **MUST**, **MUST NOT**, **SHOULD**, **MAY** in
> **bold** follow [BCP 14 / RFC 2119](https://www.rfc-editor.org/rfc/rfc2119),
> exactly as in [protocol-surface.md](./protocol-surface.md). Each section ends
> with its own conformance requirements; they are also consolidated into the
> [protocol-surface conformance table](./protocol-surface.md#conformance-requirements).

> **Spec-version implications.** Everything here is **additive** within the
> `contextgraph/1` family: new optional fields (`content_digest`,
> `egress_scopes`), new capability-gated methods (`verify`), and new host-side
> artifacts (`UsageReport`, `ConsentReceipt`) that ride no new required wire
> field. A `contextgraph/1.0-draft` provider that implements none of them still
> handshakes and answers queries; a host that wants them degrades to its
> existing behavior (re-query, boolean consent) when a provider opts out. No
> flag day — see [stability.md](./stability.md).

---

## 1. Deterministic composition

### The problem

Provider prompt caches reward a **byte-stable prompt prefix**: Anthropic bills
cache reads at 0.1× input, OpenAI and Gemini cache long prefixes automatically.
Context retrieval is the part of a prompt most likely to destroy that
stability. A host that re-queries its providers each turn and concatenates
frames in arrival order produces a *different* prefix every turn — silently
forfeiting the cache and multiplying the very token costs this protocol exists
to make honest. And nothing today stops two conformant hosts, given the
identical frames, from rendering arbitrarily different prompts.

This is distinct from the reference composition module (budget packing, dedup,
injection-resistance — issue #15). This section is the narrower **contract**
that any composition, reference or not, can satisfy to be cache-friendly.

### Frame identity

A frame's exact bytes are identified by the triple **`(provider id, frame id,
content digest)`**, bound to Rust as
[`FrameId`](https://docs.rs/contextgraph-types/latest/contextgraph_types/identity/struct.FrameId.html):

```rust
pub struct FrameId {
    pub provider_id: String,           // the host's routing/consent key for the serving provider
    pub frame_id: String,              // ContextFrame::id — provider-scoped, stable for dedup
    pub content_digest: Option<String>,// provider-declared, opaque (e.g. "sha256:<hex>")
}
```

- **The `content_digest` is provider-declared and opaque.** A provider chooses
  the algorithm; the reference frames use `sha256:<hex>`, matching the
  `provenance` digests. The protocol never re-derives it. This is deliberate:
  a host that computed the digest from its *own* canonical serialization would
  force every non-Rust provider to byte-exactly reproduce that serialization
  just to answer a [verify](#4-context-verification) request — precisely the
  lock-in the protocol avoids. The cost is that identity is a provider
  *promise* rather than a host-checkable fact; §4's conformance case is what
  holds a provider to that promise.
- A frame **MAY** omit its digest (`content_digest: None` /
  [`ContextFrame::content_digest`](./protocol-surface.md#context-frame) absent).
  Such a frame is **not verifiable**: a host **MUST** treat it as un-revalidatable
  and re-query rather than reuse it unchecked (§4).

Two frames with the same identity **MUST** have the same content bytes;
changing a frame's content **MUST** change its `content_digest` (else §4's
staleness detection cannot work).

### Canonical ordering

The **canonical composition order** is the total order over `FrameId` given by
comparing `provider_id`, then `frame_id`, then `content_digest`
(`Option<String>`, `None` before `Some`). A host that composes a set of frames
into a single context block:

- **MUST** emit them in canonical order, so an unchanged frame set renders
  byte-identically across turns *and* across hosts;
- **MUST NOT** let a frame's `score` or `token_cost` affect the rendered bytes
  — relevance is query-dependent and cost is derived, so a re-query that only
  re-ranks the same frames would otherwise bust the prefix for no content
  change;
- **SHOULD** de-duplicate identical identities to a single rendered block.

The reference host implements this in
[`compose_context`](https://docs.rs/contextgraph-host/latest/contextgraph_host/compose/fn.compose_context.html)
(and [`FanOut::compose`](https://docs.rs/contextgraph-host/latest/contextgraph_host/host/struct.FanOut.html#method.compose)).
Frame `content` is emitted inside an explicit `<frame>…</frame>` fence as
quoted material, never as instructions (protocol-surface R3).

### Prefix stability (informative)

Canonical ordering guarantees cross-turn and cross-host determinism. To
*maximize* cache hits, a host **SHOULD** additionally prefer **append-only**
composition: place newly-retrieved frames after the frames retained from the
previous turn, so a turn that only *adds* context extends the cached prefix
instead of reordering it. This is guidance, not a conformance requirement — a
host that re-sorts the whole set each turn is still conformant, just less
cache-efficient on turns that add high-sorting frames.

### Rationale: the cache economics (appendix)

Why bake this into the protocol rather than leave it to each host? Because the
failure is silent and expensive, and it compounds exactly where this protocol
claims to help. Consider a 20-turn agent session with an 8-frame, ~4,000-token
context block:

- **Arrival-order composition.** Each turn's retrieval returns the same frames
  in a slightly different order (vector search is not order-stable), so the
  prompt prefix changes every turn. Every turn pays full input price on the
  whole context block: `20 × 4,000 = 80,000` context tokens billed at 1×.
- **Canonical-order composition.** The context block is byte-identical on turns
  that don't change the underlying frames. Turn 1 pays 1× to write the cache;
  turns 2–20 read it at 0.1×: `4,000 + 19 × 4,000 × 0.1 = 11,600` tokens. A
  **~7× reduction** on the context portion of the prompt — with no change to
  what the model sees.

The saving is not hypothetical head-room; it is the difference between a
protocol that *measures* context cost honestly (budget honesty, §protocol-surface
B1) and one that also *lets a host control* it. Determinism is the small,
mechanical precondition that turns "we can see the cost" into "we can cut the
cost," and pairing it with [verification](#4-context-verification) is what makes
reuse both cache-friendly *and* safe from serving stale evidence.

### Conformance (§1)

| # | Requirement | Enforced / verified by |
| - | ----------- | ---------------------- |
| D1 | Frames with the same `FrameId` **MUST** have identical content bytes; changing content **MUST** change `content_digest`. | provider contract; `verify` conformance case (§4) |
| D2 | A host composing a frame set **MUST** emit frames in canonical `FrameId` order, independent of arrival order. | `compose_context` round-trip test |
| D3 | Composition **MUST NOT** depend on `score` or `token_cost`. | `compose_context` relevance-invariance test |
| D4 | A frame with no `content_digest` **MUST** be treated as unverifiable and re-queried, never reused unchecked. | host verify fallback (§4) |

---

## 2. Usage reports

### The problem

Every frame carries an honest `token_cost` ([budget honesty](./protocol-surface.md#budget-honesty), B1), but the protocol otherwise stops at the frame. A host that meters context into a billing system — the usage-events → ClickHouse → Stripe loop that platforms reselling agents run — has to invent its own aggregate: which providers served how many frames, at what token cost, against which budget. Every host inventing that shape independently means context cost stays unauditable one level up from the wire — the blob-pipe problem reborn at the accounting layer, where "what did this turn cost, and which sources drove it?" has no standard answer.

### The report

A **usage report** is the per-request roll-up, bound to Rust as
[`UsageReport`](https://docs.rs/contextgraph-types/latest/contextgraph_types/usage/struct.UsageReport.html):

```rust
pub struct UsageReport {
    pub budget_requested: u32,          // the query's max_tokens
    pub budget_consumed: u64,           // summed token_cost of every served frame
    pub as_of: String,                  // the report's accounting snapshot (RFC 3339)
    pub providers: Vec<ProviderUsage>,  // one entry per provider the query reached
}

pub struct ProviderUsage {
    pub provider_id: String,
    pub frames_served: u32,             // accepted (passed consent, timeout, budget audit)
    pub frames_rejected: u32,           // dropped — e.g. a budget lie, or (§4) a stale verify verdict
    pub token_cost: u64,                // this provider's contribution to budget_consumed
    pub served_frames: Vec<ServedFrame>,// each served frame, by identity + declared cost
}

pub struct ServedFrame {
    pub frame: FrameId,                 // the stable identity from §1
    pub token_cost: u32,                // the cost this frame contributed
}
```

Three properties make it usable as an accounting record rather than a debug dump:

- **It is a host-side artifact, not a wire message.** No new envelope variant, no new required field: a provider implements nothing to make one possible. The report rides the frames a query already returned.
- **It references served frames by their §1 identity.** `budget_requested` / `budget_consumed` are the roll-up an invoice line quotes; `served_frames` is the drill-down an auditor walks — from a billed total to the exact `(provider id, frame id, content digest)` triples behind it.
- **Its `as_of` is the report's snapshot time**, stamped by the producing host — *not* a query's bi-temporal [`as_of`](./protocol-surface.md#context-query) retrieval pin. The two are different clocks: one dates the accounting event, the other dates the facts retrieved.

The reference host produces one from a fan-out with
[`FanOut::usage_report(&query, as_of)`](https://docs.rs/contextgraph-host/latest/contextgraph_host/host/struct.FanOut.html#method.usage_report).
Accepted frames are itemized; a budget-lying provider's dropped frames count as
`frames_rejected` and contribute zero cost; a consent-gated or failed provider
served nothing. Because the host sums the totals from the very frames it
itemizes, the result always satisfies the arithmetic identity below.

### The arithmetic identity

The report is **self-verifying**: `budget_consumed` re-sums from
`providers[].token_cost`, which re-sums from `served_frames[].token_cost`. A
metering pipeline checks this before trusting a total —
[`UsageReport::is_consistent()`](https://docs.rs/contextgraph-types/latest/contextgraph_types/usage/struct.UsageReport.html#method.is_consistent)
is the one call — so a corrupted or tampered total is a checkable arithmetic
failure, never a silent misbill. The conformance case (below) proves the
identity holds against a *real* provider: it re-sums the accepted frames
independently and asserts equality with the report the host built.

### Worked example: mapping a report into a metering pipeline

A reseller host bills its customers for context by the token. After each turn it
takes the fan-out's report and fans it out into per-provider usage events:

```rust
// `as_of` is caller-supplied: the host stamps the report with its own RFC 3339
// clock (the accounting-event time), keeping the type free of a time dependency.
let report = fanout.usage_report(&query, now_rfc3339());
assert!(report.is_consistent()); // refuse to bill an inconsistent total

for provider in &report.providers {
    // One append-only usage event per (turn, provider) — the ClickHouse row.
    meter.emit(UsageEvent {
        request_id,
        turn_id,
        provider_id: &provider.provider_id,
        frames_served: provider.frames_served,
        frames_rejected: provider.frames_rejected,
        tokens: provider.token_cost,         // the metered quantity
        as_of: &report.as_of,
        // The drill-down: the exact frames behind this line, for dispute
        // resolution and audit. Stored alongside the event, not billed twice.
        frames: &provider.served_frames,
    });
}
```

- **ClickHouse (append-only runtime events):** each `UsageEvent` is one row.
  `tokens` is the metered measure; `provider_id` and `as_of` are the grouping
  keys; `frames` (the `ServedFrame` list) is the citation trail an auditor
  follows from a monthly total back to individual turns and frames.
- **Stripe (billing):** a periodic job sums `tokens` per customer over the
  billing window and reports it to a Stripe metered price. Because
  `budget_consumed` equals the summed `token_cost` of served frames by
  construction, the number a customer is billed is the number the frames
  actually cost — the honest-cost guarantee carried all the way to the invoice.
- **Dispute path:** a customer questioning a charge is answered by the same
  `served_frames` identities — every billed token maps to a cited frame with a
  provider, a frame id, and a content digest, not an opaque aggregate.

This is the payoff of pairing the report with §1's identity: the identity makes
the frames *nameable*, and the report makes them *countable*, so context cost is
auditable from the wire all the way up to the invoice line.

### Conformance (§2)

| # | Requirement | Enforced / verified by |
| - | ----------- | ---------------------- |
| U1 | A host **MUST** be able to produce a usage report for any query it executed, whose `budget_consumed` equals the summed `token_cost` of the served frames it reports. | `FanOut::usage_report`; `usage_report` conformance case (drives the real fixture, re-sums independently) |

---

## 4. Context verification

### The problem

§1 makes reusing context **cheap**: an unchanged frame set renders
byte-identically and rides the provider's prompt cache. But cheap reuse is only
safe if the frames are still **true**. A retrieved snippet is a claim about a
source, and sources move — a file is edited, a doc is rewritten, a record is
deleted. Reuse the frame anyway and the agent cites evidence that no longer
exists.

Today's only live mechanism for this is push invalidation ([#6](https://github.com/macanderson/context-graph-protocol/issues/6)'s
`subscribe`), which needs a long-lived channel and a provider able to watch its
sources. Plenty of providers can't: a stateless HTTP endpoint, a batch-rebuilt
index, a serverless function. And plenty of hosts don't want a subscription's
lifecycle just to answer a much simpler question at a turn boundary: *are the
frames I already hold still valid?*

Without a way to ask, a host is left choosing between two bad options every
turn — re-query everything (paying tokens and latency, and destroying the very
prefix stability §1 bought) or reuse silently and risk citing stale evidence.

### The exchange

`context/verify` is the cheap third option. The host sends the
[frame identities](#frame-identity) it holds; the provider answers one verdict
each.

```rust
pub struct VerifyRequest {
    pub frames: Vec<FrameId>,       // identities only — never bodies
}

pub struct FrameVerdict {
    pub frame: FrameId,             // the identity being answered, echoed in full
    // flattened: {"status": "...", "replacement_digest": "..."}
    pub verdict: Verdict,
}

pub enum Verdict {
    Valid,                                          // unchanged — keep reusing it
    Stale { replacement_digest: Option<String> },   // changed — drop it
    Gone,                                           // no longer exists — drop it
    Unknown,                                        // can't say — don't reuse it
}
```

**No frame body travels in either direction.** That is the entire economic
point: verification costs **bytes, not tokens**, so a host can afford to run it
every turn over frames it would otherwise have re-fetched in full. Even a
`stale` verdict carries at most the provider's *current digest* — enough for a
host to tell what it would be re-fetching, never the replacement content
itself.

The **digest is the ground truth**. A provider compares the digest the host
presents against the digest its source has now: equal ⇒ `valid`, different ⇒
`stale`, source gone ⇒ `gone`, can't tell ⇒ `unknown`. Because the digest is
provider-declared and opaque (§1), the provider is the only party that *can*
answer — which is why the conformance case below exists to hold it honest.

Each verdict **echoes the full identity it answers** rather than relying on
array position, so a provider that reorders, omits, or duplicates entries can
never shift a `valid` onto the wrong frame.

### Host behavior

Verification is **default-deny**. A host **MUST** keep reusing a held frame
only on an explicit `valid`; every other outcome drops it (requirement V2). In
particular an identity that comes back with **no verdict at all** is treated as
`unknown` — silence is not validity.

The reference host implements this in
[`Host::verify_frames`](https://docs.rs/contextgraph-host/latest/contextgraph_host/host/struct.Host.html#method.verify_frames),
which groups held identities by provider, asks each capable provider once, and
returns a **total partition** of the input into `retained` and `dropped` — no
held frame is ever silently lost. Each drop carries a
[`DropReason`](https://docs.rs/contextgraph-host/latest/contextgraph_host/host/enum.DropReason.html),
and `warrants_requery()` tells the host which dropped frames are worth fetching
again — false only for `gone`, which is not there to re-fetch.

**Capability gating and fallback.** A host sends `context/verify` only to a
provider whose handshake advertised `capabilities.verify`. Against a provider
that doesn't, the host **MUST** fall back to re-querying rather than reusing
unchecked (requirement V3) — the same fallback a frame with no
`content_digest` gets (§1, D4). Since `verify` defaults to `false`, an existing
provider that implements nothing keeps working unchanged, and the reference
`ContextProvider::verify` default answers `unknown` for everything, so a
provider can never *accidentally* bless a stale frame.

A verify failure is isolated exactly like a query fan-out leg: a provider that
errors or times out has *its own* frames dropped for re-query, and never
affects another provider's.

**When to verify** is host policy, not protocol. A host **SHOULD** re-verify
held frames at turn boundaries once they are older than the freshness window it
is willing to tolerate. This is deliberately informative: `verify_frames` holds
no state, caches no frames, and tracks no turns — the protocol's job is to
answer the question when asked, not to decide when to ask it.

The window is framed as *host-tolerated* rather than provider-declared on
purpose. A provider knows how often its sources *tend* to change, but only the
host knows how much staleness this particular task can absorb, and making the
window a wire field would push a scheduling decision into the protocol and give
the host state to keep. A provider that wants to share what it knows can
advertise a suggested freshness hint as a **future additive capability field**;
that would inform the host's policy without ever overriding it, and it needs no
change to the exchange specified here.

Note the ordering payoff: because only *evicted* frames change the composed
context, a turn in which everything verifies `valid` leaves §1's canonical
rendering byte-identical, so the prompt prefix — and its cache — survives. Only
a real change breaks the prefix, which is exactly when it *should* break.

### Verify vs subscribe (for the 1.0 freeze)

[#6](https://github.com/macanderson/context-graph-protocol/issues/6)'s
`subscribe` and this section's `verify` answer the same question — *is it still
true?* — from opposite directions, and the freeze can keep both, either, or
one:

| | `subscribe` (push, #6) | `verify` (pull, §4) |
| - | ---------------------- | ------------------- |
| Who initiates | Provider, when a source changes | Host, when it wants to reuse |
| Transport need | A long-lived channel | Any request/response round trip |
| Provider must | Watch its sources | Compare a digest on demand |
| Latency to detect | Immediate | At the host's next check |
| Cost shape | Idle connection + change events | One small round trip per check |
| Fits | Stateful local indexers, file watchers | Stateless HTTP, batch indexes, serverless |

They are **complementary, not alternatives**, and the capability flags are
independent: a provider may advertise either, both, or neither. Neither
subsumes the other — push has no answer for a host that reconnects and wants to
know whether what it cached is still good, and pull has no answer for a host
that needs to know *within milliseconds*. A provider advertising both gives a
host immediate invalidation plus a way to re-establish trust after any gap in
the channel.

Because both are capability-gated and default to `false`, the freeze can ship
`verify` without `subscribe` (this section), `subscribe` without `verify`, or
both, with no flag day either way — a host degrades to re-query whenever the
mechanism it wants is absent.

### Conformance (§4)

| # | Requirement | Enforced / verified by |
| - | ----------- | ---------------------- |
| V1 | A provider advertising `verify` **MUST** answer honestly by comparing digests: `valid` for a frame whose presented digest matches what it currently serves, `stale` when it differs on a frame it still serves. It **MUST NOT** answer `valid` for content bytes it is not serving. | `verify-honesty` conformance check (asks twice about just-served frames — real digests, then mutated); `--misbehave rubber-stamp-verify` and `hollow-verify` fixture modes prove it bites both ways |
| V2 | A host **MUST** keep reusing a held frame only on an explicit `valid`; `stale`, `gone`, `unknown`, and a missing verdict all evict it. | `Verdict::permits_reuse`; `Host::verify_frames` default-deny partition; host eviction tests |
| V3 | A host **MUST NOT** send `context/verify` to a provider that does not advertise `capabilities.verify`, and **MUST** fall back to re-querying those frames. | `Host::verify_frames` capability gate; `ContextProvider::verify` default; host fallback test |
| V4 | Neither a verify request nor its response **MAY** carry frame bodies; a `stale` verdict carries at most a replacement **digest**. | `VerifyRequest`/`VerifyResponse` shapes; `verify_wire` no-bodies test asserted against the serialized envelope |
