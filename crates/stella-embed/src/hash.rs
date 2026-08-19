// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! [`HashEmbedder`] — the pure-Rust, offline, zero-download fallback, and the
//! [`l2_normalize`] helper every backend shares.
//!
//! # Why a hashing embedder is only ever the fallback
//!
//! It is a hashed character-n-gram projection: deterministic, allocation-light
//! and genuinely useful for near-duplicate detection, and **not semantic**. It
//! cannot map "the code that handles telemetry drains" onto a file called
//! `sink.rs`, because the two share no character trigram. It therefore
//! declares [`SimilarityPosture::Surface`] and its scores may order candidates
//! but never admit one. When it is the active backend, a semantic query is
//! answered with a labelled degradation, not with a confident wrong answer.

use async_trait::async_trait;

use crate::seam::{EmbedError, Embedder, EmbedderFingerprint, Embedding, SimilarityPosture};

/// The pure-Rust default embedder: the hashing trick over character n-grams.
///
/// For each length-`n` character window of the (lowercased) input we compute
/// two FNV-1a hashes — one selects a dimension bucket, and the OTHER's top
/// bit picks a sign — and accumulate ±1 into that bucket (a signed random
/// projection that keeps the expected dot-product an unbiased similarity
/// estimate).
///
/// The sign has to come from a high bit. FNV-1a multiplies by an odd number,
/// and both starting values are odd, so bit 0 of the hash is carried straight
/// through from the input. With a power-of-two `dims` the bucket is
/// `h % dims`, which keeps that same bit. So taking the sign from bit 0 made
/// it a pure function of which bucket the n-gram landed in — every collision
/// then added instead of cancelling, turning the signed projection into an
/// unsigned count sketch. That was revision 1's bug; revision 2 takes the
/// sign from the second hash's top bit instead.
///
/// The result is L2-normalized so cosine similarity is a plain dot product.
/// Fully deterministic and platform-independent: the same string always
/// yields the same vector, which is what makes the `(content_hash,
/// fingerprint)` skip in `L-C2` sound.
#[derive(Debug, Clone)]
pub struct HashEmbedder {
    dims: usize,
    ngram: usize,
    revision: String,
}

impl Default for HashEmbedder {
    fn default() -> Self {
        // 256 dims / 3-grams is a reasonable default for CLI-local corpora:
        // small enough that brute-force cosine is cheap, wide enough that
        // character-trigram collisions stay low.
        Self {
            dims: 256,
            ngram: 3,
            // Revision 2: the sign now comes from the seeded hash's top bit.
            // Revision 1 derived it from bit 0, which bucket parity fully
            // determined — bumping the revision re-embeds stored vectors on
            // next touch (`L-C2`) instead of mixing the two projections.
            revision: "2".to_string(),
        }
    }
}

impl HashEmbedder {
    /// Construct with an explicit revision (bumping it re-fingerprints, which
    /// forces re-embedding). `model_id`/`normalization` are fixed for this
    /// backend.
    pub fn with_revision(revision: impl Into<String>) -> Self {
        Self {
            revision: revision.into(),
            ..Self::default()
        }
    }

    /// The pure projection, exposed for direct testing and reuse by
    /// [`Embedder::embed`].
    pub fn project(&self, text: &str) -> Vec<f32> {
        let mut acc = vec![0.0f32; self.dims];
        let chars: Vec<char> = text.to_lowercase().chars().collect();
        if chars.is_empty() {
            return acc; // zero vector; cosine against it is defined as 0.
        }
        // Windows of `ngram` chars; for very short inputs fall back to the
        // whole string as one window so we never index empty content to zero.
        let window = self.ngram.min(chars.len());
        // One reusable buffer rather than a fresh `String` per window: this
        // loop runs once per character of every indexed node at warm time, so
        // the per-n-gram allocation was the projection's dominant cost.
        let mut gram = String::with_capacity(window * 4);
        for start in 0..=(chars.len() - window) {
            gram.clear();
            gram.extend(chars[start..start + window].iter());
            let h = fnv1a(gram.as_bytes());
            let bucket = (h % self.dims as u64) as usize;
            // The second hash's TOP bit chooses the sign — its low bit is
            // preserved from the input by FNV-1a's odd multiplier and equals
            // the bucket hash's low bit, so `& 1` made the sign a function of
            // bucket parity (see the type-level docs).
            let sign = if fnv1a_seeded(gram.as_bytes(), 0x9E37_79B9_7F4A_7C15) >> 63 == 0 {
                1.0
            } else {
                -1.0
            };
            acc[bucket] += sign;
        }
        l2_normalize(&mut acc);
        acc
    }
}

