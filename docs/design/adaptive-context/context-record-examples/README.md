---
id: context-record-examples
title: "Context record examples"
status: living
---

# Context record examples

Worked examples of the TOML context-record surface. For the fields they cover
these are **the schema reference**: [ADR 0012](../adr/0012-context-record-field-schema.md)
ratified that schema on 2026-07-30 and retired this file's former "illustrative,
not ratified" caveat, and `crates/stella-core/src/records/` implements it.

`docs/design/adaptive-context/context-pr.md` remains the canonical specification for the workflow around
records, and none of these files is loaded by the engine — they are examples, not
fixtures. What changed is that the shapes below are now decided rather than
proposed, so a disagreement with one is a disagreement with a ratified ADR.

## The shape

A context record is the smallest quotable unit of agent context. Every record
carries three orthogonal axes:

| Axis            | Answers                                                    | Table                  |
| --------------- | ---------------------------------------------------------- | ---------------------- |
| **Steering**    | how hard does this push behavior, and when is it injected? | `[record.steering]`    |
| **Enforcement** | how is a violation detected, and what happens?             | `[record.enforcement]` |
| **Truth**       | is the claim still accurate, and how would you know?       | `[record.truth]`       |

Separating these matters because the current engine conflates the first two —
enforcement level _is_ authority level today, so a rule cannot be strongly
steering and cheaply checked, or weakly steering and hard-blocked.

## Files

| File                           | Demonstrates                                                                                      |
| ------------------------------ | ------------------------------------------------------------------------------------------------- |
| `01-constraint-decreed.toml`   | Hard constraint, human decree, deny-shaped guard enforcement                                      |
| `02-fact-observed.toml`        | A claim that rots — declarative probe, TTL, `on_expiry`                                           |
| `03-preference-asserted.toml`  | Real steering force, unfalsifiable claim, model-judged rubric                                     |
| `04-procedure-atomic-set.toml` | One sentence → three atomic records joined by typed links                                         |
| `05-imported-proposals.toml`   | Ingest output: proposals, per-record provenance, quarantined executables, three-valued refutation |
| `06-lineage-supersession.toml` | Revision identity without allocated version numbers                                               |
| `07-agent-projection.md`       | What the model actually receives (~16% of the record), and how a handle closes the feedback loop   |
| `08-witness.toml`              | Enforceable context ships with a test — bidirectional guard tables, and the mutation check         |
| `09-effect-witness.toml`       | Did the record actually help? Scored from the diff and from the tool-call log                      |

## Rules encoded in these examples

**Atomicity is functional, not stylistic.** A record is atomic if it can receive
exactly one refutation verdict. If you can construct a world where half the claim
holds and half does not, it is compound and must be split. `04` shows the split;
`05` shows the validator rejecting a compound extraction.

**Identity is derived, never allocated.** `lineage_id` is a stable slug;
`record_id` is derived from the record's own content hash; `record_hash` is the
JCS hash of the canonical record with `record_hash` itself omitted from the
preimage. There is no monotonic `version` counter — allocating one would require
reading the store before extracting, which breaks replay-idempotence and with it
the property that re-ingesting unchanged content is a no-op.

A hand-authored record omits `record_id` and `record_hash`; the loader stamps
them on first validation. `03` is written that way on purpose.

**Provenance lives on the record, not on the file.** One source document yields
N records, records get hand-edited after ingest, and records regroup by topic.
A file-level origin block would weld file boundaries to source-document
boundaries forever. `[defaults.provenance]` keeps the ergonomics without the
coupling — any record may override.

**Records describe; they never execute.** Enforcement is deny-shaped
(`guard_tool`, `guard_deny_path`, `guard_deny_command`) — it can block a tool
call, never cause one. Truth probes use a closed, declarative vocabulary rather
than arbitrary shell. See "Probe kinds" below.

**Two channels, chosen by `force`.** `must` and `should` records are **always**
injected and live in the byte-stable system prefix, so they ride the prompt cache
(`crates/stella-cli/src/agent.rs:698`). `may` and `info` records are selected by
relevance and ride the volatile block alongside memories
(`inject_recall_block`).

A binding rule should always bind, so it cannot be relevance-gated without
breaking the cache every turn. A fact is only worth tokens when it applies, and
that is exactly how memories already behave — so facts belong in the memory
channel.

There is no `inject` field. The channel is derived from `force`, and an archived
record is not loaded at all, so `status` covers the rest.

This is why `applies_to` has one meaning and two consumers. It answers "when does
this record apply." For a volatile record that drives **selection**; for a cached
record it drives **scoring** — whether the record had a chance to matter this turn
(see `09-effect-witness.toml`).

**Memories live in the database. Context records live in files.** One place
each, and no record is stored twice.

A memory is fetched per turn by relevance and is not cached. A context record is
a file: the project's git tree for `repository` and `organization` scope,
`~/.stella/rules/` for `personal` scope — which the loader already reads
(`rule_search_dirs`, `crates/stella-core/src/rules.rs:345`). `sharing_scope` chooses
*which* file location, not whether it is a file at all.

Promotion is how a memory becomes a context record.

An earlier draft of this file said personal records stay in the database. That
was wrong: it would have meant one record type stored two different ways, and a
personal record that could never be cached.

## Vocabularies

Values marked **frozen** are ratified in ADR 0009 and cannot be extended without
a superseding ADR.

- `origin` — **frozen**: `user`, `system`, `observed`, `inferred`, `imported`
- `status` — **frozen**: `active`, `retracted`, `archived`
  (there is deliberately no `superseded` — a replaced record is `archived`,
  and its successor points back at it with a `derived_from` link; the forward
  `supersedes` pointer lives in the lifecycle ledger's append, never in the
  immutable file. `retracted` means the claim was _wrong_, `archived` means
  it was _replaced_)
