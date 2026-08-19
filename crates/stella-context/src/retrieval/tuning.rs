//! The knobs that shape a recall, and the defaults that are the shipped
//! behavior.
//!
//! Split out of `retrieval.rs` (#3705): the pipeline that reads these knobs is
//! the module above; what the knobs *are*, what each default is, and why it
//! has the value it has is a vocabulary of its own, and the reasoning behind
//! several of them is the longest prose in the crate.
//!
//! Every constant here is the value that shipped before [`RecallTuning`]
//! existed, so a host that configures nothing gets byte-identical behavior.

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
/// (`docs/spec/adaptive-context/adaptive-context.md` §5.5). The exact full scan stays the
/// default path and therefore stays the tested one; a workspace that wants
/// sublinear recall turns it on in `context.retrieval` and gets a
/// [`crate::RecallResult::used_ann_index`] flag saying when it fired.
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
/// Whether a candidate must carry query-conditional evidence to be admitted.
///
/// **`true`, and that is a deliberate behavior change (#2289).** Before it,
/// `max_frames` was a cap that always filled: every embedded node is in the
/// vector list, so on any store with ≥`max_frames` live nodes five frames rode
/// into every turn no matter how badly they scored — a small workspace
/// surfaced the same five irrelevant memories on every call. With the gate
/// on, admission requires an anchor, anchor adjacency, a distinctive lexical
/// match, domain overlap, or a semantic-posture cosine floor (the `evidence`
/// module holds the channels; [`crate::embed::SimilarityPosture`] says why
/// the default embedder's cosine is not one of them) — and a recall where
/// nothing qualifies returns **zero frames**, which downstream already
/// renders as "no recalled context".
///
/// `false` is the documented escape hatch back to the old padding behavior,
/// for a workspace that would rather see weak matches than nothing.
pub const DEFAULT_REQUIRE_EVIDENCE: bool = true;

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
    /// Whether admission requires query-conditional evidence, or the budget
    /// may fill with the best-ranked of whatever exists. See
    /// [`DEFAULT_REQUIRE_EVIDENCE`] for why the default changed.
    pub require_evidence: bool,
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
            require_evidence: DEFAULT_REQUIRE_EVIDENCE,
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
            // A bool has no invalid range; carried so the escape hatch a file
            // sets survives sanitization.
            require_evidence: self.require_evidence,
        }
    }
}
