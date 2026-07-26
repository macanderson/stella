# Session Telemetry & Context Receipts — Schema Spec

Status: proposed for implementation — extends epic **#364** ("Context receipts").

## Purpose

This spec defines the most complete telemetry shape Stella can collect for a
single **session that fans across many models**, such that:

1. Any turn, at any step, is **reconstructable and inspectable after the fact** —
   the exact messages the model saw, which were cached, and what was added,
   compacted, or removed since the previous step.
2. Every token carried in context is **attributable as useful or unuseful**, with
   a named method and a real cost-of-carry number rather than a vibe.
3. **Memory writes and their subsequent citations** are joined by a stable id, so
   the write→recall→citation→usefulness loop is a first-class, queryable object.
4. A model can **consume its own receipt and self-rate its performance**, grounded
   in block ids and step coordinates rather than free-floating scalars.
5. Every record is **sliceable by `(role, provider, model)`**, because the whole
   point of one-session-many-models is *comparative* per-model attribution.

The design is **additive over the existing event-sourced fold**. It introduces no
daemon, no parallel content store, and no span/trace layer. It adds new
`AgentEvent` variants (all `#[serde(default)]`, old journals keep parsing), one
content coordinate (`block_id`), one grouping id (`turn_instance`), two closed
content gaps, and a small set of additive SQLite tables and columns.

### Relationship to epic #364

Epic #364 already scopes the *receipt core*. This spec adopts it verbatim and
labels each section:

| Section | Origin |
| --- | --- |
| §5 Per-step request manifest | **#364 as filed** (item 2) |
| §6 Compaction identities + typed decision events | **#364 as filed** (items 1, 3) |
| §6.4 Divergence closers (budget-abort results, discarded speculation, policy plane) | **#364 as filed** (items 4, 5, 6) |
| §4 Context-block registry (`block_id`) | New — the index that makes §5–§9 join |
| §7 Per-block cache attribution | New extension |
| §8 Usefulness attribution / cost-of-carry | **New extension #1** (the user's "useful vs unuseful tokens") |
| §9 Memory write→citation join | **New extension #2** (the user's "memory writes and subsequent citations") |
| §11 Self-reflection & self-rating | **New extension #3** (the user's "self-reflect and rate their own performance") |

**Build scope.** This spec describes the full telemetry shape, but §14 splits it
into a **verifiable core to build now** (Tier A) and an **inferred layer gated on
what the core's data shows** (Tier B) — the two should not be built as one
program. Every section header's work is tagged A or B in §14 and the appendices.

Sibling audit issues this spec is aware of and must not regress: #359
(loop-detection evidence), #360/#369 (budget session axis), #363 (error-output
compaction), #366 (per-turn `files_touched`), #368 (overflow summarizer),
#371 (usage attribution), #372 (dedup cache invalidation).

---

## 1. What already exists (do not rebuild)

The fold is already most of the way there. This spec **indexes what the journal
already carries** rather than duplicating it.

- **`AgentEvent`** (`stella-protocol/src/event.rs`) — one internally-tagged
  (`#[serde(tag = "type")]`) enum, additive-only, round-trip and legacy-parse
  tested. `--output-format stream-json` is `serde_json` of this enum, one line
  per event. This is the schema; everything below is new variants + fields on it.
- **`StepUsage`** — the paid-call ledger, already per-call and multi-model:
  carries `role: ModelCallRole`, served `provider`, `model`, actual +
  `estimated_input_tokens`, `cached_input_tokens`, `cache_write_tokens`,
  `cost_usd`, `duration_ms`, `retries`, `tool_calls`, `complete`, and
  `output_text` for management calls.
- **`journal.jsonl`** (`stella-store/src/journal.rs`) — append-only per session.
  Only streaming `Text`/`Reasoning` deltas are coalesced; **`ToolResult` events
  are journaled whole** (verified: journal.rs:177-190 drops only `TextDelta`
  previews; #363 confirms up to 100 KB error outputs are stored full). Compaction
  stubbing the *live* `Vec<CompletionMessage>` never touches the historical event.
  **Therefore the journal already holds every tool-output preimage.**
- **`memory_citations`** table (`stella-store/src/ddl.rs`) — the durable
  write↔citation link already exists, keyed `(execution_id, memory_id)` where
  `memory_id` is a `nod_…` node id.
- **Journal-replay equivalence** — replaying `journal.jsonl` through the deck's
  pure fold rebuilds the visible session byte-for-byte. This is the property the
  whole spec must preserve.

---

## 2. Non-negotiable invariants

1. **Additive-only wire contract.** Every new field is `#[serde(default)]`; every
   new event variant is optional. Old `journal.jsonl` and `events` rows keep
   parsing; `--output-format stream-json` never breaks. (Matches the existing
   `AgentEvent` compatibility discipline.)
2. **Ride the fold; add no parallel content store.** `block_id` is a
   content-addressed *index* over content the journal already carries. Receipts
   reference blocks by id + digest; they never re-store tool-output bytes.
3. **Replay equivalence is preserved.** Adding receipt events must not change what
   the fold reconstructs. Receipt events are observational; they are never read
   back into the `Vec<CompletionMessage>`.
4. **Durable before visible.** A receipt event is journaled before its effect is
   shown, matching the engine's existing durable-before-visible ordering. A step's
   manifest is emitted before the model call it describes commits.
5. **Usefulness labels are inferred heuristics, not ground truth.** Every
   usefulness judgment carries a named `method` and a `confidence`. Only two
   signals are treated as provable: an explicit `cite_memory`, and "block was
   evicted/compacted before it was ever cited or referenced" (= provably wasted
   spend). All positive-detection signals (referenced-downstream, model
   self-report) are labeled, never asserted as binary truth.
6. **Every attributable record carries the model-call coordinate.** Receipts,
   usefulness rows, and self-ratings all carry `(turn_instance, step, role,
   provider, model)` so any query can `GROUP BY` model. A telemetry row that
   cannot name the model that produced or consumed it is a bug.
