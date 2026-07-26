# Adaptive Context

**Status:** Specification — supersedes the 2026-07 spec bundle
**Scope:** Stella's context plane: what enters a turn, why, at what cost, and how that improves over time

---

## 1. What this replaces, and why

An earlier bundle of six documents specified this feature across ~7,700 lines
and an eleven-phase roadmap. Phases 0 and 1 shipped. This document replaces the
whole bundle.

The replacement is not a change of goal. It is a change of *starting position*.
The old bundle was written against a codebase that no longer exists — it
described mechanisms as absent that now ship, encoded file positions and version
constants that drifted within days, and planned two subsystems that would
duplicate ones already running. Rewriting was cheaper than reconciling.

Three rules follow from that failure and govern this document:

1. **No coordinates.** No line numbers, version constants, dependency pins, or
   file positions. They rot faster than they can be maintained, and a plan
   nobody trusts is worse than no plan. Decisions live in ADRs; positions live
   in the code.
2. **Extend before inventing.** Where a mechanism already ships, this spec names
   it and extends it. A parallel implementation of a shipped mechanism is a
   defect, not a phase.
3. **Every phase is observable.** A phase that changes no behavior a user can
   see is not a phase. Phases 0 and 1 delivered ~3,200 lines of correct,
   well-tested types with no runtime consumer, and produced no signal about
   whether the design was right. That does not happen again.

## 2. The goal

Stella should get better at a codebase by working in it, without the user
restating preferences, curating a queue of synthetic proposals, or trusting
opaque model-authored memories.

That decomposes into two properties, in dependency order:

- **Accountable** — for any past turn, you can see exactly what context entered
  it, why each item was selected, what it cost, and where it came from. The
  answer is deterministic and verifiable, not reconstructed by guess.
- **Adaptive** — durable context is proposed from evidence, governed before it
  takes effect, attributed after it is used, and retired when it stops helping.

Accountable comes first because adaptive without it is unfalsifiable: a system
that learns but cannot show its work cannot be debugged, trusted, or corrected.

## 3. What already ships

This section is deliberately first. The largest error in the previous bundle was
planning against an imagined baseline.

**Retrieval is real and hybrid.** A bi-temporal property graph with a
fingerprinted embedding index backs a recall path that fuses vector similarity,
recency, one-hop graph adjacency, and domain overlap via reciprocal-rank fusion,
diversifies with MMR, and packs to a token budget while reporting what it
dropped. Provider fan-out goes through a Context Graph Protocol host with
consent gating, per-provider timeouts, and crash isolation. Frames carry
mandatory citation labels and content digests.

**Eviction is real.** Conversation compaction runs every step before the model
call, applying four increasingly-lossy passes — dedup, supersession, aging,
eviction — with the system message and the most recent tool result never
touched, and a model-call summarizer as the overflow fallback with a
failure latch.

**Receipts are real.** Every committed step emits a content-addressed block
registry and an ordered manifest recording what the model saw, in what order, in
which cache zone, at what token cost, and how long each block has been resident.
Turns reconstruct byte-exactly, digest-verified.

**Suppression is real and re-learn-proof.** Forgetting an item writes a tombstone
that also suppresses later *restatements* of it by lexical similarity, so a
forgotten lesson does not return through the mining path.

**A lexical adaptive loop is real, and runs by default.** Reflection lessons
append to a private log; that log is clustered into candidate skills; candidates
that clear a threshold are written to disk as skills, capped per session, never
clobbering an existing file, and filtered against tombstones from two surfaces.
Those skills load into later prompts. This is observe → cluster → propose →
govern → persist → re-inject, already closed.

**Also shipped:** a code graph with a query tool and live re-indexing;
exploration sharing between sessions in a workspace; a storage map with a
pre-write schema gate; `gather_context` packs; citation-driven memory
quarantine and promotion.

**Shipped but inert:** the `context.*` settings block, and a domain type layer
covering the record taxonomy, scope, temporal intervals, canonical hashing,
representations, contracts, outcomes, and context-use. Both are correct and
tested. Neither has a runtime consumer.

## 4. What is actually missing

Against that baseline, the gap is much narrower than eleven phases:

