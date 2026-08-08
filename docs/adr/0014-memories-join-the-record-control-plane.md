---
id: adr/0014-memories-join-the-record-control-plane
title: "ADR 0014: Memories join the context-record control plane"
status: proposed
---

# ADR 0014: Memories join the context-record control plane

- Status: **Proposed** — awaiting ratification by the repository owner.
- Date: 2026-08-08
- Deciders: repository owner (pending)
- Tracking: [issue #2283](https://github.com/macanderson/stella/issues/2283)
  (epic); resolves the memory half of
  [#2284](https://github.com/macanderson/stella/issues/2284)
- Scope note: outside the Phase 0 series, like ADR 0013. It decides governance
  of an existing surface, not a new feature.

## Context

The 2026-08-08 steering census (#2283) found one durable steering surface
still outside the record control plane: **workspace memories**.
`.stella/memories/*.md` files are written by the `save_memory` tool
(`crates/stella-tools/src/memory.rs`) with no review step, no lifecycle, no
lineage, and no provenance; they are enumerable only through the
citation-centric `stella memory list`; their suppression mechanism
(`ContextSurface::WorkspaceMemory` tombstones,
`crates/stella-store/src/forget.rs`) is disjoint from record retirement
(`PromotionAction::Retired`); and nothing ever re-checks whether a memory is
still true. Rules already converged: markdown and TOML rules alike load
through one registry, and every code path that mints a new rule publishes a
stamped TOML record (#2286).

The design question, asked from the agent's chair — *what context would help
you if you found yourself in that situation?* — has four answers, and the
split control plane fails each one:

1. **One enumerable answer to "what steers me right now."** Today that
   answer is spread across `stella context list` (records) and
   `stella memory list` (memories), with different vocabularies.
2. **The *why* behind each piece.** A record carries origin, provenance, and
   evidence links, so an agent can weight it — or challenge it. A memory is
   bare prose: nothing says where it came from, what it earned, or when it
   was last true.
3. **Delivery matched to relevance.** Records declare a channel
   (`steering.force`); memories are implicitly always-on prefix content.
   That is the *right* channel for them — but it is an accident of code, not
   a declared, reviewable property.
4. **One way to correct the record.** Retiring a stale directive and
   forgetting a stale memory are today two mechanisms with two audit trails
   (`docs/spec/adaptive-context/adaptive-context.md` §5.7 promises one).

ADR 0011 drew the format line — "if the prose is a **field**, TOML; if the
prose is the **document**, Markdown" — and a memory's prose is the document.
So this is **not** a file-format migration. The unification is of
*governance*, not storage.

## Decision 1 — A workspace memory is a context record

A workspace memory becomes a context record of kind `memory` (the taxonomy
already reserves `ContextRecordKind::Memory` and `MemoryKind`). Its content
representation stays a Markdown document at `.stella/memories/<slug>.md`, per
ADR 0011's line. Its typed fields — origin, status, provenance, review
cadence — live in the lifecycle ledger, content-addressed to the file body.
**No frontmatter record grows back inside the `.md` file**: an eleven-field
header is exactly the shape ADR 0011 retired, and memories do not get to
re-create it.

## Decision 2 — `save_memory` writes a record

The `save_memory` tool keeps writing the Markdown file and, in the same
operation, registers the record (origin `inferred` when the model authored
the lesson; `user` when the user dictated it) in the lifecycle ledger. A
failed registration is loud, not best-effort silent: an unregistered memory
is invisible to the control plane, which is the defect this ADR exists to
close. The tool's write-only-mid-session contract (new memories take effect
next session) is unchanged.

## Decision 3 — One enumeration, one lifecycle

`stella context list` shows memory records beside directives, and record
retirement applies to them. `stella memory forget` remains as UX and becomes
sugar over the same retirement lifecycle — one audit trail, the ledger —
resolving #2284's fork for the memory surface. The tombstone's second job,
suppressing the reflection loop from re-learning a forgotten lesson, is
preserved as an effect of retirement rather than a parallel mechanism.

## Decision 4 — The channel is declared, and it is the cached prefix

Memory records render into the byte-stable cached prefix exactly as today:
loaded once per session, sorted by filename, budget-capped. The spec's §5.1
byte-stability invariant is an explicit acceptance criterion for every
implementing PR — prompt-cache goldens must stay green. What changes is only
that the channel becomes a declared property of the record instead of an
accident of `agent::prompt`.

## What this deliberately does not decide

- **Episodic memory.** `context.db`'s mined episodes/facts recall path is
  volatile-channel context, already governed by ADR 0010's incremental
  authority transfer. Untouched here.
- **Skills.** Documents by ADR 0011's line; their governance is a separate
  decision.
- **Team sharing of memories.** Workspace memories stay repository-local;
  sharing scope beyond that is deferred with workspace publication.

## Consequences

- Implementing PRs land per decision, each with a witness: a memory saved by
  `save_memory` appears in `stella context list` (D2/D3 witness); a
  forgotten memory shows as retired on the ledger and stops rendering next
  session (D3); the prompt-prefix goldens byte-match before and after (D4).
- `stella memory list` keeps its citation view; it stops being the only
  window into memories.
- The migration is incremental in ADR 0010's sense: existing memory files
  gain ledger rows lazily (on first load or via `stella context adopt`);
  no big-bang rewrite of `.stella/memories/`, and losing a memory is a
  hard failure, never a migration cost.