7. **Local reconstruction and content-free export stay separated.** Full
   preimage reconstruction is a **local-only** capability (preimages live in the
   local `journal.jsonl` and never leave it). Every receipt/usefulness/self-rating
   event is **content-free-capable**: it carries digests, ids, counts, and scores —
   never payload bytes — so the enterprise content-free export
   (`enterprise-authority-telemetry.md`) can admit it unchanged. Recalled-frame
   content (G1) is journaled locally but is a redactable field, never exported.
8. **One added id, no span layer.** The coordinate model gains exactly
   `block_id` (content digest) and `turn_instance` (monotonic per session). No
   `TraceId`, no `Span` — those would be net-new against the codebase's deliberate
   plain-`i64`/`String` convention.

---

## 3. Coordinate & identity model

One session uses many models across many stages, turns, and steps. The full
coordinate of any model call is:

```
session_id      "ses-<ms>-<pid>"     — the whole session (existing)
  └ execution_id  i64 AUTOINCREMENT   — one run/goal/turn row (existing)
     └ stage       StageKind           — pipeline stage: Triage…Execute…Judge (existing)
        └ turn_instance  u32           — NEW: monotonic per session; groups the
        │                                steps of one Engine::run_turn without
        │                                relying on event-order correlation
           └ step     usize             — one committed model call (existing, on StepUsage)
              ├ call_role  ModelCallRole — concrete purpose (existing)
              ├ model_ref  {provider, model_id} — who served it (existing)
              └ manifest   Vec<block_id> — NEW: what it saw, in order (§5)
```

New identifiers (the only two this spec adds):

- **`block_id`** — content-addressed id for one durable unit of context.
  `blk_<first 24 hex of sha256(kind \0 canonical_content)>`, mirroring the
  existing `node_public_id` shape (`nod_<24 hex>`). Two byte-identical blocks
  share a `block_id`; this is what makes dedup/supersession *identities* rather
  than counts, and what lets usefulness attribution survive across steps.
- **`turn_instance`** — `u32`, monotonic within a session, stamped once per
  `Engine::run_turn`. Closes the "no id links steps to their stage" gap the audit
  found, without inventing a trace tree.

Existing ids reused unchanged: `session_id`, `execution_id`, `call_id`
(tool-call correlation), `memory_id` = `nod_…` (memory node), agent/lane id
(`"lead"`, `"req:1"`, `"sub:2"` for parallel sub-sessions).

**Model identity.** `ModelRef { provider, model_id }` is the pin; `provider` on a
receipt is always the provider that *actually served* the call (never the
session default), consistent with `StepUsage.provider`. `role: Role`
(Worker/Triage/Plan/Judge/…) is the routing tier; `call_role: ModelCallRole` is
the fine purpose (Worker/Judge/Summarization/…). Both are retained on every
receipt so "which model, in which role, saw this block" is always answerable.

---

## 4. The context-block registry — the spine

A **context block** is one durable, individually-attributable unit that can enter
the `Vec<CompletionMessage>`. The registry assigns each a stable `block_id` and
records its provenance once, at birth. Everything downstream (manifest, cache,
usefulness, self-rating) references blocks by id.

```rust
/// Emitted once, when a block first becomes eligible to enter context.
/// Content-free: carries a digest + provenance, never the payload bytes
/// (the bytes already live in the originating event — ToolResult, Text, etc.).
AgentEvent::BlockRegistered {
    block_id: String,          // blk_<24 hex of sha256(kind \0 content)>
    kind: BlockKind,
    origin: BlockOrigin,       // which event/tool/turn produced it
    token_cost: u32,           // estimated tokens at birth (estimator.rs)
    content_digest: String,    // "sha256:<full hex>" — verifies the preimage
    #[serde(default, skip_serializing_if = "Option::is_none")]
    citation_label: Option<String>,  // for recall frames / memory nodes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_ref: Option<String>,      // nod_… / file uri / call_id it derives from
}

enum BlockKind {
    SystemPrefix,      // index 0 — stable, never compacted
    UserGoal,          // the task text
    RecalledFrame,     // a ContextFrame injected by recall (memory/graph/file)
    AssistantText,     // model prose
    ToolCall,          // an assistant tool-call request
    ToolResult,        // a tool output (Ok or Error)
    Steered,           // a mid-turn injected user message
    Summary,           // an overflow-summarizer replacement span
    Attachment,        // multimodal image/doc/audio/video
}

struct BlockOrigin {
    turn_instance: u32,
    step: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,   // for ToolCall/ToolResult
    #[serde(default, skip_serializing_if = "Option::is_none")]
    memory_id: Option<String>, // nod_… for RecalledFrame that is a memory
}
```

The registry is the join hub: a `RecalledFrame` block carries the `memory_id` it
came from (closing the memory→context link that `ContextRecall` lacks today), and
a `ToolResult` block carries the `call_id` that produced it. Because `block_id` is
content-addressed, a tool output that reappears verbatim (supersession) resolves
to the same block, so "this exact content was carried for N steps" is directly
countable.

