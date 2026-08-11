> **Revision note:** this issue originally proposed auto-loading AGENTS.md/CLAUDE.md into the volatile recall block. That design was rejected: auto-reading a whole instruction file every turn re-imports the token bloat that Stella's context-record recall exists to eliminate — most of any CLAUDE.md is irrelevant to any single turn. `stella ingest` remains the only entry path. What follows replaces it: close the gaps that make ingest-only feel risky (missed bindings, silent staleness) so the ingest-first model is strictly superior to file auto-load.

## The gap

`stella ingest` converts AGENTS.md/CLAUDE.md into context records (`crates/stella-cli/src/ingest_cmd.rs`), but three things are missing:

1. **No promotion guarantee** — an ingest can produce hundreds of records, all competing in relevance-ranked recall. A binding constraint ("never push to main") can lose a ranking contest to chattier records and simply not ship on the turn where it mattered.
2. **No staleness detection** — the source file drifts after ingest and nothing notices. The agent operates on instructions the maintainers have since rewritten.
3. **No re-ingest semantics** — records are bitemporal and never edited/invalidated in place, so "update after the file changed" needs a defined retire-and-add pipeline, not an overwrite.

## Design

### Ingest provenance
Each ingest run records a lineage: `(source_path, source_blob_hash, commit_sha?, ingested_at, run_id)` + the record IDs produced. Use git's own blob hash (free from the index for clean tracked files; `git hash-object` for dirty; content hash for non-git files).

### Staleness detection
At session start, async, off the turn path: compare current blob hash vs stored per lineage.
- Changed → **non-blocking inbox item + notification**: "CLAUDE.md changed since ingest (a3f21c → 9be04d). Run `stella ingest --refresh CLAUDE.md`." Never injected into the model prompt.
- Deleted → inbox item offering lineage retirement.
- Never auto-mutates; re-ingest stays a deliberate act.

### Re-ingest: bitemporal diff, retire-and-add only
Parse the new file into candidates; match against the lineage's live records by stable identity `(section_anchor, normalized_content_hash)`:
- identical → **keep existing record untouched** (preserves age, appraisal history, recall stats — the churn guard: a one-line edit retires one record, not hundreds)
- changed at same anchor → add new, retire old with a supersession link (`superseded_by`) so live recall can never surface both; as-of queries still see the old record
- anchor removed → retire
- new anchor → add

Retirement closes valid time; transaction-time rows are permanent. No record is ever edited.

### Auto-promotion tiers (the "no gaps without bloat" mechanism)
Classify each record at ingest (classifier-assigned, user-overridable):
- **Pinned** — standing constraints binding on every turn. Auto-promoted into the byte-stable prefix region alongside workspace memories. Cache-safe *by construction*: pinned content changes only at explicit re-ingest, never mid-session — the property a mutable auto-read file can never have. Budget-capped; overflow is a named ingest-time diagnostic, never silent truncation.
- **Scoped** — conditional rules indexed by path glob / tool / domain, surfaced in the volatile recall block only when the turn's activity trips the trigger.
- **Retrieved** — everything else, ordinary relevance-ranked recall.

Coverage signal: if a scoped record's trigger matched but the record was budget-evicted, log it — systematic gaps become observable instead of silent.

### Why this beats CLAUDE.md
Per-turn cost scales with relevance, not file size; the binding subset is guaranteed present; staleness is detected mechanically via git; instruction history is auditable as-of any date; and dead instructions are measurable by the existing appraisal machinery — a paragraph in a monolithic CLAUDE.md is invisible forever.
