# ADR 0006: ContextFrame vs. CompiledContextFrame

- Status: Accepted (Phase 0)
- Date: 2026-07-23
- Deciders: (Phase 0 baseline)

## Context

Stella already consumes provider-emitted `ContextFrame`s: `contextgraph-types`
defines `ContextFrame`, and `stella-cli/src/contextgraph.rs` hosts in-tree
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
