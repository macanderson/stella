# Context record examples

Worked examples of the TOML context-record surface. These are **illustrative, not
normative** — `docs/context-pr.md` remains the canonical specification, and
nothing here is loaded by the engine. They exist so the schema can be argued
against concretely before it is implemented.

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

**Substrate follows `sharing_scope`.** `personal` records live only in
`.stella/private/context.db` and are never materialized to a file.
`repository` and `organization` records are git-tracked TOML. The record type is
universal; scope alone decides where the bytes land.

## Vocabularies

Values marked **frozen** are ratified in ADR 0009 and cannot be extended without
a superseding ADR.

- `origin` — **frozen**: `user`, `system`, `observed`, `inferred`, `imported`
- `status` — **frozen**: `active`, `retracted`, `archived`
  (there is deliberately no `superseded` — a replaced record is `archived` and
  carries `superseded_by`; `retracted` means the claim was _wrong_, `archived`
  means it was _replaced_)
- `kind` — `memory`, `fact`, `rule`, `preference`, `constraint`, `procedure`
- `sharing_scope` — `personal`, `repository`, `organization`
- `steering.force` — `must`, `should`, `may`, `info`
- `steering.inject` — `always`, `on-match`, `on-demand`
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
rule says is a correctness hazard, not just duplication. And `docs/context-pr.md`
§6.2 already excludes presentational `name` / `description` from the canonical
record — by that logic they are not part of the record's meaning at all.

A display label is a read-time concern: list views can truncate `statement`, and
the stable handle across revisions is `lineage_id`, which is already present.

**No `budget_tokens`.** Same principle as `uri` — measure the rendered
`statement` rather than storing an author-declared number that drifts from it.
If injection cost needs managing, the lever is selecting fewer records, not
compressing each one.

**No float confidence.** `confidence` is an integer 0–100.

## Open questions

1. Is `set_id` stable enough to appear in citations? It is a repo slug today,
   and forks or renames would invalidate every citation that embeds it.
2. `applies_to.tasks` is an open vocabulary. An unknown or misspelled task name
   means the record silently never matches — the failure mode this design
   otherwise works hard to avoid. Ratify the task vocabulary, or warn loudly on
   unrecognized names.
3. `review_every` (recurring) versus the existing `review_after` (one-shot).
   The recurring form is better, but it is a change to a shipped field.
4. `tags` overlaps `applies_to`. It is cheap (8 bytes per record here) and aimed
   at human browsing rather than matching, but if nothing consumes it, it is the
   next field to cut.
