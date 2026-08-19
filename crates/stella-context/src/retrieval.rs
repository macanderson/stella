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
//! data, and live in [`ranking`] — this module owns the I/O and the pipeline
//! that sequences them. Brute-force top-k cosine is the default and stays the
//! default: it is exact, it is fine at CLI-local scale, and an approximation
//! that changes which frames a turn recalls is not something to switch on for
//! someone. The IVF accelerator in [`crate::ann`] is opt-in through
//! [`RecallTuning::ann_enabled`] and announces itself on
//! [`RecallResult::used_ann_index`] when it fires. They are property-tested in
//! `tests` (a `#[cfg(test)]` module, so rustdoc cannot link it).

use std::collections::{HashMap, HashSet};

use contextgraph_types::frame::FrameEmbedding;
use contextgraph_types::{ContextFrame, ContextQuery, Provenance, Representation};
use rusqlite::Connection;

use crate::candidates::{
    NodeMeta, domains_for_nodes, live_node_metas, nodes_by_ids, scan_terms, score_nodes_by_vector,
    vectors_for_ids,
};
use crate::error::ContextError;
use crate::store::{
    ContextStore, NodeRow, domains_by_node, lock_conn, neighbors_valid_at,
    node_ids_excluded_by_scope, node_ids_for_uris,
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
/// Provenance `kind` carrying a frame's [`SelectionReason`] across the CGP
/// seam — Phase 2 (#713). The `method` field holds
/// [`SelectionReason::as_str`].
pub const SELECTION_PROVENANCE_KIND: &str = "selection";
/// Provenance `kind` carrying a frame's [`RecallTier`] across the CGP seam.
/// The `method` field holds [`RecallTier::as_str`]. Written only for a frame
/// that is *not* [`RecallTier::Normal`], so the overwhelmingly common frame
/// gains no bytes and every existing frame's provenance chain is unchanged.
pub const RECALL_TIER_PROVENANCE_KIND: &str = "recall_tier";

impl ContextStore {
    /// Hybrid retrieval with no domain scope — grounding drawn from the whole
    /// workspace. The CGP-shaped `ContextProvider::query` adapts this down to
    /// a [`contextgraph_types::ContextQueryResult`].
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
    /// # Suppression happens before packing
    ///
    /// A forgotten memory is marked `node.superseded_at` in this plane, so it is
    /// excluded by the same predicate every candidate reader already applies —
    /// before ranking, before the budget, at the SQL boundary (#712 deliverable
    /// 4). It used to be filtered at the CLI projection layer *after* the budget
    /// was spent, so a suppressed memory won a slot and was then discarded,
    /// silently handing that turn four frames instead of five. Suppression the
    /// plane cannot mark on its own rows arrives through
    /// [`Self::recall_scoped_excluding`].
    /// Domain overlap **ranks** here but never **admits**: this entry point
    /// takes only a session scope, and admission evidence needs a per-query
    /// one ([`RecallScope`]). A caller that can derive one goes through
    /// [`Self::recall_scoped_excluding`].
    pub async fn recall_scoped(
        &self,
        q: &ContextQuery,
        domains: &[String],
    ) -> Result<RecallResult, ContextError> {
        self.recall_scoped_excluding(q, &RecallScope::session_only(domains), &HashSet::new())
            .await
    }

    /// [`Self::recall_scoped`], additionally suppressing `excluded_ids`
    /// **before** the budget pass.
    ///
    /// This exists for the suppression the plane cannot mark: quarantine is
    /// derived — a count of untruthful citations in `store.db`, recomputed on
    /// every read and never stored as state — so there is no row here to
    /// tombstone without duplicating a derivation and letting the copy go
    /// stale.
    ///
    /// The set is applied where the candidate metadata is assembled, so an
    /// excluded memory is never ranked, never packed, and never costs a body
    /// read. The CLI's post-recall filter survives as a net, and is now provably
    /// a no-op for this provider.
    /// This is also the only entry point that can admit on domain overlap,
    /// because it is the only one that takes a per-query scope — see
    /// [`RecallScope`] for why the two scopes cannot be one value.
    pub async fn recall_scoped_excluding(
        &self,
        q: &ContextQuery,
        scope: &RecallScope,
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

        // Steps 2-5 are synchronous SQLite plus scoring with no `.await` in
        // them, so once polled they used to run to completion on whichever tokio
        // worker polled the future — on the first-token path of every turn, with
        // no yield from the candidate load to the budget pack. That is what made
        // the two timeouts wrapped around recall unenforceable: neither
        // `pipeline`'s `recall_latency_ceiling` nor the host's per-provider
        // ceiling can fire while the task itself is not yielding, and the triage
        // call `join!`ed against recall could not actually overlap it.
        //
        // `spawn_blocking` puts the pass on the blocking pool, so the timeouts
        // are real and the worker keeps serving other tasks. It needs `'static`
        // inputs, which is why the query is projected into an owned
        // [`RecallInputs`] first — bounded by the query, never by the corpus.
        let similarity_floor = match self.embedder().similarity_posture() {
            crate::embed::SimilarityPosture::Semantic { admission_floor } => Some(admission_floor),
            crate::embed::SimilarityPosture::Surface => None,
        };
        let inputs = RecallInputs {
            anchors: q.anchors.clone(),
            as_of: q.as_of.clone(),
            // Live recall asks "what holds now"; an `as_of` query asks "what did
            // we believe then" and must keep answering it unfiltered.
            valid_at: q.as_of.is_none().then(|| self.clock().now_rfc3339()),
            excluded_ids: excluded_ids.clone(),
            tuning: self.tuning(),
            max_frames: q.max_frames,
            max_tokens: q.max_tokens,
            terms: query_terms(q),
            domains: scope.session.clone(),
            query_scope: scope.query.clone(),
            fingerprint: self.fingerprint().id(),
            similarity_floor,
        };
        let handle = self.conn_handle();
        let task = tokio::task::spawn_blocking(move || {
            let conn = lock_conn(&handle);
            let result = recall_blocking(&conn, &inputs, &query_vec);
            // The pass ran on a blocking-pool thread, so its cost counters live
            // in THAT thread's locals. Carry them out with the result or the
            // guards that read them would measure zero and pass vacuously.
            #[cfg(test)]
            let result = (result, crate::cost_counters::take());
            result
        });
        match task.await {
            Ok(result) => {
                #[cfg(test)]
                let result = {
                    let (result, counts) = result;
                    crate::cost_counters::merge(counts);
                    result
                };
                result
            }
            Err(join) => match join.try_into_panic() {
                // A panic inside the pass stays a panic, exactly as it did when
                // this ran inline. The store's locks are poison-tolerant, so the
                // next recall still works.
                Ok(payload) => std::panic::resume_unwind(payload),
                // The blocking pool is shutting down mid-recall (the runtime is
                // going away). Report it rather than serving empty grounding as
                // if the workspace had no memories.
                Err(_) => Err(ContextError::Corruption(
                    "context recall was cancelled by runtime shutdown".into(),
                )),
            },
        }
    }
}

/// The query, projected into the owned form [`recall_blocking`] needs to run on
/// the blocking pool.
///
/// Every field is bounded by the QUERY — a handful of anchors, a timestamp, two
/// budgets, the lexical terms, the active scope — so building it is a fixed small
/// cost per turn and never scales with the corpus. Notably absent: the caller's
/// embedding, which is already owned separately as the query vector, and
/// `goal`/`query_text`, whose only use downstream was producing `terms`.
struct RecallInputs {
    /// Uris to resolve to anchor node ids for the graph expansion.
    anchors: Vec<String>,
    /// Transaction-time pin for which fact edges are visible.
    as_of: Option<String>,
    /// World-time pin for which edges still *hold*, as distinct from which are
    /// still *believed* (`as_of`).
    ///
    /// `Some(now)` on the live path, so an anchor to a file that has since been
    /// deleted stops feeding graph adjacency the moment the staleness scan ends
    /// its validity. Every edge written before anchors existed has
    /// `valid_from`/`valid_to` NULL, and NULL is unbounded on both ends, so this
    /// is a no-op for all of them — it excludes exactly the edges something
    /// deliberately ended, and nothing else.
    ///
    /// `None` when the caller pinned `as_of`, which is the time-travel/audit
    /// path: reconstructing a past belief set must not be filtered by today's
    /// world, and that is the behaviour `as_of_ignores_world_validity_valid_from_valid_to`
    /// pins.
    valid_at: Option<String>,
    /// Frame-count budget — also what bounds the candidate cut.
    max_frames: u32,
    /// Token budget the packer enforces.
    max_tokens: u32,
    /// Lowercased query terms for the lexical fallback, precomputed so the
    /// blocking pass needs neither `goal` nor `query_text`.
    terms: Vec<String>,
    /// The session's domain scope (empty = whole workspace). Filters
    /// out-of-scope nodes and ranks overlap; never admits.
    domains: Vec<String>,
    /// The domains THIS query selected ([`RecallScope::query`]). Admission
    /// evidence, and only when it narrows [`Self::domains`] — the check is
    /// [`evidence::scope_is_query_conditional`], applied at the point of use so
    /// the two scopes travel together and cannot be conflated in between.
    query_scope: Vec<String>,
    /// The embedder fingerprint whose vectors this recall may read (`L-C2`).
    fingerprint: String,
    /// `Some(floor)` when the active embedder declares
    /// [`SimilarityPosture::Semantic`](crate::embed::SimilarityPosture) — a
    /// cosine at or above it is admission evidence on its own. `None` under a
    /// `Surface` posture: scores order, they never admit.
    similarity_floor: Option<f32>,
    /// Public ids this workspace suppresses, applied before the budget.
    ///
    /// Suppression the plane can mark on its own rows goes through
    /// [`ContextStore::supersede_node`] and needs nothing here. This carries the
    /// suppression it *cannot* mark: quarantine is derived — a count of
    /// untruthful citations in `store.db`, recomputed on every read and never
    /// stored as state — so there is no row in this database to tombstone
    /// without duplicating a derivation and letting the copy go stale
    /// (#712 deliverable 4).
    excluded_ids: std::collections::HashSet<String>,
    /// The ranking knobs in force, resolved from settings once per store
    /// (#712 deliverable 8).
    tuning: RecallTuning,
}

/// The synchronous body of [`ContextStore::recall_scoped`]: candidate gathering,
/// fusion, diversification, and packing, under one lock acquisition.
///
/// # Two-phase by design
///
/// The corpus-wide passes (recency, domain overlap, hash dedup, the `L-C5`
/// drop report) read only identity, time, hash, and content *size*, so they
/// run on [`NodeMeta`] rows that leave every body in SQLite. Bodies and
/// embedding vectors are fetched by id **after** the candidate cut, for the
/// ≤`DEFAULT_MMR_CANDIDATE_MULTIPLE × max_frames` rows that can still become frames.
///
/// This is what the cut at [`DEFAULT_MMR_CANDIDATE_MULTIPLE`] could not fix on its
/// own: it bounded the per-candidate *work* but the loaders above it still
/// materialized every live body and every decoded vector first, so a 5-frame
/// recall's I/O and peak heap grew with lifetime memory size regardless.
fn recall_blocking(
    conn: &Connection,
    q: &RecallInputs,
    query_vec: &[f32],
) -> Result<RecallResult, ContextError> {
    // Scope excludes only nodes whose tags are all out of scope;
    // untagged nodes pass (the overlap boost in 3b still ranks
    // in-scope tags above them). Dropping untagged nodes here silenced
    // recall completely after `stella init`: reflections and episodes
    // are commonly written with no domain tag. The exclusion set is an
    // empty no-op when `domains` is empty.
    let excluded = node_ids_excluded_by_scope(conn, &q.domains)?;
    // Metadata only — no content bodies cross the boundary here.
    let mut metas = live_node_metas(conn, q.as_of.as_deref())?;
    // Suppressed ids drop out here, where every signal converges — one filter
    // rather than four that can drift apart, and early enough that a suppressed
    // memory is never ranked, never packed, and never costs a body read
    // (#712 deliverable 4).
    if !q.excluded_ids.is_empty() {
        metas.retain(|m| !q.excluded_ids.contains(&m.public_id));
    }
    if !excluded.is_empty() {
        metas.retain(|m| !excluded.contains(&m.id));
    }
    let anchor_ids = node_ids_for_uris(conn, &q.anchors)?;

    let meta_by_id: HashMap<i64, &NodeMeta> = metas.iter().map(|m| (m.id, m)).collect();

    // 3a. Vector-similarity ranking + the cosine values coverage reads.
    //     Streamed: each vector is scored straight off its BLOB and never
    //     decoded into an owned `Vec<f32>`. Only the ids and cosines are
    //     kept; the candidates' vectors are re-read after the cut.
    //
    //     The IVF probe is tried first ONLY when the workspace opted in, and it
    //     declines (returning `None`) whenever there is no usable index — so
    //     the exact scan below is both the default and the fallback, and the
    //     two produce the identical `(score desc, id asc)` ordering over
    //     whatever candidates each considered.
    let probe = if q.tuning.ann_enabled {
        crate::ann::probe_by_vector(
            conn,
            &crate::ann::ProbeRequest {
                fingerprint: &q.fingerprint,
                query_vec,
                excluded: &excluded,
                as_of: q.as_of.as_deref(),
                probes: q.tuning.ann_probes,
                min_candidates: ann_min_candidates(q),
            },
            cosine_blob,
        )?
    } else {
        None
    };
    let (mut cos_scored, used_ann_index, ann_probes) = match probe {
        Some(p) => (p.scored, true, (p.probed_centroids, p.centroid_count)),
        None => (
            score_nodes_by_vector(
                conn,
                &q.fingerprint,
                query_vec,
                &excluded,
                q.as_of.as_deref(),
                cosine_blob,
            )?,
            false,
            (0, 0),
        ),
    };
    let vectors_scored = cos_scored.len();
    // Ties break on node id. The scan has no ORDER BY, so without it two
    // nodes with an identical cosine swap ranks between runs, and rank is
    // exactly what RRF scores — the same store would answer the same query
    // in a different order.
    cos_scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let coverage = coverage_score(&cos_scored, q.tuning.coverage_topk);

    // 3b. Domain-overlap ranking (only when the query is domain-scoped):
    //     nodes sharing more of the query's domains rank higher. Folded
    //     into RRF like any other signal.
    //
    //     The corpus-wide tag map is loaded ONLY here, for the overlap
    //     scan. An unscoped recall needs tags for the frames it mints and
    //     nothing else, and fetches just those ([`candidate_domains`]).
    let query_domains: HashSet<&str> = q.domains.iter().map(String::as_str).collect();
    let scoped_domains: HashMap<i64, Vec<String>> = if query_domains.is_empty() {
        HashMap::new()
    } else {
        domains_by_node(conn)?
    };
    let domain_ranked = scope::overlap_ranking(&metas, &scoped_domains, &query_domains);

    // 4. Coverage gate (`L-C6`). Below threshold the vector signal is too
    //    weak to trust; rather than dress fused graph/recency hits up as
    //    grounding, serve bounded lexical matches, **explicitly labeled**.
    //    Above threshold, fuse the signals into real grounding.
    let used_lexical_fallback = coverage < q.tuning.min_coverage;
    // Candidates cut before the budget pass ever sees them (the fused tail
    // beyond `DEFAULT_MMR_CANDIDATE_MULTIPLE`).
    //
    // Counted, not itemized, and reported apart from the budget's own drops
    // (#712 deliverable 3). Folding them in made `dropped` a number about the
    // workspace's accumulated size rather than about this query: on a
    // 500-memory store a 5-frame recall reported ~495 drops and permanent
    // truncation, every turn, forever. `L-C5` is still satisfied — nothing
    // vanishes unreported — but the two facts are kept distinct, because a
    // budget drop is reversible by asking for more and a cut candidate is not.
    //
    // The lexical-fallback arm is already bounded by `DEFAULT_LEXICAL_LIMIT`,
    // so it leaves this zero.
    let mut candidates_cut = 0usize;
    // Candidates the evidence gate refused: nothing tied them to this query
    // (`require_evidence`, #2289). Counted on whichever arm ran.
    let mut no_evidence_cut = 0usize;
    let candidates: Vec<Ranked> = if used_lexical_fallback {
        // One streaming pass computes both the historical term-fraction
        // scores and, when the gate is on, which matches are distinctive.
        let pass = term_pass(conn, &excluded, q)?;
        let mut scored = pass.fallback_scores();
        if q.tuning.require_evidence {
            // A match made only of glue terms (present in most of the corpus)
            // says nothing about this query; see `evidence::term_is_distinctive`.
            let distinctive = pass.distinctive_matchers();
            let before = scored.len();
            scored.retain(|(id, _)| distinctive.contains(id));
            no_evidence_cut = before - scored.len();
        }
        // Ties break on node id — same discipline, same reason as the scan
        // this replaced: term-fraction scores collide heavily, and the
        // truncate below must keep the same *set* from run to run.
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.truncate(q.tuning.lexical_limit);
        scored
            .into_iter()
            .filter_map(|(id, relevance)| {
                meta_by_id.get(&id).map(|meta| Ranked {
                    meta: (*meta).clone(),
                    relevance,
                    // Every candidate on this arm came from labeled lexical
                    // search, by construction — the arm only runs when vector
                    // coverage fell below the floor.
                    selection_reason: SelectionReason::LexicalFallback,
                })
            })
            .collect()
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
        // The expansion is split by seed origin — anchor-seeded adjacency is
        // an evidence channel (the user named the anchor; its neighborhood is
        // grounded by that), vector-seeded adjacency is ranking only (under a
        // `Surface` embedder the strongest hit certifies nothing, so neither
        // can its neighbors). The two partitions' contributions sum to
        // exactly what the single call over their union contributed.
        let mut seeds: Vec<i64> = anchor_ids.clone();
        seeds.extend(
            vector_ranked
                .iter()
                .take(q.tuning.max_vector_seeds)
                .copied(),
        );
        seeds.sort_unstable();
        seeds.dedup();
        let anchor_set: HashSet<i64> = anchor_ids.iter().copied().collect();
        let (anchor_seeds, vector_seeds): (Vec<i64>, Vec<i64>) =
            seeds.iter().partition(|s| anchor_set.contains(s));
        let mut graph_weight: HashMap<i64, f64> = HashMap::new();
        for &s in &seeds {
            // Seeds themselves are relevant context (an open file, a
            // mentioned symbol), so they enter the list with a base weight.
            *graph_weight.entry(s).or_insert(0.0) += 1.0;
        }
        let mut anchor_adjacent: HashSet<i64> = HashSet::new();
        for (neighbor, weight) in neighbors_valid_at(
            conn,
            &anchor_seeds,
            q.as_of.as_deref(),
            q.valid_at.as_deref(),
        )? {
            anchor_adjacent.insert(neighbor);
            *graph_weight.entry(neighbor).or_insert(0.0) += weight;
        }
        for (neighbor, weight) in neighbors_valid_at(
            conn,
            &vector_seeds,
            q.as_of.as_deref(),
            q.valid_at.as_deref(),
        )? {
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
        // enters damped ([`DEFAULT_RECENCY_WEIGHT`]).
        let fused = rrf_fuse(
            &[
                (vector_ranked, 1.0),
                (recency_ranked, q.tuning.recency_weight),
                (graph_ranked, 1.0),
                (domain_ranked, 1.0),
            ],
            q.tuning.rrf_k,
        );
        let mut ordered_all = dedup_by_content_hash(&fused, &meta_by_id);

        // The evidence gate (#2289): admission requires something that ties
        // the candidate to THIS query. Ranking alone enforced nothing — every
        // embedded node is in the vector list, so on any store with
        // ≥`max_frames` live nodes the budget filled every slot regardless of
        // score, and a small workspace surfaced the same frames on every
        // prompt. Note that admission is a **stricter** test than the fusion's
        // "grounded signal": domain overlap ranks above against the SESSION
        // scope, but admits only against the narrower per-query scope, because
        // only the latter varies with what was asked (see [`evidence`]'s
        // module doc, #2289 and #2333). The gate runs before the shortlist cut
        // so an inadmissible head cannot displace admissible candidates ranked
        // below it, and its refusals are counted, never silent (`L-C5`).
        if q.tuning.require_evidence {
            let pass = term_pass(conn, &excluded, q)?;
            let semantic_hits: Vec<i64> = match q.similarity_floor {
                // Only a `Semantic`-posture embedder may admit by cosine; the
                // default `Surface` posture contributes nothing here. See
                // `crate::embed::SimilarityPosture`.
                Some(floor) => cos_scored
                    .iter()
                    .filter(|(_, c)| *c >= floor)
                    .map(|(id, _)| *id)
                    .collect(),
                None => Vec::new(),
            };
            // The domain evidence channel. Free of extra I/O by construction:
            // a query scope that narrows a non-empty session scope means
            // `scoped_domains` — the corpus tag map — was already loaded for
            // the overlap ranking above, so this is a filter over rows in
            // hand, never a second scan.
            let domain_evidence: Vec<i64> =
                if evidence::scope_is_query_conditional(&q.query_scope, &q.domains) {
                    scope::evidence_ids(&metas, &scoped_domains, &q.query_scope)
                } else {
                    Vec::new()
                };
            let admissible = evidence::admissible_ids(
                &anchor_ids,
                &anchor_adjacent,
                &pass.distinctive_matchers(),
                &domain_evidence,
                &semantic_hits,
            );
            let before = ordered_all.len();
            ordered_all.retain(|(id, _)| admissible.contains(id));
            no_evidence_cut = before - ordered_all.len();
        }
        // Bound the candidate set BEFORE the MMR pass and before any frame
        // is built. Both are per-candidate and both are wasted on a tail
        // that `pack_to_budget` cannot keep. The cut is by fused rank, so
        // what survives is the head the ranking already judged best.
        let keep_candidates = (q.max_frames as usize)
            .saturating_mul(q.tuning.mmr_candidate_multiple)
            .max(q.tuning.lexical_limit);
        let considered = ordered_all.len().min(keep_candidates);
        let ordered = &ordered_all[..considered];
        // The tail is still reported — a bound that truncates silently is
        // exactly the failure `L-C5` exists to prevent — but as a count on the
        // result rather than as itemized drops. Nothing about it needs a name,
        // a title, or a token cost: a caller cannot recover a cut candidate by
        // raising a budget, so there is nothing to act on per item. Counting
        // also drops the last per-tail-candidate work on this path.
        let tail_cut = ordered_all.len() - considered;

        // The cut is in: from here on, everything is per-candidate and
        // bounded by `keep_candidates`. Only the vectors are fetched here —
        // MMR needs them. Bodies and domain tags wait until the budget has
        // chosen, so nothing is read for a candidate that will not ship.
        let candidate_ids: Vec<i64> = ordered.iter().map(|(id, _)| *id).collect();
        let candidate_vectors =
            vectors_for_ids(conn, &q.fingerprint, &candidate_ids, q.as_of.as_deref())?;

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
        let mmr_order = mmr_select(&mmr_items, q.tuning.mmr_lambda);

        candidates_cut = tail_cut;
        mmr_order
            .into_iter()
            .filter_map(|idx| {
                let (id, _) = ordered[idx];
                meta_by_id.get(&id).map(|meta| Ranked {
                    meta: (*meta).clone(),
                    relevance: mmr_items[idx].relevance,
                    // An anchor is a file the goal named verbatim. It is the
                    // one signal in this whole ranking that came from the user
                    // rather than from a heuristic, so it is required and the
                    // budget honors it before anything competes for the space
                    // (#713 deliverable 5).
                    selection_reason: if anchor_ids.contains(&id) {
                        SelectionReason::Anchored
                    } else {
                        SelectionReason::Ranked
                    },
                })
            })
            .collect()
    };

    // 5. Budget-pack; report what was dropped (`L-C5`, never silent). This runs
    //    on metadata, so a candidate the budget refuses never costs a body read
    //    or a frame.
    let (kept, dropped) = pack_to_budget(candidates, q.max_tokens, q.max_frames);
    // The denominator: exactly the candidates the budget chose between —
    // `kept + dropped` partitions them by construction. Counted here, before
    // frame construction, because the vanished-body skip below can make
    // `frames` fall short of `kept` and the budget's decision would then be
    // under-reported.
    let considered = kept.len() + dropped.len();

    // 6. Build frames for the survivors — the only read on this path that
    //    moves content.
    let kept_ids: Vec<i64> = kept.iter().map(|r| r.meta.id).collect();
    let bodies = nodes_by_ids(conn, &kept_ids, q.as_of.as_deref())?;
    let tags = candidate_domains(conn, &kept_ids, &scoped_domains, &query_domains)?;
    // An untagged frame carries an empty domain list, not a missing one.
    let no_domains: Vec<String> = Vec::new();
    let mut frames = Vec::with_capacity(kept.len());
    for candidate in &kept {
        // A row that vanished between packing and serving is skipped rather
        // than faked: a frame's digest must describe bytes that exist.
        let Some(node) = bodies.get(&candidate.meta.id) else {
            continue;
        };
        frames.push(frame_from_node(
            node,
            candidate.relevance,
            &q.fingerprint,
            used_lexical_fallback,
            tags.get(&candidate.meta.id)
                .unwrap_or(&no_domains)
                .as_slice(),
            candidate.selection_reason,
            candidate.meta.recall_tier,
        )?);
    }

    Ok(RecallResult {
        frames,
        dropped,
        coverage,
        used_lexical_fallback,
        considered,
        candidates_cut,
        no_evidence_cut,
        used_ann_index,
        ann_probes,
        vectors_scored,
    })
}

/// The candidate floor an IVF probe must clear before its width stops growing.
///
/// Every consumer of the similarity list that reads *depth* rather than the
/// shortlist is folded in here, so widening any one of them widens the probe
/// with it rather than silently outrunning it:
///
/// - `coverage_topk` — the mean this many cosines decide the `L-C6` fallback
///   gate. A probe that under-fetches lowers that mean and moves the gate.
/// - `max_vector_seeds` — the strongest hits that seed the graph expansion.
/// - `max_frames × mmr_candidate_multiple` — the shortlist itself, which the
///   vector list must be able to fill on its own when the other signals are
///   silent (a store with no edges and no domain tags).
/// - `lexical_limit` — the floor the shortlist bound already applies.
///
/// Then multiplied by [`crate::ann::PROBE_OVERFETCH`], because posting counts
/// are an upper bound on *candidates*, not on survivors: liveness, the `as_of`
/// cutoff, and the `excluded` anti-set all remove rows after the width is
/// chosen, so a probe planned to return exactly the floor can return less.
fn ann_min_candidates(q: &RecallInputs) -> usize {
    q.tuning
        .coverage_topk
        .max(q.tuning.max_vector_seeds)
        .max(q.tuning.lexical_limit)
        .max((q.max_frames as usize).saturating_mul(q.tuning.mmr_candidate_multiple))
        .saturating_mul(crate::ann::PROBE_OVERFETCH)
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
    selection_reason: SelectionReason,
    recall_tier: RecallTier,
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
    // Phase 2 (#713): why this frame was selected, on the frame itself.
    // Provenance is the only channel that survives the CGP seam — the
    // `RecallResult` → `ContextQueryResult` conversion keeps frames and a
    // collapsed drop count and nothing else — so a reason recorded anywhere
    // but here is a reason a CGP consumer never sees. Same trick, and the same
    // reason, as the lexical-fallback marker below.
    provenance.push(Provenance {
        kind: SELECTION_PROVENANCE_KIND.into(),
        uri: None,
        range: None,
        digest: None,
        method: Some(selection_reason.as_str().to_string()),
        by: Some(PROVIDER_ID.into()),
    });
    // The tier rides the same channel and for the same reason: the host packs
    // a second time across providers, and a precedence the context plane
    // honored but the host could not read would be undone there exactly as the
    // required-item guarantee was before #713. Written only when the frame is
    // deferred — a `Normal` frame is the default on both sides of the seam, so
    // saying so would add a provenance entry to every frame ever recalled and
    // change the bytes of prompts whose frames did not.
    if recall_tier != RecallTier::Normal {
        provenance.push(Provenance {
            kind: RECALL_TIER_PROVENANCE_KIND.into(),
            uri: None,
            range: None,
            digest: None,
            method: Some(recall_tier.as_str().to_string()),
            by: Some(PROVIDER_ID.into()),
        });
    }
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
        // `docs/spec/adaptive-context/context-reuse.md` §1: the frame's identity triple is
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
/// provider seam: the `RecallResult` → [`contextgraph_types::ContextQueryResult`]
/// conversion keeps the frames and the drop report but drops `coverage` and
/// `used_lexical_fallback`, so a CGP consumer that never reads provenance sees
/// weak-coverage frames as ordinary grounding.
#[must_use]
pub fn is_lexical_fallback(frame: &ContextFrame) -> bool {
    frame
        .provenance
        .iter()
        .any(|p| p.method.as_deref() == Some(LEXICAL_FALLBACK_METHOD))
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

/// The one streaming term pass a recall runs: the corpus's bodies go past
/// [`evidence::TermMatcher`] a row at a time, and only per-term document
/// frequencies plus per-node matched-term sets come back (`evidence` — the
/// admission gate — and the lexical fallback's scores are both derived from
/// it). The per-row match is byte-for-byte the substring scan the fallback
/// always did; the same call now also serves admission, so a recall never
/// scans twice.
///
/// An empty term list skips the scan outright: nothing could match, and the
/// fallback's historical empty-result short-circuit is preserved.
fn term_pass(
    conn: &Connection,
    excluded: &HashSet<i64>,
    q: &RecallInputs,
) -> Result<evidence::TermEvidence, ContextError> {
    let mut matcher = evidence::TermMatcher::new(&q.terms);
    if !q.terms.is_empty() {
        scan_terms(
            conn,
            excluded,
            q.as_of.as_deref(),
            |id, display_name, content| {
                let haystack = format!("{display_name} {content}").to_lowercase();
                matcher.observe(id, &haystack);
            },
        )?;
    }
    Ok(matcher.finish())
}

mod evidence;
mod outcome;
mod ranking;
mod scope;
mod tuning;

pub(crate) use outcome::Ranked;
pub use outcome::{DropReason, DroppedFrame, RecallResult, RecallTier, SelectionReason};
pub use scope::RecallScope;
pub use tuning::{
    DEFAULT_ANN_ENABLED, DEFAULT_ANN_PROBES, DEFAULT_COVERAGE_TOPK, DEFAULT_LEXICAL_LIMIT,
    DEFAULT_MAX_VECTOR_SEEDS, DEFAULT_MIN_COVERAGE, DEFAULT_MMR_CANDIDATE_MULTIPLE,
    DEFAULT_MMR_LAMBDA, DEFAULT_RECENCY_WEIGHT, DEFAULT_REQUIRE_EVIDENCE, DEFAULT_RRF_K,
    RecallTuning,
};

pub(crate) use ranking::{
    MmrItem, cosine, cosine_blob, coverage_score, dedup_by_content_hash, mmr_select,
    pack_to_budget, rrf_fuse,
};
// Only the packer itself spends this now that packing moved to `ranking`; the
// tests still assert the byte-derived cost equals the protocol's own count.
#[cfg(test)]
pub(crate) use ranking::budget_tokens_for_bytes;

#[cfg(test)]
mod evidence_tests;
#[cfg(test)]
mod recall_tests;
#[cfg(test)]
mod tests;