**`BlockOrigin.call_id` is birth provenance, not a call set.** Content-addressing
cuts both ways: a block is registered exactly once, so when two *distinct* calls
produce byte-identical output (two `git status` runs), `origin.call_id` names only
the first. Reading eviction identities through it would silently under-report —
the second call's contribution to context would be invisible, and an eviction
would be attributed wholly to the first. Per-occurrence attribution therefore
lives on [`ManifestEntry.call_id`](#5-per-step-request-manifest--the-receipt-364-item-2),
which is recorded per manifest row rather than per block. #364 item 1 asks "which
`call_id`s each pass evicted"; that question is answered by joining the compaction
event's block ids against the manifests that carried them, *not* by the registry.

---

## 5. Per-step request manifest — the receipt (#364 item 2)

The manifest is the single most load-bearing addition: the ordered list of blocks
the model actually saw on step *N*. It makes "re-run `compact()` over journal
replay" a faithful audit and turns every subsequent attribution into a join.

```rust
/// Emitted immediately before the model call of step N commits.
/// Ordered, content-free, and reconciled against the reported usage.
AgentEvent::StepManifest {
    turn_instance: u32,
    step: usize,
    role: ModelCallRole,
    provider: String,
    model: String,
    /// Blocks in wire order, index 0 = system prefix. This is the exact
    /// message sequence sent (closes gap G2).
    blocks: Vec<ManifestEntry>,
    /// The budget the compaction pass actually compared against THIS step —
    /// raw budget / calibration factor (driver.rs). #364 item 1 notes the
    /// Compaction event's before/after don't line up with this today.
    effective_budget_tokens: u64,
    calibration_factor: f64,
    estimated_input_tokens: u64,   // sum over blocks, pre-call
}

struct ManifestEntry {
    block_id: String,
    /// Cache position class relative to the last stable breakpoint (§7).
    cache_zone: CacheZone,
    token_cost: u32,               // estimated tokens of this block on this step
    /// Steps this block has been resident so far (drives cost-of-carry, §8).
    resident_since_step: usize,
    /// The tool call THIS occurrence belongs to. Per entry, not per block:
    /// `BlockOrigin.call_id` (§4) records only the first call to mint a
    /// content-addressed id, so duplicates would otherwise be unattributable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
}

enum CacheZone {
    StablePrefix,   // at/before the system-block breakpoint — should cache-hit
    Cacheable,      // before the conversation-tail breakpoint
    Volatile,       // after the last breakpoint — recomputed every step
}
```

### 5.1 Reconstruction algorithm

To reconstruct exactly what step *N* of turn *T* saw:

1. Replay `journal.jsonl` to the `StepManifest { turn_instance: T, step: N }`.
2. For each `ManifestEntry.block_id`, resolve the preimage from its
   `BlockRegistered.origin` event (the `ToolResult` / `Text` / recalled-frame
   content already in the journal — §5.3 for recall frames).
3. Verify each preimage against `content_digest`. A mismatch is a torn-journal or
   tampering signal, surfaced by the inspector.
4. Concatenate in manifest order → the byte-exact `CompletionRequest.messages`.

No new content store is read; the manifest is an index over the fold.

### 5.2 Cost

A manifest is O(context depth) `block_id`s + small ints per step. At ~200 steps
and ~200 blocks that is a few tens of KB per turn in the journal — cheap next to
the tool outputs already stored. Manifests may be emitted at `receipts = full`
and suppressed (digest-of-manifest only) at `receipts = summary` for cost control.

### 5.3 Closing G1 — recall-frame content

`ContextRecall` / `ContextFrameRef` carry `citation_label`, `source`, `uri`,
`token_cost` — but **no content**, so the exact recalled text that entered the
prompt is absent from the event stream. This is one of the only two content gaps
(the other, G2, is per-step ordering, closed by the manifest above).

`ContextRecall` gains an optional, **local-only, redactable** content carrier so
the exact recalled text is reconstructable:

```rust
// Extension to the existing ContextFrameRef (event.rs) — additive fields.
struct ContextFrameRef {
    // ...existing: id, citation_label, provider, source, kind, uri, method, token_cost
    #[serde(default, skip_serializing_if = "Option::is_none")]
    block_id: Option<String>,       // NEW: the registry id of this frame
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_digest: Option<String>, // NEW: "sha256:<hex>" of the injected text
}
```

The frame *content* itself is journaled once via `BlockRegistered` for the frame's
`block_id` (local only; the `content` field is stripped by the content-free export
projection). This is the minimal closure of G1: digest in the wire event, bytes in
the local block registry, never in export.

---

## 6. Context mutation & compaction identities (#364 items 1, 3, 4, 5, 6)

### 6.1 Mutations are manifest diffs, not a separate log

A context mutation (block added / removed / summarized) is **derivable as the diff
between consecutive manifests** of the same turn. The spec deliberately does *not*
add a per-mutation event — that would be a second thing to keep consistent with
the manifest series. The inspector computes `added = manifest[N] − manifest[N−1]`
and `removed = manifest[N−1] − manifest[N]` from `block_id` sets. Typed events are
retained only for the **why** a removal happened.

### 6.2 Compaction with identities (not counts)

Today `AgentEvent::Compaction` carries `evicted/deduped/superseded/aged` as
`usize` counts (`CompactionReport`). This spec keeps those for back-compat and
adds the identities:

```rust
// Additive fields on the existing AgentEvent::Compaction.
Compaction {
    // ...existing: before_tokens, after_tokens, evicted, deduped,
    //              superseded, aged, summarized
    #[serde(default)] pass: CompactionPass,   // which tier ran
    #[serde(default)] evicted_blocks: Vec<String>,     // block_ids stubbed by eviction
    #[serde(default)] deduped_blocks: Vec<String>,     // block_ids stubbed by dedup
    #[serde(default)] superseded_blocks: Vec<String>,  // block_ids stubbed as stale
    #[serde(default)] aged_blocks: Vec<String>,        // block_ids truncated
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary_block_id: Option<String>,          // the Summary block spliced in
    #[serde(default)] effective_budget_tokens: u64,    // #364 item 1: the real target
    #[serde(default)] calibration_factor: f64,
}

enum CompactionPass { Dedup, Supersession, Aging, Eviction, OverflowSummary }
```

This lets the inspector answer "block `blk_ab12…` was evicted at turn 4 step 9 by
the Eviction pass" and — critically for §8 — "it was never cited or referenced
before eviction, so its carried tokens were provably wasted."

### 6.3 Typed decision events (#364 item 3)

Loop aborts, budget denials, and retry exhaustion are today prose `Error`s
distinguishable only by string prefix. Add machine-readable variants (the prose
`Error` remains for display and back-compat):

```rust
AgentEvent::LoopDetected {
    turn_instance: u32,
    step: usize,
    evidence: LoopEvidence,   // see #359 — period, the repeated call signature,
                              // and whether it was input-only or full-output match
},
AgentEvent::BudgetDenied {
    scope: BudgetScope,       // Turn | Run | Session — see #360/#369
    spent_usd: f64,
    limit_usd: f64,
    mode: BudgetMode,
},
AgentEvent::RetriesExhausted {
    turn_instance: u32,
    step: usize,
    attempts: u32,
    reasons: Vec<String>,     // per-attempt reason — lost today (retry.rs:174)
},
```

### 6.4 Divergence closers (#364 items 4, 5, 6)

Three places where transcript and history diverge today; each closed by an
additive event so the receipt is complete:

```rust
// item 4: budget-abort synthetic tool_results enter history but not the stream.
AgentEvent::ToolResultSynthetic { call_id: String, reason: String },

// item 5: discarded speculation ran real I/O and fired hooks but left no event.
AgentEvent::SpeculationDiscarded { call_id: String, name: String, reason: String },

// item 6: the policy/extension audit plane (policy.evaluated/blocked,
// approval.requested, secret.detected) is a 64-entry in-memory ring that
// evaporates at exit. Bridge it into the journal, content-free:
AgentEvent::PolicyDecision {
    kind: PolicyKind,         // Evaluated | Blocked | ApprovalRequested | SecretDetected
    subject: String,          // tool name / capability — never the secret value
    outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    block_id: Option<String>, // if a specific block triggered it
},
```

---

## 7. Cache attribution — cached vs not, per block

The user wants, at any point, "how much is cached / not cached." The reported
usage (`cached_input_tokens`, `cache_write_tokens`, and the derived
`cache_miss_tokens` in the store) is per-call; this section attributes it back to
**blocks**, using the fact that breakpoints are structural and known.

Stella sets two Anthropic breakpoints — the system block and the conversation
tail — and places volatile recall *after* the stable prefix (L-E8). So each
`ManifestEntry.cache_zone` is computable at manifest time from block position
relative to the last stable breakpoint. At step commit, reconcile reported usage
against zones:

```rust
/// Emitted alongside StepUsage, attributing the reported cache split to zones.
AgentEvent::CacheAttribution {
    turn_instance: u32,
    step: usize,
    provider: String,
    model: String,
    prefix_hit_tokens: u64,     // cached_input attributed to StablePrefix/Cacheable
    volatile_tokens: u64,       // recomputed every step by construction
    write_tokens: u64,          // cache_write_tokens — new/changed cacheable content
    /// When the prefix that SHOULD have hit did not, name why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    miss_cause: Option<CacheCause>,   // reuse existing enum
    /// The block whose mutation broke the prefix (for PrefixInstability).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    culprit_block_id: Option<String>,
}
```

`CacheCause` already exists (`PrefixInstability`, `IdleBeyondTtl`,
`OptInNeverEngaged`). By attaching `culprit_block_id`, a low hit rate becomes a
first-class output: "step 7 paid full input because `blk_9f…` (a recalled frame)
was inserted before the tail breakpoint and shifted the cache boundary" — not a
manual diagnosis. This directly connects to #372 (dedup keeping the *latest*
duplicate maximally invalidates the prefix): the inspector can show the exact
block whose relocation cost a cache miss.

