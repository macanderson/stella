---
id: adaptive-context-plan
title: "Adaptive Context — Implementation Plan"
status: proposed
---

# Adaptive Context — Implementation Plan

**Status:** Plan — supersedes the eleven-phase roadmap
**Spec:** [`adaptive-context.md`](adaptive-context.md)
**Phases:** four, each shippable and observable

---

## 0. How this plan differs

The previous roadmap had eleven phases and reached its first observable
behavior at phase five or six. Two phases shipped; the product did not move.

This plan has four. **Every one changes something a user can see**, and each is
useful even if the next never ships. Phase 1 is worth doing if you abandon
adaptive context entirely — it fixes defects in code that runs today.

Ordering rule: **defects before features.** Three of the four capabilities the
old plan wanted to build next sit on top of a retrieval plane with unbounded
candidate generation, a half-applied point-in-time cutoff, no supersede path,
and duplicate-on-edit identity. Building on that propagates the problems into
the layer that is supposed to make them accountable.

### Working rules

- One phase, one branch, one PR. Never cross phases on a branch.
- Behavior-changing work ships behind `context.lifecycle.enabled`. Phase 1 was
  defect repair and shipped on unconditionally — a bug fix behind a flag is a
  bug that is still shipping. **The flag itself now defaults to on** (changed
  after Phases 2 and 3 landed): a lifecycle nobody runs cannot be evaluated,
  and the behavior-compatibility suite asserts every spec §8 guarantee on both
  the typed and the opted-out path, so "on" is a claim with tests behind it
  rather than a hope. Setting it `false` restores the previous behavior
  wholesale.
- `make gate` green before a phase is claimed done. Do not claim a gate passed
  without running it and reading the output.
- No coordinates in any document this plan produces (spec §1).
- New `.rs` files stay under the size ratchet. Where a phase must grow a file
  already at its baseline, splitting that file is an explicit first task, done
  as a pure move with no behavior change.

---

## Phase 1 — Repair the retrieval plane

**Observable:** recall gets faster and stops returning stale duplicates.
Forgetting something actually removes it, from every surface, immediately.
Reported context numbers become true.

**Why first:** these are defects in code that runs by default for every user.
They are also hard prerequisites — a bitemporal repository cannot sit on a
cutoff that reaches one signal in five, and retention windows cannot sit on a
plane with no supersede path.

### Deliverables

1. **Split the store module** — pure move, no behavior change, first commit.
   Divide along the seams already present: schema and migration, node access,
   edge access, embedding access, domain and scope. Unfreezes the size ratchet
   for everything after.

2. **Bound candidate generation.** Today recall loads every live node and every
   vector, ranks the whole corpus by recency, runs MMR over all of it, and mints
   a full frame — cloning each node's entire content — before any budget
   applies. Replace with top-k readers at the SQL boundary, a bounded fusion
   set, MMR over that set, and frame construction only for packed survivors.
   The requested frame count must actually bound the work.

3. **Make the drop report mean something.** Its denominator becomes candidates
   *considered*, not corpus size. Today a workspace with 500 memories reports
   ~495 drops and permanent truncation every turn, which makes the honest-budget
   discipline honest about the wrong number.

4. **Supersede and tombstone in the plane that owns the data.** Write the
   supersede column that exists in the schema and has never been written. Move
   tombstone filtering **before** budget packing — it currently runs at the CLI
   projection layer after the budget is spent, so a quarantined memory consumes
   one of five slots and is then discarded, silently giving that turn four.

5. **Fix memory identity.** Identity is currently the hash of kind plus content,
   so editing a memory mints a new record and leaves the old one live — with its
   old text, its own vector, and full participation in every future recall.
   Identity moves to lineage; an edit becomes a revision that supersedes.
   Migration is lossless and detects existing duplicate lineages without
   merging them automatically.

6. **Extend suppression to the system prefix.** Workspace memory files are baked
   into the system prompt with no tombstone filter, so forgetting one does not
   stop it shipping. Apply the same tombstone-plus-restatement filter used on
   every other surface. This closes a hole in a guarantee already made.