| Gap | Nature |
|---|---|
| Recall considers the entire corpus per query | Defect — unbounded candidate generation |
| Point-in-time recall applies to edges only | Defect — the query cutoff reaches one of five signals |
| No supersede or tombstone in the plane that owns the data | Defect — retention lives in a different database than the records |
| Editing a memory mints a duplicate | Defect — identity is content-addressed, so a revision is a new record |
| Receipts cannot say *which memories* a turn recalled | Gap — recall collapses into one undifferentiated block |
| No deterministic, hash-stable frame identity | Gap — the manifest is close but not addressed as a compiled artifact |
| Proposals are lexical, untyped, and unauditable | Gap — the loop exists but keeps no reviewable record of why |
| Efficacy is per-memory citation counting only | Gap — no attribution across the items a turn actually used |
| Nothing retires context that stops helping | Gap — quarantine covers explicit negatives, not disuse |

Four of the nine are defects in shipped code, and they sit underneath every
remaining gap. That ordering drives the plan.

**Status, 2026-07-26.** Phase 1 ([#712](https://github.com/macanderson/stella/issues/712))
closed all four defects: candidate generation is bounded by the requested frame
count, the point-in-time cutoff reaches every signal, supersede and tombstone
live in the plane that owns the records, and a memory's identity is its lineage
so an edit revises rather than duplicates. The five remaining rows are gaps, and
Phases 2–4 build them. The table is left as written — it is the analysis the
plan was ordered by.

## 5. Invariants

These are non-negotiable and apply to every phase.

### 5.1 Cache stability outranks allocation cleverness

The system prefix — persona, project orientation, workspace memories,
exploration index, rules — is assembled **once per session** and must stay
byte-identical for the session's life. Sub-budgets within it are fixed and
computed once.

**Only the post-prefix volatile block may vary per turn.** Any allocator,
ranking change, or budget policy that can alter prefix bytes mid-session is
rejected regardless of its retrieval merit. Rewriting an early message per turn
collapses the provider cache prefix to the system message alone; this has
already happened once and the guard comments in the assembly path exist because
of it.

A consequence worth stating plainly: a memory saved mid-session does not appear
until the next session. That is correct behavior, not a bug to fix.

### 5.2 Immutability and derivation

No canonical record is mutated. Semantic, status, scope, sharing, or enforcement
changes create a new revision in the same lineage, superseding its predecessor.
`superseded`, `expired`, and `stale` are derived projections, never stored
states. Observations, evidence, promotion events, uses, and feedback are
append-only.

### 5.3 Determinism

Identical inputs, cutoffs, policy version, tokenizer, and budget produce a
byte-identical frame body and identical hash. Every ranking tie has an explicit,
documented tiebreak. Every token count declares the tokenizer that produced it.
Canonical hashing follows the scheme already ratified in ADR 0004.

### 5.4 Authority

Observations carry no instruction authority. Evidence, memories, and knowledge
inform; only directives and contracts steer. Inferred directives start advisory
and may never *start* blocking; blocking always requires explicit human
confirmation. Scope and sharing never widen automatically. Learned context can
never grant authorization — there is no `allow` effect.

### 5.5 Honesty

No silent truncation: anything dropped is reported, with a denominator that
means something. No silent write failure: a discarded persistence error is a
defect. Gaps are declared where a caller can see them, not only in prose — an
unhonored query parameter must be refused loudly or documented at its own call
site.

### 5.6 Local-first

No account, no server, no new outbound traffic, no phone-home. Secrets are
redacted before persistence, indexing, or embedding. Export happens only through
an explicit export path with scope, sensitivity, and consent checks. Mining
failure must never fail the user's task.

### 5.7 Suppression is reversible and singular

There is one suppression mechanism — tombstones with restatement matching — and
it applies to **every** surface that can re-inject context, including the
system-prefix workspace memories. A second suppression mechanism is a defect.
Suppression never physically deletes; deletion is a separate retention workflow.

## 6. Architecture

Three layers, each with one job.

### 6.1 The retrieval plane

Owns durable context and answers "what is relevant to this goal, within this
budget." Bounded candidate generation, hybrid ranking, diversification, budget
packing with an honest drop report. Owns supersession and tombstones for the
records it stores, because retention policy cannot live in a different database
from the data it governs.

### 6.2 The frame

The per-turn compiled artifact: what entered, in what order, why, at what cost,
with what provenance — deterministic and content-addressed.

**This is the receipts manifest, extended — not a parallel aggregate.** The
manifest already records ordered, content-addressed, cache-zoned, cost-attributed
blocks and reconstructs byte-exactly. It lacks recall-block decomposition,
per-item provenance back to source records, and a stable frame-level identity.
Those are additions to one artifact, not a second artifact. Two immutable
records of one turn's context can disagree; one cannot.

### 6.3 The lifecycle ledger

Append-only, immutable, hashed records of the loop: observations, proposals,
promotion events, uses, and feedback. New record kinds are **born canonical**
here. Legacy rows transfer incrementally per ADR 0010 — never by cutover.

## 7. The loop

```
work happens
  → evidence is captured (reflection lessons, citations, tool outcomes, diffs)
  → observations are extracted, replay-idempotent, secrets redacted
  → recurring observations across DISTINCT tasks induce a proposal
  → governance activates it (solo: advisory, auto; blocking: explicit only)
  → it is selected into frames, with provenance
  → its uses are attributed to outcomes
  → it is reaffirmed, corrected, or retired — reversibly
```

Two anti-poisoning rules are load-bearing and inherited from the prior bundle:
**count distinct tasks, not events** — thirty repetitions inside one task must
never satisfy a three-task threshold — and **never promote from model prose
alone**; a proposal cannot cite itself.

## 8. Migration obligation

The lexical loop in §3 ships and users depend on it. Replacing it with a typed
loop is a **migration with a behavior-compatibility obligation**, not a
greenfield build. Specifically preserved:

- tombstone suppression across re-learning, on every surface
- the per-session creation cap
- never clobbering a hand-edited file
- deterministic, stable candidate identity
- failure isolation — mining never fails the user's task

A typed loop that regresses forget/restore is a worse product than the lexical
one it replaces.

## 9. Explicitly out of scope

Named so they are decisions rather than omissions:

- **Artifact contracts and completion truth.** Valuable, independent of this
  loop, large. The type layer already exists; nothing consumes it. Revisit when
  completion truth is the priority.
- **Team governance and repository publication.** Needs the Markdown-canonical
  policy (ADR 0008) exercised by a real team. Solo first.
- **Protocol export of lifecycle records.** Gated on local replay proving value
  and a second provider proving portability.
- **Rehydration and `Compact`/`Reference` representations.** Eviction ships;
  rehydration does not, and its type layer sits unused. Real, but not on the
  path to adaptive — eviction stubs already instruct the model to re-run the
  tool, which is manual rehydration. Revisit after the loop closes.
- **Dynamic allocation across prefix sections.** Rejected permanently under §5.1,
  not deferred.

## 10. Decision record

| ADR | Subject | Status |
|---|---|---|
| 0001 | Semantic taxonomy | Accepted |
| 0002 | Scope vs. sharing | Accepted, ratified |
| 0003 | Bitemporal semantics | Accepted — see note |
| 0004 | Record revision identity | Accepted |
| 0005 | Storage authority | Accepted, amended by 0010 |
| 0006 | ContextFrame vs. CompiledContextFrame | Accepted — see note |
| 0007 | Immutable promotion history | Accepted, ratified, amended |
| 0008 | Markdown-canonical rules | Accepted |
| 0009 | Enum-freeze resolutions | Accepted, ratified |
| 0010 | Incremental authority transfer | Accepted, ratified |

**Note on 0003.** ~~Within `recall`, the cutoff reaches adjacency only.~~
**Closed 2026-07-26 by Phase 1 (#712).** Its characterization was accurate for
the low-level edge query it pins, and the conclusion did not hold one layer up —
a point-in-time recall returned current content with historical edges. Every
candidate reader now shares one predicate, so `as_of` is honored across node,
vector, recency, and adjacency alike. The world-validity axis
(`valid_from`/`valid_to`) remains unconsulted, as the ADR describes.

**Note on 0006.** Its distinction stands, but it predates the receipts plane. The
compiled frame is now reached by *extending the step manifest* (§6.2), not by
building a second aggregate. An amendment should record this before Phase 2
ships.

**Note on 0010.** Ratified 2026-07-26 ([#711](https://github.com/macanderson/stella/issues/711)).
It amends 0005 by replacing a big-bang authority cutover with incremental
transfer, and settles in the same act that the retrieval index — `node`, `edge`,
`embedding` — never transfers authority: `lineage_id` lands on `memory` and
`episode` only. Phase 3 is unblocked. Whether the backfill ever becomes
mandatory remains open and gates nothing before Phase 3.

---

Implementation phases, gates, and sequencing: [`adaptive-context-plan.md`](adaptive-context-plan.md).
