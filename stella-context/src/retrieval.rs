//! The hybrid, budgeted, cited retrieval pipeline (arch §2.3).
//! [`ContextStore::recall`] fuses three signals
//! — vector similarity, recency, and 1-hop graph adjacency — via reciprocal-
//! rank fusion, dedupes by content hash, diversifies with MMR, then **packs to
//! the query's token budget and reports what was dropped** (silent truncation
//! is banned, `L-C5`). Every frame carries a human `citation_label` (`L-C4`).
//! When graph/vector coverage of the goal is weak, it falls back to bounded
//! lexical search and **labels those frames as lexical fallback** rather than
//! dressing weak context up as grounding (`L-C6`).
//!
//! The scoring/fusion/packing steps are plain synchronous functions over owned
//! data — brute-force top-k cosine is fine at CLI-local scale; an ANN
//! accelerator is a size-threshold follow-up. They are property-tested at the
//! bottom of the file.

use std::collections::{HashMap, HashSet};

use contextgraph_types::frame::FrameEmbedding;
use contextgraph_types::{
    BYTES_PER_BUDGET_TOKEN, ContextFrame, ContextQuery, ContextQueryResult, Provenance,
    Representation,
};
use rusqlite::Connection;

use crate::candidates::{
    NodeMeta, domains_for_nodes, live_node_metas, nodes_by_ids, scan_lexical,
    score_nodes_by_vector, vectors_for_ids,
};
use crate::error::ContextError;
use crate::store::{
    ContextStore, NodeRow, domains_by_node, neighbors, node_ids_excluded_by_scope,
    node_ids_for_uris,
};

/// Provenance `kind` marking a frame's domain tag, so a citation view can show
/// the domains a frame belongs to.
pub(crate) const DOMAIN_PROVENANCE_KIND: &str = "domain";

/// Provider identity stamped into frame provenance. Built from the crate
/// version rather than a literal so it cannot drift from the version
/// `ContextProvider::info` advertises — a hard-coded `0.1` outlived four minor
/// releases and made every frame's `by` chain misattribute its producer.
pub(crate) const PROVIDER_ID: &str = concat!("stella-context/", env!("CARGO_PKG_VERSION"));
/// The lexical-fallback marker written into a frame's provenance chain so a
/// host can see the frame is a weak-coverage substitute, not graph grounding.
pub(crate) const LEXICAL_FALLBACK_METHOD: &str = "stella-context/lexical-fallback";

/// Reciprocal-rank-fusion constant (the standard 60).
const RRF_K: f64 = 60.0;
/// How much the recency list counts for, relative to vector similarity.
///
/// Recency used to be fused at full weight, as a peer of similarity. Because
/// RRF is flat (see [`rrf_fuse`]), that made the N most recently written nodes
/// structurally guaranteed a top-N slot no matter what the query asked: the
/// newest node banked `1/61` from recency alone — the exact contribution of the
/// single best semantic match — so with `max_frames: 5` the five newest rows
/// could occupy every slot.
///
/// That is not hypothetical. A run asking to remove some test files wrote four
/// reflections plus an episode; the very next run, on a completely unrelated
/// TUI keybinding, recalled all five and handed them to the witness author,
/// which then went looking for test files to delete.
///
/// A relevance floor was measured and rejected as the fix: under the default
/// [`crate::embed::HashEmbedder`] (character-trigram hashing, not semantics)
/// those five contaminants scored 0.38–0.50 against that prompt while
/// genuinely relevant frames scored 0.45–0.63. The sets overlap, so no
/// threshold separates them. Recency was the thing doing the damage, so
/// recency is what changed.
///
/// At 0.15 the newest node banks `0.15/61 ≈ 0.0025`, which cannot outweigh
/// even a mid-ranked semantic hit (`1/150 ≈ 0.0067`). Every embedded node is
/// already in the vector list, so recency keeps its real job — ordering among
/// comparably-relevant frames — and loses only its ability to inject a frame
/// the query never asked for.
const RECENCY_WEIGHT: f64 = 0.15;
/// MMR relevance/diversity trade-off; 0.7 favors relevance while still
/// breaking up near-duplicate clusters.
const MMR_LAMBDA: f32 = 0.7;
/// Below this mean top-k cosine, retrieval is deemed low-coverage and falls
/// back to lexical search (`L-C6`).
const MIN_COVERAGE: f32 = 0.15;
/// How many top vector hits define the coverage estimate.
const COVERAGE_TOPK: usize = 5;
/// Graph expansion seeds beyond anchors: the strongest vector hits.
const MAX_VECTOR_SEEDS: usize = 8;
/// Cap on lexical-fallback frames added.
const LEXICAL_LIMIT: usize = 8;
/// How many fused candidates survive into the MMR pass and frame construction,
/// as a multiple of the query's `max_frames`.
///
/// Everything downstream of the fusion is per-candidate work — a cosine fold
/// against every other candidate, a full clone of the node's content body, and
/// a token count over it — but `pack_to_budget` then keeps at most
/// `max_frames` of them. Before this bound the candidate list was *every live
/// node* (the recency ranking contributes all of them, at any relevance), so a
/// 5-frame recall minted and scored one frame per node in the workspace's
/// entire lifetime and discarded >99% of them.
///
/// 4x leaves the diversity pass real choice — MMR's whole job is to reject a
/// cluster of near-duplicates in favour of something further down the list, so
/// handing it exactly `max_frames` candidates would make it a no-op — while
/// keeping the pass `Θ(max_frames² )` instead of `Θ(n²)`. Floored at
/// [`LEXICAL_LIMIT`] so a small `max_frames` still considers a sane window.
const MMR_CANDIDATE_MULTIPLE: usize = 4;

