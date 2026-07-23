# ADR 0003: Bitemporal Semantics

- Status: Accepted (Phase 0)
- Date: 2026-07-23
- Deciders: (Phase 0 baseline)

## Context

The plan forbids guessing what today's `as_of` means before migrating off the
live graph (roadmap §2, plan §4). It must be characterized, not assumed.

The current `stella-context` behavior is now characterized. `edges_as_of`/
`neighbors` in `stella-context/src/store.rs` filter on **transaction/belief time
only**: `recorded_at <= t AND (superseded_at IS NULL OR superseded_at > t)`
(store.rs:806-807). This is a half-open `[recorded_at, superseded_at)` belief
interval — it selects which beliefs were *held* at `t`. It does **not** filter
world-validity (`valid_from`/`valid_to`); those columns exist on `node`/`edge`
but are never consulted by `as_of`. The store's own doc comment confirms
`as_of` is "transaction time" pinning "which beliefs are visible" (store.rs:787).

This ADR RECORDS the observed semantics and the target separation. A Phase 0
characterization test pins this behavior against `store.rs` `neighbors`/
`edges_as_of` so the migration cannot silently change it.

## Decision

The new bitemporal API separates two axes explicitly (lifecycle §7.5):

- `known_at` — transaction/belief (provider-local knowledge) time;
- `valid_at` — world validity.

Both use half-open intervals. Reconstruction is prefix-safe: restrict to
knowledge `<= known_at`, then apply `valid_at`/`valid_overlaps` per lineage
(§7.5). The legacy single-axis `as_of` maps to `known_at` — never silently to
`valid_at`, which would produce false historical results.

## Consequences

Callers of legacy `as_of` keep belief-time behavior through the `known_at`
mapping and the characterization fixture. Any query needing world-validity must
opt into `valid_at`; conflating the two is a correctness bug. Phase 3 supersedes
the single `as_of` filter with the typed two-axis temporal query.

## Open questions

None.
