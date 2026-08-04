# stella-context

The context plane: one SQLite file (`.stella/private/context.db`) and one engine
([`ContextStore`](src/store.rs)) holding a bi-temporal property graph, a
fingerprinted embedding index, and episodic memory — with a hybrid, budgeted,
cited retrieval pipeline (`recall`) on top and a close-and-supersede write path
(`upsert`) underneath. It is the single door between the engine and everything
the agent knows that isn't already in the prompt.

Two boundaries are deliberate. **Wire types are never redefined here**: frames,
queries, capabilities and verdicts come from `contextgraph-types` and are
re-exported from [`src/lib.rs`](src/lib.rs) so a consumer needs only this crate.
And **external context sources do not register here** — third-party stdio/HTTP
providers are admitted onto the CGP host by
[`crates/stella-cli/src/contextgraph.rs`](../stella-cli/src/contextgraph.rs), gated on
the conformance suite and egress consent, which is why this crate takes no
dependency on `contextgraph-host`. [`ProviderRegistry`](src/provider.rs) is the
seam for *in-plane* sources that share the workspace store's lifetime. The
built-in store declares `egress: false`: it reads and writes locally and nothing
leaves the machine.

## Where it sits

No workspace crate is a dependency — only `contextgraph-types` (pinned by git
rev), `rusqlite`, `tokio`, `serde`, `sha2`, `async-trait` and `thiserror`. It
even formats its own RFC-3339 timestamps rather than pull a date-time crate
([`src/clock.rs`](src/clock.rs)). It is a library, not a binary.

`stella-cli` is the only crate that depends on it (`Cargo.toml` path dep):
session recall, `stella memory`, and reflection write-back all flow through it.
Despite what `stella-graph`'s package description says, **`stella-graph` does not
link this crate** — it writes its own `.stella/private/codegraph.db`.

## Boundary — does this change belong here?

The decision rule: a change belongs here if it alters what comes back out of
`.stella/private/context.db` or what goes into it — the schema and migration
ladder, the upsert/supersede write path, embeddings and fingerprints, the
fuse → dedup → diversify → pack recall pipeline, compaction, or the
`ContextProvider` seam for in-plane sources. A change that alters *when*
recall is invoked or what the engine does with the frames — prompt assembly,
steering, session wiring — belongs in `stella-cli`, the only dependent;
decision logic over recalled content belongs in `stella-core`, which per
AGENTS.md invariant #2 can never absorb this crate's I/O in return.

Three adjacent homes absorb the changes that look like they belong here but do
not. Context Graph Protocol wire types come from the external
`contextgraph-*` registry crates — the intro's first deliberate boundary — so
a new wire field is a PR against `context-graph-protocol` plus a pin bump
here, never a local type that shadows the registry definition. The in-engine `context_record` value layer
(`crates/stella-core/src/context_record.rs`) stays in `stella-core` by
decision, not by accident — it is the adaptive-context taxonomy the engine
validates, not a retrieval type, and `stella-core`'s README ("`context_record`
is not `stella-context`") explains the coexistence; do not merge it in. And
code indexing is [`stella-graph`](../stella-graph): tree-sitter, symbol
extraction, and `codegraph.db` changes land there — while retrieval *fusion*,
over the code graph or any other source, lands here and never there.

Extension points are modules, not crates: a new node kind, retrieval signal,
migration, or embedder follows the recipes under "Extending it" below. A new
crate is justified only when the work (a) sits behind a port and would
otherwise drag heavy dependencies into a deliberately light crate — the live
example is the R14 ONNX-embedder follow-up, which would put a model runtime
behind the [`src/embed.rs`](src/embed.rs) `Embedder` seam rather than into a
crate that today builds from `rusqlite`, `tokio`, and `serde`; (b) needs a
dependency direction the current graph forbids — exactly why this crate takes
no dependency on `contextgraph-host` and `stella-graph` takes none on this
crate; or (c) is a genuinely separate deliverable with its own binary or
release cadence. Failing those, extend this crate: a new crate costs a
workspace-table row, an impacted-crates scope, CI time, and a README — with
AGENTS.md's workspace table and the root `Cargo.toml` members updated in the
same PR — and a wrong split is harder to undo than a wrong merge.

## God files — do not add lines

This crate has no god files: no file here exceeds the gate's 1500-line
ratchet (`scripts/check-file-size.sh`), and none may appear — a new file
crossing the limit fails the gate outright, and
`scripts/file-size-baseline.txt` accepts no new entries. When a file
approaches the limit, split it before it crosses, the way
[`src/store.rs`](src/store.rs) already fans out into
[`src/store/schema.rs`](src/store/schema.rs), [`src/store/node.rs`](src/store/node.rs)
and friends — and know that [`src/store/tests.rs`](src/store/tests.rs) sits at
1498 lines today, so the very next test added there must instead start a new
sibling test module.

