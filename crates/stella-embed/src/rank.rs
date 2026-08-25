// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Pure cosine ranking: vectors in, an ordered answer out.
//!
//! This module is the invariant-2 half of the crate. It performs no I/O, holds
//! no handle and knows nothing about SQLite, HTTP or the code graph, which is
//! what makes it property-testable and what makes the tool output it feeds
//! byte-stable (invariant 7). The ordering is **total**: score descending,
//! then key ascending, so two candidates that tie on a float still come back
//! in one fixed order rather than whatever order the storage layer happened to
//! yield them in.

use std::cmp::Ordering;

/// One candidate's identity and its vector, as handed to [`top_k`].
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate<'a> {
    /// The candidate's stable key — a workspace-relative path, in this crate's
    /// only current caller. It is also the tie-break, so it must be unique
    /// within a call for the ordering to be total.
    pub key: &'a str,
    /// The candidate's vector, in the same space as the query.
    pub vector: &'a [f32],
}

/// A scored candidate. `score` is the cosine similarity in `[-1.0, 1.0]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    /// The candidate's key, owned so the answer outlives the borrowed input.
    pub key: String,
    /// Cosine similarity against the query vector.
    pub score: f32,
}

/// Cosine similarity between two vectors.
///
/// Vectors of differing length score `0.0` rather than panicking or
/// truncating: a mismatch means the two came from different embedders, and the
/// honest answer to "how similar are these" across vector spaces is "this
/// comparison is meaningless". Callers that can distinguish the two cases
/// filter by fingerprint *before* getting here; this is the backstop that
/// makes a leak harmless instead of wrong.
///
/// A zero vector scores `0.0` against everything, which is the defined value
/// for an undefined direction.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a <= 0.0 || norm_b <= 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

/// Rank `candidates` against `query` and return the best `limit`.
///
/// Candidates scoring below `floor` are dropped. Pass `f32::NEG_INFINITY` to
/// keep everything — a `Surface` embedder has no defensible floor, and
/// inventing one would be exactly the unmeasured number
/// [`crate::SimilarityPosture`] exists to prevent.
///
/// The result is deterministic for identical input, including ties: score
/// descending, then key ascending. A `NaN` score sorts last rather than
/// poisoning the comparator.
pub fn top_k(query: &[f32], candidates: &[Candidate<'_>], floor: f32, limit: usize) -> Vec<Scored> {
    let mut scored: Vec<Scored> = candidates
        .iter()
        .map(|candidate| Scored {
            key: candidate.key.to_string(),
            score: cosine(query, candidate.vector),
        })
        .filter(|s| !s.score.is_nan() && s.score >= floor)
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });
    scored.truncate(limit);
    scored
}

/// How much larger than the average gap a gap must be to count as the edge of
/// relevance rather than ordinary spacing.
///
/// Provisional, and **scale-free by construction**, which is the whole reason
/// it is defensible without a per-model measurement: it compares gaps in a
/// ranking against other gaps in the same ranking, so it carries no assumption
/// about where a given embedder's cosines sit. An absolute cutoff does — and
/// `DEFAULT_ADMISSION_FLOOR` is the cautionary case, sitting at 0.25 while
/// `voyage-code-3` scores unrelated files at 0.604, dropping nothing.
///
/// Measured once and not yet retuned. On 2026-08-24 the four labelled queries
/// in `crates/stella-tools/tests/relevance_calibration.rs`, run against
/// `voyage-code-3` over this repository, put the boundary at the true
/// relevant/irrelevant frontier at **+5.36x** the mean gap on the one query
/// with a clean frontier and **+0.27x** on the one whose answer ranked 12th;
/// the other two answers ranked 62nd and 77th of 28 269, below the 40-wide
/// window the boundary is read at, so they have no frontier to measure. That
/// is a window, not a miss — both were retrieved (#4784). Two usable points,
/// pointing opposite ways, is not enough to move a threshold — the ratio stays
/// where it is, and this paragraph records that it has now been looked at
/// rather than that it has been settled (#3096).
pub const DEFAULT_RELEVANCE_GAP_RATIO: f32 = 2.5;