/// Why a candidate frame did not make it into the assembled context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Keeping it would have exceeded the query's `max_tokens`.
    TokenBudget,
    /// The query's `max_frames` count was already reached.
    FrameCount,
}

/// A frame that was retrieved and scored but did not fit the budget. Reported
/// so assembly is never a silent truncation (`L-C5`).
#[derive(Debug, Clone)]
pub struct DroppedFrame {
    /// The frame id it would have carried (the node's `nod_…` public id), so a
    /// caller can ask for it explicitly on a follow-up query.
    pub id: String,
    /// Its citation label, so the drop report reads as names rather than ids
    /// (`L-C4`).
    pub title: String,
    /// What it would have cost, so a caller can size the budget a re-query
    /// needs instead of guessing.
    pub token_cost: u32,
    /// Which limit dropped it.
    pub reason: DropReason,
}

/// The typed, inspectable result of a recall (typed
/// outputs, not stringly telemetry). Carries the packed frames, the dropped
/// report, the coverage score, and the honesty flag for lexical fallback.
#[derive(Debug, Clone)]
pub struct RecallResult {
    /// Budget-respecting, MMR-ordered frames ready to assemble into a prompt.
    pub frames: Vec<ContextFrame>,
    /// What was scored but dropped, and why (`L-C5`).
    pub dropped: Vec<DroppedFrame>,
    /// Mean top-k vector coverage of the goal, in `[0, 1]`.
    pub coverage: f32,
    /// True when coverage fell below threshold and lexical fallback ran
    /// (`L-C6`). Individual fallback frames are also marked in their provenance.
    pub used_lexical_fallback: bool,
}

impl RecallResult {
    /// Total token cost of the assembled frames — must never exceed the
    /// query's `max_tokens` (the invariant the packer guarantees).
    #[must_use]
    pub fn assembled_tokens(&self) -> u64 {
        self.frames.iter().map(|f| f.token_cost as u64).sum()
    }
}

/// The CGP wire shape of a recall — the drop report survives as
/// `truncated`/`dropped_estimate`, so adapting a recall to the provider seam
/// never silently discards it (`L-C5`).
impl From<RecallResult> for ContextQueryResult {
    fn from(result: RecallResult) -> Self {
        ContextQueryResult {
            truncated: !result.dropped.is_empty(),
            dropped_estimate: u32::try_from(result.dropped.len()).ok(),
            frames: result.frames,
        }
    }
}

impl ContextStore {
    /// Hybrid retrieval with no domain scope — grounding drawn from the whole
    /// workspace. The CGP-shaped `ContextProvider::query` adapts this down to
    /// a [`ContextQueryResult`].
    pub async fn recall(&self, q: &ContextQuery) -> Result<RecallResult, ContextError> {
        self.recall_scoped(q, &[]).await
    }

    /// Hybrid retrieval scoped to `domains`: fuse → dedup →
    /// diversify → budget-pack → coverage gate. When `domains` is non-empty it
    /// **filters out** nodes tagged exclusively with out-of-scope domains AND
    /// **boosts** relevance by domain overlap (a frame sharing more of the
    /// query's domains ranks higher). Untagged nodes always stay candidates:
    /// most memories carry no domain tag, and a scope that dropped them would
    /// return nothing the moment a workspace taxonomy exists. An empty
    /// `domains` slice behaves exactly like [`Self::recall`]. Every returned
    /// frame carries its domains in provenance so a citation view can show
    /// them.
    ///
    /// # Query fields not honored yet
    ///
    /// Two `ContextQuery` fields are read by nothing below and are documented
    /// here rather than left for a caller to discover from surprising output:
    ///
    /// - `kinds` — kind filtering happens only at the registry seam
    ///   ([`ProviderRegistry`](crate::ProviderRegistry) routes a query away
    ///   from a provider whose declared kinds do not intersect). Once the store
    ///   *is* selected it returns every kind, so a `kinds: [Memory]` query can
    ///   come back with `File` frames.
    /// - `representation_preferences` — every frame is minted
    ///   `Representation::Full` with its whole body inline; the store never
    ///   offers a digest-only or `content_ref` representation, so a
    ///   budget-conscious host cannot ask for a cheaper shape.
    ///
    /// Both are honest gaps, not intentional policy: honoring them changes
    /// which frames and how many tokens a recall returns, so they belong in a
    /// deliberate change with its own tests, not in a silent tightening here.
    ///
    /// # Suppression happens after this, not inside it
    ///
    /// There is no forget/quarantine seam here: `stella memory forget` stores
    /// its tombstone in `store.db`, and the CLI filters the frames this call
    /// already returned. A forgotten memory therefore still competes for — and
    /// can win — a slot against `max_frames`/`max_tokens`, and the turn ends up
    /// with fewer frames rather than with a replacement. Suppression belongs
    /// upstream of packing; moving it here means an exclusion set on the query,
    /// which is an API change with its own tests.
    pub async fn recall_scoped(
        &self,
        q: &ContextQuery,
        domains: &[String],
    ) -> Result<RecallResult, ContextError> {
        // 1. Query vector: reuse the caller's if it matches our dims, else
        //    embed the query text ourselves. This is the ONLY embedding recall
        //    ever does — it never embeds stored content inline; that is warm's
        //    job (`L-C1`). So a cold store degrades to lexical, it never blocks.
        let dims = self.fingerprint().dims;
        let query_vec = match &q.embedding {
            Some(v) if v.len() == dims => v.clone(),
            _ => {
                let text = q.query_text.clone().unwrap_or_else(|| q.goal.clone());
                self.embedder()
                    .embed(&[text])
                    .await?
                    .into_iter()
                    .next()
                    .map(|e| e.vector)
                    .unwrap_or_else(|| vec![0.0; dims])
            }
        };

        let fp_id = self.fingerprint().id();
        // Steps 2–5 are synchronous SQLite plus scoring with no `.await` in
        // them, so once polled they run to completion on whichever tokio worker
        // polled the future — on the first-token path of every turn. Hand the
        // worker's other tasks to a sibling for the duration.
        without_blocking_the_worker(|| self.recall_blocking(q, domains, &query_vec, &fp_id))
    }

