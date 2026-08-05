//! Deterministic k-means — the clustering the IVF index is built from.
//!
//! # Why there is no PRNG here, at all
//!
//! Textbook k-means seeds its centroids at random (k-means++ included: its
//! seeding is randomized, only its *distribution* is clever). This one may not.
//! `docs/spec/adaptive-context/adaptive-context.md` §5.3 requires that identical inputs produce
//! byte-identical output, and the retrieval plane around it already keeps that
//! bargain everywhere it ranks — every tie in `crate::retrieval` breaks on the
//! lower node id precisely so the same store answers the same query the same
//! way twice.
//!
//! A seeded PRNG would satisfy "reproducible" and still fail the requirement in
//! the way that matters: the seed becomes a hidden input, and the index built by
//! `stella memory index` on Tuesday would only match Wednesday's if the seed
//! were also persisted and honored by every future build. So there is no seed to
//! forget. Initialization is a fixed stride over the input; the input arrives
//! ordered by `content_hash`; the iteration count is fixed; and every assignment
//! tie breaks on the lower centroid id. Two builds over the same rows produce
//! byte-identical centroids, which is what
//! `building_the_index_twice_is_byte_identical` pins.
//!
//! Float arithmetic is deterministic given a fixed operation order, so the
//! accumulation below is written to have one: sums are folded in input order,
//! never in a `HashMap`'s drain order.

use crate::embed::l2_normalize;

/// How many assign/update rounds a build runs.
///
/// Fixed rather than "iterate to convergence" for two reasons. Convergence is
/// itself a threshold — a float comparison that can flip on a different CPU's
/// FMA contraction — so it would reintroduce the platform dependence the fixed
/// stride exists to remove. And the marginal quality of rounds past ~8 is small
/// next to their cost: each round is `n × k` cosines, which at the `k = √n` this
/// module picks is `n^1.5` per round.
pub(super) const ITERATIONS: usize = 8;

/// The result of a build: one centroid per cluster, and the cluster each input
/// vector landed in (parallel to the input slice, by index).
pub(super) struct Clustering {
    /// Cluster representatives, L2-normalized. Position is the centroid id.
    pub(super) centroids: Vec<Vec<f32>>,
    /// `assignment[i]` is the centroid id of input vector `i`.
    pub(super) assignment: Vec<usize>,
}

/// Cluster `vectors` into at most `k` groups.
///
/// `k` is capped at `vectors.len()`; `k == 0` — like an empty input — yields
/// an empty clustering rather than being clamped up to 1. Every vector is assigned, including a zero-norm one (empty
/// content projects to the zero vector under [`crate::embed::HashEmbedder`]):
/// it scores 0.0 against every centroid, ties everywhere, and therefore lands in
/// centroid 0 — deterministically, rather than being dropped and silently
/// excluded from every probe.
pub(super) fn cluster(vectors: &[Vec<f32>], k: usize) -> Clustering {
    let n = vectors.len();
    if n == 0 || k == 0 {
        return Clustering {
            centroids: Vec::new(),
            assignment: Vec::new(),
        };
    }
    let k = k.min(n);

    // Initialization: a fixed stride through the input, which arrives ordered
    // by `content_hash`. Content hashes are sha256, so this is a spread over an
    // arbitrary-but-stable permutation of the corpus — the property random
    // seeding is after, obtained without a random number.
    let mut centroids: Vec<Vec<f32>> = (0..k).map(|j| vectors[j * n / k].clone()).collect();

    let mut assignment = vec![0usize; n];
    for _ in 0..ITERATIONS {
        assign(vectors, &centroids, &mut assignment);
        update(vectors, &assignment, &mut centroids);
    }
    // One final assignment so what is stored in `ann_assignment` is consistent
    // with what is stored in `ann_centroid`: a probe scores the query against
    // the centroids, so a posting list that answers to the *previous* round's
    // centroids would put vectors in lists whose representative no longer
    // describes them.
    assign(vectors, &centroids, &mut assignment);

    Clustering {
        centroids,
        assignment,
    }
}

/// Point every vector at its nearest centroid. Ties — which are the common case
/// for a zero vector and are reachable for any two equidistant centroids — go to
/// the **lower centroid id**, the same explicit-tiebreak rule every ranking in
/// `crate::retrieval` follows.
fn assign(vectors: &[Vec<f32>], centroids: &[Vec<f32>], out: &mut [usize]) {
    for (i, v) in vectors.iter().enumerate() {
        let mut best = 0usize;
        let mut best_score = f32::NEG_INFINITY;
        for (j, c) in centroids.iter().enumerate() {
            let s = similarity(v, c);
            // Strictly greater: the first (lowest-id) centroid holding the best
            // score keeps it.
            if s > best_score {
                best_score = s;
                best = j;
            }
        }
        out[i] = best;
    }
}

/// Recompute each centroid as the L2-normalized mean of its members.
///
/// An **empty cluster keeps its previous centroid** rather than being reseeded.
/// Reseeding is the standard repair and it is wrong here: every repair strategy
/// worth having (steal from the largest cluster, take the furthest point) is a
/// choice made from a tie, and one more place for a nondeterminism to enter.
/// An empty cluster costs a probe nothing — it has no postings — so carrying a
/// dead centroid is cheaper than the determinism it would buy back.
fn update(vectors: &[Vec<f32>], assignment: &[usize], centroids: &mut [Vec<f32>]) {
    let dims = centroids.first().map_or(0, Vec::len);
    let mut sums = vec![vec![0.0f32; dims]; centroids.len()];
    let mut counts = vec![0usize; centroids.len()];
    // Folded in input order — the input is `content_hash`-ordered, so the
    // addition sequence is fixed and the float sum is reproducible.
    for (i, v) in vectors.iter().enumerate() {
        let j = assignment[i];
        counts[j] += 1;
        for (acc, x) in sums[j].iter_mut().zip(v.iter()) {
            *acc += *x;
        }
    }
    for (j, centroid) in centroids.iter_mut().enumerate() {
        if counts[j] == 0 {
            continue;
        }
        let inv = 1.0 / counts[j] as f32;
        let mut mean: Vec<f32> = sums[j].iter().map(|x| x * inv).collect();
        l2_normalize(&mut mean);
        *centroid = mean;
    }
}

/// Cosine similarity between two owned vectors — the clustering metric.
///
/// Deliberately **not** [`crate::retrieval::cosine`], despite computing the same
/// number: that function increments the test-only cosine counter, and those
/// counters exist to measure what a *recall* cost. A build is not a recall, and
/// folding an offline `stella memory index` into a per-turn budget assertion
/// would make the sublinearity guard measure the wrong thing entirely.
fn similarity(a: &[f32], b: &[f32]) -> f32 {
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

/// The cluster count a corpus of `n` vectors is built with: `ceil(√n)`, the
/// standard IVF rule of thumb.
///
/// It is the width that balances the two halves of a probe. Scoring the query
/// against every centroid is `k` cosines and reading one posting list is about
/// `n/k`, so total probe work is `k + p·n/k`, minimized near `√n` for a small
/// probe count `p`. Anything much smaller makes each posting list a scan of the
/// corpus in miniature; anything much larger makes the centroid pass itself the
/// linear cost the index exists to remove.
pub(super) fn cluster_count(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    // `isqrt` truncates, so `+1` unless n is a perfect square: ceil(√n).
    let root = n.isqrt();
    if root * root == n { root } else { root + 1 }
}