## Layout

| File | What it holds |
|---|---|
| [`src/lib.rs`](src/lib.rs) | Crate docs, the module list, and the whole public re-export surface. `#![warn(missing_docs)]` — with `make lint` at `-D warnings`, an undocumented public item is a build failure. |
| [`src/store.rs`](src/store.rs) | `ContextStore` itself — opening, warming, tuning, suppression — plus the re-export surface that keeps every moved name reachable as `crate::store::X`. |
| [`src/store/schema.rs`](src/store/schema.rs) | The DDL, the migration ladder, the connection pragmas, and the fingerprint registry. Open this to change the schema. |
| [`src/candidates.rs`](src/candidates.rs) | The bounded, `as_of`-aware readers `recall` ranks over: `NodeMeta` (no bodies), the shared `NODE_AS_OF` predicate, and `nodes_by_ids` — the one read on the recall path that moves content. |
| [`src/store/node.rs`](src/store/node.rs) | `NodeKind`/`NodeInput`/`NodeRow`, the upsert, and supersede/restore. |
| [`src/store/edge.rs`](src/store/edge.rs) | Fact assertion, supersession, and the two point-in-time readers. |
| [`src/store/embedding.rs`](src/store/embedding.rs) | The vector codec and the fingerprinted index reads. |
| [`src/store/record.rs`](src/store/record.rs) | Episode and memory rows, including memory lineage and revisions. |
| [`src/store/domain.rs`](src/store/domain.rs) | The domain tag table, its junctions, and the scope anti-join. |
| [`src/retrieval.rs`](src/retrieval.rs) | `recall` / `recall_scoped` / `recall_scoped_excluding`: fusion, dedup, MMR, budget packing, the coverage gate, `RecallTuning`, and `frame_from_node` (where a `ContextFrame` is actually minted — for packed survivors only). |
| [`src/writeback.rs`](src/writeback.rs) | `ContextDelta` and `upsert`, plus bi-temporal fact supersession (`apply_fact`) and the `facts_as_of` audit read. |
| [`src/provider.rs`](src/provider.rs) | The `ContextProvider` trait, `ContextStore`'s implementation of it (`info`/`capabilities`/`query`/`verify`), and `ProviderRegistry` fan-out. |
| [`src/embed.rs`](src/embed.rs) | The `Embedder` seam, `EmbedderFingerprint`, and the offline `HashEmbedder` default. |
| [`src/clock.rs`](src/clock.rs) | `Clock`, `SystemClock`, `FixedClock`, `format_rfc3339`. Inject `FixedClock` whenever a test needs an exact T1→T2 correction. |
| [`src/error.rs`](src/error.rs) | `ContextError` — the typed failure set, including the invariants the plane refuses to violate. |

## Key concepts

**Vectors are keyed `(content_hash, fingerprint)`, never by node.** A
fingerprint is `model_id@revision/dims/normalization` (`hash-ngram@2/256/l2` for
the default). Retrieval reads only vectors under the *active* fingerprint, so
swapping embedders is a new fingerprint plus an incremental re-embed on next
touch — old vectors go invisible, they are never invalidated in place. Identical
content is therefore never embedded twice; `UpsertReceipt::embeddings_reused`
counts the skips.

**Recall embeds the query and nothing else.** Indexing happens at mount:
`open_and_warm` spawns `warm_index`, which embeds every live node missing a
vector under the active fingerprint, 64 at a time, one transaction per chunk. A
cold store degrades to lexical results; it never blocks the first prompt.

**The pipeline is fuse → dedup → diversify → pack → report.** Reciprocal-rank
fusion (`RRF_K = 60`) over four ranked lists — vector cosine, recency, 1-hop
graph adjacency from anchors and the top vector seeds, and (when scoped) domain
overlap — then dedup by content hash, an MMR pass (`MMR_LAMBDA = 0.7`), then
`pack_to_budget`. Packing returns `(kept, dropped)` as a *partition* of the
input: nothing may vanish silently, and every drop names its `token_cost` and
`DropReason` so a caller can size a re-query instead of guessing.

**Weak coverage is labeled, not disguised.** If mean top-5 cosine falls below
`MIN_COVERAGE = 0.15`, recall abandons fusion and serves up to 8 bounded lexical
matches, each stamped `crates/stella-context/lexical-fallback` in its provenance chain
(`is_lexical_fallback` reads it back). `RecallResult::used_lexical_fallback`
says so at the result level.

**Citation and digest are constructor invariants, not conventions.**
`upsert_node` rejects a blank `display_name` at write time and `frame_from_node`
returns `ContextError::MissingCitation` at read time, so an uncitable frame
cannot reach a prompt from either direction. Every minted frame also declares
`content_digest: sha256:<content_hash>` over exactly the bytes it serves, which
is what lets `ContextProvider::verify` answer `Valid`/`Stale`/`Gone` by
comparing hashes — no frame body moves in either direction. An identity
presented with no digest is `Unknown`, never `Valid`.

