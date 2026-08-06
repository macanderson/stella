---
id: adr/0012-context-record-field-schema
title: "ADR 0012: The context-record field schema, and records-live-in-files"
status: implemented
---

# ADR 0012: The context-record field schema, and records-live-in-files

- Status: **Accepted** — ratified by repository owner 2026-07-30 (was: Proposed).
  Every substantive question below was resolved and implemented before
  ratification; the signature is on the surface those answers already assume.
  Ratified together with [ADR 0011](0011-context-records-are-toml.md), which
  deferred this schema and named ratifying one without the other as the state to
  avoid.
- Date: 2026-07-29 (ratified 2026-07-30)
- Deciders: repository owner (ratified 2026-07-30)
- Follows: [ADR 0011](0011-context-records-are-toml.md), which settled the
  *format* and explicitly deferred "which fields a context record carries, and
  what their vocabularies are" to a separate decision.
- Constrained by: [ADR 0009](0009-enum-freeze-resolutions.md) (enum freeze),
  [ADR 0002](0002-scope-vs-sharing.md) (scope versus sharing),
  [ADR 0004](0004-record-revision-identity.md) (canonical hash).

## Context

ADR 0011 chose TOML and stopped there, on purpose. But `docs/context-record-examples/`
has since been written against a concrete field schema, `crates/stella-core/src/ingest/record.rs`
implements it as typed serde bodies, and epic #897 built a loader, a renderer, a
truth sweep, and a guard bridge on top of it. Code now depends on answers the
schema never formally gave, and the examples' own README carries four open
questions plus a fifth in `07-agent-projection.md`.

Deciding late is still deciding — it just means the decision is recorded in
whichever module happened to need it first. This ADR writes the answers down in
one place.

## Decision 1 — Records live in files, and `sharing_scope` selects which location

This is the substrate rule ADR 0011 flagged as unratified in its own open
question, and it is the sentence the rest of the design leans on:

> **Memories live in the database. Context records live in files.** A record's
> `sharing_scope` selects *which* file location, never whether it is a file.

| `sharing_scope` | Location | Git-tracked | May enter a Context PR |
| --- | --- | --- | --- |
| `personal` | `~/.stella/rules/` | no | **never** (§10) |
| `repository` | `<repo>/.stella/rules/` | yes | yes |
| `organization` | `<repo>/.stella/rules/` of a repository the organization owns | yes | yes |

`organization` deliberately has no third location. An organization-scoped record
is published *through* a repository the organization owns, which is the same
substrate with a different audience — inventing an org-wide file location would
require a central service, and the solo path must work without one (§18).

Provider-hosted **workspace** publication is not on this table because it is not
a Context PR at all: the provider-hosted record is authoritative for workspace
scope and is never materialized into a rule file unless a separate repository
publication is approved (`docs/spec/adaptive-context/context-pr.md` §2). That channel remains deferred.

Implemented as `stella_core::records::publication_dir`.

## Decision 2 — The surface keeps `personal`, and maps to the ledger's `user`

The examples spell the audience `personal` / `repository` / `organization`. The
ratified `SharingScope` enum (ADR 0002, frozen by ADR 0009) is `user` /
`repository` / `workspace` / `organization`. Both spellings stay, with one
mapping:

- **On the file surface:** `personal`, because the file lives in the user's home
  directory and "personal" is what a person calls that. `user` reads like an
  account identifier in a context where the neighbouring value is a repository.
- **In the ledger:** `user`. A proposal promoted into the lifecycle ledger maps
  `personal → user`.
- `workspace` has **no surface spelling**, because workspace publication is a
  separate channel that never produces a file (Decision 1).

The alternative — renaming the surface value to `user` — was rejected because
ADR 0009 froze the *ledger* vocabulary, not the file surface, and the file
surface is what humans hand-edit. Two vocabularies with one documented mapping
beats one vocabulary that reads wrong in one of its two homes.

Implemented as `stella_core::ingest::record::SharingScope`, with the mapping
documented on the type.

## Decision 3 — `set_id` is a citation *namespace*, not a citation

**Question:** is `set_id` stable enough to appear in citations? It is a repo slug,
and a fork or rename would invalidate every citation embedding it.

**Answer: nothing cites `set_id`.** The citation key is the `^handle`, derived
from the `lineage_id` tail (Decision 4). `set_id` is the namespace a record set
declares for itself, used for grouping and as the *last* tiebreaker when two
lineages collide — so a fork or rename changes how records are grouped and
changes no citation.

This is why the handle is derived from the lineage rather than from the set: a
lineage is authored, and a set id is inferred from a git remote.

## Decision 4 — `^handle` is the lineage tail, widened only where it collides