    /// The synchronous body of [`Self::recall_scoped`]: candidate gathering,
    /// fusion, diversification, and packing, under one lock acquisition.
    ///
    /// # Two-phase by design
    ///
    /// The corpus-wide passes (recency, domain overlap, hash dedup, the `L-C5`
    /// drop report) read only identity, time, hash, and content *size*, so they
    /// run on [`NodeMeta`] rows that leave every body in SQLite. Bodies and
    /// embedding vectors are fetched by id **after** the candidate cut, for the
    /// ≤`MMR_CANDIDATE_MULTIPLE × max_frames` rows that can still become frames.
    ///
    /// This is what the cut at [`MMR_CANDIDATE_MULTIPLE`] could not fix on its
    /// own: it bounded the per-candidate *work* but the loaders above it still
    /// materialized every live body and every decoded vector first, so a 5-frame
    /// recall's I/O and peak heap grew with lifetime memory size regardless.
    fn recall_blocking(
        &self,
        q: &ContextQuery,
        domains: &[String],
        query_vec: &[f32],
        fp_id: &str,
    ) -> Result<RecallResult, ContextError> {
        // 2. Gather candidates under one lock acquisition. The domain filter
        //    (if any) is applied here so every downstream signal sees only the
        //    in-scope nodes.
        let conn = self.conn();
        // Scope excludes only nodes whose tags are all out of scope;
        // untagged nodes pass (the overlap boost in 3b still ranks
        // in-scope tags above them). Dropping untagged nodes here silenced
        // recall completely after `stella init`: reflections and episodes
        // are commonly written with no domain tag. The exclusion set is an
        // empty no-op when `domains` is empty.
        let excluded = node_ids_excluded_by_scope(&conn, domains)?;
        // Metadata only — no content bodies cross the boundary here.
        let mut metas = live_node_metas(&conn)?;
        if !excluded.is_empty() {
            metas.retain(|m| !excluded.contains(&m.id));
        }
        let anchor_ids = node_ids_for_uris(&conn, &q.anchors)?;

        let meta_by_id: HashMap<i64, &NodeMeta> = metas.iter().map(|m| (m.id, m)).collect();

        // 3a. Vector-similarity ranking + the cosine values coverage reads.
        //     Streamed: each vector is scored straight off its BLOB and never
        //     decoded into an owned `Vec<f32>`. Only the ids and cosines are
        //     kept; the candidates' vectors are re-read after the cut.
        let mut cos_scored =
            score_nodes_by_vector(&conn, fp_id, query_vec, &excluded, cosine_blob)?;
        // Ties break on node id. The scan has no ORDER BY, so without it two
        // nodes with an identical cosine swap ranks between runs, and rank is
        // exactly what RRF scores — the same store would answer the same query
        // in a different order.
        cos_scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let coverage = coverage_score(&cos_scored);

        // 3b. Domain-overlap ranking (only when the query is domain-scoped):
        //     nodes sharing more of the query's domains rank higher. Folded
        //     into RRF like any other signal.
        //
        //     The corpus-wide tag map is loaded ONLY here, for the overlap
        //     scan. An unscoped recall needs tags for the frames it mints and
        //     nothing else, and fetches just those ([`candidate_domains`]).
        let query_domains: HashSet<&str> = domains.iter().map(String::as_str).collect();
        let scoped_domains: HashMap<i64, Vec<String>> = if query_domains.is_empty() {
            HashMap::new()
        } else {
            domains_by_node(&conn)?
        };
        let no_domains: Vec<String> = Vec::new();
        let domain_ranked: Vec<i64> = if query_domains.is_empty() {
            Vec::new()
        } else {
            let mut scored: Vec<(i64, usize)> = metas
                .iter()
                .filter_map(|m| {
                    let overlap = scoped_domains
                        .get(&m.id)
                        .unwrap_or(&no_domains)
                        .iter()
                        .filter(|d| query_domains.contains(d.as_str()))
                        .count();
                    (overlap > 0).then_some((m.id, overlap))
                })
                .collect();
            // Descending overlap, ties on node id: `nodes` arrives in SQLite's
            // unordered scan order, and overlap counts collide constantly (most
            // tagged nodes carry exactly one domain), so ordering by overlap
            // alone would hand equally-tagged nodes a different rank each run.
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            scored.into_iter().map(|(id, _)| id).collect()
        };

        // 4. Coverage gate (`L-C6`). Below threshold the vector signal is too
        //    weak to trust; rather than dress fused graph/recency hits up as
        //    grounding, serve bounded lexical matches, **explicitly labeled**.
        //    Above threshold, fuse the signals into real grounding.
        let used_lexical_fallback = coverage < MIN_COVERAGE;
        // Candidates cut before the budget pass ever sees them (the fused tail
        // beyond `MMR_CANDIDATE_MULTIPLE`). Reported alongside the budget's own
        // drops so the partition in `L-C5` still covers every scored candidate.
        // The lexical-fallback arm is already bounded by `LEXICAL_LIMIT`, so it
        // leaves this empty.
        let mut extra_dropped: Vec<DroppedFrame> = Vec::new();
        let candidates: Vec<ContextFrame> = if used_lexical_fallback {
            let terms = query_terms(q);
            let scored = lexical_search(&conn, &excluded, &terms, LEXICAL_LIMIT)?;
            let ids: Vec<i64> = scored.iter().map(|(id, _)| *id).collect();
            // Bodies for the ≤LEXICAL_LIMIT matches only.
            let bodies = nodes_by_ids(&conn, &ids)?;
            let tags = candidate_domains(&conn, &ids, &scoped_domains, &query_domains)?;
            let mut frames = Vec::with_capacity(ids.len());
            for (id, score) in scored {
                if let Some(node) = bodies.get(&id) {
                    frames.push(frame_from_node(
                        node,
                        score,
                        fp_id,
                        true,
                        tags.get(&id).unwrap_or(&no_domains).as_slice(),
                    )?);
                }
            }
            frames
        } else {
            let vector_ranked: Vec<i64> = cos_scored.iter().map(|(id, _)| *id).collect();

            // 4a. Recency ranking — recorded_at is fixed-width RFC-3339, so a
            //     descending string sort IS descending time order (no parsing).
            //     Ties break on descending rowid, which matters more than it
            //     looks: every node in one `upsert` shares a single `now`, so
            //     whole batches tie. Leaving those to the scan order ranked a
            //     batch oldest-first inside a newest-first list, and made the
            //     order depend on SQLite's unordered scan.
            let mut recency: Vec<&NodeMeta> = metas.iter().collect();
            recency.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at).then(b.id.cmp(&a.id)));
            let recency_ranked: Vec<i64> = recency.iter().map(|m| m.id).collect();

