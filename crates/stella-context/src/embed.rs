// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Embeddings: re-exported from [`stella_embed`], the leaf crate this seam now
//! lives in.
//!
//! The trait, the fingerprint, the posture and the offline [`HashEmbedder`]
//! were defined here until `stella-graph` needed the same seam to give
//! `graph_query` semantic file lookup. Neither plane may depend on the other,
//! so the seam moved down to a leaf rather than being copied — the
//! `stella-diff` precedent (#1511). This module stays as the re-export so
//! every call site inside this crate, and every doc link that names
//! `crate::embed::…`, keeps working unchanged.
//!
//! # Embedder policy
//!
//! The built-in default here remains [`HashEmbedder`]: deterministic, offline,
//! zero-download, and honest about being lexical rather than semantic
//! ([`SimilarityPosture::Surface`], so its scores order candidates but never
//! admit one). `stella-embed`'s `http` feature carries the semantic backend;
//! this crate does not enable it, because the context plane's embedder is
//! chosen by its host, not by the store.

pub use stella_embed::{
    EmbedError, Embedder, EmbedderFingerprint, Embedding, HashEmbedder, SimilarityPosture,
    l2_normalize,
};
