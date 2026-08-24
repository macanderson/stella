# Experiments store: the normalization decision

Status: decided 2026-08-14 (#3215). The answer is **not yet**, with named
triggers for when it changes. Amended 2026-08-23: identity and provenance
columns landed (see the addendum at the end); the content fields stay
unnormalized exactly as decided here.

## The question

`experiments.py` stores whole experiment documents in one `JSONB` column
(`experiment_results.results`) — deliberately unnormalized, so the schema
cannot silently drop a field a document carries. #3215 asked for a written
answer on whether/when to promote fields (experiment id, status,
created_at, comparability keys) into columns.

## The decision, made against the real stored documents

The store currently holds one canonical document
(`stella-vs-claude-code-fable5-tb21`). Every query the product surface
makes today — the gallery listing (`experiments_payload`) and the
single-document read (`experiment_document`) — is served by a full scan
plus `json_extract`-shaped reads over a table whose row count is the number
of *experiments ever run*, a quantity measured in dozens per year, not per
second. Promoting columns now would buy no measurable read and would cost
the one property the phase-1 design bought: that a document with a field
the schema never anticipated is stored intact.

So: **no normalization yet.** The document stays the record; identity,
status, and comparability keys live inside `results` and are read with
`json_extract`.

## What changes the answer

Promote a field to a column (via an additive-only `migrate()` in the
`bench/telemetry_store/ingest.py:201` style — never a rewrite) when any of
these becomes true:

1. **A query needs an index** — e.g. the gallery filters or sorts by status
   or a comparability key across enough documents that a scan is felt.
   Promote exactly the filtered field, keep the document authoritative, and
   treat the column as a projection (rebuildable from `results`).
2. **Uniqueness needs enforcing** — today "latest row wins" resolves a
   re-stored experiment id (`experiment_document`); if concurrent writers
   ever race on one id, `experiment_id` becomes a column with a unique
   index and an upsert.
3. **Cross-document joins appear** — comparing experiments by
   comparability key as a product feature (not a one-off script) wants a
   `comparability_key` column.

Until one of those is real, a column is a second copy of a fact the
document already states — and two copies of one fact is how stores start
lying.

## Constraints that survive any future migration

- stdlib `sqlite3` only (the package's empty-dependencies charter).
- Additive-only migration; existing rows are never rewritten in place.
- The document column remains authoritative; promoted columns are derived
  and rebuildable.
- The store stays generic: agents, models, datasets are data inside
  documents, never schema concepts.

## Addendum (2026-08-23): identity and provenance columns

The table gained `id INTEGER PRIMARY KEY`, `experiment_id`, `created_at`,
`migrated`, and `migration_source`, applied by a one-time table rebuild in
`experiments.py::_migrate` that copies every row unchanged. Neither cause
is the normalization this document declined:

- **The implicit rowid was a defect, not a design.** "Latest row wins" and
  "insertion order" both leaned on rowids the schema never declared, and
  SQLite may renumber implicit rowids on `VACUUM`. An explicit `id` makes
  the ordering the store already promised durable. `experiment_id` rides
  along as trigger 1's index (the lookup no longer parses every document),
  derived and rebuildable as required above.
- **The durable mirror needs provenance.** `arenabench/mirror.py` copies
  the working set into the benchmarks database on the data instance;
  `migrated`/`migration_source` record, on both sides, which rows moved
  and from which machine. These are row metadata, not document fields —
  the document column stays authoritative for everything it states.

The content fields (title, status, comparability keys) remain inside
`results`, and the triggers above still govern promoting them. The rebuild
is a one-time exception to "never rewritten in place", taken because a
primary key cannot be added by `ALTER TABLE ADD COLUMN`; the copied rows
are byte-identical.