---

## 8. Usefulness attribution — useful vs unuseful tokens (New extension #1)

This is the user's headline ask. The metric is **cost-of-carry**, anchored in real
token economics; usefulness is a *labeling layer* on top of it.

### 8.1 Cost-of-carry (the real number)

For every block on every step it is resident, the carry cost is:

```
carry_cost(block, step) = block.token_cost
                          × price_of(cache_zone, model, step)   // hit vs miss vs write $/tok
```

Summed over a block's residency:

```
total_carry(block) = Σ_step carry_cost(block, step)
```

This is derived, not stored per step — it is computed by the inspector from the
manifest series (§5) + `CacheAttribution` (§7) + catalog pricing. A block resident
40 steps in the volatile zone costs 40× its tokens at the cache-miss rate; the
same block in the stable prefix costs its tokens once at the write rate plus 39×
at the hit rate. **This is what makes "unuseful tokens" a dollar figure, not an
opinion.**

### 8.2 Usefulness signals (the labeling layer)

A block's `total_carry` is *wasted* to the extent the block was never useful.
Usefulness is judged by four signals of decreasing provability. Each judgment is a
row carrying its `method` and `confidence` (invariant §5):

```rust
AgentEvent::BlockUsefulness {
    block_id: String,
    /// The model whose consumption this judgment is about — usefulness is
    /// per-consumer, so this is sliceable by model (invariant §6).
    turn_instance: u32,
    step: usize,           // the step whose output this judgment is derived from
    provider: String,
    model: String,
    signal: UsefulnessSignal,
    method: String,        // exact detector, e.g. "cite_memory",
                          // "evicted_before_reference", "token_overlap>=0.4",
                          // "model_self_report"
    confidence: f32,       // 0..1; 1.0 only for the two provable signals
    #[serde(default)] score: Option<i64>,  // 1..5 when signal is Cited/SelfReported
}

enum UsefulnessSignal {
    /// PROVABLE. An explicit cite_memory referenced this block's memory_id.
    Cited,
    /// PROVABLE. Block was evicted/compacted before it was ever cited OR
    /// referenced downstream → its carried tokens were wasted spend.
    WastedEvicted,
    /// INFERRED. The next assistant message or tool-call args reused this
    /// block's content (normalized token-overlap ≥ threshold). method names
    /// the exact detector; never asserted as binary truth.
    ReferencedDownstream,
    /// INFERRED. The model, consuming its own receipt, rated this block
    /// noise/useful in a SelfAssessment (§11). Weakest; self-report bias noted.
    SelfReported,
    /// NEUTRAL. Block survived to a successful outcome but no positive
    /// reference detected — carried "just in case." Recorded, not penalized.
    SurvivedUnreferenced,
}
```

