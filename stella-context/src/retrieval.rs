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
    ContextStore, NodeMeta, NodeRow, domain_ranked_ids, domains_for_nodes, lexical_node_meta,
    neighbors, node_ids_excluded_by_scope, node_ids_for_uris, node_meta_for_ids, nodes_by_ids,
    recent_node_meta, vectors_for_fingerprint,
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
pub const DEFAULT_RRF_K: f64 = 60.0;
/// How much the recency list counts for, relative to vector similarity.
///
/// Recency used to be fused at full weight, as a peer of similarity. Because
/// RRF is flat (see `rrf_fuse`), that made the N most recently written nodes
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
pub const DEFAULT_RECENCY_WEIGHT: f64 = 0.15;
/// MMR relevance/diversity trade-off; 0.7 favors relevance while still
/// breaking up near-duplicate clusters.
pub const DEFAULT_MMR_LAMBDA: f32 = 0.7;
/// Below this mean top-k cosine, retrieval is deemed low-coverage and falls
/// back to lexical search (`L-C6`).
pub const DEFAULT_MIN_COVERAGE: f32 = 0.15;
/// How many top vector hits define the coverage estimate.
pub const DEFAULT_COVERAGE_TOPK: usize = 5;
/// Graph expansion seeds beyond anchors: the strongest vector hits.
pub const DEFAULT_MAX_VECTOR_SEEDS: usize = 8;
/// Cap on lexical-fallback frames added.
pub const DEFAULT_LEXICAL_LIMIT: usize = 8;
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
/// [`DEFAULT_LEXICAL_LIMIT`] so a small `max_frames` still considers a sane window.
pub const DEFAULT_MMR_CANDIDATE_MULTIPLE: usize = 4;

/// How far down each ranked signal the fusion looks, as a multiple of the
/// shortlist it will cut to.
///
/// Not tunable, and deliberately so: it is not a quality knob but the width of
/// the window in which RRF can notice that a node appears in several lists at
/// once. Too narrow and cross-signal agreement stops being visible; wider buys
/// nothing, because a node below it in *every* list cannot out-score one above
/// it in any. It rides `max_frames` so the reads it bounds still shrink when a
/// caller asks for less.
const SIGNAL_DEPTH_MULTIPLE: usize = 8;

/// Cap on query terms the lexical fallback matches.
///
/// Each term is one `LIKE` in the fallback's SQL, so an unbounded term list
/// makes a pathological goal — a pasted stack trace, a whole file — compile
/// into a statement with hundreds of predicates over every row. The cap is
/// generous relative to a real goal sentence and is applied by truncation, so
/// the terms kept are the ones the caller wrote first.
const LEXICAL_TERM_CAP: usize = 32;

/// The knobs that shape a recall, resolved once per store.
///
/// These were eight `const`s, unreachable from the settings block that exists
/// to hold them (#712 deliverable 8). They are now data, defaulting to exactly
/// the values that shipped — a host that configures nothing gets byte-identical
/// behavior — so tuning retrieval no longer means editing and rebuilding.
///
/// Frame count and token budget are *not* here: they are per-query, not
/// per-store, and already travel on `ContextQuery`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecallTuning {
    /// Reciprocal-rank-fusion constant. See [`DEFAULT_RRF_K`].
    pub rrf_k: f64,
    /// Weight of the recency list relative to vector similarity. See
    /// [`DEFAULT_RECENCY_WEIGHT`] for why it is damped rather than a peer.
    pub recency_weight: f64,
    /// MMR relevance/diversity trade-off. See [`DEFAULT_MMR_LAMBDA`].
    pub mmr_lambda: f32,
    /// Coverage floor below which retrieval falls back to labeled lexical
    /// search (`L-C6`). See [`DEFAULT_MIN_COVERAGE`].
    pub min_coverage: f32,
    /// How many top vector hits define the coverage estimate. See
    /// [`DEFAULT_COVERAGE_TOPK`].
    pub coverage_topk: usize,
    /// Graph expansion seeds beyond anchors. See [`DEFAULT_MAX_VECTOR_SEEDS`].
    pub max_vector_seeds: usize,
    /// Cap on lexical-fallback frames. See [`DEFAULT_LEXICAL_LIMIT`].
    pub lexical_limit: usize,
    /// Shortlist size as a multiple of `max_frames`. See
    /// [`DEFAULT_MMR_CANDIDATE_MULTIPLE`].
    pub mmr_candidate_multiple: usize,
}

