# ADR 0006: ContextFrame vs. CompiledContextFrame

- Status: Accepted (Phase 0) — amended 2026-07-26 (compiled frame extends the
  step manifest; no parallel aggregate)
- Date: 2026-07-23
- Deciders: (Phase 0 baseline); amendment recorded 2026-07-26
- Tracking: [issue #713](https://github.com/macanderson/stella/issues/713)
  deliverable 6 (part of Epic #469)

> **Amendment notice, 2026-07-26.** The *distinction* below stands:
> `ContextFrame` is provider-emitted input, the compiled frame is stella's own
> artifact. Two things no longer hold. **The Context section's premise is now
> false** — there *is* a manifest, it is content-addressed and cost-attributed,
> and turns reconstruct byte-exactly and digest-verified. And with it the
> route: the compiled frame is reached by **extending the step manifest**, not
> by building a parallel aggregate. The phase attribution has also moved — this
> is Phase 2 in the current plan, not Phase 3. See the amendment under
> *Decision*. Prose below is left as written: it is an accurate record of what
> was true when it was written.

## Context

Stella already consumes provider-emitted `ContextFrame`s: `contextgraph-types`
defines `ContextFrame`, and `crates/stella-cli/src/contextgraph.rs` hosts in-tree
providers. Today `recall` returns frames with honest token budgeting and
drop-reports, but there is no immutable *compiled* aggregate, no manifest, and
no byte-stable hash — so an invocation's context is not reproducible or
inspectable after the fact.

This ADR RECORDS the distinction the plan draws (Phase 3, roadmap Layer 1). It
opens no question.

## Decision

Keep two distinct concepts:

- `ContextFrame` — the **provider-emitted input**, from `contextgraph-types`.
- `CompiledContextFrame` — a new, deterministic, inspectable aggregate stella
  builds, carrying a `FrameManifest` and a byte-stable `frame_hash` (Phase 3).

Compilation is **deterministic**: identical inputs produce a byte-identical
frame body and identical `frame_hash`. Required items **cannot be evicted by
ranking** — precedence is category-aware, and budget packing may drop only
non-required items, always with a drop-report (reusing today's honest
budgeting discipline from `retrieval.rs`, never silent truncation).

**Amendment — the compiled frame extends the step manifest (recorded
2026-07-26, issue #713 deliverable 6):** the *distinction* above stands —
`ContextFrame` is provider-emitted input, the compiled frame is stella's own
artifact — but this ADR predates the receipts plane, and its account of **how**
the compiled frame gets built no longer describes the system. It is reached by
**extending the step manifest, not by building a second aggregate.**

When this ADR was written there was "no manifest". There is now. Each step
already records an ordered, content-addressed, cache-zoned, cost-attributed
list of blocks — `ManifestEntry` (`crates/stella-protocol/src/event.rs:1118`) carries
`block_id`, `cache_zone`, `token_cost`, `resident_since_step`, `message_index`,
and `call_id` — and reconstruction from it is byte-exact. Provenance is stamped
at a block's birth by `BlockOrigin` (`:1095`), whose `memory_id` is the join
back to the record a recalled frame came from. The block taxonomy in
`BlockKind` (`:244`) already names `RecalledFrame`, `Steered`, `Summary`, and
`Attachment`.

So the three things a `CompiledContextFrame` was invented to supply are not
missing artifacts; they are missing *fields and producers* on an artifact that
ships:

1. Recall enters the manifest whole, as a single goal block. The four kinds
   above are defined and never produced (#713 deliverable 1).
2. Block identity and content digest are null on frame references (#713
   deliverable 2).
3. There is no stable frame-level identity over the manifest body (#713
   deliverable 4).

The governing reason is not economy of implementation. **Two immutable records
of one turn's context can disagree; one cannot** (spec §6.2). A parallel
aggregate built beside the manifest would be a second immutable account of the
same turn, and the first time the two diverged, neither would be evidence. The
determinism this ADR requires — identical inputs producing a byte-identical
body and hash (spec §5.3) — is a property to establish *over the manifest
body*, and `frame_hash` becomes a hash of that body with volatile fields
excluded, not the identity of a separate object.

The volatile fields excluded are the ones that vary between two runs of
identical work — who served the call, what the budget arithmetic produced, and
how long a block had been resident. Determinism is a property of *what entered
the prompt*, not of the accounting around it. The hash itself reuses the
canonical scheme ADR 0004 already ratified: drop the hash field, strip nulls,
RFC 8785 JCS, sha256. A second hashing scheme in the same codebase would be two
answers to "are these the same bytes".

Two constraints follow, and they bind Phase 2:

- **Additive and version-tolerant.** The manifest is a persisted wire type with
  existing readers. The precedent is the forward-compatibility path already
  built for unknown block kinds: `BlockKind::Other` and `CacheZone::Other` are
  `#[serde(other)]`, so a manifest from a newer emitter deserializes instead of
  failing the whole event. New fields land `#[serde(default)]`, exactly as
  `cache_zone`, `message_index`, and `call_id` did.
- **Cache zones must not shift** — `adaptive-context.md` §5.1, *cache stability
  outranks allocation cleverness*. (Cite it by document: the receipts spec's
  own §5.1 is the reconstruction algorithm, a different rule.) Extending the
  manifest is a recording change; it may not move a byte of the system prefix,
  and any reshaping of what recall contributes stays in the post-prefix
  volatile block.

What survives unchanged is more than the distinction. The **event** this ADR
promised is already built: `CompiledContextFrameBuilt`
(`crates/stella-protocol/src/context_event.rs:273-280`) carries `compiled_frame_id`
and a `sha256:`-prefixed `frame_hash`, with a pinned golden JCS vector, and is
deliberately unwired — the wire shape was fixed before its first emitter. So
the amendment retires the *aggregate*, not the announcement: what that event
names becomes the extended manifest's frame-level identity.

One correction of record while amending: `Consequences` below attributes this
work to **Phase 3**. In the current plan it is **Phase 2** — the accountable
frame — and Phase 3 is the typed adaptive loop. What this ADR called
`CompiledContextFrame` therefore names the *extended step manifest plus its
frame-level identity*, not a new persisted aggregate, and it lands a phase
earlier than written. The `Consequences` section should be read with both
substitutions.

## Consequences

Phase 3 emits `CompiledContextFrameBuilt` events and persists compiled frames +
manifests. Gate: identical inputs → byte-identical frame/hash; required items
survive ranking; scope-leakage tests pass at every dimension. This is the
"accountable" milestone (M-A) — every invocation gets a deterministic,
provenance-bearing frame with honest costs. Compaction (Phase 4) then wires the
CGP `Compact`/`Reference` representations stella defines but never emits, under
per-item minimum fidelity so compaction can never weaken a blocking constraint.

## Open questions

None.