**Build tier (see §14).** `Cited` and `WastedEvicted` are **Tier A** — provable,
build now. `ReferencedDownstream`, `SelfReported`, and `SurvivedUnreferenced` are
**Tier B** — inferred, shipped in shadow and gated on data before they are trusted
in the ledger.

### 8.3 The derived ledger

Per block, the inspector produces:

| Field | Meaning |
| --- | --- |
| `total_carry_usd` | §8.1 — dollars spent keeping this block in context |
| `steps_resident` | how long it stayed |
| `best_signal` | strongest usefulness signal observed |
| `verdict` | `useful` (Cited/ReferencedDownstream) · `wasted` (WastedEvicted) · `speculative` (SurvivedUnreferenced) · `disputed` (SelfReported-negative but referenced) |
| `waste_usd` | `total_carry_usd` when `verdict = wasted`, else 0 |

Rolled up, this answers the exact question — **"of the tokens we kept in context,
which were useful and which were dead weight, in dollars, per model"** — and
because `ReferencedDownstream`/`SelfReported` carry a method+confidence, the
weak signals never masquerade as the provable ones.

### 8.4 Reference detection (defining the weak signal precisely)

`ReferencedDownstream` is the fuzziest and most dangerous signal, so its method is
pinned, not left to "the model seemed to use it":

- **Primary:** normalized token-overlap between block content and the subsequent
  assistant message / tool-call arguments, over a shingled n-gram set, with an
  explicit threshold recorded in `method` (e.g. `token_overlap>=0.4`).
- **Secondary:** exact id/path echo — the block's `uri`, symbol name, `call_id`,
  or `nod_…` id appears literally in downstream output.
- **Never:** raw model self-assertion alone counts as `ReferencedDownstream`; that
  path is `SelfReported` with its own lower confidence.

---

## 9. Memory writes → citations → usefulness (New extension #2)

The write↔citation link exists (`memory_citations`), but three gaps break the
loop the user asked for: `ContextWrite` carries no memory ids, retrieval is only
logged when it *culminates* in a citation, and there is no per-retrieval record.

### 9.1 Write side — give `ContextWrite` its ids

```rust
// Additive fields on the existing AgentEvent::ContextWrite.
ContextWrite {
    // ...existing: provider, upserts, superseded
    #[serde(default)] written: Vec<MemoryWriteRef>,   // NEW
}

struct MemoryWriteRef {
    memory_id: String,     // nod_… — the join key to memory_citations
    kind: String,          // reflection | note | insight (MemoryKind)
    salience: f32,
    domains: Vec<String>,
    content_digest: String,// "sha256:<hex>" — content stays local
    origin_turn: u32,      // which turn produced this memory
}
```

Now a memory write is a first-class event with the same `nod_…` id used by
`cite_memory` and the `memory_citations` table — the write, the recall, and the
citation all share one key.

### 9.2 Retrieval side — log every injection, not just cited ones

Today a memory injected into context but never cited leaves no durable trace
(only the transient `ContextRecall`). The `BlockRegistered` event for a
`RecalledFrame` (§4) already carries `memory_id` in its origin, so **every memory
injection is now recorded** as the birth of a `RecalledFrame` block. The full loop
becomes queryable:

```
write (ContextWrite.written[].memory_id = nod_X)
  → inject (BlockRegistered{kind: RecalledFrame, origin.memory_id: nod_X, block_id: blk_Y})
    → carry (blk_Y appears in StepManifest across steps; §8 cost-of-carry)
      → cite (memory_citations{memory_id: nod_X, useful_score, truthful})   [optional]
      → OR never cited + evicted → UsefulnessSignal::WastedEvicted on blk_Y
```

This closes the user's exact request: a memory write and **every** subsequent
retrieval (not just the ones that ended in a citation), with the carry cost of
memories that were recalled but never paid off.

### 9.3 Memory scorecard (derived)

Per `memory_id`, joining the above:

| Field | Source |
| --- | --- |
| `write_turn`, `kind`, `salience` | `MemoryWriteRef` |
| `injections` | count of `RecalledFrame` blocks with this `memory_id` |
| `citations`, `mean_useful_score`, `truthful_rate` | `memory_citations` |
| `carry_usd` | §8.1 summed over its injected blocks |
| `payoff` | `citations / injections` weighted by `useful_score` |
| `verdict` | promote (payoff high, ≥ existing `PROMOTION_CITATIONS_REQUIRED`) · quarantine (existing negative-threshold) · **dead weight** (many injections, zero citations, high `carry_usd`) — a new class this telemetry makes visible |

The "dead weight" verdict — memories that keep getting recalled, cost real
cache-miss tokens, and are never cited — is only computable once §8 + §9.2 exist.

---

## 10. Multi-model session ledger

Everything above already carries `(role, provider, model)`. This section names the
two rollups that make "one session, many models" the payoff rather than a caption.

### 10.1 Model roster (per session)

```rust
AgentEvent::ModelRosterEntry {   // emitted the first time a model serves in a session
    role: Role,
    call_role: ModelCallRole,
    provider: String,
    model: String,
    first_seen_turn: u32,
}
```

Plus `ProviderFallback { from, to, reason }` (exists) records every mid-session
switch, so the roster is never silently incomplete (L-M7).

### 10.2 Comparative per-model attribution (derived)

The inspector groups every receipt-derived metric by `(role, provider, model)`:

| Per-model metric | Derived from |
| --- | --- |
| paid calls, tokens, cost, cache-hit rate | `StepUsage` + `CacheAttribution` |
| context it consumed: useful vs wasted tokens/$ | §8, filtered to steps this model served |
| its outputs' citation/reference rate | did *its* outputs get referenced downstream |
| memories *it* wrote and their payoff | §9 filtered to `MemoryWriteRef.origin_turn` served by this model |
| its self-ratings vs judge verdicts | §11 vs `JudgeVerdict`/`GoalVerdict` |