impl Default for RecallTuning {
    fn default() -> Self {
        Self {
            rrf_k: DEFAULT_RRF_K,
            recency_weight: DEFAULT_RECENCY_WEIGHT,
            mmr_lambda: DEFAULT_MMR_LAMBDA,
            min_coverage: DEFAULT_MIN_COVERAGE,
            coverage_topk: DEFAULT_COVERAGE_TOPK,
            max_vector_seeds: DEFAULT_MAX_VECTOR_SEEDS,
            lexical_limit: DEFAULT_LEXICAL_LIMIT,
            mmr_candidate_multiple: DEFAULT_MMR_CANDIDATE_MULTIPLE,
        }
    }
}

impl RecallTuning {
    /// Clamp every knob into the range it is meaningful over, so a
    /// misconfiguration degrades retrieval instead of breaking it.
    ///
    /// A zero shortlist multiple would make every recall empty; a zero
    /// `coverage_topk` divides by zero; a negative `rrf_k` inverts the ranking.
    /// Settings arrive from a file a person edits, so the invalid values are
    /// reachable, and failing a turn over a typo in a tuning knob is a worse
    /// answer than ignoring it.
    #[must_use]
    pub fn sanitized(self) -> Self {
        Self {
            rrf_k: if self.rrf_k.is_finite() && self.rrf_k > 0.0 {
                self.rrf_k
            } else {
                DEFAULT_RRF_K
            },
            recency_weight: if self.recency_weight.is_finite() && self.recency_weight >= 0.0 {
                self.recency_weight
            } else {
                DEFAULT_RECENCY_WEIGHT
            },
            mmr_lambda: self.mmr_lambda.clamp(0.0, 1.0),
            min_coverage: self.min_coverage.clamp(0.0, 1.0),
            coverage_topk: self.coverage_topk.max(1),
            max_vector_seeds: self.max_vector_seeds,
            lexical_limit: self.lexical_limit.max(1),
            mmr_candidate_multiple: self.mmr_candidate_multiple.max(1),
        }
    }
}

