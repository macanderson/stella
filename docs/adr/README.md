# Architecture Decision Records — Phase 0 (Adaptive Context)

These ADRs capture the baseline decisions for the adaptive-context work in
Stella. ADRs 0001–0009 mostly *record* a decision the 2026-07 planning bundle
already made, rather than making a new one; each grounds its claims in those
source docs and, where relevant, in the current Stella code.

**That bundle has been superseded** and removed from the tree (it remains in
git history). The current specification is
[`../design/adaptive-context.md`](../design/adaptive-context.md), with phases in
[`../design/adaptive-context-plan.md`](../design/adaptive-context-plan.md).
References to the old plan/lifecycle pair in ADRs 0001–0009 are left as written:
they are accurate records of what was decided and why, and rewriting them to
cite a document that did not exist at the time would falsify the record.

The current spec annotates two ADRs whose conclusions did not survive contact
with the shipped code — 0003 (the point-in-time cutoff reaches adjacency only,
one layer below `recall`) and 0006 (the compiled frame is now reached by
extending the step manifest, not by building a parallel aggregate). Read those
notes alongside the ADRs.

ADRs 0002 and 0007 originally FLAGGED open questions for human sign-off; the
repository owner **ratified both on 2026-07-23**, so all Phase 0 ADRs are now
Accepted. The ratified resolutions: `SharingScope` is the 4-value set
(`user, repository, workspace, organization`); `DirectiveEnforcement` is the
2-value set from the 4→2 mapping. The related `Origin`-arity item (ADR 0001) was
spec-verified as the full 5-value set for all families.

ADR 0009 (Phase 1) resolves the seven decisions flagged in issue #483 that
blocked the Phase-1 enum/validator freeze — four resolved by existing
spec/ADRs, three ratified by the owner on 2026-07-24 (including the
`informational → advisory` edge amended into ADR 0007).

ADR 0010 amends ADR 0005: the destination is unchanged, but the route from
big-bang cutover becomes incremental transfer. The owner **ratified it on
2026-07-26** (issue #711), settling its second open question in the same act —
`node`, `edge`, and `embedding` are a derived index and never transfer
authority, so `lineage_id` lands on `memory` and `episode` only. Its first open
question (whether the backfill ever becomes mandatory) is deliberately left
open; nothing before Phase 3 forces it.

| # | Title | Status |
|---|---|---|
| [0001](0001-semantic-taxonomy.md) | Semantic Taxonomy | Accepted (Phase 0) |
| [0002](0002-scope-vs-sharing.md) | Scope vs. Sharing | Accepted — ratified 2026-07-23 (4-value SharingScope) |
| [0003](0003-bitemporal-semantics.md) | Bitemporal Semantics | Accepted (Phase 0) |
| [0004](0004-record-revision-identity.md) | Record Revision Identity | Accepted (Phase 0) |
| [0005](0005-storage-authority.md) | Storage Authority | Accepted (Phase 0) |
| [0006](0006-contextframe-vs-compiledcontextframe.md) | ContextFrame vs. CompiledContextFrame | Accepted (Phase 0) |
| [0007](0007-immutable-promotion-history.md) | Immutable Promotion History | Accepted — ratified 2026-07-23 (enforcement 4→2); amended 2026-07-24 (`informational`→advisory) |
| [0008](0008-markdown-canonical-rules.md) | Markdown Repository Rules Remain Canonical | Accepted (Phase 0) |
| [0009](0009-enum-freeze-resolutions.md) | Enum-Freeze Resolutions (issue #483) | Accepted — ratified 2026-07-24 |
| [0010](0010-incremental-authority-transfer.md) | Incremental Authority Transfer (amends 0005) | Accepted — ratified 2026-07-26 (index tables do not transfer) |