This is the Stella-specific shape a generic OpenTelemetry export cannot produce:
not just "model X cost $Y," but "the triage model's recalled context was 70%
wasted while the worker's was 20%," and "the judge and worker disagree on the
worker's self-rating." That comparison is only possible because usefulness and
self-rating are attributed per model-call coordinate (invariant §6).

---

## 11. Self-reflection & self-rating (New extension #3)

The user wants the model to "self-reflect and rate their own performance." The
receipt makes this *grounded*: the model reflects by **consuming its own receipt**
(§13 read API) and emits critique referencing `block_id`s and step coordinates —
not a free-floating scalar. This builds on the existing reflection loop
(`reflect_and_record`, the `reflections` / `execution_reflection` tables,
`GoalVerdict`, `JudgeVerdict`), not a parallel path.

```rust
AgentEvent::SelfAssessment {
    turn_instance: u32,
    /// The model doing the assessing (usually the worker at turn/goal end).
    provider: String,
    model: String,
    role: ModelCallRole,      // Reflection
    rubric: SelfRubric,
    /// Grounded critique — each item points at what it is about.
    notes: Vec<SelfNote>,
}

struct SelfRubric {
    goal_progress: i64,       // 1..5 — did this turn move the goal
    context_efficiency: i64,  // 1..5 — self-estimate of useful vs carried context
    tool_effectiveness: i64,  // 1..5 — were tool calls productive
    confidence: i64,          // 1..5 — calibration signal vs actual outcome
    #[serde(default, skip_serializing_if = "String::is_empty")]
    what_i_would_change: String,
}

struct SelfNote {
    /// What the note is about — grounds the critique in the receipt.
    anchor: SelfAnchor,       // Block(block_id) | Step(n) | Memory(nod_…) | Tool(call_id)
    verdict: String,          // e.g. "noise", "load-bearing", "should have cited"
    #[serde(default)] usefulness_score: Option<i64>,  // 1..5, feeds §8 SelfReported
}
```

Two properties make this trustworthy rather than a hallucinated scorecard:

1. **Receipt-grounded.** Every `SelfNote.anchor` must resolve to a real block/step
   in the turn's manifest; the inspector rejects notes that anchor to nonexistent
   blocks. A model cannot rate context it did not actually see.
2. **Adjudicated.** `SelfAssessment` scores are stored alongside the independent
   `JudgeVerdict` / `GoalVerdict`, so self-rating is always presentable *against*
   an external verdict. Calibration = `self.goal_progress` vs `judge.verdict`;
   systematic self-overrating per model is a first-class, sliceable metric (§10.2).

`SelfNote.usefulness_score` feeds the `SelfReported` signal in §8 at low
confidence — closing the loop: the model's own opinion of which context was noise
becomes one (clearly labeled, weakest) input to the usefulness ledger, never the
authority over it.

---

## 12. Storage schema (additive SQLite migrations)

All additive, following the existing `PRAGMA user_version` migration pattern
(current `SCHEMA_VERSION = 10`; this is `→ 11`). New events also land in the
generic `events(execution_id, seq, ts, event_type, payload)` table automatically
(they are `AgentEvent`s); the normalized tables below exist for query performance.

```sql
-- The block registry. One row per (execution, block).
CREATE TABLE IF NOT EXISTS context_blocks (
  execution_id   INTEGER NOT NULL,
  block_id       TEXT    NOT NULL,     -- blk_<24hex>
  kind           TEXT    NOT NULL,     -- BlockKind
  origin_turn    INTEGER NOT NULL,
  origin_step    INTEGER NOT NULL,
  call_id        TEXT,                 -- for ToolCall/ToolResult
  memory_id      TEXT,                 -- nod_… for RecalledFrame memories
  token_cost     INTEGER NOT NULL,
  content_digest TEXT    NOT NULL,     -- sha256:<hex>
  citation_label TEXT,
  first_seen_ts  INTEGER NOT NULL,
  PRIMARY KEY (execution_id, block_id)
);
CREATE INDEX IF NOT EXISTS context_blocks_by_memory ON context_blocks(memory_id, execution_id);

-- Per-step manifest membership. One row per (step, block) — the receipt, normalized.
CREATE TABLE IF NOT EXISTS step_manifest (
  execution_id  INTEGER NOT NULL,
  turn_instance INTEGER NOT NULL,
  step          INTEGER NOT NULL,
  ordinal       INTEGER NOT NULL,      -- position in the message sequence
  block_id      TEXT    NOT NULL,
  cache_zone    TEXT    NOT NULL,      -- StablePrefix | Cacheable | Volatile
  resident_since_step INTEGER NOT NULL,
  call_id       TEXT,                  -- per-occurrence tool attribution (§4)
  PRIMARY KEY (execution_id, turn_instance, step, ordinal)
);
CREATE INDEX IF NOT EXISTS step_manifest_by_block ON step_manifest(execution_id, block_id);

-- Per-step model call + cache attribution (extends what telemetry already has).
CREATE TABLE IF NOT EXISTS step_receipt (
  execution_id  INTEGER NOT NULL,
  turn_instance INTEGER NOT NULL,
  step          INTEGER NOT NULL,
  provider      TEXT NOT NULL,
  model         TEXT NOT NULL,
  call_role     TEXT NOT NULL,
  effective_budget_tokens INTEGER NOT NULL,
  calibration_factor      REAL   NOT NULL,
  prefix_hit_tokens  INTEGER NOT NULL DEFAULT 0,
  volatile_tokens    INTEGER NOT NULL DEFAULT 0,
  cache_write_tokens INTEGER NOT NULL DEFAULT 0,
  miss_cause     TEXT,               -- CacheCause
  culprit_block_id TEXT,
  PRIMARY KEY (execution_id, turn_instance, step)
);

-- Usefulness judgments. Many per block (one per signal/consumer).
CREATE TABLE IF NOT EXISTS block_usefulness (
  execution_id  INTEGER NOT NULL,
  block_id      TEXT    NOT NULL,
  turn_instance INTEGER NOT NULL,
  step          INTEGER NOT NULL,
  provider      TEXT NOT NULL,
  model         TEXT NOT NULL,
  signal        TEXT NOT NULL,        -- UsefulnessSignal
  method        TEXT NOT NULL,        -- exact detector
  confidence    REAL NOT NULL,
  score         INTEGER,              -- 1..5 for Cited/SelfReported
  ts            INTEGER NOT NULL,
  PRIMARY KEY (execution_id, block_id, signal, step)
);
CREATE INDEX IF NOT EXISTS block_usefulness_by_model ON block_usefulness(provider, model, execution_id);

-- Memory writes (join partner to the EXISTING memory_citations table).
CREATE TABLE IF NOT EXISTS memory_writes (
  execution_id  INTEGER NOT NULL,
  memory_id     TEXT    NOT NULL,     -- nod_… — joins memory_citations.memory_id
  kind          TEXT    NOT NULL,
  salience      REAL    NOT NULL,
  domains       TEXT,                 -- json array
  content_digest TEXT   NOT NULL,
  origin_turn   INTEGER NOT NULL,
  ts            INTEGER NOT NULL,
  PRIMARY KEY (execution_id, memory_id)
);

-- Self-assessments (join partner to the EXISTING reflections / execution_reflection).
CREATE TABLE IF NOT EXISTS self_assessments (
  execution_id  INTEGER NOT NULL,
  turn_instance INTEGER NOT NULL,
  provider      TEXT NOT NULL,
  model         TEXT NOT NULL,
  goal_progress      INTEGER,
  context_efficiency INTEGER,
  tool_effectiveness INTEGER,
  confidence         INTEGER,
  what_i_would_change TEXT,
  notes         TEXT,                 -- json array of SelfNote (anchors + verdicts)
  ts            INTEGER NOT NULL,
  PRIMARY KEY (execution_id, turn_instance)
);
```