/// The smallest drop in cosine that may be called a boundary at all.
///
/// **This exists because the ratio alone is not enough, and the number that
/// proves it is the one #3089 measured.** On that real ten-result ranking
/// (0.656 down to 0.604) the widest gap is 0.015 — which is *2.6x the mean
/// gap*, comfortably past [`DEFAULT_RELEVANCE_GAP_RATIO`], and would have cut
/// the list to a single confident result. The correct answer was not in the
/// list at all. A ratio is scale-free about the *level* of a score and says
/// nothing about its *resolution*: 0.015 is both 2.6x the mean and 1.5% of a
/// cosine, and the second reading is the true one.
///
/// So a boundary must be two independent things at once — wide relative to
/// the ranking's other gaps, and wide enough absolutely that it is not
/// measurement noise. This constant is the second test.
///
/// Provisional like [`DEFAULT_RELEVANCE_GAP_RATIO`], but far less fragile than
/// an absolute *floor*: gaps between scores are much more comparable across
/// models than the scores themselves, which is exactly why
/// `DEFAULT_ADMISSION_FLOOR` sits at 0.25 while a real backend scores
/// unrelated files at 0.604.
///
/// # What the measurement found, and why the number did not move
///
/// The 2026-08-24 run of `crates/stella-tools/tests/relevance_calibration.rs`
/// against `voyage-code-3` over this repository observed absolute boundary
/// drops of **0.0140** and **0.0003** — both far under this 0.05. So on this
/// corpus the boundary never fires, [`relevant_prefix`] returns the whole
/// list, and every ranking runs to its end. That is visible from outside as
/// 200+ hit answers and as `search`'s RANK CEILING note still appearing on
/// nearly every query, since the note's condition is precisely "the boundary
/// kept everything it was given" (#4385).
///
/// The number stays at 0.05 anyway, because the alternative is worse. Two
/// usable frontiers — one of them a query whose answer ranked 12th — is a
/// sample to report, not one to tune a threshold against, and
/// [`DEFAULT_MIN_BOUNDARY_GAP`]'s whole reason for existing is the #3089
/// ranking where a 0.015 gap looked decisive and was noise. Retuning it to
/// admit 0.014 would re-open exactly that failure on the strength of one
/// query. What settles it is more labelled queries, which is
/// `relevance_calibration.rs`'s `QUERIES` table (#3096).
pub const DEFAULT_MIN_BOUNDARY_GAP: f32 = 0.05;

