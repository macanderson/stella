# Context reuse: identity, accounting, consent, and verification

<!-- NORMATIVE-HOME: macanderson/context-graph-protocol @ v0.1.2 (contextgraph/1.0-draft) -->

<!--
  VENDORED — do not edit this copy.

  Source: macanderson/context-graph-protocol, docs/context-reuse.md
  Rev:    c5fb2fec5820494ab6921dc088c03d7f43301fa7

  This is the normative contract cited by 23 rustdoc comments across five
  crates (stella-cli, stella-context, stella-graph, stella-pipeline,
  stella-protocol). It lived only upstream, so every one of those citations
  dangled: `cargo doc` rendered them as plain text and nothing could check
  them. Vendoring makes them resolve offline and lets
  scripts/check-doc-citations.sh verify both the path and the cited section.

  The NORMATIVE-HOME pin above is enforced by scripts/check-normative-home.sh
  against the exact contextgraph-* version in the root Cargo.toml's
  [workspace.dependencies] (crates.io releases since #819), so this copy
  cannot silently drift from the release the workspace compiles against. To
  re-sync when that pin moves, bump the manifest and re-fetch the doc at the
  git tag of the new release:

      gh api "repos/macanderson/context-graph-protocol/contents/docs/context-reuse.md?ref=<rev>" \
        --jq .content | base64 -d

  Upstream is dual-licensed MIT OR Apache-2.0 (LICENSE-MIT / LICENSE-APACHE in
  that repository). That licensing carries with this copy; it is not changed by
  this workspace's AGPL-3.0-only terms.

  One thing about the body below is upstream's shape, kept verbatim so this copy
  stays diffable against the pinned rev — do NOT "fix" it here:

  1. Sibling links (`./protocol-surface.md`, `./stability.md`, and their
     anchors) are CGP-repo-relative and have no counterpart in this workspace,
     because only this one document is vendored. Read them at
     https://github.com/macanderson/context-graph-protocol/blob/c5fb2fe/docs/
     — e.g. .../docs/protocol-surface.md.

  A second caveat used to live here and is now resolved: the intro numbered four
  guarantees while the document shipped only §1, §2 and §4, so a rustdoc comment
  citing §3 (consent scopes and receipts) failed check-doc-citations.sh. §3 is
  written upstream as of this rev and the body below carries it, so §3 citations
  now resolve like any other.

  WARNING — re-vendor the BODY when you bump the pin, not just the header.
  check-normative-home.sh compares the pinned release against the manifest; it
  cannot tell whether these bytes came from that release. Bumping the pin alone
  leaves a copy that *claims* a rev it does not match, and the guard goes green.
  That is exactly how this file spent a commit pinned at one rev with another
  rev's body — §3 existed upstream and was missing here. Re-fetch with the
  command above (or `git show <rev>:docs/context-reuse.md` from a checkout),
  keeping this header and replacing everything below it.
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
| UR1 | A host **MUST** be able to produce a usage report for any query it executed, whose `budget_consumed` equals the summed `token_cost` of the served frames it reports. | `FanOut::usage_report`; `usage_report` conformance case (drives the real fixture, re-sums independently) |

---

## 3. Consent scopes and receipts

### The problem

Consent-gating is one of the seven guarantees: retrieval that transmits workspace content off-machine must be agreed to. `DataFlow.egress` already gates it — but a boolean answers the question only *in the moment it is asked*. When an auditor asks, months later, "**what** left the machine, **to whom**, and **who** agreed?", a `true` in a since-exited process is no answer. And without a shared vocabulary of egress classes, "consent" means something different to every provider.

This section makes consent an **artifact** rather than an event, in two parts: a closed **scope vocabulary** that classes *where* content goes, and durable **consent receipts** that record each grant.

### The egress-scope vocabulary

A provider declares — alongside the boolean `egress` — the [egress scopes](./protocol-surface.md#handshake--capability) its served content falls under, bound to Rust as
[`EgressScope`](https://docs.rs/contextgraph-types/latest/contextgraph_types/scope/enum.EgressScope.html)
in the new [`DataFlow.egress_scopes`](./protocol-surface.md#handshake--capability) field. Four **normative base classes** form the closed core:

| Scope | Wire string | Off-machine? | Meaning |
| ----- | ----------- | ------------ | ------- |
| `LocalOnly` | `local-only` | no | Nothing leaves the machine. |
| `OrgTenant` | `org-tenant` | yes | Leaves the machine, stays in the org's own infrastructure. |
| `ThirdPartyIndex` | `third-party-index` | yes | Content sent to an external index / embedding service. |
| `ThirdPartyModel` | `third-party-model` | yes | Content sent to an external model API. |

The vocabulary is **extensible** by namespaced custom scopes — `EgressScope::Custom("vendor:scope-name")` — which **MUST** contain a `:` separator with non-empty sides, so a custom scope can never collide with or be mistaken for a base class. Everything other than `local-only` is treated as off-machine, including an unrecognized custom scope: the conservative default is that an unknown destination *leaves*, so a host never under-gates.

**A scope is declared at the provider (data-flow) level, and it governs every frame that provider serves.** There is no per-frame scope: the serving provider's declaration *is* the egress class of each frame it returns. This keeps the consent gate — which fires once, before a query is transmitted — the single place egress is decided, rather than scattering a scope across every frame.

The declaration must be **truthful**: an off-machine scope alongside `egress: false` is a contradiction (a provider claiming local posture while naming a destination that leaves), and a host holds a provider to this at the handshake
([`DataFlow::scopes_consistent`](https://docs.rs/contextgraph-types/latest/contextgraph_types/struct.DataFlow.html#method.scopes_consistent),
requirement C5).

### Consent receipts

When a host grants consent, it records a
[`ConsentReceipt`](https://docs.rs/contextgraph-types/latest/contextgraph_types/consent/struct.ConsentReceipt.html):

```rust
pub struct ConsentReceipt {
    pub provider_id: String,        // the provider this authorizes
    pub scope: EgressScope,         // the exact egress class consented to
    pub provider_name: String,      // provider identity, pinned at grant time
    pub provider_version: String,
    pub grantor: Grantor,           // Human(id) | Policy(id) — who agreed
    pub granted_at: String,         // RFC 3339
    pub expires_at: Option<String>, // RFC 3339, if consent lapses
}
```

A receipt turns "is this allowed?" (a boolean, now) into "what left, to whom, who agreed, and when?" (a durable record). It pins the provider's identity **at grant time**, so a later rename can't retroactively rewrite what was agreed. Receipts live in an **append-only** ledger
([`ConsentStore::record_receipt`](https://docs.rs/contextgraph-host/latest/contextgraph_host/consent/struct.ConsentStore.html#method.record_receipt)):
a new grant never edits or erases an old one, so the history of consent *is* the audit trail, and it is serde-able for durable persistence across host runs.

Like the [usage report](#2-usage-reports), a receipt is a **host-side artifact, not a wire message** — it rides no envelope variant, and a provider implements nothing to make one possible. It nonetheless lives in `contextgraph-types` rather than the host crate, for the same reason the usage report does: it is a *protocol-defined shape*. Any host in any language that claims the consent guarantee must produce this shape, and an auditor reading a persisted ledger must be able to parse it without depending on one particular host implementation. The ledger and gate that *consume* receipts are host machinery and stay in `contextgraph-host`.

### Host behavior: reject unconsented egress

A host's pre-query consent gate
([`ConsentStore::evaluate`](https://docs.rs/contextgraph-host/latest/contextgraph_host/consent/struct.ConsentStore.html#method.evaluate))
is scope-aware:

- A provider declaring **off-machine egress scopes** is permitted only when *every* such scope has a recorded receipt. A scope with no matching receipt **MUST** cause the query to be refused with a **typed error** —
  [`HostError::ConsentScopeRequired`](https://docs.rs/contextgraph-host/latest/contextgraph_host/error/enum.HostError.html)
  naming exactly the scopes that would leave unconsented — and the payload **MUST NOT** be transmitted (requirement C6). A budget-style boolean `ConsentRecord` does **not** satisfy a scope gate; only a receipt for that scope does.
- A provider declaring only the boolean `egress` flag (no scopes) keeps the pre-scope legacy gate — a `ConsentRecord` unlocks it. This is what keeps the change additive: an existing provider that never declares scopes behaves exactly as before.

The runtime gate is **presence-based** (does a receipt for the scope exist?) and carries no clock, so it never depends on wall-time to make a decision. **Expiry** is a first-class receipt property
([`ConsentReceipt::is_live`](https://docs.rs/contextgraph-types/latest/contextgraph_types/consent/struct.ConsentReceipt.html#method.is_live)):
a host that enforces expiry consults
[`ConsentStore::live_receipt`](https://docs.rs/contextgraph-host/latest/contextgraph_host/consent/struct.ConsentStore.html#method.live_receipt)
against its own `now`, and treats an expired receipt as absent — re-shutting the gate — while the receipt itself is never pruned from the audit ledger.

### Worked audit scenario: "what left, where, who agreed, when?"

Six months after the fact, an auditor asks whether a customer's repository snippets were ever sent to an external model. The host answers from its persisted consent ledger — no live process required:

```rust
for receipt in store.receipts_for("acme-cloud-model") {
    println!(
        "{scope} — granted by {grantor:?} at {at}{expiry}",
        scope = receipt.scope,                 // third-party-model
        grantor = receipt.grantor,             // Human("ops@oxagen.sh")
        at = receipt.granted_at,               // 2026-01-14T09:02:00Z
        expiry = receipt.expires_at            // Some("2026-07-14T00:00:00Z")
            .map(|e| format!(", expiring {e}"))
            .unwrap_or_default(),
    );
}
```

Every question is answered by a field:

- **What left?** The `scope` (`third-party-model`) — the class of destination — combined with the [usage report](#2-usage-reports)'s `served_frames`, which name the exact frames that provider served.
- **To whom?** The pinned `provider_name` / `provider_version` at grant time.
- **Who agreed?** The `grantor` — a named human or a named policy, not an anonymous "yes".
- **When?** `granted_at`, and `expires_at` if the grant was time-boxed. A query after `expires_at` would have been refused by the gate, so the window of authorized egress is itself on the record.

Because the ledger is append-only, a *revocation* or a lapsed expiry adds to the record rather than erasing it — the audit shows not just the current state but the full history of what was permitted and when.

### Conformance (§3)

| # | Requirement | Enforced / verified by |
| - | ----------- | ---------------------- |
| C5 | A provider **MUST** declare its egress scopes (`egress_scopes`) truthfully and consistently with `data_flow.egress`; an off-machine scope alongside `egress: false`, or a non-namespaced custom scope, is a conformance failure. | `DataFlow::scopes_consistent`; `consent-scope` conformance check |
| C6 | A host **MUST** refuse a query, with a typed error naming the scopes, when a provider declares off-machine egress scopes and any such scope has no recorded consent receipt; the payload **MUST NOT** be transmitted. A boolean `ConsentRecord` does not satisfy a scope gate. | `ConsentStore::evaluate`; `scope-lie` witness |

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