**Only edges are bi-temporal.** A single-valued fact correction calls
`close_edge` (setting `superseded_at`, and `valid_to` if still open) and inserts
a replacement linked through `edge.supersedes` — never a delete, so
`facts_as_of(Some(t))` still reconstructs what was believed at `t`. Timestamps
are fixed-width RFC-3339 precisely so that reconstruction is a string range scan
(`recorded_at <= ?1 AND (superseded_at IS NULL OR superseded_at > ?1)`) with no
parsing. Nodes are mutable current-state: `upsert_node` overwrites content in
place.

## Gotchas

- **`context.db` is not `codegraph.db`.** The tree-sitter index used to share
  this file with `code_graph_*`-prefixed tables; `MIGRATION_V3` drops any that
  survive, and `stella-graph` opens its own `codegraph.db`. The witness is
  `opening_drops_orphaned_code_graph_tables_from_context_db`
  ([`src/store/tests.rs`](src/store/tests.rs)). Do not reintroduce graph tables here.
- **`NodeRow::valid_from` is always `None`.** The columns exist on `node` but
  `upsert_node` never writes them. Fact history is recoverable; node content
  history is not.
- **This plane forgets *and* reclaims — but they are different mechanisms.**
  Forgetting is `supersede_node`: a tombstone, with `restore_node` as its exact
  inverse, so a point-in-time query still sees what was believed before.
  Reclaiming is `ContextStore::compact` (`stella memory compact`), and it deletes
  only *derived index entries whose owner is already gone*: embeddings no node in
  any state and no live memory points at — every `stella memory edit` strands one
  — orphaned `node_domains`/`edge_domains` rows, and, opt-in behind
  `--stale-fingerprints`, vectors under an embedder recall is forbidden to read.
  `edge` rows, `memory` revisions and superseded `node` rows are **named
  exclusions**; see the `store::compact` module docs for each one's reason.
  `L-C3` is a guarantee about *queryability*, not bytes — the authority for
  treating the index as disposable is
  [ADR 0010](../../docs/adr/0010-incremental-authority-transfer.md) decision point 6.
  What compaction does *not* bound is the live row count: a node whose uri
  changed (renamed or deleted file) is still orphaned live, serving its
  last-known content until something supersedes it.
- **`stella memory forget` reaches in here now.** The tombstone in `store.db`
  stays canonical — it is surface-aware, carries the reason, and outlives the
  row — and the forget is *projected* onto `node.superseded_at`, where every
  candidate reader already filters. Derived quarantine has no row here to mark
  (it is a citation count, recomputed per read), so it arrives as an id set on
  `recall_scoped_excluding`. Either way suppression lands before the budget, so
  a suppressed memory frees its slot instead of spending it and being discarded.
- **Only the cosine scan still touches the whole corpus.** Every other signal is
  `LIMIT`-bounded at the SQL boundary by a bound derived from the query's
  `max_frames`, ranking runs over metadata with no bodies, and node content is
  read for packed survivors only. The similarity scan is the honest exception:
  "most similar" is not something SQLite can `ORDER BY` without an ANN index. It
  reads ids and vectors, never content. An opt-in IVF accelerator
  (`src/ann.rs`, `context.retrieval.ann_enabled`, built by `stella memory
  index`) makes it `O(√n)`; every recall it serves says so on
  `RecallResult::used_ann_index`.
- **`dropped` counts the shortlist, not the store.** Its denominator is
  `RecallResult::considered` — the candidates the budget actually chose between
  — so "12 of 20 dropped" is a statement about this query. Candidates ranked
  below the shortlist are reported separately as `candidates_cut`; nothing is
  silent (`L-C5`).
- **A memory's identity is its lineage, not its text.** `memory.lineage_id` is
  the durable id and `public_id` identifies a revision, so editing a memory
  supersedes rather than duplicates and the mirror node — keyed
  `memory://<lineage>` — updates in place. The old revision's vector is orphaned
  rather than deleted: the similarity scan (`score_nodes_by_vector`) joins through
  `node.content_hash`, so a vector no live node points at is never selected.
  Orphaned is not permanent — `stella memory compact` is what reclaims it, and
  the stranded vectors are the mass that verb exists for.
- **`await_warm()` returning `Ok(0)` does not mean "the index is complete."** It
  means there is no warm left to join — the handle is taken, so a second call
  also returns `Ok(0)`, as does a store opened outside a tokio runtime (where
  `open_and_warm` silently returns un-warmed) or an in-memory store (which
  `warm_index` skips outright).