/// How many of a ranked list are relevant — the answer to "where does this
/// result set stop being about the query".
///
/// # Why a count is the wrong way to shape an answer
///
/// A fixed `limit` is wrong in both directions at once. A query whose answer
/// genuinely spans forty chunks is truncated at ten and the caller cannot tell;
/// a query with two real hits returns ten and the eight passengers read as
/// findings. Neither failure is visible in the output, which is what makes it
/// worse than a wrong answer.
///
/// # The mechanism
///
/// `scored` is assumed sorted descending — [`top_k`]'s output. The largest
/// **drop** between adjacent scores is the candidate boundary, and it is
/// accepted only when it passes **both** tests: at least `gap_ratio` times the
/// mean gap across the list, *and* at least `min_gap` in absolute cosine.
///
/// Both, because either alone is wrong in a way that has already been
/// observed. Without the ratio, an evenly-spaced ranking gets cut at whichever
/// gap rounding made widest. Without `min_gap`, the ranking #3089 measured —
/// ten results spanning 0.05 — cuts to a single confident answer on a gap of
/// 0.015, and the correct answer was not in that list at all
/// ([`DEFAULT_MIN_BOUNDARY_GAP`] carries the full argument).
///
/// When no gap passes both, the honest report is that this ranking does not
/// resolve: the whole list is returned, and something downstream — a context
/// budget — has to be the one to say it could not show all of it.
///
/// Never returns zero for a non-empty list: admission is the absolute floor's
/// job ([`crate::SimilarityPosture`]), and this function's job is only to find
/// the edge *within* what was already admitted. Returning zero here would
/// silently overrule a floor that had already said yes.
///
/// Pure and total: no I/O, no clock, no panic on `NaN`, empty or single-item
/// input.
#[must_use]
pub fn relevant_prefix(scored: &[Scored], gap_ratio: f32, min_gap: f32) -> usize {
    if scored.len() <= 1 {
        return scored.len();
    }
    let gaps: Vec<f32> = scored
        .windows(2)
        .map(|pair| (pair[0].score - pair[1].score).max(0.0))
        .collect();
    let total: f32 = gaps.iter().sum();
    // Every score identical: no boundary exists anywhere, and dividing by a
    // zero mean below would produce one out of a rounding artefact.
    if total <= 0.0 {
        return scored.len();
    }
    let mean = total / gaps.len() as f32;

    let (index, widest) = gaps
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(Ordering::Equal))
        .map(|(index, gap)| (index, *gap))
        .unwrap_or((0, 0.0));

    if widest >= mean * gap_ratio && widest >= min_gap {
        index + 1
    } else {
        scored.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn scores(values: &[f32]) -> Vec<Scored> {
        values
            .iter()
            .enumerate()
            .map(|(index, score)| Scored {
                key: format!("k{index}"),
                score: *score,
            })
            .collect()
    }

    /// The shape the whole function exists for: three real hits, then a cliff,
    /// then noise. The cut lands at the cliff regardless of how much noise
    /// follows it.
    #[test]
    fn a_cliff_between_relevant_and_irrelevant_is_where_the_cut_lands() {
        let ranked = scores(&[0.91, 0.88, 0.86, 0.42, 0.41, 0.40, 0.39]);
        assert_eq!(
            relevant_prefix(
                &ranked,
                DEFAULT_RELEVANCE_GAP_RATIO,
                DEFAULT_MIN_BOUNDARY_GAP
            ),
            3
        );
    }

    /// A smoothly-decaying ranking has no boundary in it. Inventing one would
    /// be an arbitrary cut wearing a threshold's authority, so the whole list
    /// is returned and the budget is left to say what it could not show.
    #[test]
    fn a_smooth_decay_has_no_boundary_and_is_not_cut() {
        let ranked = scores(&[0.90, 0.85, 0.80, 0.75, 0.70, 0.65]);
        assert_eq!(
            relevant_prefix(
                &ranked,
                DEFAULT_RELEVANCE_GAP_RATIO,
                DEFAULT_MIN_BOUNDARY_GAP
            ),
            ranked.len()
        );
    }

    /// The exact distribution #3089 measured — ten results spanning 0.05.
    /// Nothing in it separates, and the honest answer is that this ranking
    /// does not resolve, not that the top three are the answer.
    #[test]
    fn the_file_level_ranking_that_motivated_this_does_not_resolve() {
        let ranked = scores(&[
            0.656, 0.641, 0.636, 0.628, 0.621, 0.615, 0.612, 0.607, 0.606, 0.604,
        ]);
        assert_eq!(
            relevant_prefix(
                &ranked,
                DEFAULT_RELEVANCE_GAP_RATIO,
                DEFAULT_MIN_BOUNDARY_GAP
            ),
            ranked.len(),
            "a tie dressed as a ranking must not be cut into a confident answer"
        );
    }

    #[test]
    fn one_hit_and_no_hits_are_both_answered_without_a_gap_to_measure() {
        assert_eq!(
            relevant_prefix(&[], DEFAULT_RELEVANCE_GAP_RATIO, DEFAULT_MIN_BOUNDARY_GAP),
            0
        );
        assert_eq!(
            relevant_prefix(
                &scores(&[0.9]),
                DEFAULT_RELEVANCE_GAP_RATIO,
                DEFAULT_MIN_BOUNDARY_GAP
            ),
            1
        );
    }

    #[test]
    fn an_exact_tie_across_the_whole_list_is_never_cut() {
        let ranked = scores(&[0.5, 0.5, 0.5, 0.5]);
        assert_eq!(
            relevant_prefix(
                &ranked,
                DEFAULT_RELEVANCE_GAP_RATIO,
                DEFAULT_MIN_BOUNDARY_GAP
            ),
            ranked.len()
        );
    }

    /// A single dominant hit is the other end of the same shape, and the one
    /// most easily got wrong: the cut must keep it rather than reading "one
    /// item" as "no boundary".
    #[test]
    fn one_dominant_hit_cuts_to_one() {
        let ranked = scores(&[0.95, 0.31, 0.30, 0.29, 0.28]);
        assert_eq!(
            relevant_prefix(
                &ranked,
                DEFAULT_RELEVANCE_GAP_RATIO,
                DEFAULT_MIN_BOUNDARY_GAP
            ),
            1
        );
    }

    proptest! {
        /// Never zero, never past the end. A zero would silently overrule the
        /// admission floor, which has already said these are relevant.
        #[test]
        fn the_cut_is_always_a_non_empty_prefix(
            mut values in proptest::collection::vec(0.0f32..1.0, 1..40),
            ratio in 0.1f32..8.0,
        ) {
            values.sort_by(|a, b| b.partial_cmp(a).unwrap());
            let ranked = scores(&values);
            let cut = relevant_prefix(&ranked, ratio, DEFAULT_MIN_BOUNDARY_GAP);
            prop_assert!(cut >= 1);
            prop_assert!(cut <= ranked.len());
        }

        /// Appending more noise below an existing cliff can never move the cut
        /// deeper — the answer to "what is relevant" must not grow because
        /// irrelevant things were added.
        #[test]
        fn extra_noise_below_the_cliff_never_widens_the_answer(
            tail_len in 1usize..20,
        ) {
            let head = [0.95f32, 0.93, 0.92];
            let base: Vec<f32> = head.iter().copied().chain([0.20]).collect();
            let longer: Vec<f32> = head
                .iter()
                .copied()
                .chain((0..=tail_len).map(|i| 0.20 - i as f32 * 0.001))
                .collect();
            let short_cut = relevant_prefix(&scores(&base), DEFAULT_RELEVANCE_GAP_RATIO, DEFAULT_MIN_BOUNDARY_GAP);
            let long_cut = relevant_prefix(&scores(&longer), DEFAULT_RELEVANCE_GAP_RATIO, DEFAULT_MIN_BOUNDARY_GAP);
            prop_assert!(
                long_cut <= short_cut,
                "cut widened from {short_cut} to {long_cut} when only noise was added"
            );
        }
    }
}