- `kind` — `memory`, `fact`, `rule`, `preference`, `constraint`, `procedure`
- `sharing_scope` — `personal`, `repository`, `organization`
- `steering.force` — `must`, `should`, `may`, `info`
- (there is no `steering.inject` — the channel follows from `force`, see below)
- `enforcement.mode` — `hard` (block), `soft` (warn), `none` (advisory)
- `truth.basis` — `decree`, `measured`, `derived`, `asserted`
- `link.relation` — `derived_from`, `refines`, `requires`, `supports`, `contradicts`

### Why `measured` and not `observed`

`origin` and `truth.basis` answer different questions — where a record came from
versus why it is believed true. A record can be `origin = "imported"` (extracted
from CLAUDE.md) and `basis = "decree"` (true because a human said so). But
`observed` appears in the frozen `origin` set, so the truth axis uses `measured`
to keep the two axes unambiguous when both appear on one record.

### Probe kinds

A probe answers "does this claim still hold?" without executing arbitrary code:

| `kind`                        | Checks                            | Gated?  |
| ----------------------------- | --------------------------------- | ------- |
| `path_exists` / `path_absent` | a path in the repo                | no      |
| `file_contains`               | a pattern in a tracked file       | no      |
| `manual`                      | a human re-verifies on a schedule | no      |
| `command_succeeds`            | exit status of a command          | **yes** |
| `http_ok`                     | an HTTP endpoint responds         | **yes** |

Gated probes are honored **only** when `basis = "decree"` and a human
`verified_by` is recorded, and **never** on a record whose `origin` is
`imported` or `inferred`. Ingest is the headline path here: a model extracting
records from arbitrary markdown must not be able to mint a record that runs a
command or fetches a URL. An `http_ok` probe pointed at an attacker-chosen host
is an exfiltration channel — anything interesting fits in a query string. `05`
shows a would-be shell probe quarantined rather than honored.

## Refutation is evidence, not a boolean

A refutation attaches as a typed link with relation `contradicts` and a
timestamp, so "refuted Tuesday, true again Friday" is representable on the
existing bitemporal model. Verdicts are three-valued:

- `supported` — the probe ran and the claim held
- `refuted` — the probe ran and the claim did not hold
- `unfalsifiable` — no probe could judge this claim

`unfalsifiable` must stay visible in review surfaces and must never be folded
into "passed." A refuter that reports OK for claims it never checked launders
unvalidated content with a validated stamp — the same failure shape as a guard
script that prints OK while silently skipping most of its inputs.

## What is deliberately absent

**No `parent_id` / hierarchy.** Records co-derived from one sentence are
siblings, not parent and child. Hierarchy would be a second relationship
mechanism alongside typed links, forces a single parent where a DAG is needed,
and leaks into selection and retirement semantics ("does retiring a parent
retire its children?"). Co-derivation is answered by shared provenance
(`source_uri` + `source_lines` + `ingest_run_id`) for free; genuine
relationships get typed links. Where hierarchy is real it is _derived_ — general
versus specific falls out of `applies_to.paths` containment, which cannot go
stale or contradict itself.

**No `uri` field.** It is fully derivable from `set_id` + `lineage_id` +
revision, so storing it creates state that can disagree with its own components.
Compute it at read time.

**No `title` or `summary` — one text field, `statement`.** Atomicity makes both
redundant: a record that can receive exactly one refutation verdict is already
one sentence, and there is nothing in one sentence to summarize. Across these
examples `title` + `summary` cost 987 bytes against 1152 bytes of `statement` —
an 86% overhead on the text payload for no added meaning.

The cost is not only tokens. A summary that drops a qualifier ("in CI", "for new
code") changes what the agent does, so two fields that can disagree about what a
rule says is a correctness hazard, not just duplication. And `docs/design/adaptive-context/context-pr.md`
§6.2 already excludes presentational `name` / `description` from the canonical
record — by that logic they are not part of the record's meaning at all.

A display label is a read-time concern: list views can truncate `statement`, and
the stable handle across revisions is `lineage_id`, which is already present.

**No `budget_tokens`.** Same principle as `uri` — measure the rendered
`statement` rather than storing an author-declared number that drifts from it.
If injection cost needs managing, the lever is selecting fewer records, not
compressing each one.

**No float confidence.** `confidence` is an integer 0–100.

## Open questions — resolved by ADR 0012

All four, plus the handle-collision question in `07-agent-projection.md`, are
answered in [ADR 0012](../adr/0012-context-record-field-schema.md) and implemented
in `crates/stella-core/src/records/`:

1. **`set_id` in citations** → nothing cites it. The citation key is the
   `^handle`, derived from `lineage_id`; `set_id` is a grouping namespace and the
   last-resort tiebreaker, so a fork or rename invalidates no citation
   (Decision 3).
2. **`applies_to.tasks`** → closed vocabulary, and an unrecognized name is a
   reported warning rather than a load failure. A typo that matches nothing and
   reports nothing is indistinguishable from a task that never came up
   (Decision 5).
3. **`review_every` vs `review_after`** → both, in their own homes.
   `review_every` is the record surface's field; `review_after` stays on the
   markdown/lifecycle path and is not migrated (Decision 6).
4. **`tags`** → kept, for human browsing only, and never read by a matcher. A
   field that is *sometimes* semantic is one nobody can predict (Decision 7).
5. **Handle collisions** (`07`) → the renderer lengthens only the handles that
   collide, never spends lineage that cannot disambiguate, and tiebreaks on
   content so handles do not depend on load order (Decision 4).
