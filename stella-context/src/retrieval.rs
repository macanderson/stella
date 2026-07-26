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
//! data. Brute-force top-k cosine is the default and stays the default: it is
//! exact, it is fine at CLI-local scale, and an approximation that changes which
//! frames a turn recalls is not something to switch on for someone. The IVF
//! accelerator in [`crate::ann`] is opt-in through [`RecallTuning::ann_enabled`]
//! and announces itself on [`RecallResult::used_ann_index`] when it fires. They
//! are property-tested at the bottom of the file.

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
    ContextStore, NodeRow, domains_by_node, lock_conn, neighbors, node_ids_excluded_by_scope,
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
/// Whether the IVF accelerator (the crate-private `ann` module) serves the
/// similarity scan.
///
/// **`false`, and that is the decision, not a placeholder.** An approximate
/// index changes which frames a turn recalls, and making that the silent default
/// would contradict the honesty posture the rest of this module is built on
/// (`docs/design/adaptive-context.md` §5.5). The exact full scan stays the
/// default path and therefore stays the tested one; a workspace that wants
/// sublinear recall turns it on in `context.retrieval` and gets a
/// [`RecallResult::used_ann_index`] flag saying when it fired.
pub const DEFAULT_ANN_ENABLED: bool = false;
/// How many centroid posting lists an enabled probe reads before over-fetch
/// widens it.
///
/// Against the `ceil(√n)` centroids the `ann` module builds, a fixed probe count
/// means the probed *fraction* shrinks as the corpus grows — 12 of 20 lists at
/// 400 vectors, 12 of 100 at 10,000 — which is where the sublinearity comes
/// from. It is a floor, never a cap: the probe widens itself until the postings
/// it will read cover the depth `coverage_topk` and `max_vector_seeds` actually
/// consume.
///
/// **12 is measured, not guessed.** On the blended synthetic corpus in
/// `ann/tests.rs`, recall@10 against the exact scan comes out 0.925 at 8 probes
/// and 0.950 at 12, and 12 is the smallest width that holds ≥0.93 at every
/// corpus size from 200 to 5,000 vectors. Going to 16 buys 0.975 for another
/// third of the probe cost; the trade is a setting, which is why this is a
/// default rather than a constant.
pub const DEFAULT_ANN_PROBES: usize = 12;

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
    /// Whether the IVF accelerator may serve the similarity scan. Off by
    /// default; see [`DEFAULT_ANN_ENABLED`] for why that is a decision rather
    /// than caution.
    pub ann_enabled: bool,
    /// Centroid posting lists an enabled probe reads, before over-fetch widens
    /// it. See [`DEFAULT_ANN_PROBES`].
    pub ann_probes: usize,
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
            ann_enabled: DEFAULT_ANN_ENABLED,
            ann_probes: DEFAULT_ANN_PROBES,
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
            ann_enabled: self.ann_enabled,
            // Zero probes would read no posting list at all and hand the
            // ranking nothing but the unassigned tail — an empty recall on a
            // full store. Clamped like every other knob rather than rejected.
            ann_probes: self.ann_probes.max(1),
        }
    }
}