            // 4b. Graph adjacency: 1-hop from anchors + strongest vector hits.
            let mut seeds: Vec<i64> = anchor_ids.clone();
            seeds.extend(vector_ranked.iter().take(MAX_VECTOR_SEEDS).copied());
            seeds.sort_unstable();
            seeds.dedup();
            let mut graph_weight: HashMap<i64, f64> = HashMap::new();
            for &s in &seeds {
                // Seeds themselves are relevant context (an open file, a
                // mentioned symbol), so they enter the list with a base weight.
                *graph_weight.entry(s).or_insert(0.0) += 1.0;
            }
            for (neighbor, weight) in neighbors(&conn, &seeds, q.as_of.as_deref())? {
                *graph_weight.entry(neighbor).or_insert(0.0) += weight;
            }
            let mut graph_scored: Vec<(i64, f64)> = graph_weight.into_iter().collect();
            // Ties break on node id, for the same reason the cosine sort and
            // the dedup survivor do: `graph_weight` is a `HashMap`, and the
            // default edge weight is 1.0, so equally-weighted neighbors are the
            // common case rather than the exception. Their drained order is
            // exactly what RRF converts into a rank, so without the tiebreak
            // the same store answers the same query differently between runs.
            graph_scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            let graph_ranked: Vec<i64> = graph_scored.iter().map(|(id, _)| *id).collect();