/// A ranked candidate on its way to the budget: everything packing needs, and
/// no body. Frames are built from the survivors only, which is what makes the
/// cost of a recall the frame count the caller asked for rather than the size
/// of the store.
#[derive(Debug, Clone)]
pub(crate) struct Ranked {
    /// The candidate's ranking metadata — id, label, hash, byte count.
    pub meta: NodeMeta,
    /// Its MMR-adjusted relevance, carried through packing so the frame built
    /// for a survivor declares the score the ranking gave it.
    pub relevance: f32,
}

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
    /// How many candidates the budget actually chose between —
    /// `frames.len() + dropped.len()`, always.
    ///
    /// This is the denominator [`Self::dropped`] is a numerator of, and it is
    /// the field that makes the drop report mean something. It used to be the
    /// corpus: recency contributes every live node to the fusion, so a
    /// workspace with 500 memories reported ~495 drops and permanent truncation
    /// every single turn. That number was true and useless — it described how
    /// much the workspace had accumulated, not anything a caller could change
    /// by raising a budget. Now it describes the ranked shortlist the budget
    /// was offered, so "12 of 20 dropped" is an actionable statement about this
    /// query (#712 deliverable 3).
    pub considered: usize,
    /// How many fused candidates were cut by the candidate bound *before* the
    /// budget saw them — ranked below the shortlist, never offered.
    ///
    /// Reported separately rather than folded into [`Self::dropped`] because
    /// the two say different things: a budget drop is reversible by asking for
    /// more, while these were judged not worth scoring. Reported at all because
    /// `L-C5` bans silent truncation, and a bound that vanishes from the report
    /// is exactly that.
    pub candidates_cut: usize,
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
    /// # Suppression happens before packing, in SQL
    ///
    /// A forgotten or quarantined memory is marked `node.superseded_at` in this
    /// plane, so it is excluded by the same predicate every candidate reader
    /// already applies — before ranking, before the budget, at the SQL boundary
    /// (#712 deliverable 4). It used to be filtered at the CLI projection layer
    /// *after* the budget was spent, so a suppressed memory won a slot against
    /// `max_frames` and was then discarded, silently handing that turn four
    /// frames instead of five.
    ///
    /// # What bounds the work
    ///
    /// Every signal is `LIMIT`-bounded at the SQL boundary and every bound is
    /// derived from `max_frames`, so the cost of a recall is set by what the
    /// caller asked for rather than by how long the workspace has been alive.
    /// The one pass that still touches the whole corpus is the cosine scan —
    /// brute-force top-k over the vector index, which is what a similarity
    /// search *is* without an ANN accelerator. It reads ids and vectors, never
    /// bodies: content is read for packed survivors only.
    pub async fn recall_scoped(
        &self,
        q: &ContextQuery,
        domains: &[String],
    ) -> Result<RecallResult, ContextError> {
        self.recall_scoped_excluding(q, domains, &HashSet::new())
            .await
    }

    /// [`Self::recall_scoped`], additionally suppressing `excluded` public ids
    /// **before** the budget pass.
    ///
    /// Suppression that the plane can mark on its own rows goes through
    /// [`Self::supersede_node`] and needs nothing here. This exists for the
    /// suppression it *cannot* mark: quarantine is derived — it is a count of
    /// untruthful citations in `store.db`, recomputed on every read and never
    /// stored as state — so there is no row in this database to tombstone
    /// without duplicating a derivation and letting the copy go stale.
    ///
    /// The set is applied where the candidate metadata is assembled, so an
    /// excluded memory is never ranked, never packed, and never costs a body
    /// read. The CLI's post-recall filter survives as a net, and is now
    /// provably a no-op for this provider (#712 deliverable 4).
    pub async fn recall_scoped_excluding(
        &self,
        q: &ContextQuery,
        domains: &[String],
        excluded_ids: &HashSet<String>,
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

        // 2. The bounds, all derived from what the caller asked for.
        //    `keep_candidates` is the shortlist the budget chooses between;
        //    `signal_depth` is how far down each ranked signal the fusion
        //    looks. Everything below is `LIMIT`ed by one of the two, so no
        //    read on this path grows with the size of the store.
        let tuning = self.tuning();
        let as_of = q.as_of.as_deref();
        let keep_candidates = (q.max_frames as usize)
            .saturating_mul(tuning.mmr_candidate_multiple)
            .max(tuning.lexical_limit);
        let signal_depth = keep_candidates.saturating_mul(SIGNAL_DEPTH_MULTIPLE);

        let fp_id = self.fingerprint().id();

        // 3. Gather the signals under one lock acquisition — no await is held
        //    here. Each read carries `as_of`, so a point-in-time query is
        //    answered from a single instant rather than from today's content
        //    wearing yesterday's edges (#712 deliverable 7).
        let (mut cos_scored, vectors, recency_meta, anchor_ids, domain_ranked, excluded) = {
            let conn = self.conn();
            // The cosine scan is the one pass over the whole corpus, and the
            // one that cannot be a `LIMIT` without an ANN index: "most similar"
            // is not a property SQLite can order by. It reads ids and vectors,
            // never bodies.
            let vectors = vectors_for_fingerprint(&conn, &fp_id, as_of)?;
            let mut cos_scored: Vec<(i64, f32)> = vectors
                .iter()
                .map(|(id, v)| (*id, cosine(&query_vec, v)))
                .collect();
            // Ties break on node id. `vectors_for_fingerprint` has no ORDER BY,
            // so without it two nodes with an identical cosine swap ranks
            // between runs, and rank is exactly what RRF scores — the same
            // store would answer the same query in a different order.
            cos_scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            let recency_meta = recent_node_meta(&conn, as_of, signal_depth)?;
            let anchor_ids = node_ids_for_uris(&conn, &q.anchors, as_of)?;
            let domain_ranked = domain_ranked_ids(&conn, domains, as_of, signal_depth)?;
            // Scope excludes only nodes whose tags are all out of scope;
            // untagged nodes pass (the domain-overlap signal still ranks
            // in-scope tags above them). Dropping untagged nodes here silenced
            // recall completely after `stella init`: reflections and episodes
            // are commonly written with no domain tag. The exclusion set is an
            // empty no-op when `domains` is empty.
            let excluded = node_ids_excluded_by_scope(&conn, domains, as_of)?;
            (
                cos_scored,
                vectors,
                recency_meta,
                anchor_ids,
                domain_ranked,
                excluded,
            )
        };
        // Coverage reads the full ranking, before scope narrows it: it answers
        // "does the index know anything about this goal", which is a property
        // of the corpus and not of the caller's scope.
        let coverage = coverage_score(&cos_scored, tuning.coverage_topk);
        if !excluded.is_empty() {
            cos_scored.retain(|(id, _)| !excluded.contains(id));
        }
        let vector_ranked: Vec<i64> = cos_scored
            .iter()
            .take(signal_depth)
            .map(|(id, _)| *id)
            .collect();

        // 4. Coverage gate (`L-C6`). Below threshold the vector signal is too
        //    weak to trust; rather than dress fused graph/recency hits up as
        //    grounding, serve bounded lexical matches, **explicitly labeled**.
        //    Above threshold, fuse the signals into real grounding.
        let used_lexical_fallback = coverage < tuning.min_coverage;
        let (ranked, candidates_cut) = if used_lexical_fallback {
            let terms = query_terms(q, LEXICAL_TERM_CAP);
            // A scoped query filters after the fact, so over-fetch enough that
            // scope narrowing cannot starve the fallback below its cap. With no
            // scope — the common case — this asks for exactly the cap.
            let fetch = if excluded.is_empty() {
                tuning.lexical_limit
            } else {
                tuning.lexical_limit.saturating_mul(SIGNAL_DEPTH_MULTIPLE)
            };
            let mut ranked: Vec<Ranked> = lexical_node_meta(&self.conn(), &terms, as_of, fetch)?
                .into_iter()
                .filter(|(meta, _)| {
                    !excluded.contains(&meta.id) && !excluded_ids.contains(&meta.public_id)
                })
                .map(|(meta, score)| Ranked {
                    relevance: score,
                    meta,
                })
                .collect();
            ranked.truncate(tuning.lexical_limit);
            (ranked, 0)
        } else {
            // 4a. Graph adjacency: 1-hop from anchors + the strongest vector
            //     hits. Seeds are themselves relevant context (an open file, a
            //     mentioned symbol), so they enter with a base weight.
            let mut seeds: Vec<i64> = anchor_ids.clone();
            seeds.extend(vector_ranked.iter().take(tuning.max_vector_seeds).copied());
            seeds.sort_unstable();
            seeds.dedup();
            let mut graph_weight: HashMap<i64, f64> = HashMap::new();
            for &s in &seeds {
                *graph_weight.entry(s).or_insert(0.0) += 1.0;
            }
            for (neighbor, weight) in neighbors(&self.conn(), &seeds, as_of)? {
                *graph_weight.entry(neighbor).or_insert(0.0) += weight;
            }
            graph_weight.retain(|id, _| !excluded.contains(id));
            let mut graph_scored: Vec<(i64, f64)> = graph_weight.into_iter().collect();
            // Ties break on node id, for the same reason the cosine sort and
            // the dedup survivor do: `graph_weight` is a `HashMap`, and the
            // default edge weight is 1.0, so equally-weighted neighbors are the
            // common case rather than the exception. Their drained order is
            // exactly what RRF converts into a rank, so without the tiebreak
            // the same store answers the same query differently between runs.
            graph_scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            let graph_ranked: Vec<i64> = graph_scored
                .iter()
                .take(signal_depth)
                .map(|(id, _)| *id)
                .collect();
            let recency_ranked: Vec<i64> = recency_meta
                .iter()
                .filter(|m| !excluded.contains(&m.id))
                .map(|m| m.id)
                .collect();

            // 4b. Fuse (RRF). Vector, graph, and domain are all grounded
            //     signals — they answer "does this relate to what was asked".
            //     Recency answers "was this written lately", which is a
            //     tiebreaker, not evidence, so it enters damped
            //     ([`DEFAULT_RECENCY_WEIGHT`]).
            let fused = rrf_fuse(
                &[
                    (vector_ranked, 1.0),
                    (recency_ranked, tuning.recency_weight),
                    (graph_ranked, 1.0),
                    (domain_ranked, 1.0),
                ],
                tuning.rrf_k,
            );

            // 4c. Resolve the fused ids to ranking metadata — ids, labels,
            //     hashes and byte counts, no bodies. Recency already carries
            //     its own; the rest is one bounded lookup.
            let mut meta_by_id: HashMap<i64, NodeMeta> =
                recency_meta.into_iter().map(|m| (m.id, m)).collect();
            let missing: Vec<i64> = fused
                .keys()
                .copied()
                .filter(|id| !meta_by_id.contains_key(id))
                .collect();
            for meta in node_meta_for_ids(&self.conn(), &missing, as_of)? {
                meta_by_id.insert(meta.id, meta);
            }
            // Every signal converges here, and `dedup_by_content_hash` skips a
            // fused id with no metadata — so one retain suppresses a memory
            // across the vector, recency, graph, and domain signals at once,
            // rather than four filters that can drift apart.
            if !excluded_ids.is_empty() {
                meta_by_id.retain(|_, meta| !excluded_ids.contains(&meta.public_id));
            }

            // 4d. Dedup by content hash, then cut to the shortlist. The cut is
            //     by fused rank, so what survives is the head the ranking
            //     already judged best.
            let ordered_all = dedup_by_content_hash(&fused, &meta_by_id);
            let considered = ordered_all.len().min(keep_candidates);
            let cut = ordered_all.len() - considered;
            let ordered = &ordered_all[..considered];

            // 4e. MMR over the shortlist only. The pass is `Θ(n²)` in what it
            //     is handed, and it used to be handed every live node.
            let vector_by_id: HashMap<i64, &Vec<f32>> =
                vectors.iter().map(|(id, v)| (*id, v)).collect();
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
            let ranked = mmr_select(&mmr_items, tuning.mmr_lambda)
                .into_iter()
                .filter_map(|idx| {
                    let (id, _) = ordered[idx];
                    meta_by_id.get(&id).map(|meta| Ranked {
                        meta: meta.clone(),
                        relevance: mmr_items[idx].relevance,
                    })
                })
                .collect();
            (ranked, cut)
        };

        // 5. Budget-pack the shortlist; report what the budget rejected
        //    (`L-C5`, never silent). Packing happens over metadata, so a
        //    candidate the budget refuses never costs a body read, a content
        //    clone, or a frame.
        let considered = ranked.len();
        let (kept, dropped) = pack_to_budget(ranked, q.max_tokens, q.max_frames);

        // 6. Build frames for the survivors — the only read on this path that
        //    moves content.
        let kept_ids: Vec<i64> = kept.iter().map(|r| r.meta.id).collect();
        let (rows, frame_domains) = {
            let conn = self.conn();
            (
                nodes_by_ids(&conn, &kept_ids)?,
                domains_for_nodes(&conn, &kept_ids)?,
            )
        };
        let row_by_id: HashMap<i64, NodeRow> = rows.into_iter().map(|r| (r.id, r)).collect();
        let no_domains: Vec<String> = Vec::new();
        let mut frames = Vec::with_capacity(kept.len());
        for candidate in &kept {
            // A row that vanished between packing and serving is skipped rather
            // than faked: the frame's digest must describe bytes that exist.
            let Some(row) = row_by_id.get(&candidate.meta.id) else {
                continue;
            };
            frames.push(frame_from_node(
                row,
                candidate.relevance,
                &fp_id,
                used_lexical_fallback,
                frame_domains
                    .get(&candidate.meta.id)
                    .unwrap_or(&no_domains)
                    .as_slice(),
            )?);
        }

        Ok(RecallResult {
            frames,
            dropped,
            coverage,
            used_lexical_fallback,
            considered,
            candidates_cut,
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
fn coverage_score(cos_sorted: &[(i64, f32)], topk: usize) -> f32 {
    if cos_sorted.is_empty() {
        return 0.0;
    }
    let k = topk.max(1).min(cos_sorted.len());
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
/// [`DEFAULT_RECENCY_WEIGHT`].
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
    meta_by_id: &HashMap<i64, NodeMeta>,
) -> Vec<(i64, f64)> {
    // dedup key -> (best node_id, best score)
    let mut best: HashMap<DedupKey, (i64, f64)> = HashMap::new();
    for (&id, &score) in fused {
        // A fused id with no metadata was filtered by the cutoff or the scope
        // between the signal that ranked it and the metadata read, so it is not
        // a candidate. Skipping it is what makes those filters reach every
        // signal rather than only the ones that carry their own rows.
        let Some(meta) = meta_by_id.get(&id) else {
            continue;
        };
        let key = if meta.blank {
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
    candidates: Vec<Ranked>,
    max_tokens: u32,
    max_frames: u32,
) -> (Vec<Ranked>, Vec<DroppedFrame>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut spent: u64 = 0;
    for candidate in candidates {
        if kept.len() as u32 >= max_frames {
            dropped.push(dropped_from(&candidate.meta, DropReason::FrameCount));
            continue;
        }
        if spent + candidate.meta.token_cost as u64 > max_tokens as u64 {
            dropped.push(dropped_from(&candidate.meta, DropReason::TokenBudget));
            continue;
        }
        spent += candidate.meta.token_cost as u64;
        kept.push(candidate);
    }
    (kept, dropped)
}

fn dropped_from(meta: &NodeMeta, reason: DropReason) -> DroppedFrame {
    DroppedFrame {
        id: meta.public_id.clone(),
        title: meta.display_name.clone(),
        token_cost: meta.token_cost,
        reason,
    }
}

/// Lowercased query terms (length > 2) for lexical fallback, capped at `limit`.
///
/// Splitting on every non-alphanumeric character is what makes the terms safe
/// to bind into a `LIKE`: no `%` or `_` can survive it, so the fallback's SQL
/// needs no `ESCAPE` clause.
fn query_terms(q: &ContextQuery, limit: usize) -> Vec<String> {
    let text = q.query_text.clone().unwrap_or_else(|| q.goal.clone());
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .take(limit)
        .map(|t| t.to_string())
        .collect()
}

#[cfg(test)]
mod tests;