/// A ranked candidate on its way to the budget: everything packing needs, and
/// no body.
///
/// Packing used to run on `ContextFrame`s, which meant every candidate on the
/// shortlist had its body cloned and its frame minted before the budget got a
/// say — and the budget then discarded most of them. `NodeMeta` already carries
/// the two things a packer reads (a token cost and something to name a drop
/// by), so the frames are built after the cut instead: "frame construction only
/// for packed survivors" (#712 deliverable 2).
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
    /// Whether the IVF index served the similarity scan instead of the exact
    /// one.
    ///
    /// Additive and explicit, following the [`Self::considered`] precedent, and
    /// for the same reason: the alternative is a caller *inferring* it from a
    /// settings file plus a build watermark plus a drift threshold, three facts
    /// that can each be true while the probe still declined. This is the only
    /// place a recall says which scan actually ran.
    ///
    /// `false` on a store with the setting on but no index, a stale index, or a
    /// lexical-fallback turn — in every one of those the exact scan ran and the
    /// result is what an unindexed store would have returned.
    pub used_ann_index: bool,
    /// How many centroid posting lists the probe read, and how many exist:
    /// `(probed, total)`. `(0, 0)` when the exact scan ran.
    ///
    /// The ratio is the honest summary of how approximate this recall was — 8
    /// of 100 says far more about what the ranking did *not* look at than a bare
    /// "approximate: true", and it is the number to raise `ann_probes` against
    /// if a frame that should have been recalled was not.
    pub ann_probes: (usize, usize),
    /// How many vectors the similarity pass cosine-scored — the probe's
    /// candidate count when [`Self::used_ann_index`], the whole live corpus
    /// under the fingerprint when it is not.
    ///
    /// This is the denominator behind the acceleration claim, so it is reported
    /// rather than asserted in prose: a caller comparing it to their memory
    /// count can see exactly what fraction of the corpus was considered.
    pub vectors_scored: usize,
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
    pub async fn recall_scoped(
        &self,
        q: &ContextQuery,
        domains: &[String],
    ) -> Result<RecallResult, ContextError> {
        self.recall_scoped_excluding(q, domains, &HashSet::new())
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
        let inputs = RecallInputs {
            anchors: q.anchors.clone(),
            as_of: q.as_of.clone(),
            excluded_ids: excluded_ids.clone(),
            tuning: self.tuning(),
            max_frames: q.max_frames,
            max_tokens: q.max_tokens,
            terms: query_terms(q),
            domains: domains.to_vec(),
            fingerprint: self.fingerprint().id(),
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
    /// Frame-count budget — also what bounds the candidate cut.
    max_frames: u32,
    /// Token budget the packer enforces.
    max_tokens: u32,
    /// Lowercased query terms for the lexical fallback, precomputed so the
    /// blocking pass needs neither `goal` nor `query_text`.
    terms: Vec<String>,
    /// The active domain scope (empty = whole workspace).
    domains: Vec<String>,
    /// The embedder fingerprint whose vectors this recall may read (`L-C2`).
    fingerprint: String,
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
    let candidates: Vec<Ranked> = if used_lexical_fallback {
        let scored = lexical_search(
            conn,
            &excluded,
            &q.terms,
            q.tuning.lexical_limit,
            q.as_of.as_deref(),
        )?;
        scored
            .into_iter()
            .filter_map(|(id, relevance)| {
                meta_by_id.get(&id).map(|meta| Ranked {
                    meta: (*meta).clone(),
                    relevance,
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
        let mut seeds: Vec<i64> = anchor_ids.clone();
        seeds.extend(
            vector_ranked
                .iter()
                .take(q.tuning.max_vector_seeds)
                .copied(),
        );
        seeds.sort_unstable();
        seeds.dedup();
        let mut graph_weight: HashMap<i64, f64> = HashMap::new();
        for &s in &seeds {
            // Seeds themselves are relevant context (an open file, a
            // mentioned symbol), so they enter the list with a base weight.
            *graph_weight.entry(s).or_insert(0.0) += 1.0;
        }
        for (neighbor, weight) in neighbors(conn, &seeds, q.as_of.as_deref())? {
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
        let ordered_all = dedup_by_content_hash(&fused, &meta_by_id);
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
                })
            })
            .collect()
    };

    // 5. Budget-pack; report what was dropped (`L-C5`, never silent). This runs
    //    on metadata, so a candidate the budget refuses never costs a body read
    //    or a frame.
    let (kept, dropped) = pack_to_budget(candidates, q.max_tokens, q.max_frames);
    // The denominator: exactly the candidates the budget chose between, so
    // `frames + dropped` partitions it by construction.
    let considered = kept.len() + dropped.len();

    // 6. Build frames for the survivors — the only read on this path that
    //    moves content.
    let kept_ids: Vec<i64> = kept.iter().map(|r| r.meta.id).collect();
    let bodies = nodes_by_ids(conn, &kept_ids, q.as_of.as_deref())?;
    let tags = candidate_domains(conn, &kept_ids, &scoped_domains, &query_domains)?;
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
        )?);
    }

    Ok(RecallResult {
        frames,
        dropped,
        coverage,
        used_lexical_fallback,
        considered,
        candidates_cut,
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

/// Cosine similarity, guarding zero-norm vectors (defined as 0 similarity).
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(test)]
    crate::cost_counters::add_cosine();
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
    crate::cost_counters::add_cosine();
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
/// bounds them to [`DEFAULT_MMR_CANDIDATE_MULTIPLE`] x `max_frames` first. It used to be
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
    candidates: Vec<Ranked>,
    max_tokens: u32,
    max_frames: u32,
) -> (Vec<Ranked>, Vec<DroppedFrame>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut spent: u64 = 0;
    for candidate in candidates {
        let cost = budget_tokens_for_bytes(candidate.meta.content_bytes);
        if kept.len() as u32 >= max_frames {
            dropped.push(dropped_from(&candidate.meta, DropReason::FrameCount));
            continue;
        }
        if spent + cost as u64 > max_tokens as u64 {
            dropped.push(dropped_from(&candidate.meta, DropReason::TokenBudget));
            continue;
        }
        spent += cost as u64;
        kept.push(candidate);
    }
    (kept, dropped)
}

fn dropped_from(meta: &NodeMeta, reason: DropReason) -> DroppedFrame {
    DroppedFrame {
        id: meta.public_id.clone(),
        title: meta.display_name.clone(),
        token_cost: budget_tokens_for_bytes(meta.content_bytes),
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
    as_of: Option<&str>,
) -> Result<Vec<(i64, f32)>, ContextError> {
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut scored = scan_lexical(conn, excluded, as_of, |display_name, content| {
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