            // 4c. Fuse (RRF) → dedup by content hash → MMR diversity pass.
            // Vector, graph, and domain are all grounded signals — they answer
            // "does this relate to what was asked". Recency answers "was this
            // written lately", which is a tiebreaker, not evidence, so it
            // enters damped ([`RECENCY_WEIGHT`]).
            let fused = rrf_fuse(
                &[
                    (vector_ranked, 1.0),
                    (recency_ranked, RECENCY_WEIGHT),
                    (graph_ranked, 1.0),
                    (domain_ranked, 1.0),
                ],
                RRF_K,
            );
            let ordered_all = dedup_by_content_hash(&fused, &meta_by_id);
            // Bound the candidate set BEFORE the MMR pass and before any frame
            // is built. Both are per-candidate and both are wasted on a tail
            // that `pack_to_budget` cannot keep. The cut is by fused rank, so
            // what survives is the head the ranking already judged best.
            let keep_candidates = (q.max_frames as usize)
                .saturating_mul(MMR_CANDIDATE_MULTIPLE)
                .max(LEXICAL_LIMIT);
            let considered = ordered_all.len().min(keep_candidates);
            let ordered = &ordered_all[..considered];
            // The tail is still reported — a bound that truncates silently is
            // exactly the failure `L-C5` exists to prevent. It is summarized
            // from the node rows rather than by minting frames, so reporting a
            // drop stays cheaper than not bounding at all.
            // Summarized from the metadata rows: `content_bytes` reproduces
            // `budget_tokens` exactly, so the tail is reported in full without
            // its bodies ever being read.
            let mut pre_budget_dropped: Vec<DroppedFrame> = Vec::new();
            for (id, _) in &ordered_all[considered..] {
                if let Some(meta) = meta_by_id.get(id) {
                    pre_budget_dropped.push(DroppedFrame {
                        id: meta.public_id.clone(),
                        title: meta.display_name.clone(),
                        token_cost: budget_tokens_for_bytes(meta.content_bytes),
                        reason: DropReason::FrameCount,
                    });
                }
            }

            // The cut is in: from here on, everything is per-candidate and
            // bounded by `keep_candidates`. Bodies, vectors, and domain tags are
            // fetched for exactly these ids.
            let candidate_ids: Vec<i64> = ordered.iter().map(|(id, _)| *id).collect();
            let bodies = nodes_by_ids(&conn, &candidate_ids)?;
            let candidate_vectors = vectors_for_ids(&conn, fp_id, &candidate_ids)?;
            let tags = candidate_domains(&conn, &candidate_ids, &scoped_domains, &query_domains)?;

            let max_fused = ordered.first().map(|(_, s)| *s).unwrap_or(0.0);
            let mmr_items: Vec<MmrItem<'_>> = ordered
                .iter()
                .map(|(id, s)| MmrItem {
                    relevance: if max_fused > 0.0 {
                        (*s / max_fused) as f32
                    } else {
                        0.0
                    },
                    // Borrowed, not cloned: the item is read inside this
                    // function and dropped at the end of it.
                    vector: candidate_vectors.get(id).map(Vec::as_slice),
                })
                .collect();
            let mmr_order = mmr_select(&mmr_items, MMR_LAMBDA);

            let mut frames = Vec::with_capacity(mmr_order.len());
            for &idx in &mmr_order {
                let (id, _) = ordered[idx];
                if let Some(node) = bodies.get(&id) {
                    frames.push(frame_from_node(
                        node,
                        mmr_items[idx].relevance,
                        fp_id,
                        false,
                        tags.get(&id).unwrap_or(&no_domains).as_slice(),
                    )?);
                }
            }
            extra_dropped = pre_budget_dropped;
            frames
        };

        // 5. Budget-pack; report what was dropped (`L-C5`, never silent).
        let (kept, mut dropped) = pack_to_budget(candidates, q.max_tokens, q.max_frames);
        // Candidates cut ahead of the budget pass land at the end of the
        // report: they ranked below everything the budget itself rejected.
        dropped.append(&mut extra_dropped);
        Ok(RecallResult {
            frames: kept,
            dropped,
            coverage,
            used_lexical_fallback,
        })
    }
}

/// Run a synchronous unit of work without wedging the async worker that polled
/// the future.
///
/// Recall's whole pipeline below the query embedding is blocking SQLite and
/// scoring. `block_in_place` tells the multi-thread scheduler to move the
/// worker's other tasks elsewhere while it runs, which is the difference between
/// "this turn's recall is slow" and "every task sharing this worker is stalled
/// behind it". It panics on a current-thread runtime — where there is no sibling
/// worker to hand off to anyway — so that case, and a call from outside any
/// runtime at all, run inline exactly as before.
fn without_blocking_the_worker<T>(work: impl FnOnce() -> T) -> T {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(work),
        _ => work(),
    }
}

/// `budget_tokens` over a byte count instead of the bytes themselves.
///
/// `contextgraph_types::budget_tokens` is `ceil(content.len() /
/// BYTES_PER_BUDGET_TOKEN)` over the UTF-8 byte length, so a node's declared
/// cost is computable from `NodeMeta::content_bytes` with no body in hand — and
/// is equal to it by construction, not by approximation. Pinned by
/// `byte_derived_token_cost_matches_the_protocol_function`.
fn budget_tokens_for_bytes(bytes: usize) -> u32 {
    bytes.div_ceil(BYTES_PER_BUDGET_TOKEN) as u32
}

