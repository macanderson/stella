# ADR 0008: Markdown Repository Rules Remain Canonical

- Status: Accepted (Phase 0)
- Date: 2026-07-23
- Deciders: (Phase 0 baseline)

## Context

Repository rules are human-governed policy. The plan's Phase 8 model treats Git
as the authoritative policy ledger and the database as a derived read-only
mirror. The live code already seeds this: `stella memory promote` writes rules
as Markdown to `.stella/rules/*.md` after citation thresholds. `context-prs-spec.md`
proposes a `.stella/rules/*.yaml` surface instead — a *surface* conflict, since
its thesis (Git canonical, graph derived) is the same model.

This ADR RECORDS the format decision (roadmap §2 reconciliation table) and
REJECTS the YAML surface. Its one open item is a deferral, not a confirmation
gate — so this ADR is Accepted, not Proposed.

## Decision

Repository rules remain **Markdown** (`.stella/rules/*.md`) as the canonical,
human-governed authority. The database is a **derived, read-only mirror** of
that Markdown (Phase 8 model). Do **not** introduce YAML as a second authority;
`context-prs-spec.md`'s `.yaml` rule surface is rejected. A single source of
truth avoids two governance ledgers drifting.

Migration imports legacy `.stella/rules/*.md` (including guard frontmatter and
aliases) as read-only mirror rows; it never promotes the DB above Markdown.

## Consequences

Phase 8 maps rule frontmatter to/from records and keeps the DB mirror in sync on
read, never authoritative. The existing `memory promote` Markdown seam is reused,
not replaced. "Context PR" remains UX over proposal → publish (see ADR 0007), so
no YAML tooling is needed. Confirming Markdown-canonical is itself a Phase 8
human-decision gate per the roadmap.

## Open questions

Owner-routing policy (which reviewers/teams gate which rule kinds and paths,
per `context-prs-spec.md`'s ownership map) is **deferred to Phase 8**. This is a
deferral, not a blocking confirmation gate — it does not affect the Markdown-
canonical decision above.
