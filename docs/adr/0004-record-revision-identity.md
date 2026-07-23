# ADR 0004: Record Revision Identity

- Status: Accepted (Phase 0)
- Date: 2026-07-23
- Deciders: (Phase 0 baseline)

## Context

An accountable, self-correcting store must be able to reconstruct exactly what
was believed at any past point and prove a record has not been tampered with.
Mutating rows in place destroys both properties. The canonical spec therefore
makes records immutable and content-addressed (lifecycle §7.6). This ADR RECORDS
that decision; it opens no question.

## Decision

Records are **immutable**. Three identity fields (lifecycle §7.6):

- `record_id` — identifies **one immutable revision**;
- `lineage_id` — the stable conceptual record across revisions;
- `supersedes_record_id` — links a revision to its immediate predecessor.

Records are never mutated in place. Corrections, retractions, and archival each
create a **new revision** that supersedes the prior one; earlier bytes and
hashes are left unchanged. `superseded` is therefore a *derived* EffectiveStatus
of the predecessor, never written back into it.

`record_hash` is `sha256:<64 lowercase hex>` over the RFC 8785 JCS bytes of the
record, with `record_hash` **omitted from its own preimage** (§7.6). Before
canonicalization: resolve input aliases, omit absent optionals, normalize
input nulls, and normalize timestamps to UTC `Z` with trailing fractional-second
zeros removed. All semantic fields, provenance, links, and extensions
participate; transport/ingestion metadata (e.g. append idempotency keys) do not.
`EffectiveStatus` is excluded from `record_hash`.

## Consequences

Phase 1 ships golden JCS hash vectors (real 64-char digests, never ellipsized
`sha256:...` placeholders). Phase 2 stores `canonical_json` + `record_hash` per
revision. History reconstruction (ADR 0003) and immutable promotion history
(ADR 0007) depend on this: a later revision learned after `known_at` cannot
alter an earlier reconstruction. Content hashes (`content_hash`,
`canonical_content_hash`) are separate SHA-256 over exact UTF-8 bytes, not
record JCS.

## Open questions

None.