/// Domain tags for the candidate ids, in one shape whichever arm asked.
///
/// A domain-scoped recall already loaded the corpus-wide map for the overlap
/// ranking, so the candidates are projected out of it. An unscoped recall never
/// loads that map — it needs tags only for the frames it is about to mint — and
/// fetches those by id.
fn candidate_domains(
    conn: &Connection,
    ids: &[i64],
    scoped: &HashMap<i64, Vec<String>>,
    query_domains: &HashSet<&str>,
) -> Result<HashMap<i64, Vec<String>>, ContextError> {
    if query_domains.is_empty() {
        return domains_for_nodes(conn, ids);
    }
    Ok(ids
        .iter()
        .filter_map(|id| scoped.get(id).map(|tags| (*id, tags.clone())))
        .collect())
}

/// Build a frame from a node. **Constructor-level enforcement of `L-C4`:** a
/// node without a human label yields `Err(MissingCitation)`, never a frame
/// with a bare id as its identifier.
pub(crate) fn frame_from_node(
    node: &NodeRow,
    score: f32,
    fingerprint: &str,
    lexical: bool,
    domains: &[String],
) -> Result<ContextFrame, ContextError> {
    let label = node.display_name.trim();
    if label.is_empty() {
        return Err(ContextError::MissingCitation {
            id: node.public_id.clone(),
        });
    }
    let mut provenance = vec![Provenance {
        kind: "node".into(),
        uri: node.uri.clone(),
        range: None,
        digest: Some(format!("sha256:{}", node.content_hash)),
        method: None,
        by: Some(PROVIDER_ID.into()),
    }];
    if lexical {
        provenance.push(Provenance {
            kind: "derivation".into(),
            uri: None,
            range: None,
            digest: None,
            method: Some(LEXICAL_FALLBACK_METHOD.into()),
            by: Some(PROVIDER_ID.into()),
        });
    }
    if !domains.is_empty() {
        // Domain tags ride provenance so citation views can show which
        // workspace domains a frame belongs to (user requirement: domains
        // tag all graph nodes/edges; recall scores domain overlap).
        provenance.push(Provenance {
            kind: DOMAIN_PROVENANCE_KIND.into(),
            uri: None,
            range: None,
            digest: None,
            method: Some(domains.join(",")),
            by: Some(PROVIDER_ID.into()),
        });
    }
    Ok(ContextFrame {
        id: node.public_id.clone(),
        kind: node.kind.to_frame_kind(),
        title: node.display_name.clone(),
        content: Some(node.content.clone()),
        uri: node.uri.clone(),
        score: score.clamp(0.0, 1.0),
        // §B3: the declared inline cost must equal the protocol's canonical
        // count (`budget_tokens` = ceil(bytes/4)) over the content field, with
        // no tolerance — the title is NOT part of the inline content, so it is
        // not counted here. `pack_to_budget` packs against this same value.
        token_cost: contextgraph_types::budget_tokens(&node.content),
        // `docs/context-reuse.md` §1: the frame's identity triple is
        // `(provider id, frame id, content digest)`, and a frame that declares
        // no digest is *not verifiable* — a host must re-query it rather than
        // reuse it (D4). `node.content_hash` is already the sha256 of exactly
        // the bytes that become `content`, so declaring it here costs nothing
        // and makes every store-minted frame revalidatable by `context/verify`.
        content_digest: Some(format!("sha256:{}", node.content_hash)),
        representation: Representation::Full,
        content_fidelity: None,
        canonical_content_hash: None,
        content_ref: None,
        transform: None,
        minimum_content_fidelity: None,
        inline_content_requirement: None,
        canonical_token_cost: None,
        tokenizer_ref: None,
        valid_from: None,
        valid_to: None,
        recorded_at: Some(node.recorded_at.clone()),
        provenance,
        citation_label: Some(label.to_string()),
        // The vector payload is elided; the fingerprint tags provenance.
        embedding: Some(FrameEmbedding {
            fingerprint: fingerprint.to_string(),
            vector: None,
        }),
        relations: vec![],
    })
}

/// Whether a frame is a lexical-fallback frame (`L-C6`), by inspecting its
/// provenance chain. Lets a host label weak-coverage context honestly.
///
/// Per-frame provenance is the *only* place the fallback marker crosses the
/// provider seam: the `RecallResult` → [`ContextQueryResult`] conversion keeps
/// the frames and the drop report but drops `coverage` and
/// `used_lexical_fallback`, so a CGP consumer that never reads provenance sees
/// weak-coverage frames as ordinary grounding.
#[must_use]
pub fn is_lexical_fallback(frame: &ContextFrame) -> bool {
    frame
        .provenance
        .iter()
        .any(|p| p.method.as_deref() == Some(LEXICAL_FALLBACK_METHOD))
}