- **The store ignores two `ContextQuery` fields.** `kinds` is honored only at the
  registry seam, so once the store is selected a `kinds: [Memory]` query can
  come back with `File` frames; `representation_preferences` is ignored entirely
  — every frame is `Representation::Full` with its body inline.
- **Empty-content nodes are exempt from hash dedup.** They all share
  `sha256("")` despite being distinct identities, so `DedupKey::Distinct` keys
  them by node id — merging them would collapse graph and taxonomy recall on any
  initialized workspace.
- **Every ordering tie breaks explicitly on node id.** The cosine sort and the
  dedup survivor both do it because the underlying query has no `ORDER BY` and
  fusion runs through a `HashMap`; without the tiebreak the same store answers
  the same query in a different order between runs.
- **The builders are `#[must_use]` for a reason.** `NodeInput::with_content` and
  friends consume and return `self`, so a dropped result is content or domain
  tags that never reach the store, with no error to notice.
- **`edge.public_id` is collision-proof since schema v10** (#617): the mint
  folds a per-process nonce into the hash and a `UNIQUE` index backs it, so two
  stella processes writing the same db in the same second mint from different
  keyspaces and a collision that somehow survived both is a loud constraint
  error, never two silently coexisting rows.
- **A newer schema is a hard error, not a best-effort open.** `migrate` rejects
  `user_version > SCHEMA_VERSION` with `ContextError::SchemaTooNew`: episodic
  memory and the fact graph are not rebuildable, so an older binary must not
  write into a schema it does not know.

## Testing

```bash
cargo test -p stella-context
```

There is no `make test-context` target and no `tests/` directory — every test is
an inline `#[cfg(test)] mod tests` at the bottom of its module, next to the code
it pins. `tempfile` backs real on-disk stores (an in-memory one never warms, so
warming tests must use a file), `proptest` covers the packer invariant
(`packing_never_exceeds_budget_and_loses_nothing` in
[`src/retrieval/tests.rs`](src/retrieval/tests.rs)), and async tests use `#[tokio::test]`.

The tests worth reading before changing anything are the ones that encode a
past defect: `kill_mid_index_rolls_back_to_a_consistent_store`,
`rejects_a_context_db_written_by_a_newer_stella`, the `migrates_v1_/v2_…`
replays that hand-build an old schema and assert the data survives (all in
[`src/store/tests.rs`](src/store/tests.rs)), and the two `store_verifies_by_digest…` /
`store_frames_declare_a_content_digest…` witnesses in
[`src/provider.rs`](src/provider.rs).

Note that `make gate` runs `make doc-citations`, which fails if a `docs/*.md`
path cited from a doc comment in this crate does not resolve.

## Extending it

**Add a node kind.** 1. Add the variant to `NodeKind` in
[`src/store/node.rs`](src/store/node.rs). 2. Extend `as_str`, `parse`, and
`to_frame_kind` — the compiler catches all three. 3. Add it to `store_kinds()`
in [`src/provider.rs`](src/provider.rs), or kind-filtered queries will be routed
away from the store; omitting `Memory` there once did exactly that to memory
queries. No migration is needed — `node.kind` is a TEXT column.

**Add an embedder.** 1. Implement `Embedder` (`fingerprint` + `embed`) in a new
module. 2. Return a fingerprint that differs in at least one field. 3. Pass it
to `ContextStore::open_with` or `open_and_warm`. Nothing else changes: old
vectors stay under the old fingerprint, invisible to retrieval, and warm
re-embeds incrementally on the next mount.

**Add a migration.** 1. Add a `MIGRATION_V<n>` const whose doc comment says
*why*, not what the SQL says. 2. Bump `SCHEMA_VERSION`. 3. Add the
`if version < n` arm inside `migrate`'s single transaction. 4. Add a replay
test that builds the previous schema by hand and asserts the old data survives —
copy `migrates_v2_context_db_preserving_memories`. Remember that shipping this
makes older binaries refuse the workspace.

## See also

- [`../../AGENTS.md`](../../AGENTS.md) — "The `.stella/` directory (per-workspace
  state)" for where `context.db` sits and who else writes under `private/`, and
  the Gotchas entry on `context.db` vs `codegraph.db`.
- [`../../docs/design/adaptive-context/context-reuse.md`](../../docs/design/adaptive-context/context-reuse.md) — the frame identity
  triple and the `context/verify` contract that `ContextProvider::verify`
  implements.
- [`../../website/content/docs/context-engine.mdx`](../../website/content/docs/context-engine.mdx)
  — the user-facing view of the context engine and its stores.
- [`../stella-cli/src/contextgraph.rs`](../stella-cli/src/contextgraph.rs) —
  where this store is wrapped as the `workspace-memory` CGP provider and where
  external providers are registered instead.
- [`../stella-graph`](../stella-graph) — the tree-sitter code graph, a separate
  crate writing a separate file.