**Question** (`07-agent-projection.md`): two record sets in one workspace can both
end a lineage in `pkg-manager`, and the agent cannot disambiguate.

**Answer:** the renderer detects collisions and lengthens only the handles that
need it. `ctx.acme.web.pkg-manager` is `^pkg-manager` when it is alone, and
`^web-pkg-manager` only when `ctx.acme.api.pkg-manager` is loaded beside it — the
record with no collision keeps its short name.

Two properties this has to have, and does:

- **Lineage is never spent where it cannot disambiguate.** Two records sharing an
  entire lineage widen to nothing and take a content-derived suffix instead;
  widening them to `^acme-web-pkg-manager` first would have cost every segment
  and still needed the suffix.
- **Handles do not depend on load order.** They appear in the byte-stable cached
  prompt prefix, so a handle that varied with directory iteration order would
  break the prompt cache on an unrelated rename. The final tiebreak is the
  content-derived `record_id`.

Implemented as `stella_core::records::handle::assign_handles`.

## Decision 5 — `applies_to.tasks` is a closed vocabulary with a loud warning

**Question:** open vocabulary, where a misspelled task name skews scoring because
the record looks like it never had a chance to matter.

**Answer: closed, and an unrecognized name is a reported warning rather than a
load failure.** The ratified list:

```text
build  ci  deploy  docs  install  lint  migrate  refactor  release  review  run  test
```

An open vocabulary means `intall` produces a record that matches nothing and
reports nothing, which on screen is indistinguishable from a record whose task
simply never came up. Closing it makes the typo visible. Refusing the record
outright would be worse — the statement is still policy, and a `must` record
injects regardless of task matching — so the record loads and carries a
`RecordFinding::UnknownTask`.

Adding a task name is an edit to `stella_core::records::KNOWN_TASKS` and a line
in this ADR, deliberately: a vocabulary that grows without a decision is an open
vocabulary with extra steps.

## Decision 6 — `review_every` is the record-surface field; `review_after` stays

**Question:** `review_every` (recurring) versus the shipped `review_after`
(one-shot).

**Answer:** both, in their own homes. `review_every` is the record surface's
field, because a claim about the world needs re-checking on a cadence, not once.
`review_after` remains the markdown/lifecycle field and is not migrated — the
migration would touch a shipped field to gain nothing the new surface does not
already have, and ADR 0011's whole reversibility argument rests on not disturbing
the legacy path.

`ttl` and `review_every` interact as follows, and `review_every` wins when both
are set, because a recurring cadence is a stronger statement of intent than a
deadline:

| Field | Means | On lapse |
| --- | --- | --- |
| `review_every` | re-probe on this cadence | probe runs; verdict decides |
| `ttl` | this claim's shelf life | `on_expiry`, default demote-to-stale |

Implemented in `stella_core::records::sweep`.

## Decision 7 — `tags` is kept, for human browsing only

**Question:** `tags` overlaps `applies_to`; cut it if nothing consumes it.

**Answer: keep, and never match on it.** It is eight bytes per record and it is
the field a person greps when they do not know what they are looking for. The
constraint is the useful part: `tags` must never influence selection, scoring, or
enforcement, because a field that is *sometimes* semantic is one nobody can
predict. `applies_to` is the matching surface; `tags` is the index.

If a matcher ever reads `tags`, that is the signal to cut it rather than to
document it.

## Decision 8 — a statement is one sentence, and that is enforceable

Not previously an open question, but the loader now depends on it. A record is
atomic if it can receive exactly one refutation verdict, so:

- the statement carries a single independently-falsifiable assertion (the ingest
  gate's compound test, re-applied on publish);
- it does not exceed 600 characters or contain a newline. Past that the field is
  carrying something pasted rather than written, which is both an atomicity
  problem and a privacy one (§10 forbids raw prompt and tool text in a
  Git-tracked record).

Legacy markdown rule bodies are explicitly **exempt**: they have never had this
constraint and applying it retroactively would refuse rules that have shipped for
months.

## Consequences

- The examples in `docs/context-record-examples/` are the schema reference, and
  their README's "illustrative, not ratified" caveat is retired for the fields
  above.
- `docs/spec/adaptive-context/context-pr.md` §6.1's nested `scope:` example is corrected to a shape the
  loader accepts — it is the example ADR 0011 cited as not parsing, and now that
  nesting is refused loudly it would fail to load rather than load wrong.
- A new field still needs a decision. This ADR settles the fields that exist; it
  does not open a process for adding more without one.

## What this deliberately does not decide

- **Whether `.claude/rules/` remains a live read path.** ADR 0011 left this open
  and it is still a behavioural question, not a schema one.
- **Owner-routing policy.** Still deferred, as in ADR 0008.
- **Workspace publication.** Still deferred (Decision 1).