Existing tables reused unchanged as join partners: `memory_citations`
`(execution_id, memory_id, useful_score, truthful, remark)`, `telemetry`
(per-step token/cost ledger), `reflections` / `execution_reflection`, `executions`
(the `session_id` link), `tool_calls`. New tables never duplicate their columns;
they reference by `(execution_id, …)` and `memory_id`/`block_id`.

**Cross-project rollups.** The user-tier `usage.db` gains a per-model
`context_efficiency_rollup(project_id, provider, model, useful_usd, wasted_usd,
mean_self_rating, mean_judge_delta)` so "which model wastes the most context across
all my repos" is one query — mirroring the existing `execution_rollup` /
`tool_usage_rollup` one-way `store.db → usage.db` derivation.

---

## 13. Inspection API — reconstruct any point in the loop

The user wants to "at any point in time in a session inspect the state of the turn
loop." Two consumers: a human (the `stella-observatory` read-only dashboard + a
CLI) and the model itself (self-reflection). Both read the same reconstruction.

### 13.1 The read model

```
inspect(session_id, turn_instance, step) -> TurnState
```

```ts
type TurnState = {
  coord: { session_id; execution_id; stage; turn_instance; step;
           role; provider; model }
  // The exact prompt the model saw, reconstructed via §5.1 (local only).
  messages: Array<{ block_id; kind; role; content?; token_cost;
                    cache_zone; resident_since_step; digest_verified: boolean }>
  cache: { prefix_hit_tokens; volatile_tokens; write_tokens;
           hit_rate: number; miss_cause?; culprit_block_id? }
  // Diff vs the previous step (derived, §6.1) — nothing separately logged.
  mutations: { added: block_id[]; removed: block_id[];
               compacted: Array<{ block_id; pass }>;
               summarized?: { replaced: block_id[]; into: block_id } }
  budget: { effective_budget_tokens; calibration_factor;
            spent_usd; limit_usd; scope }
  usefulness: Array<{ block_id; total_carry_usd; steps_resident;
                      best_signal; verdict; waste_usd }>
  decisions: Array<LoopDetected | BudgetDenied | RetriesExhausted | PolicyDecision>
}
```

A `session` view aggregates across turns: the model roster (§10.1), per-model
comparative attribution (§10.2), the memory scorecard (§9.3), and a session-level
useful-vs-wasted-$ summary.

### 13.2 Surfaces

- **`stella inspect <session> [--turn T] [--step N]`** — renders `TurnState` as
  text/JSON. `--json` emits the raw structure for tooling.
- **Observatory** — new read-only routes over the additive tables:
  `/api/receipt?execution=&turn=&step=` (a `TurnState`),
  `/api/context-efficiency` (per-model useful-vs-wasted), `/api/memory-scorecard`.
  Consistent with observatory's `SQLITE_OPEN_READ_ONLY`, never-mutate contract.
- **Model self-reflection** — a read-only `inspect_receipt` capability the
  reflection call consumes to produce a receipt-grounded `SelfAssessment` (§11).
  It returns the content-free `TurnState` (block digests + usefulness + cache),
  never raw preimages, so the reflection prompt stays bounded.

### 13.3 What "at any point" costs

Reconstruction is a journal replay bounded by the turn, not the session — the
inspector seeks to the turn's first event and folds forward to the requested step.
Manifests + block registry make this O(context depth), not O(session length).

---

## 14. Build plan — verifiable core now, inferred layer gated on data

The sections above describe the full shape. They should **not** be built as one
program. Split by *verifiability*: build the parts that are measurements or
byte-exact reconstructions now; treat the parts that rest on inferred labels as
experiments that only earn their place once the core's data shows they would
change a decision. The dividing question for every item is: *is this a fact the
fold can prove, or a judgment a heuristic is guessing?*

### Tier A — build now (verifiable, rides the fold, debugging ROI)

Every item here is a measurement or a byte-exact reconstruction, not a judgment.
A1–A2 are #364's receipt core (partly already shipping in PR #375); A3 is the
cost-of-carry number and the two *provable* usefulness signals.