#[cfg(test)]
mod top_k_tests {
    use super::*;
    use proptest::prelude::*;

    fn candidates<'a>(rows: &'a [(String, Vec<f32>)]) -> Vec<Candidate<'a>> {
        rows.iter()
            .map(|(key, vector)| Candidate {
                key: key.as_str(),
                vector: vector.as_slice(),
            })
            .collect()
    }

    #[test]
    fn identical_vectors_score_one() {
        let v = [0.3f32, 0.4, 0.5];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_dimension_mismatch_scores_zero_rather_than_comparing_across_spaces() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn a_zero_vector_scores_zero_against_everything() {
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn ties_break_on_key_ascending_so_the_answer_is_byte_stable() {
        // Both candidates are the query itself: a pure score tie. The order
        // must not depend on the order the storage layer yielded rows in.
        let query = vec![1.0f32, 0.0];
        let forward = vec![
            ("zebra.rs".to_string(), vec![1.0f32, 0.0]),
            ("alpha.rs".to_string(), vec![1.0f32, 0.0]),
        ];
        let reverse: Vec<_> = forward.iter().cloned().rev().collect();
        let a = top_k(&query, &candidates(&forward), f32::NEG_INFINITY, 10);
        let b = top_k(&query, &candidates(&reverse), f32::NEG_INFINITY, 10);
        assert_eq!(a, b);
        assert_eq!(a[0].key, "alpha.rs");
    }

    #[test]
    fn the_floor_drops_candidates_rather_than_reordering_them() {
        let query = vec![1.0f32, 0.0];
        let rows = vec![
            ("near.rs".to_string(), vec![1.0f32, 0.0]),
            ("far.rs".to_string(), vec![0.0f32, 1.0]),
        ];
        let kept = top_k(&query, &candidates(&rows), 0.5, 10);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].key, "near.rs");
    }

    #[test]
    fn a_nan_score_is_dropped_not_ordered() {
        let query = vec![f32::NAN, 0.0];
        let rows = vec![("x.rs".to_string(), vec![1.0f32, 0.0])];
        assert!(top_k(&query, &candidates(&rows), f32::NEG_INFINITY, 10).is_empty());
    }

    proptest! {
        /// Ranking is a pure function of the *set* of candidates: shuffling the
        /// input can never change the answer. This is what invariant 7 needs
        /// from a ranker whose output reaches the prompt.
        #[test]
        fn ranking_is_independent_of_input_order(
            mut rows in proptest::collection::vec(
                (
                    "[a-z]{1,6}\\.rs",
                    proptest::collection::vec(-1.0f32..1.0, 4..=4),
                ),
                1..12,
            ),
            query in proptest::collection::vec(-1.0f32..1.0, 4..=4),
        ) {
            // Keys must be unique for the ordering to be total; dedup rather
            // than constrain the generator.
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            rows.dedup_by(|a, b| a.0 == b.0);
            let forward = top_k(&query, &candidates(&rows), f32::NEG_INFINITY, 5);
            let reversed: Vec<_> = rows.iter().cloned().rev().collect();
            let backward = top_k(&query, &candidates(&reversed), f32::NEG_INFINITY, 5);
            prop_assert_eq!(forward, backward);
        }

        /// `limit` is a cap, never a filter that changes what is at the top.
        #[test]
        fn truncation_is_a_prefix_of_the_full_ranking(
            mut rows in proptest::collection::vec(
                (
                    "[a-z]{1,6}\\.rs",
                    proptest::collection::vec(-1.0f32..1.0, 4..=4),
                ),
                1..12,
            ),
            query in proptest::collection::vec(-1.0f32..1.0, 4..=4),
            limit in 1usize..6,
        ) {
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            rows.dedup_by(|a, b| a.0 == b.0);
            let all = top_k(&query, &candidates(&rows), f32::NEG_INFINITY, usize::MAX);
            let capped = top_k(&query, &candidates(&rows), f32::NEG_INFINITY, limit);
            prop_assert_eq!(&capped[..], &all[..capped.len()]);
        }
    }
}