7. **Make the point-in-time cutoff honest.** Apply it to node, vector, and
   recency reads as well as adjacency, so a query that sets `as_of` is answered
   from one instant across every signal. Decided 2026-07-26 (#711, decision 3);
   the alternative — refusing the query — was rejected because the cutoff is
   the same predicate the edge reader already uses.

8. **Lift tuning constants into settings.** Frame count, token budget, fusion
   constant, diversification lambda, coverage floor, and the lexical-fallback
   cap are hardcoded and unreachable from the settings block that exists to hold
   them. Wire them, keeping current values as defaults.

9. **Fix the stale strings** found along the way: the overflow path advises a
   slash command that does not exist; the README describes a tool as
   conditionally registered when it is unconditional; three comments reference a
   protocol rename that was rejected.

### Gate

- Existing crate tests green; `make gate` green.
- A benchmark showing recall work is bounded by requested frame count, not
  corpus size, at three corpus sizes spanning at least an order of magnitude.
- Witness: a tombstoned memory never occupies a budget slot.
- Witness: editing a memory yields one live record, not two.
- Witness: a forgotten workspace memory does not appear in the system prompt.
- Witness: point-in-time recall returns content and edges from the same instant,
  or the query is refused.
- Drop-report denominator asserted against candidates considered.

### Risks

Identity migration touches live user memories — it is the only Phase 1 item
that rewrites existing rows. It ships with fixtures for each schema version, is
idempotent, and reports what it changed. Duplicate lineages created by past
edits are *detected and reported*, never merged automatically; merging is a
proposal for a later phase, not a migration side effect.

---

## Phase 2 — The accountable frame

**Observable:** for any past turn, `stella inspect` shows exactly which memories,
skills, and code-graph frames entered it, why each was selected, what each cost,
and where each came from — verifiable, not reconstructed.

**Why second:** accountability is the prerequisite for trusting anything
adaptive. It also gives the dead Phase-1 type layer its first consumer, which is
the only way to find out whether those types are right.

### Deliverables

1. **Decompose the recall block.** Four block kinds are defined and never
   produced; recall is swallowed whole into a single goal block, so receipts can
   say a turn had recall but not what was in it. Emit the recalled-frame,
   steered, summary, and attachment kinds. This is the join that makes every
   later question answerable.

2. **Per-item provenance.** Block identity and content digest on frame
   references are currently null. Populate them so a frame reference resolves to
   the record it came from.

3. **Emit recall telemetry from every path.** The recall event fires from the
   pipeline path only; interactive and one-shot emit nothing, so most real usage
   is invisible.

4. **Frame identity and determinism.** A stable frame hash over the manifest
   body, excluding volatile fields. Identical inputs produce identical bytes and
   identical hash. Golden fixtures pin it.

5. **Selection reasons and non-evictable items.** Each entry records why it was
   selected. Category-aware precedence, with required items unable to be evicted
   by ranking — only by an explicit, reported decision.

6. **Amend ADR 0006** to record that the compiled frame is reached by extending
   the step manifest rather than building a parallel aggregate.

### Gate

- Identical inputs produce byte-identical frames and hashes across runs.
- Byte-exact turn reconstruction still passes, now including decomposed recall.
- Required items survive ranking pressure; drops are reported with reasons.
- Scope-leakage tests at every scope dimension.
- No measurable regression in per-turn latency.

### Risks

The manifest is a persisted wire type with existing readers. Additions are
additive and version-tolerant; the forward-compatibility path added for unknown
event types is the precedent. Cache zones must not shift — spec §5.1 applies.

---

## Phase 3 — The typed adaptive loop

**Observable:** Stella proposes durable context from evidence and explains why —
"three separate tasks, here they are" — and Keep/Edit/Ignore is auditable
afterward. The proposal that becomes a skill is reviewable *before* it lands,
which is the part users cannot do today.

**Why third:** this is the goal line. It is also a **migration, not a build**
(spec §8) — a live lexical loop already does this, and it must not regress.

**Unblocked:** ADR 0010 ratified 2026-07-26 (#711). `lineage_id` lands on
`memory` and `episode` only — the retrieval index does not transfer.

### Deliverables

1. **The lifecycle ledger.** Append-only, immutable, hashed records for
   observations, proposals, and promotion events, born canonical per ADR 0010.
   No legacy rows are rewritten. New kinds have no legacy counterpart and
   therefore need no migration.

2. **Typed observation extraction**, replay-idempotent with cursors, secrets
   redacted before persistence, sourced from the evidence already captured —
   reflection lessons, citations, tool outcomes.

3. **Proposal induction over the existing miner.** Keep the clustering, the
   stable identity, and the thresholds. Add a durable, reviewable proposal
   record carrying its supporting observations, distinct-task count, and scored
   components. Distinct tasks, never raw events.

4. **Migrate the skills loop onto the typed path**, behavior-compatible.
   Existing skill files are untouched. The per-session cap, no-clobber, and
   two-surface tombstone filtering all survive — asserted by test, not by
   inspection.

5. **Wire the dead rules miner** through the same path. Its twin has shipped
   unwired since the shared clustering module was created to keep them aligned.

6. **Solo advisory governance.** Immutable promotion events; inferred directives
   start advisory and can never start blocking; no automatic sharing or scope
   widening; a re-proposal cooldown so a declined proposal does not return next
   turn.

7. **Review surface.** List proposals with their evidence; Keep, Edit, or Ignore;
   every outcome recorded as an immutable event.

### Gate

- Three distinct tasks produce one eligible proposal; thirty repetitions inside
  one task produce none.
- No inferred directive reaches blocking by any path.
- No sharing or scope widens without an explicit act.
- A tombstoned lesson still cannot return as a proposal or a skill.
- Existing skill files are byte-identical after migration.
- Keep/Edit/Ignore replay identically from the event log.
- Mining failure cannot fail the user's task.

### Risks

The behavior-compatibility obligation is the whole risk. A typed loop that
regresses forget/restore is worse than the lexical one it replaces. Every
guarantee in §8 of the spec gets a test before the migration lands, not after.

---

## Phase 4 — Efficacy and reversible retirement

**Observable:** context that repeatedly fails to help stops being selected —
visibly, with reasons, and reversibly. The system stops accumulating advice
nobody benefits from.

**Why last:** it needs the frame (Phase 2) to know what was actually used, and
the ledger (Phase 3) to have something to attribute to.

### Deliverables

1. **Context-use records** linking a frame's items to their turn and outcome,
   with a trace id joining use to feedback.

2. **Generalize the shipped citation loop.** Explicit citation with usefulness
   and truthfulness already drives quarantine at two negatives and promotion at
   a positive streak. It becomes one evidence source among several rather than
   the only one — keeping its semantics.

3. **Opportunity-aware attribution.** Negative attribution requires that the
   item had an opportunity to help and that the assessment method is recorded.
   Absence of citation is not evidence of uselessness.

4. **Derived selection health**, computed from immutable records and rebuildable
   exactly — never a stored mutable state.

5. **Reversible retirement.** Mark stale → stop auto-selecting → notify → archive
   after a grace period unless reaffirmed. Never touches blocking, critical,
   user-confirmed, published, or pinned records. Never physically deletes.

### Gate

- Aggregates rebuild exactly from immutable records.
- Negative attribution requires opportunity and a recorded method.
- Retired records are excluded from automatic selection, still explicitly
  retrievable, and restorable.
- Protected categories are provably never retired.
- Retirement decisions carry a human-readable reason.

### Status — shipped 2026-07-26 (#715)

All five deliverables landed, and the gate is met. What is worth knowing
beyond the checklist:

**Use records live in `context.db`, not `store.db`.** `memory_citations` is in
`DEPENDENT_TABLES`, so `stella stats prune` deletes citation rows with their
execution — an aggregate folded over them is *not* rebuildable across a prune,
and the first gate criterion would have been false the first time anyone
pruned. `context_records` is append-only enforced by SQLite triggers and is
never pruned. The consequence is an ordering dependency, stated rather than
hidden: extraction must reach an execution before a prune does.

**Retirement never writes `superseded_at`.** `node_by_public_id` filters on
that column, so routing retirement through `supersede_node` would have made a
retired record impossible to fetch by id — failing "still explicitly
retrievable" outright. Retirement is instead a derived projection over
`promotion_event`s (`Retired`/`Reverted`, folded last-write-wins exactly as
keep/ignore already are) that joins the union the suppression reader already
computes. No second suppression mechanism, per §5.7.

**Two of the five protected categories do not exist and are documented as
absent rather than faked.** `DirectivePriority::Critical` is carried on no
record and set by no path; there is no pin concept for memory or context
records at all. Both are guarded by `protection_for` being the single predicate
every retirement passes through, so each check has exactly one home when the
concept becomes real.

**The pruning-eligibility tier is now enforced, and it is the load-bearing
correctness result.** The type layer always said `agent_self_report` is
recognized but may never drive pruning; nothing implemented it. `cite_memory`
is the *agent* judging context the agent was given, so citation-derived
verdicts are recorded as `agent_self_report` and are deliberately **not**
pruning-eligible — a self-report that can retire its own subject is
self-reinforcing, because a model that misreads a memory reports it unhelpful,
retires it, and destroys the evidence that the reading was wrong. Selection
health therefore carries two populations: everything assessed (what is known)
and the pruning-eligible subset (what may remove). Only the latter decides
`failing`.

That has a deliberate consequence: **automatic retirement does not fire on
citation evidence alone.** Today a human is the pruning-eligible source, via
`stella memory retire <id> --reason`. Wiring a deterministic source — the
anchor check behind `stella memory validate` is the obvious candidate — is
tracked separately and is what makes the sweep autonomous.

---

## Sequencing and decisions

```
Phase 1  Repair the plane        defects; ships ON; no flag
Phase 2  Accountable frame       ← accountability reached
Phase 3  Typed adaptive loop     ← "adaptive context is real"
Phase 4  Efficacy + retirement   ← the loop closes
```

Phase 1 stands alone and is worth shipping regardless. Phases 2–4 are each
useful without their successor: 2 without 3 gives inspectable turns; 3 without 4
gives governed proposals that never retire themselves.

### Decisions — all four settled 2026-07-26 (#711)

1. **ADR 0010 (incremental authority transfer): ratified.** The big-bang
   authority cutover leaves the critical path; `context_records` becomes
   canonical for the records it owns, and that set grows monotonically.
   Phase 3 unblocked.
2. **`node`/`edge` transfer authority: no.** They are a derived index over
   content, not a record store — as is `embedding`. `lineage_id` lands on
   `memory` and `episode`, two tables, not five. Folded into ADR 0010 as
   decision point 6.
3. **Point-in-time recall: apply the cutoff.** It is the same predicate the
   edge reader already uses. Phase 1 item 7 builds it; a query that sets
   `as_of` is answered from one instant across every signal, not refused.
4. **Duplicate memory lineages: report only.** Phase 1's migration detects and
   reports them and never merges. Merging becomes a Phase 3 proposal, where
   proposals have a review surface and an undo.

### Tracker reconciliation

Epic #469 tracks eleven phases against the superseded bundle. Phase issues for
old phases 2–10 no longer map onto this plan and should be closed with a
pointer here rather than silently abandoned; the epic body should be rewritten
around these four phases. The ADRs and the closed Phase-0/1 issues stand as-is.

Old → new, for anyone following an existing issue:

| Old | Disposition |
|---|---|
| 2 — migration | Superseded by ADR 0010; folded into Phase 3 as incremental transfer |
| 3 — bitemporal + compiler | Split: cutoff correctness → Phase 1; frame → Phase 2 |
| 4 — compaction + representations | Eviction already ships; representations deferred (spec §9) |
| 5 — observation extractor | Phase 3 |
| 6 — proposals + governance | Phase 3 |
| 7 — artifact contracts | Deferred (spec §9) |
| 8 — publication + team | Deferred (spec §9) |
| 9 — efficacy + pruning | Phase 4 |
| 10 — protocol export | Deferred (spec §9) |