**A1. Block registry + manifest (§4, §5).** Emit `BlockRegistered` +
`StepManifest`; close G1/G2. Unlocks point-in-time reconstruction (§13).
*Gate:* journal-replay equivalence still passes; a new test asserts
manifest-reconstruction is byte-identical to the live `CompletionRequest`.

**A2. Compaction identities + typed decisions + divergence closers (§6).**
Identity fields on `Compaction`, the `LoopDetected`/`BudgetDenied`/
`RetriesExhausted` events, and the three transcript/history closers. Cheap;
unlocks the mutation diff and turns the compaction/cache/budget bugs the audit
found (#360/#363/#372) into visible data instead of code reads.

**A3. Cache attribution + cost-of-carry + provable usefulness (§7, §8.1, and
only the provable signals of §8.2).** `CacheAttribution`, the cost-of-carry
derivation, and only `Cited` (from the existing `cite_memory`) and `WastedEvicted`
(evicted before any reference). Delivers a *dollar figure* for wasted context
carry — provable, and directly actionable for compaction tuning.

Tier A is justified independent of self-reflection: it is the instrument that
catches the next compaction/cache/budget regression, and it is the #364
differentiator — "proof, per step, of what the model saw and what it cost." Ship
it even if no one ever self-reflects.

### Gate — measure before building Tier B

After Tier A has run over real sessions, decide with data, not a hunch. Proceed
to Tier B only where the receipts show the inferred layer would change a decision:

- **Waste headroom.** Is a material fraction of token spend `WastedEvicted`
  (say ≥ ~20%)? If most context is provably load-bearing and cache hits are
  healthy, the inferred layer is solving a problem you do not have — stop here.
- **Label signal.** Do the provable signals leave enough unexplained useful
  context that an inferred `ReferencedDownstream` detector would add real
  coverage? If `Cited` + exact-echo already explain usefulness, the fuzzy detector
  is redundant risk.
- **Calibration feasibility.** Tier A gives you `SelfAssessment`-shaped slots next
  to `JudgeVerdict`. Run self-rating in *shadow* first (§11 emitted, not
  surfaced), and productize only if self-scores correlate with judge verdicts per
  model. If they do not, self-rating is theater — keep it as a research signal,
  not a product surface.

### Tier B — gated on the above (experiments, validated against ground truth)

Build these as measured experiments and keep only what pays off. Each is `serde
(default)`, so **shipping the event for shadow measurement costs nothing and can
precede the productization decision** — the gate governs whether you build its UI
and act on it, not whether you can measure it.

**B1. Inferred usefulness (§8.2 `ReferencedDownstream`, §8.4).** The token-overlap
detector. Emit in shadow, validate its labels against `Cited` + exact-echo ground
truth before trusting it in the ledger.

**B2. Memory scorecard incl. the "dead weight" verdict (§9.3).** Needs A3 + the
injection log (§9.2). Promote/quarantine reuse existing thresholds; "dead weight"
(recalled repeatedly, real carry cost, never cited) is the new, data-dependent
verdict only computable once A3 exists.

**B3. Self-rating + per-model self-vs-judge rollups (§11, §10.2, §12
`context_efficiency_rollup`).** The lowest-provability, highest-uncertainty layer,
and the one the original ask led with. Shadow-measure calibration first;
productize per model only where it holds.

### Additivity holds for both tiers

Every item: new fields `#[serde(default)]`, new tables `IF NOT EXISTS`, old
journals and `stream-json` consumers unaffected. No item adds a daemon, a
background thread, or a network call; all emission rides the existing
`EventSender` seam (`stella-core/src/event_sender.rs`) and the journal writer.
Emitting a Tier B event for shadow measurement is not the same as building its UI
or acting on it — the gate governs the latter.

---

## Appendix A — new `AgentEvent` variants at a glance

| Variant | Section | Tier | Content-free | Purpose |
| --- | --- | --- | --- | --- |
| `BlockRegistered` | §4 | A | digest only (content local via registry) | birth of a context block |
| `StepManifest` | §5 | A | yes | ordered receipt of what step N saw |
| `Compaction` (extended) | §6.2 | A | yes | eviction/dedup **identities** + real budget |
| `LoopDetected` / `BudgetDenied` / `RetriesExhausted` | §6.3 | A | yes | typed decisions |
| `ToolResultSynthetic` / `SpeculationDiscarded` / `PolicyDecision` | §6.4 | A | yes | close transcript/history divergence |
| `CacheAttribution` | §7 | A | yes | per-zone cache split + miss culprit |
| `BlockUsefulness` | §8 | A/B | yes | provable signals (A: `Cited`/`WastedEvicted`) + inferred (B) per block/model |
| `ContextWrite` (extended) | §9.1 | A | digest only | memory write ids (`nod_…`) |
| `ContextFrameRef` (extended) | §5.3 | A | digest + local content | close recall-content gap |
| `ModelRosterEntry` | §10.1 | A | yes | first appearance of a model in the session |
| `SelfAssessment` | §11 | B | yes | receipt-grounded self-rating (shadow-measure first) |

## Appendix B — the five asks, mapped to sections

| User ask | Delivered by | Tier |
| --- | --- | --- |
| Optimal shape for one session, many models | §3 coordinate model + §6 invariant (every record carries the model coord) + §10 | A (coordinate + roster); comparative self-vs-judge is B |
| LLM self-reflect and rate own performance | §11 (receipt-grounded, adjudicated against JudgeVerdict) | **B — gated on calibration** |
| Capture memory writes + subsequent citations | §9 (`nod_…` join key: `ContextWrite.written` ↔ `memory_citations`) + §9.2 every injection | A (write/inject/cite join); "dead weight" verdict is B |
| Tokens kept in context — useful vs unuseful | §8 cost-of-carry + labeled usefulness signals; §7 cache economics underneath | A (cost-of-carry + provable signals); inferred labels B |
| Inspect turn-loop state at any point (mutations / cached / added / compacted / removed) | §5 manifest + §6 mutations & compaction identities + §7 cache + §13 inspection API | A |