#[async_trait]
impl Embedder for HashEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        EmbedderFingerprint {
            model_id: "hash-ngram".to_string(),
            revision: self.revision.clone(),
            dims: self.dims,
            normalization: "l2".to_string(),
        }
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedError> {
        if texts.is_empty() {
            return Err(EmbedError::EmptyInput);
        }
        let fingerprint = self.fingerprint().id();
        Ok(texts
            .iter()
            .map(|t| Embedding {
                fingerprint: fingerprint.clone(),
                vector: self.project(t),
            })
            .collect())
    }

    /// Hashed character trigrams are lexical surface overlap, not semantics —
    /// this type's own docs say so, and the measured overlap quoted on
    /// [`SimilarityPosture`] proved no admission floor exists for it.
    fn similarity_posture(&self) -> SimilarityPosture {
        SimilarityPosture::Surface
    }
}

/// L2-normalize in place; a zero vector is left as zero (its norm is 0).
///
/// Public because every backend behind [`Embedder`] normalizes the same way,
/// and because `stella-context`'s k-means centroids re-normalize with it.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// FNV-1a 64-bit. Chosen over `std::hash::DefaultHasher` because the latter's
/// output is explicitly not stable across releases — and stored-vector
/// determinism must survive a compiler upgrade.
fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_seeded(bytes, 0xcbf2_9ce4_8422_2325)
}

fn fnv1a_seeded(bytes: &[u8], offset_basis: u64) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = offset_basis;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_embedder_is_deterministic() {
        let e = HashEmbedder::default();
        assert_eq!(
            e.project("the quick brown fox"),
            e.project("the quick brown fox")
        );
    }

    #[test]
    fn hash_embedder_output_is_l2_normalized() {
        let e = HashEmbedder::default();
        let v = e.project("some representative content to embed");
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    #[test]
    fn similar_text_scores_higher_than_dissimilar() {
        // The property that makes retrieval work at all: near-duplicate text
        // is closer in cosine than unrelated text.
        let e = HashEmbedder::default();
        let base = e.project("open the sqlite connection in wal mode");
        let near = e.project("open the sqlite connection using wal mode");
        let far = e.project("render a bar chart of quarterly revenue");
        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(cos(&base, &near) > cos(&base, &far));
    }

    #[test]
    fn empty_string_projects_to_zero_vector() {
        let e = HashEmbedder::default();
        assert!(e.project("").iter().all(|&x| x == 0.0));
    }

    #[tokio::test]
    async fn embed_batch_returns_one_vector_per_input_tagged_with_fingerprint() {
        let e = HashEmbedder::default();
        let out = e
            .embed(&["alpha".to_string(), "beta".to_string()])
            .await
            .expect("batch embeds");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].fingerprint, "hash-ngram@2/256/l2");
        assert_eq!(out[0].vector.len(), 256);
    }

    #[tokio::test]
    async fn empty_batch_is_an_error_not_an_empty_ok() {
        let e = HashEmbedder::default();
        assert!(matches!(e.embed(&[]).await, Err(EmbedError::EmptyInput)));
    }

    #[test]
    fn bumping_revision_changes_the_fingerprint() {
        assert_ne!(
            HashEmbedder::with_revision("1").fingerprint().id(),
            HashEmbedder::with_revision("2").fingerprint().id(),
        );
    }

    #[test]
    fn the_hashing_backend_never_claims_to_be_semantic() {
        // The whole reason this crate exists is that a lexical projection
        // cannot answer a semantic question. If this assertion ever needs
        // changing, the number replacing it has to be measured first.
        assert_eq!(
            HashEmbedder::default().similarity_posture(),
            SimilarityPosture::Surface
        );
    }
}
