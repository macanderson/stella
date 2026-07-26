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
    ContextFrame, ContextQuery, ContextQueryResult, Provenance, Representation,
};

use crate::error::ContextError;
use crate::store::{
    ContextStore, NodeRow, domains_by_node, live_nodes, neighbors, node_ids_excluded_by_scope,
    node_ids_for_uris, vectors_for_fingerprint,
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

        // 2. Gather candidates under one lock acquisition — no await is
        //    held here (the graph-neighbor expansion in 4b briefly
        //    re-acquires the connection for its 1-hop lookup). The domain
        //    filter (if any) is applied here so every downstream signal
        //    sees only the in-scope nodes.
        let fp_id = self.fingerprint().id();
        let (nodes, vectors, anchor_ids, domains_by_id) = {
            let conn = self.conn();
            let mut nodes = live_nodes(&conn)?;
            let mut vectors = vectors_for_fingerprint(&conn, &fp_id)?;
            let anchor_ids = node_ids_for_uris(&conn, &q.anchors)?;
            // Scope excludes only nodes whose tags are all out of scope;
            // untagged nodes pass (the overlap boost in 3b still ranks
            // in-scope tags above them). Dropping untagged nodes here silenced
            // recall completely after `stella init`: reflections and episodes
            // are commonly written with no domain tag. The exclusion set is an
            // empty no-op when `domains` is empty.
            let excluded = node_ids_excluded_by_scope(&conn, domains)?;
            if !excluded.is_empty() {
                nodes.retain(|n| !excluded.contains(&n.id));
                vectors.retain(|(id, _)| !excluded.contains(id));
            }
            // One grouped scan (live nodes only) instead of one statement
            // per node — entries for scope-filtered nodes are harmless
            // (lookups below are keyed by candidate id only).
            let domains_by_id: HashMap<i64, Vec<String>> = domains_by_node(&conn)?;
            (nodes, vectors, anchor_ids, domains_by_id)
        };

        let node_by_id: HashMap<i64, &NodeRow> = nodes.iter().map(|n| (n.id, n)).collect();
        let vector_by_id: HashMap<i64, &Vec<f32>> =
            vectors.iter().map(|(id, v)| (*id, v)).collect();
        let no_domains: Vec<String> = Vec::new();
        let frame_domains_for = |id: &i64| domains_by_id.get(id).unwrap_or(&no_domains).as_slice();

        // 3a. Vector-similarity ranking + the cosine values coverage reads.
        let mut cos_scored: Vec<(i64, f32)> = vectors
            .iter()
            .map(|(id, v)| (*id, cosine(&query_vec, v)))
            .collect();
        // Ties break on node id. `vectors_for_fingerprint` has no ORDER BY, so
        // without it two nodes with an identical cosine swap ranks between
        // runs, and rank is exactly what RRF scores — the same store would
        // answer the same query in a different order.
        cos_scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let coverage = coverage_score(&cos_scored);

        // 3b. Domain-overlap ranking (only when the query is domain-scoped):
        //     nodes sharing more of the query's domains rank higher. Folded
        //     into RRF like any other signal.
        let query_domains: HashSet<&str> = domains.iter().map(String::as_str).collect();
        let domain_ranked: Vec<i64> = if query_domains.is_empty() {
            Vec::new()
        } else {
            let mut scored: Vec<(i64, usize)> = nodes
                .iter()
                .filter_map(|n| {
                    let overlap = frame_domains_for(&n.id)
                        .iter()
                        .filter(|d| query_domains.contains(d.as_str()))
                        .count();
                    (overlap > 0).then_some((n.id, overlap))
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
            let mut frames = Vec::new();
            for (id, score) in lexical_search(&nodes, &terms, LEXICAL_LIMIT) {
                if let Some(node) = node_by_id.get(&id) {
                    frames.push(frame_from_node(
                        node,
                        score,
                        &fp_id,
                        true,
                        frame_domains_for(&id),
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
            let mut recency: Vec<&NodeRow> = nodes.iter().collect();
            recency.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at).then(b.id.cmp(&a.id)));
            let recency_ranked: Vec<i64> = recency.iter().map(|n| n.id).collect();

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
            for (neighbor, weight) in neighbors(&self.conn(), &seeds, q.as_of.as_deref())? {
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
            let ordered_all = dedup_by_content_hash(&fused, &node_by_id);
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
            let mut pre_budget_dropped: Vec<DroppedFrame> = Vec::new();
            for (id, _) in &ordered_all[considered..] {
                if let Some(node) = node_by_id.get(id) {
                    pre_budget_dropped.push(DroppedFrame {
                        id: node.public_id.clone(),
                        title: node.display_name.clone(),
                        token_cost: contextgraph_types::budget_tokens(&node.content),
                        reason: DropReason::FrameCount,
                    });
                }
            }

            let max_fused = ordered.first().map(|(_, s)| *s).unwrap_or(0.0);
            let mmr_items: Vec<MmrItem> = ordered
                .iter()
                .map(|(id, s)| MmrItem {
                    relevance: if max_fused > 0.0 {
                        (*s / max_fused) as f32
                    } else {
                        0.0
                    },
                    vector: vector_by_id.get(id).map(|v| (*v).clone()),
                })
                .collect();
            let mmr_order = mmr_select(&mmr_items, MMR_LAMBDA);

            let mut frames = Vec::with_capacity(mmr_order.len());
            for &idx in &mmr_order {
                let (id, _) = ordered[idx];
                if let Some(node) = node_by_id.get(&id) {
                    frames.push(frame_from_node(
                        node,
                        mmr_items[idx].relevance,
                        &fp_id,
                        false,
                        frame_domains_for(&id),
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

/// Cosine similarity, guarding zero-norm vectors (defined as 0 similarity).
/// Counts [`cosine`] calls so a test can pin recall's *complexity class*
/// rather than its wall clock. The candidate bound is output-preserving by
/// construction — the frames a query returns are the same either way — so the
/// only thing a witness can observe is how much work was done to produce them.
/// Test-only: no counter exists in a release build.
#[cfg(test)]
pub(crate) static COSINE_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(test)]
    COSINE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    node_by_id: &HashMap<i64, &NodeRow>,
) -> Vec<(i64, f64)> {
    // dedup key -> (best node_id, best score)
    let mut best: HashMap<DedupKey, (i64, f64)> = HashMap::new();
    for (&id, &score) in fused {
        let Some(node) = node_by_id.get(&id) else {
            continue;
        };
        let key = if node.content.trim().is_empty() {
            DedupKey::Distinct(id)
        } else {
            DedupKey::Content(node.content_hash.as_str())
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
struct MmrItem {
    relevance: f32,
    vector: Option<Vec<f32>>,
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
fn mmr_select(items: &[MmrItem], lambda: f32) -> Vec<usize> {
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
        if let Some(picked_vec) = &items[picked].vector {
            for &idx in &remaining {
                if let Some(v) = &items[idx].vector {
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
fn lexical_search(nodes: &[NodeRow], terms: &[String], limit: usize) -> Vec<(i64, f32)> {
    if terms.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(i64, f32)> = Vec::new();
    for node in nodes {
        let haystack = format!("{} {}", node.display_name, node.content).to_lowercase();
        let hits = terms.iter().filter(|t| haystack.contains(*t)).count();
        if hits > 0 {
            scored.push((node.id, hits as f32 / terms.len() as f32));
        }
    }
    // Ties break on node id. Term-fraction scores collide heavily (there are
    // only `terms.len() + 1` possible values) and `nodes` arrives in SQLite's
    // unordered scan order, so without the tiebreak the `truncate` below keeps
    // a *different set* of frames from run to run — not merely a different
    // order — which is the one thing the fallback path must not do.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(limit);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;
    use contextgraph_types::FrameKind;
    use proptest::prelude::*;

    fn frame(id: &str, token_cost: u32) -> ContextFrame {
        ContextFrame {
            id: id.into(),
            kind: FrameKind::Snippet,
            title: id.into(),
            content: Some(String::new()),
            uri: None,
            score: 0.5,
            token_cost,
            content_digest: None,
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
            recorded_at: None,
            provenance: vec![],
            citation_label: Some(id.into()),
            embedding: None,
            relations: vec![],
        }
    }

    #[test]
    fn cosine_is_one_for_identical_and_zero_for_orthogonal() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    fn node_row(id: i64, content: &str) -> NodeRow {
        NodeRow {
            id,
            public_id: format!("nod_{id}"),
            kind: crate::store::NodeKind::Concept,
            display_name: format!("node {id}"),
            content: content.into(),
            content_hash: crate::store::sha256_hex(content),
            uri: None,
            valid_from: None,
            recorded_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn dedup_keeps_distinct_empty_content_nodes_but_collapses_true_dupes() {
        // Three distinct nodes with empty content (all share sha256("")) plus
        // two nodes with identical non-empty content (a true duplicate pair).
        let nodes = [
            node_row(1, ""),
            node_row(2, ""),
            node_row(3, ""),
            node_row(4, "the same body"),
            node_row(5, "the same body"),
        ];
        let node_by_id: HashMap<i64, &NodeRow> = nodes.iter().map(|n| (n.id, n)).collect();
        let fused: HashMap<i64, f64> = [(1, 0.5), (2, 0.4), (3, 0.3), (4, 0.9), (5, 0.8)]
            .into_iter()
            .collect();

        let out = dedup_by_content_hash(&fused, &node_by_id);
        let kept: HashSet<i64> = out.iter().map(|(id, _)| *id).collect();

        // Every distinct empty-content node survives (the graph/taxonomy recall
        // the old sha256("")-collapse destroyed).
        assert!(kept.contains(&1) && kept.contains(&2) && kept.contains(&3));
        // The true duplicate pair collapses to exactly the strongest (id 4).
        assert!(kept.contains(&4));
        assert!(!kept.contains(&5));
        // 3 distinct empties + 1 survivor of the dup pair = 4.
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn rrf_rewards_appearing_high_in_multiple_lists() {
        let fused = rrf_fuse(&[(vec![1, 2, 3], 1.0), (vec![1, 3, 2], 1.0)], 60.0);
        // id 1 is rank-0 in both lists → strictly highest.
        assert!(fused[&1] > fused[&2]);
        assert!(fused[&1] > fused[&3]);
    }

    /// Build a ranked list of `len` ids, placing specific ids at specific
    /// ranks. Filler ids start at 1000 so they never collide with the ids
    /// under test.
    fn ranked_with(placements: &[(i64, usize)], len: usize) -> Vec<i64> {
        let mut list: Vec<i64> = (0..len).map(|i| 1000 + i as i64).collect();
        for &(id, rank) in placements {
            list[rank] = id;
        }
        list
    }

    /// The contamination regression, reduced to its arithmetic.
    ///
    /// Faithful to the real failure rather than a symmetric reversal (which
    /// scores identically and proves nothing): `STALE` is the best semantic
    /// match but was written long ago, while `FRESH` is the newest row in the
    /// store with only middling similarity — exactly the shape of a previous
    /// run's leftover reflections meeting an unrelated new prompt. At equal
    /// weight recency hands `FRESH` the win; damped, similarity decides.
    #[test]
    fn recency_cannot_outrank_similarity_on_its_own() {
        const STALE: i64 = 1;
        const FRESH: i64 = 2;
        let vector_ranked = ranked_with(&[(STALE, 0), (FRESH, 50)], 200);
        let recency_ranked = ranked_with(&[(FRESH, 0), (STALE, 150)], 200);

        let equal_weight = rrf_fuse(
            &[(vector_ranked.clone(), 1.0), (recency_ranked.clone(), 1.0)],
            RRF_K,
        );
        assert!(
            equal_weight[&FRESH] > equal_weight[&STALE],
            "the pre-fix behavior this test exists to prevent: the newest row \
             beats the best semantic match purely on recency"
        );

        let damped = rrf_fuse(
            &[(vector_ranked, 1.0), (recency_ranked, RECENCY_WEIGHT)],
            RRF_K,
        );
        assert!(
            damped[&STALE] > damped[&FRESH],
            "the best semantic match must outrank the newest unrelated node"
        );
    }

    /// Damping must not *silence* recency — ordering comparably-relevant
    /// frames is its legitimate job, and a weight of 0 would be a different
    /// (also wrong) change. Adjacent similarity ranks, big recency gap: the
    /// newer one still wins.
    #[test]
    fn recency_still_reorders_comparably_relevant_frames() {
        const OLDER: i64 = 1;
        const NEWER: i64 = 2;
        // Effectively tied on similarity (ranks 10 and 11)...
        let vector_ranked = ranked_with(&[(OLDER, 10), (NEWER, 11)], 200);
        // ...and far apart in age.
        let recency_ranked = ranked_with(&[(NEWER, 0), (OLDER, 100)], 200);

        let damped = rrf_fuse(
            &[
                (vector_ranked.clone(), 1.0),
                (recency_ranked, RECENCY_WEIGHT),
            ],
            RRF_K,
        );
        assert!(
            damped[&NEWER] > damped[&OLDER],
            "recency must still decide between frames similarity ranks alike"
        );
        // Without any recency signal the similarity order stands, which is
        // what makes the assertion above attributable to recency.
        let no_recency = rrf_fuse(&[(vector_ranked, 1.0)], RRF_K);
        assert!(no_recency[&OLDER] > no_recency[&NEWER]);
    }

    #[test]
    fn packing_respects_frame_count() {
        let frames = vec![frame("a", 1), frame("b", 1), frame("c", 1)];
        let (kept, dropped) = pack_to_budget(frames, 1000, 2);
        assert_eq!(kept.len(), 2);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].reason, DropReason::FrameCount);
    }

    #[test]
    fn packing_skips_an_oversized_frame_but_fits_a_later_small_one() {
        let frames = vec![frame("big", 500), frame("small", 10)];
        let (kept, dropped) = pack_to_budget(frames, 100, 10);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "small");
        assert_eq!(dropped[0].id, "big");
        assert_eq!(dropped[0].reason, DropReason::TokenBudget);
    }

    #[test]
    fn missing_citation_is_a_constructor_error() {
        let node = NodeRow {
            id: 1,
            public_id: "nod_x".into(),
            kind: crate::store::NodeKind::Concept,
            display_name: "   ".into(), // whitespace-only → no human label
            content: "body".into(),
            content_hash: "h".into(),
            uri: None,
            valid_from: None,
            recorded_at: "2026-01-01T00:00:00Z".into(),
        };
        let err = frame_from_node(&node, 0.5, "fp", false, &[]).unwrap_err();
        assert!(matches!(err, ContextError::MissingCitation { .. }));
    }

    #[test]
    fn lexical_frames_are_labeled_in_provenance() {
        let node = NodeRow {
            id: 1,
            public_id: "nod_x".into(),
            kind: crate::store::NodeKind::Concept,
            display_name: "a concept".into(),
            content: "body".into(),
            content_hash: "h".into(),
            uri: None,
            valid_from: None,
            recorded_at: "2026-01-01T00:00:00Z".into(),
        };
        let frame = frame_from_node(&node, 0.5, "fp", true, &["billing".to_string()]).unwrap();
        assert!(
            is_lexical_fallback(&frame),
            "fallback frames must be labeled"
        );
        let graph_frame = frame_from_node(&node, 0.5, "fp", false, &[]).unwrap();
        assert!(!is_lexical_fallback(&graph_frame));
    }

    proptest! {
        /// The core budgeting guarantee (`L-C5`): the packer never exceeds
        /// either budget, and no frame is silently lost — kept ⊎ dropped == in.
        #[test]
        fn packing_never_exceeds_budget_and_loses_nothing(
            costs in prop::collection::vec(0u32..300, 0..40),
            max_tokens in 0u32..500,
            max_frames in 0u32..20,
        ) {
            let n = costs.len();
            let frames: Vec<ContextFrame> =
                costs.iter().enumerate().map(|(i, c)| frame(&format!("f{i}"), *c)).collect();
            let (kept, dropped) = pack_to_budget(frames, max_tokens, max_frames);
            let kept_tokens: u64 = kept.iter().map(|f| f.token_cost as u64).sum();
            prop_assert!(kept_tokens <= max_tokens as u64);
            prop_assert!(kept.len() as u32 <= max_frames);
            prop_assert_eq!(kept.len() + dropped.len(), n);
        }
    }

    // End-to-end recall over a real store (public-API integration)

    use crate::clock::FixedClock;
    use crate::embed::HashEmbedder;
    use crate::store::{ContextStore, NodeInput, NodeKind};
    use crate::writeback::ContextDelta;
    use contextgraph_types::ContextQuery;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn base_query(goal: &str, query_text: &str) -> ContextQuery {
        ContextQuery {
            goal: goal.into(),
            query_text: Some(query_text.into()),
            embedding: None,
            kinds: vec![],
            anchors: vec![],
            max_frames: 10,
            max_tokens: 4000,
            as_of: None,
            representation_preferences: vec![],
        }
    }

    async fn seeded() -> (TempDir, ContextStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let store = ContextStore::open_with(
            &path,
            Arc::new(HashEmbedder::default()),
            FixedClock::shared(1_000),
        )
        .unwrap();
        store
            .upsert(
                ContextDelta::new()
                    .with_node(
                        NodeInput::new(NodeKind::File, "src/store.rs")
                            .with_uri("file:///repo/src/store.rs")
                            .with_content(
                                "open the sqlite connection in wal mode with foreign keys on",
                            ),
                    )
                    .with_node(
                        NodeInput::new(NodeKind::Artifact, "notes").with_content(
                            "render a bar chart of quarterly revenue in the dashboard",
                        ),
                    )
                    .with_node(
                        NodeInput::new(NodeKind::Concept, "budgeting").with_content(
                            "pack context frames to the token budget and report drops",
                        ),
                    ),
            )
            .await
            .unwrap();
        (dir, store)
    }

    /// Witness for the unbounded candidate set. The MMR pass is `Θ(n²)` in the
    /// candidates handed to it, and it used to be handed *every live node* —
    /// the recency ranking contributes all of them at any relevance — so a
    /// 5-frame recall did quadratic work in lifetime memory size and threw
    /// >99% of it away at the budget pass.
    ///
    /// This asserts the complexity class, not the wall clock: with the bound
    /// in place the MMR fold is quadratic in `max_frames`, so total cosines
    /// stay near the unavoidable `n` similarity scorings. Without it the fold
    /// alone costs ~n²/2, which at n=120 is over 7000 extra calls.
    #[tokio::test]
    async fn recall_does_not_scale_quadratically_with_lifetime_memory_size() {
        use std::sync::atomic::Ordering as AtomicOrdering;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let store = ContextStore::open_with(
            &path,
            Arc::new(HashEmbedder::default()),
            FixedClock::shared(1_000),
        )
        .unwrap();

        // A workspace with some history. Every one of these is live, so every
        // one lands in the recency ranking and therefore in the fused list.
        const NODES: usize = 120;
        let mut delta = ContextDelta::new();
        for i in 0..NODES {
            delta = delta.with_node(
                NodeInput::new(NodeKind::Artifact, format!("note-{i}"))
                    .with_content(format!("note number {i} about packing frames to a budget")),
            );
        }
        store.upsert(delta).await.unwrap();

        let mut q = base_query("open the database", "packing frames to a budget");
        q.max_frames = 5;

        COSINE_CALLS.store(0, AtomicOrdering::Relaxed);
        let result = store.recall(&q).await.unwrap();
        let calls = COSINE_CALLS.load(AtomicOrdering::Relaxed);

        assert!(
            result.frames.len() <= 5,
            "the frame budget still binds: {}",
            result.frames.len()
        );

        // The similarity scoring pass legitimately touches every stored vector
        // once. The MMR fold on top must be bounded by the candidate cut
        // (5 x 4 = 20 candidates → at most ~20²/2 = 200 more), NOT by n²/2.
        let unbounded_mmr_cost = NODES * NODES / 2;
        let generous_ceiling = NODES + unbounded_mmr_cost / 4;
        assert!(
            calls < generous_ceiling,
            "recall made {calls} cosine calls for {NODES} nodes and 5 frames; an \
             unbounded MMR pass would cost about {} on top of the {NODES} scorings, \
             which is the quadratic blowup this bound exists to remove",
            unbounded_mmr_cost
        );
    }

    /// The bound must not become a silent truncation — `L-C5` requires every
    /// scored candidate to appear in exactly one of `frames` / `dropped`.
    #[tokio::test]
    async fn candidates_cut_before_the_budget_pass_are_still_reported() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let store = ContextStore::open_with(
            &path,
            Arc::new(HashEmbedder::default()),
            FixedClock::shared(1_000),
        )
        .unwrap();

        const NODES: usize = 60;
        let mut delta = ContextDelta::new();
        for i in 0..NODES {
            delta = delta.with_node(
                NodeInput::new(NodeKind::Artifact, format!("note-{i}"))
                    .with_content(format!("note number {i} about packing frames to a budget")),
            );
        }
        store.upsert(delta).await.unwrap();

        let mut q = base_query("open the database", "packing frames to a budget");
        q.max_frames = 5;
        let result = store.recall(&q).await.unwrap();

        assert_eq!(
            result.frames.len() + result.dropped.len(),
            NODES,
            "kept + dropped must partition every scored candidate, not just the \
             ones that reached the budget pass"
        );
    }

    #[tokio::test]
    async fn recall_returns_cited_budget_respecting_frames() {
        let (_dir, store) = seeded().await;
        let q = base_query(
            "open the database",
            "open the sqlite connection in wal mode with foreign keys on",
        );
        let result = store.recall(&q).await.unwrap();
        assert!(!result.frames.is_empty(), "recall found grounding");
        assert!(
            result.assembled_tokens() <= q.max_tokens as u64,
            "packing respects the budget"
        );
        // The strongly-matching node is retrieved (coverage should be high).
        assert!(
            result.coverage >= MIN_COVERAGE,
            "coverage {} too low",
            result.coverage
        );
        assert!(
            !result.used_lexical_fallback,
            "strong coverage → real grounding, not fallback"
        );
        assert!(
            result
                .frames
                .iter()
                .any(|f| f.content.as_deref().unwrap_or("").contains("sqlite"))
        );
        // Every frame is humanly citable (`L-C4`).
        assert!(result.frames.iter().all(|f| {
            f.citation_label
                .as_deref()
                .map(|l| !l.is_empty())
                .unwrap_or(false)
        }));
    }

    #[tokio::test]
    async fn recalled_frames_declare_honest_token_cost() {
        // §B3: a full frame's `token_cost` MUST equal `budget_tokens` over its
        // inline content — exact, no tolerance. This drives the real `recall`
        // builder with a query that provably surfaces a frame (same shape as
        // `recall_returns_cited_budget_respecting_frames`), so the check has a
        // non-empty result to bite on rather than passing vacuously.
        let (_dir, store) = seeded().await;
        let q = base_query(
            "open the database",
            "open the sqlite connection in wal mode with foreign keys on",
        );
        let result = store.recall(&q).await.unwrap();
        assert!(!result.frames.is_empty(), "the query must surface a frame");
        for frame in &result.frames {
            assert!(
                frame.declares_honest_token_cost(),
                "frame {:?} declares token_cost {} but its content is worth {} budget tokens (§B3)",
                frame.id,
                frame.token_cost,
                frame.expected_inline_token_cost(),
            );
        }
    }

    #[tokio::test]
    async fn recall_reports_dropped_frames_under_a_tight_frame_budget() {
        let (_dir, store) = seeded().await;
        let mut q = base_query(
            "open the database",
            "open the sqlite connection in wal mode",
        );
        q.max_frames = 1;
        let result = store.recall(&q).await.unwrap();
        assert_eq!(result.frames.len(), 1, "only one frame fits");
        assert!(
            !result.dropped.is_empty(),
            "the rest are reported dropped, never silent (L-C5)"
        );
        assert!(
            result
                .dropped
                .iter()
                .all(|d| d.reason == DropReason::FrameCount)
        );
    }

    #[tokio::test]
    async fn recall_falls_back_to_labeled_lexical_when_no_vectors_under_fingerprint() {
        // Seed vectors under fingerprint rev "1", then recall through a store
        // whose active embedder is rev "2": its vector index is empty for this
        // content, so coverage is 0 and retrieval honestly falls back to
        // lexical search — and labels those frames (`L-C6`). This also proves
        // retrieval never mixes fingerprints (`L-C2`).
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        {
            let store_a = ContextStore::open_with(
                &path,
                Arc::new(HashEmbedder::with_revision("1")),
                FixedClock::shared(1_000),
            )
            .unwrap();
            store_a
                .upsert(
                    ContextDelta::new().with_node(
                        NodeInput::new(NodeKind::Concept, "flux capacitor note")
                            .with_content("the flux capacitor requires exactly gigawatts"),
                    ),
                )
                .await
                .unwrap();
        }
        let store_b = ContextStore::open_with(
            &path,
            Arc::new(HashEmbedder::with_revision("2")),
            FixedClock::shared(2_000),
        )
        .unwrap();
        let q = base_query("capacitor question", "flux capacitor");
        let result = store_b.recall(&q).await.unwrap();
        assert_eq!(result.coverage, 0.0, "no rev-2 vectors → zero coverage");
        assert!(result.used_lexical_fallback);
        assert!(
            !result.frames.is_empty(),
            "lexical search found the node by term"
        );
        assert!(
            result.frames.iter().all(is_lexical_fallback),
            "every fallback frame is labeled, never dressed up as grounding (L-C6)"
        );
    }
}