// Counts cosine evaluations — `cosine` and `cosine_blob` both — so a test can
// pin recall's COMPLEXITY CLASS rather than its wall clock. The candidate bound
// is output-preserving by construction — the frames a query returns are the same
// either way — so the only thing a witness can observe is how much work was done
// to produce them. Test-only: no counter exists in a release build.
//
// Thread-local for the same reason as `store::CONTENT_BYTES_LOADED`: a global
// counter is shared with every concurrently-running test in the binary, which
// forces the ceiling to be loose enough to be uninformative.
#[cfg(test)]
thread_local! {
    static COSINE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Zero this thread's cosine counter and return the previous value.
#[cfg(test)]
pub(crate) fn take_cosine_calls() -> usize {
    COSINE_CALLS.with(|c| c.replace(0))
}

#[cfg(test)]
#[inline]
fn count_cosine() {
    COSINE_CALLS.with(|c| c.set(c.get() + 1));
}

/// Cosine similarity, guarding zero-norm vectors (defined as 0 similarity).
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(test)]
    count_cosine();
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// [`cosine`] against an *undecoded* little-endian f32 BLOB.
///
/// The whole-corpus similarity pass reads each stored vector exactly once, so
/// decoding it into an owned `Vec<f32>` first bought nothing and cost one heap
/// allocation per live node per turn. The accumulation order is identical to
/// [`cosine`]'s, so the two agree bit for bit — pinned by
/// `blob_cosine_matches_the_decoded_one`. A length that does not match the query
/// (including a blob that is not a whole number of f32s) scores 0.0, the same
/// answer [`cosine`] gives for mismatched lengths.
pub(crate) fn cosine_blob(a: &[f32], blob: &[u8]) -> f32 {
    #[cfg(test)]
    count_cosine();
    if blob.len() != a.len() * 4 {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, chunk) in a.iter().zip(blob.chunks_exact(4)) {
        let y = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Mean of the top-k positive cosine values — the goal-coverage estimate.
fn coverage_score(cos_sorted: &[(i64, f32)]) -> f32 {
    if cos_sorted.is_empty() {
        return 0.0;
    }
    let k = COVERAGE_TOPK.min(cos_sorted.len());
    let sum: f32 = cos_sorted.iter().take(k).map(|(_, c)| c.max(0.0)).sum();
    sum / k as f32
}

/// Reciprocal-rank fusion over several ranked id lists, each contributing
/// `weight / (k + rank + 1)`.
///
/// The weights exist because RRF is deliberately *flat*: with `k = 60`, rank 1
/// scores 1/61 and rank 100 scores 1/161 — barely a 2.6× spread across the
/// whole corpus. A list added at weight 1.0 is therefore not a hint, it is a
/// peer that can single-handedly decide the top of the result. See
/// [`RECENCY_WEIGHT`].
fn rrf_fuse(lists: &[(Vec<i64>, f64)], k: f64) -> HashMap<i64, f64> {
    let mut scores: HashMap<i64, f64> = HashMap::new();
    for (list, weight) in lists {
        for (rank, &id) in list.iter().enumerate() {
            *scores.entry(id).or_insert(0.0) += weight / (k + rank as f64 + 1.0);
        }
    }
    scores
}

/// The dedup key for [`dedup_by_content_hash`]. Genuinely-identical non-empty
/// content collapses under `Content`, while every empty-content node keeps its
/// own identity under `Distinct`.
#[derive(PartialEq, Eq, Hash)]
enum DedupKey<'a> {
    /// Real content: distinct nodes that share this hash are true duplicates
    /// and collapse to the strongest one.
    Content(&'a str),
    /// Empty (or whitespace-only) content: these nodes all share `sha256("")`
    /// yet are distinct identities — code-graph and taxonomy nodes routinely
    /// carry no text. Keying on the node id keeps each as its own candidate so
    /// the graph/taxonomy portion of recall is not silently collapsed.
    Distinct(i64),
}

/// Collapse fused scores to one entry per content hash (keep the strongest),
/// returning `(node_id, fused_score)` sorted by score descending. Dedup by
/// content hash is step 4. Empty-content nodes are exempt from hash dedup —
/// they share `sha256("")` despite being distinct identities, so merging them
/// would destroy graph/taxonomy recall on any initialized workspace.
fn dedup_by_content_hash(
    fused: &HashMap<i64, f64>,
    meta_by_id: &HashMap<i64, &NodeMeta>,
) -> Vec<(i64, f64)> {
    // dedup key -> (best node_id, best score)
    let mut best: HashMap<DedupKey, (i64, f64)> = HashMap::new();
    for (&id, &score) in fused {
        let Some(meta) = meta_by_id.get(&id) else {
            continue;
        };
        let key = if meta.content_blank {
            DedupKey::Distinct(id)
        } else {
            DedupKey::Content(meta.content_hash.as_str())
        };
        let entry = best.entry(key).or_insert((id, f64::MIN));
        // The lower id wins an exact tie: `fused` is a `HashMap`, so which of
        // two equally-scored duplicates is visited first varies run to run,
        // and the survivor is the frame the prompt actually cites.
        if score > entry.1 || (score == entry.1 && id < entry.0) {
            *entry = (id, score);
        }
    }
    let mut out: Vec<(i64, f64)> = best.into_values().collect();
    // Same reason: sort by score, then by id, so identical fused scores yield
    // one stable order rather than whatever the map drained.
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// One candidate for the MMR pass.
///
/// The vector is **borrowed** from the candidate-vector map. It used to be an
/// owned `Option<Vec<f32>>`, which meant a full heap copy of every candidate's
/// embedding purely so the item could own it, for a struct that never outlives
/// the function that builds it.
struct MmrItem<'a> {
    relevance: f32,
    vector: Option<&'a [f32]>,
}

/// Maximal-marginal-relevance selection. Greedily picks the item maximizing
/// `λ·relevance − (1−λ)·max_similarity_to_already_selected`. Items without a
/// vector are treated as maximally diverse (similarity 0), so graph/recency-
/// only hits are never penalized for lacking an embedding. Returns indices in
/// selection order.
///
/// The diversity penalty is a running maximum folded forward once per pick,
/// not a rescan of the selected set for every remaining candidate: `max` is
/// associative, so the two are numerically identical, but the rescan cost
/// `Θ(n³)` cosines.
///
/// This pass is `Θ(n²)` in the candidates handed to it, which is why the caller
/// bounds them to [`MMR_CANDIDATE_MULTIPLE`] x `max_frames` first. It used to be
/// fed *every live node* — the recency ranking contributes all of them — so
/// recall was quadratic in lifetime memory size and ran to exhaustion selecting
/// candidates the budget pass then threw away.
fn mmr_select(items: &[MmrItem<'_>], lambda: f32) -> Vec<usize> {
    let n = items.len();
    let mut selected: Vec<usize> = Vec::with_capacity(n);
    let mut remaining: Vec<usize> = (0..n).collect();
    // penalty[i] == max cosine between item i and anything already selected,
    // floored at 0.0 (the fold's identity) so a vector-less item scores as
    // maximally diverse.
    let mut penalty: Vec<f32> = vec![0.0; n];
    while !remaining.is_empty() {
        let mut best_pos = 0usize;
        let mut best_score = f32::MIN;
        for (pos, &idx) in remaining.iter().enumerate() {
            let mmr = lambda * items[idx].relevance - (1.0 - lambda) * penalty[idx];
            if mmr > best_score {
                best_score = mmr;
                best_pos = pos;
            }
        }
        let picked = remaining.remove(best_pos);
        selected.push(picked);
        // Fold the new pick into every still-unselected candidate's penalty.
        if let Some(picked_vec) = items[picked].vector {
            for &idx in &remaining {
                if let Some(v) = items[idx].vector {
                    penalty[idx] = penalty[idx].max(cosine(v, picked_vec));
                }
            }
        }
    }
    selected
}

/// Pack frames (already in priority order) into the token and count budgets,
/// returning `(kept, dropped)`. **Invariants (property-tested):** kept token
/// sum ≤ `max_tokens`, `kept.len()` ≤ `max_frames`, and `kept + dropped` is a
/// partition of the input (nothing vanishes silently — `L-C5`). A frame that
/// individually exceeds the remaining budget is dropped, but packing continues
/// so a smaller later frame can still fit.
pub(crate) fn pack_to_budget(
    frames: Vec<ContextFrame>,
    max_tokens: u32,
    max_frames: u32,
) -> (Vec<ContextFrame>, Vec<DroppedFrame>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut spent: u64 = 0;
    for frame in frames {
        if kept.len() as u32 >= max_frames {
            dropped.push(dropped_from(&frame, DropReason::FrameCount));
            continue;
        }
        if spent + frame.token_cost as u64 > max_tokens as u64 {
            dropped.push(dropped_from(&frame, DropReason::TokenBudget));
            continue;
        }
        spent += frame.token_cost as u64;
        kept.push(frame);
    }
    (kept, dropped)
}

fn dropped_from(frame: &ContextFrame, reason: DropReason) -> DroppedFrame {
    DroppedFrame {
        id: frame.id.clone(),
        title: frame.title.clone(),
        token_cost: frame.token_cost,
        reason,
    }
}

/// Lowercased query terms (length > 2) for lexical fallback.
fn query_terms(q: &ContextQuery) -> Vec<String> {
    let text = q.query_text.clone().unwrap_or_else(|| q.goal.clone());
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(|t| t.to_string())
        .collect()
}

/// Bounded substring/term search over stored content — the honest fallback
/// when graph/vector coverage is weak (`L-C6`). Score is the fraction of query
/// terms found in the node's content or label.
///
/// Streams the corpus past the matcher rather than taking a materialized
/// `&[NodeRow]`: the scan is inherently corpus-wide, but it no longer requires
/// the corpus to be *resident* first. Only `(id, score)` for matching nodes is
/// kept, and the ≤`limit` survivors' bodies are fetched by the caller. The
/// per-row match is byte-for-byte the one this always did.
fn lexical_search(
    conn: &Connection,
    excluded: &HashSet<i64>,
    terms: &[String],
    limit: usize,
) -> Result<Vec<(i64, f32)>, ContextError> {
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut scored = scan_lexical(conn, excluded, |display_name, content| {
        let haystack = format!("{display_name} {content}").to_lowercase();
        let hits = terms.iter().filter(|t| haystack.contains(*t)).count();
        (hits > 0).then(|| hits as f32 / terms.len() as f32)
    })?;
    // Ties break on node id. Term-fraction scores collide heavily (there are
    // only `terms.len() + 1` possible values) and the scan arrives in SQLite's
    // unordered order, so without the tiebreak the `truncate` below keeps
    // a *different set* of frames from run to run — not merely a different
    // order — which is the one thing the fallback path must not do.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(limit);
    Ok(scored)
}

#[cfg(test)]
mod tests;
