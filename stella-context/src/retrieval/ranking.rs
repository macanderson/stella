//! The ranking math: similarity, fusion, diversification, and the budget pack.
//!
//! Split out of [`super`] when it crossed the 1500-line ratchet (#629). The
//! seam is not arbitrary — everything here is a **pure function of already-
//! loaded candidate metadata**. Nothing in this module touches the connection,
//! reads a body, or knows what a `ContextStore` is; the parent module owns the
//! I/O and the pipeline that sequences these steps.
//!
//! That is also why the whole file is testable without a store: the packer and
//! the fusion are exactly the parts whose behavior is worth pinning directly,
//! and `retrieval/tests.rs` reaches them through `use super::*`.

use std::collections::HashMap;

use contextgraph_types::BYTES_PER_BUDGET_TOKEN;

use crate::candidates::NodeMeta;

use super::{DropReason, DroppedFrame, Ranked, RecallTier};

/// `budget_tokens` over a byte count instead of the bytes themselves.
///
/// `contextgraph_types::budget_tokens` is `ceil(content.len() /
/// BYTES_PER_BUDGET_TOKEN)` over the UTF-8 byte length, so a node's declared
/// cost is computable from `NodeMeta::content_bytes` with no body in hand — and
/// is equal to it by construction, not by approximation. Pinned by
/// `byte_derived_token_cost_matches_the_protocol_function`.
pub(crate) fn budget_tokens_for_bytes(bytes: usize) -> u32 {
    bytes.div_ceil(BYTES_PER_BUDGET_TOKEN) as u32
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
pub(crate) fn coverage_score(cos_sorted: &[(i64, f32)], topk: usize) -> f32 {
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
pub(crate) fn rrf_fuse(lists: &[(Vec<i64>, f64)], k: f64) -> HashMap<i64, f64> {
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
pub(crate) enum DedupKey<'a> {
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
pub(crate) fn dedup_by_content_hash(
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
pub(crate) struct MmrItem<'a> {
    pub(crate) relevance: f32,
    pub(crate) vector: Option<&'a [f32]>,
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
pub(crate) fn mmr_select(items: &[MmrItem<'_>], lambda: f32) -> Vec<usize> {
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
///
/// **Required items are admitted first** — Phase 2 (#713) deliverable 5.
/// [`SelectionReason::is_required`] marks a candidate the caller asked for by
/// name (today: a goal that names a file verbatim), and ADR 0006 says ranking
/// may not evict one. A required item is therefore charged against the token
/// budget before any ranked candidate competes for it, and **`max_frames`
/// bounds the ranked admissions only**. Counting required items against the
/// frame budget would let it evict one for `FrameCount`, which is exactly the
/// eviction the ADR forbids — so a query for five frames that anchors two
/// files yields up to seven, and the caller gets what it asked for plus what
/// it named. The result stays bounded: anchors are capped where they are
/// extracted, and the token budget still applies to every one of them.
///
/// The two passes exist purely to reserve budget; **the kept set is emitted in
/// the original candidate order**, so the ranking's ordering — and with it the
/// byte-stability of the rendered block — is untouched. A packer that returned
/// required-first would reorder the prompt whenever an anchor appeared, which
/// is a cache-prefix change (spec §5.1) bought for nothing.
///
/// The one way a required item is dropped is that the token budget cannot
/// hold it — and the drop reason says which way that happened. A cost above
/// `max_tokens` on its own is [`DropReason::RequiredOverBudget`]: no ordering
/// could have admitted it. A required item that fits alone but not after
/// earlier required items were charged is [`DropReason::TokenBudget`]: that is
/// budget pressure a caller can relieve by raising `max_tokens`, and labeling
/// it "could never fit" would misreport exactly the drops a bigger budget
/// fixes. Either way the drop is explicit and reported, never a rank it
/// happened to lose.
pub(crate) fn pack_to_budget(
    candidates: Vec<Ranked>,
    max_tokens: u32,
    max_frames: u32,
) -> (Vec<Ranked>, Vec<DroppedFrame>) {
    let mut spent: u64 = 0;
    let mut dropped = Vec::new();
    // Pass 1: required items take their budget first, in order. Only the
    // ranked passes come later, so `spent` here is what EARLIER required
    // items charged — which is why the drop reason must be derived from the
    // item's own cost, not from the remainder: "does not fit the remainder"
    // means "could never fit" only for a cost above the whole budget.
    let mut admitted = vec![false; candidates.len()];
    for (index, candidate) in candidates.iter().enumerate() {
        if !candidate.is_required() {
            continue;
        }
        let cost = budget_tokens_for_bytes(candidate.meta.content_bytes);
        if spent + cost as u64 > max_tokens as u64 {
            let reason = if cost as u64 > max_tokens as u64 {
                DropReason::RequiredOverBudget
            } else {
                // Crowded out by earlier required items: ordinary budget
                // pressure, reversible by asking for more tokens.
                DropReason::TokenBudget
            };
            dropped.push(dropped_from(&candidate.meta, reason));
            continue;
        }
        spent += cost as u64;
        admitted[index] = true;
    }
    // Pass 2: everything else competes for what is left, in rank order.
    // `max_frames` bounds the RANKED admissions only — see the doc comment:
    // counting required items against it would let the count budget evict one,
    // which is precisely what ADR 0006 forbids.
    //
    // The pass runs once per precedence band, `Normal` before `Deferred`, so a
    // deferred candidate is admitted only out of what the normal ones left. The
    // walk within a band is still strict rank order, and a band is not a score:
    // with budget to spare every candidate is admitted exactly as before, and
    // the tier decides nothing until something has to be dropped. Two passes
    // over a shortlist that `max_frames * mmr_candidate_multiple` already
    // bounded, so the extra walk is not a cost worth avoiding.
    let mut ranked_kept = 0usize;
    for band in [RecallTier::Normal, RecallTier::Deferred] {
        for (index, candidate) in candidates.iter().enumerate() {
            if candidate.is_required() || candidate.meta.recall_tier != band {
                continue;
            }
            let cost = budget_tokens_for_bytes(candidate.meta.content_bytes);
            if ranked_kept as u32 >= max_frames {
                dropped.push(dropped_from(&candidate.meta, DropReason::FrameCount));
                continue;
            }
            if spent + cost as u64 > max_tokens as u64 {
                dropped.push(dropped_from(&candidate.meta, DropReason::TokenBudget));
                continue;
            }
            spent += cost as u64;
            ranked_kept += 1;
            admitted[index] = true;
        }
    }
    let kept = candidates
        .into_iter()
        .zip(&admitted)
        .filter_map(|(candidate, keep)| keep.then_some(candidate))
        .collect();
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
