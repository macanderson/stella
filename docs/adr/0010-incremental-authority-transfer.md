---
id: 0010-incremental-authority-transfer
title: "ADR 0010: Incremental Authority Transfer"
status: living
---

# ADR 0010: Incremental Authority Transfer

- Status: **Accepted** — ratified by repository owner 2026-07-26 (was: Proposed).
  Amends [ADR 0005](0005-storage-authority.md).
- Date: 2026-07-25 (ratified 2026-07-26)
- Deciders: repository owner (ratified 2026-07-26)
- Tracking: [issue #711](https://github.com/macanderson/stella/issues/711)
  (part of Epic #469)

## Context

ADR 0005 accepted an authority model *and* a delivery shape, in one decision:

> A new immutable `context_records` table becomes the canonical **local**
> authority (Phase 2). Today's `node`/`edge`/`memory`/`episode` tables become
> **transactionally-rebuilt projections / compatibility views** derived from it.

The model is right. The delivery shape is a big-bang cutover of a live,
user-owned database, and every plan document that touches it says so in the
same words — "the single riskiest step in the roadmap," gated on a human
confirming the design against a copy of real user data, under a hard constraint
to "not lose a memory or break the live `recall` path."

Three facts learned since ADR 0005 was written argue that this cutover should
not be the next thing built.

**1. The retrieval plane it would migrate is not currently sound.**
`ContextQuery::as_of` reaches exactly one of the five signals that feed
`recall` — it is passed to `neighbors` and nowhere else, so a point-in-time
recall returns today's node content wearing yesterday's edges. Candidate
generation is unbounded: neither `live_nodes` nor `vectors_for_fingerprint`
carries a `LIMIT`, recency ranks the whole corpus, MMR runs over all of it, and
a full `ContextFrame` is minted — cloning every node's content — before any
budget applies. `node.superseded_at` is in the v1 DDL and is never written;
there is no supersede or tombstone path in the plane that owns the data.
Migrating an unsound plane to a new authority model preserves the unsoundness
and makes it harder to reach.

**2. The migration's payoff does not depend on the cutover.**
What "accountable" requires (ADR 0006) is that every item in a compiled frame
carry a stable identity, a content hash, and provenance, and that the frame
itself be deterministic and inspectable. None of that requires the *legacy*
rows to become projections. It requires that records the frame cites be
addressable and hashed — which new records can be from birth.

**3. Most lifecycle records have no legacy counterpart at all.**
Observations, proposals, promotion events, context-uses, and compiled frames
are new. They can be born immutable, JCS-hashed, and append-only in
`context_records` with no migration whatsoever. Only memories, episodes, and
directive-shaped rules have legacy rows — and those are exactly the rows the
hard constraint protects.

Splitting the model from the cutover therefore costs nothing and removes the
roadmap's largest single risk from its critical path.

## Decision

**Keep ADR 0005's destination. Replace its route.**

`context_records` becomes the canonical local authority **for the records it
owns**, and the set of records it owns grows monotonically over time. Legacy
`node`/`edge`/`memory`/`episode` rows remain authoritative for rows that have
not yet been transferred. The boundary is explicit, queryable, and shrinks; it
is never crossed by a single big-bang transaction.

Concretely:

1. **New lifecycle records are born canonical.** Every record kind with no
   legacy counterpart is written only to `context_records`, immutable and
   JCS-hashed per ADR 0004, from the first phase that emits it. No migration
   step exists for them because they were never anywhere else.

2. **Legacy rows transfer on write, not on migrate.** When a memory, episode,
   or rule is next created, edited, superseded, or promoted, it is written to
   `context_records` and *projected* into its legacy table in the same
   transaction. Rows never touched are never rewritten. This is a strangler
   pattern, not a cutover.

3. **Ownership is explicit, not inferred.** A legacy row carries a nullable
   `lineage_id` referencing `context_records`. Populated means the record layer
   owns it and the legacy row is a projection. Null means the legacy row is
   still authoritative. Any reader can tell which regime a row is in, and the
   invariant "`lineage_id` non-null implies the projection matches its record"
   is checkable at any time against real data — not only immediately after a
   migration.

4. **A backfill exists but is optional, resumable, and observable.** Transfer
   of untouched rows is a separately-invocable, interruptible, idempotent
   command reporting how many rows remain in each regime — never a startup
   side effect and never a precondition for a release. It may run for weeks of
   wall-clock across many sessions.

5. **The one-transaction rule survives and strengthens.** Where a record and
   its projection are both written, they commit in one transaction, and a
   projection-rebuild reproduces the projection byte-for-byte from the record.
   ADR 0005's guarantee is unchanged — it simply applies per record rather than
   to a whole-database cutover.

6. **The retrieval index does not transfer authority.** `node`, `edge`, and
   `embedding` are a derived retrieval index over content, not a record store —
   `embedding` is already keyed by `(content_hash, fingerprint)` and rebuildable
   from content. Only `memory` and `episode` are candidates for authority
   transfer, so `lineage_id` lands on **two** tables, not five. (Directive-shaped
   rules are the third record kind that transfers, but they are Markdown-canonical
   per [ADR 0008](0008-markdown-canonical-rules.md) and are not a `context.db`
   table.) A record's content is authoritative; its index entries are disposable
   and may be dropped and rebuilt without consulting `context_records`.
   Ratified 2026-07-26 (was open question 2).

ADR 0005's hard constraints are inherited verbatim and are *easier* to hold
under this route: no memory can be lost by a migration that does not rewrite
it, and the live `recall` path is never cut over.

## Consequences

**Gained.** The riskiest step leaves the critical path. Its human-decision gate
("confirm the model on a copy of a real `context.db`") is no longer a
release-blocking ceremony, because the model is now proven continuously by rows
that transfer during normal use. Accountability (ADR 0006) becomes reachable
without touching a single legacy row. Phases can be ordered by user-visible
value rather than by migration dependency.

**Paid.** Two regimes coexist for as long as the backfill is incomplete —
possibly indefinitely. That is the real cost of this ADR and must not be
hand-waved:

- Every reader over memories/episodes must be correct for both regimes. The
  mitigation is that readers go through the projection, which is identical in
  both regimes by construction — `lineage_id` changes who *writes* the row, not
  its shape.
- "Reconstruct what was believed at time T" is only complete for transferred
  records. Until backfill finishes, historical reconstruction over legacy rows
  is best-effort and must say so rather than implying completeness.
- The temptation to leave the backfill permanently unfinished is real. The
  remaining-row count is therefore a reported number, so the debt is visible
  rather than silent.

**Unchanged.** ADRs 0001, 0002, 0003, 0004, 0006, 0007, 0008, 0009 all stand.
This ADR touches only the route by which `context_records` becomes canonical,
not the taxonomy, the temporal axes, the hashing scheme, the frame model, the
promotion history, or the Markdown-canonical rule.

## Open questions

1. **Does the backfill ever become mandatory?** Open. Recommendation: no — but a
   release that depends on complete historical reconstruction must state that
   dependency and gate on the remaining-row count reaching zero. Nothing before
   Phase 3 forces this, so it stays open.
2. ~~**Do `node`/`edge` transfer at all?**~~ **Resolved 2026-07-26** ([#711](https://github.com/macanderson/stella/issues/711)
   decision 2): no. Folded into the decision above as point 6.
