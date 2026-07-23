# ADR 0005: Storage Authority

- Status: Accepted (Phase 0)
- Date: 2026-07-23
- Deciders: (Phase 0 baseline)

## Context

Today `context.db` is a bi-temporal knowledge graph — `node`/`edge`/`memory`/
`episode` tables managed by `stella-context/src/store.rs` (`migrate()` +
`SCHEMA_VERSION`, currently v3) — **not** a record authority. The live `recall`
path, `memory promote`, and the code graph all read these tables. The roadmap
names Phase 2 the single riskiest step: it evolves a live authoritative store,
so it cannot be greenfielded.

This ADR RECORDS the authority model already decided by the plan (§6) and
roadmap. It opens no question, but Phase 2 execution carries a human-decision
gate (confirm the model on a copy of a real `context.db`).

## Decision

A new immutable `context_records` table becomes the canonical **local**
authority (Phase 2). Today's `node`/`edge`/`memory`/`episode` tables become
**transactionally-rebuilt projections / compatibility views** derived from it.
The canonical row and its projections commit in **one transaction**; a
projection-rebuild command reproduces them byte-for-byte.

Hard migration constraints:

- Must not lose a memory or break the live `recall` path.
- **No LLM or semantic reclassification inside a migration.** Ambiguous
  memories migrate losslessly as `memory` (per ADR 0001); reclassification is a
  later reviewable proposal, never a migration side effect.
- Extend the existing `store.rs` migration harness — do not create a replacement
  database. Fixtures for every schema version (v1/v2/v3) must prove replay is a
  no-op.

## Consequences

`context_records` holds `record_hash` + `canonical_json` (ADR 0004) as the
source of truth; legacy tables become derived read paths kept working through
adapters. Front-loaded fixtures and a tested rollback are mandatory before the
migration runs against real data.

## Open questions

None.
