// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The seam itself: the [`Embedder`] trait, the [`EmbedderFingerprint`] that
//! stamps every stored vector, and the [`SimilarityPosture`] a backend must
//! declare about what its scores are allowed to mean.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A failure from an embedding backend.
#[derive(Debug, Error)]
pub enum EmbedError {
    /// A caller asked to embed an empty batch or an empty string where the
    /// backend requires content.
    #[error("cannot embed empty input")]
    EmptyInput,

    /// A vector arrived with the wrong dimensionality for its fingerprint.
    #[error("embedding dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// The dimensionality the active fingerprint declares.
        expected: usize,
        /// The dimensionality the vector actually carried.
        got: usize,
    },

    /// The backend returned a different number of vectors than the number of
    /// texts it was handed, so no vector can be attributed to an input.
    #[error("embedding backend returned {got} vectors for {expected} inputs")]
    CountMismatch {
        /// How many texts were sent.
        expected: usize,
        /// How many vectors came back.
        got: usize,
    },

    /// A concrete backend (a hosted API, a local inference server) failed.
    /// Unused by the hashing default; carried so a transport error keeps its
    /// own variant instead of being flattened into a message.
    #[error("embedding backend error: {0}")]
    Backend(String),
}

/// Identifies exactly which embedder produced a vector. Stored beside every
/// vector; retrieval compares only vectors sharing the active fingerprint
/// (`L-C2`). Changing any field is a new fingerprint and invalidates old
/// vectors incrementally rather than in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedderFingerprint {
    /// Model identity, e.g. `"hash-ngram"` or `"voyage-code-3"`.
    pub model_id: String,
    /// A revision within the model id — bump to force re-embedding when the
    /// projection changes without the model id changing.
    pub revision: String,
    /// Vector dimensionality.
    pub dims: usize,
    /// How vectors are normalized, e.g. `"l2"` or `"none"`.
    pub normalization: String,
}

impl EmbedderFingerprint {
    /// The canonical string form stored in the DB and compared for equality:
    /// `model_id@revision/dims/normalization`. Fixed-shape so it is a stable
    /// primary-key component.
    pub fn id(&self) -> String {
        format!(
            "{}@{}/{}/{}",
            self.model_id, self.revision, self.dims, self.normalization
        )
    }
}

/// A single embedding vector tagged with the fingerprint that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    /// The [`EmbedderFingerprint::id`] of the embedder that produced `vector`.
    /// Retrieval only ever compares vectors sharing the active fingerprint
    /// (`L-C2`), so this is what makes a stale vector invisible rather than
    /// silently comparable.
    pub fingerprint: String,
    /// The vector itself: `dims` long and normalized as the fingerprint
    /// declares, so for `l2` a cosine similarity is a plain dot product.
    pub vector: Vec<f32>,
}

/// What a similarity score from this embedder is allowed to *mean* — the
/// declared-posture pattern from `stella-model`'s provider parity matrix
/// (`CachePosture`/`ReasoningPosture`), applied to the one seam where an
/// embedder's honesty matters: recall admission.
///
/// Every consumer may use scores from either posture to **order** candidates.
/// Only a `Semantic` posture lets a score **admit** one: retrieval's evidence
/// gate treats a cosine at or above the declared floor as proof of relevance,
/// and under `Surface` it treats the cosine as ordering only and demands
/// evidence from another channel (anchor, graph adjacency, distinctive term,
/// domain). The distinction is measured, not cautious — hashed trigram scores
/// of contaminants (0.38–0.50) overlapped genuinely relevant frames
/// (0.45–0.63), which is exactly a `Surface` embedder being asked a `Semantic`
/// question.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SimilarityPosture {
    /// Scores reflect meaning. A cosine at or above `admission_floor` is, on
    /// its own, evidence that the node is relevant to the query. The floor is
    /// per-embedder because it is an empirical property of the model's score
    /// distribution — declare it from measurement, the way a provider's cache
    /// posture names its witness test.
    Semantic {
        /// The measured cosine at which this embedder's relevant/irrelevant
        /// score distributions separate.
        admission_floor: f32,
    },
    /// Scores reflect surface text overlap only (hashed character n-grams).
    /// They order candidates but certify nothing; admission must come from
    /// another evidence channel. The safe default for a new backend: claiming
    /// `Semantic` is an opt-in with a number to defend, never an omission.
    Surface,
}

impl SimilarityPosture {
    /// Whether a score from this backend may admit a candidate on its own.
    pub fn admits(&self) -> bool {
        matches!(self, SimilarityPosture::Semantic { .. })
    }

    /// The cosine below which a candidate is not worth showing. `Surface`
    /// backends have no defensible floor and report `None` rather than a
    /// number nobody measured.
    pub fn admission_floor(&self) -> Option<f32> {
        match self {
            SimilarityPosture::Semantic { admission_floor } => Some(*admission_floor),
            SimilarityPosture::Surface => None,
        }
    }
}

/// Produces vectors for text. Async because real backends (a hosted API, a
/// local inference server over HTTP) are; the hashing default resolves
/// immediately.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// The fingerprint every vector this embedder produces is stamped with.
    fn fingerprint(&self) -> EmbedderFingerprint;

    /// Embed a batch. Returns one vector per input, in order. An empty batch
    /// is an error, not an empty success — callers should not ask for nothing.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedError>;

    /// Whether this backend's similarity scores may admit a recall candidate
    /// on their own, and at what floor. Deliberately without a default: an
    /// implementor must look at this question, exactly as a provider must
    /// declare its row on every parity axis (invariant 8).
    fn similarity_posture(&self) -> SimilarityPosture;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_id_is_canonical_and_stable() {
        let fp = EmbedderFingerprint {
            model_id: "hash-ngram".into(),
            revision: "2".into(),
            dims: 256,
            normalization: "l2".into(),
        };
        assert_eq!(fp.id(), "hash-ngram@2/256/l2");
    }

    #[test]
    fn only_a_semantic_posture_admits() {
        assert!(!SimilarityPosture::Surface.admits());
        assert_eq!(SimilarityPosture::Surface.admission_floor(), None);
        let semantic = SimilarityPosture::Semantic {
            admission_floor: 0.4,
        };
        assert!(semantic.admits());
        assert_eq!(semantic.admission_floor(), Some(0.4));
    }
}
