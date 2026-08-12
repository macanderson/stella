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

#[cfg(test)]
mod tests {
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
