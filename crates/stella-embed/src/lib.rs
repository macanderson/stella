// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-embed` — **the embedding seam**, extracted so more than one plane
//! can share it.
//!
//! Two consumers now need to turn text into a vector and compare vectors
//! honestly: `stella-context` (the retrieval plane, which has owned this code
//! since it existed) and `stella-graph` (the code graph, which needs semantic
//! file lookup so `stella search` and CGP recall can answer a question the
//! asker can only phrase in English). Neither may depend on the other — `stella-graph` owns
//! `codegraph.db` independently of `stella-context`, and a
//! `stella-graph → stella-context` edge would drag the whole retrieval, ANN
//! and episodic-memory machinery into the code indexer for the sake of one
//! trait. So the seam moves down to a leaf, exactly as [`stella-diff`] moved
//! out of `stella-cli` so the Observatory could share one differ without
//! costing itself its isolation (#1511).
//!
//! [`stella-diff`]: https://github.com/macanderson/stella/tree/main/crates/stella-diff
//!
//! # What lives here
//!
//! - [`Embedder`] — the trait. Async, because real backends are.
//! - [`EmbedderFingerprint`] — identifies *exactly* which embedder produced a
//!   vector. Every stored vector is stamped with one, and retrieval only ever
//!   compares vectors sharing the active fingerprint, so changing embedders is
//!   an incremental re-embed rather than a silently mixed vector space.
//! - [`SimilarityPosture`] — what a score from this backend is allowed to
//!   *mean*. The declared-posture pattern from `stella-model`'s provider
//!   parity matrix (invariant 8), pointed at the one seam where an embedder's
//!   honesty matters.
//! - [`rank`] — pure, deterministic cosine ranking. No I/O, no allocation of a
//!   database handle, nothing to mock: invariant 2's half of this crate, and
//!   the half that is property-tested.
//! - [`HashEmbedder`] — the offline, pure-Rust, zero-download fallback. It is
//!   a *lexical* projection and says so ([`SimilarityPosture::Surface`]).
// `HttpEmbedder` is named below without a link on purpose: the item is
// `#[cfg(feature = "http")]`, so an intra-doc link to it is an unresolved
// link — and a hard error — whenever the crate's docs are built on the
// default feature set, which is what CI's doc-warnings step does (#4453).
//! - `HttpEmbedder` (feature `http`) — the semantic backend: one
//!   OpenAI-shaped `POST /v1/embeddings` client that speaks to Voyage
//!   (`voyage-code-3`), OpenAI (`text-embedding-3-*`), or a local server
//!   (Ollama, llama.cpp, TEI) at a configured base URL. This is the I/O
//!   adapter; it is the only thing in this crate that can fail for a reason
//!   the caller did not cause.
//!
//! # Why the semantic backend is HTTP and not in-process ONNX
//!
//! The alternative considered was a vendored ONNX/candle runtime with
//! checksum-pinned bge-small weights (the long-standing "R14" follow-up). It
//! buys zero-configuration semantics and costs a large native dependency
//! tree, a ~130 MB weight fetch, and a materially bigger binary — a
//! supply-chain decision with an owner, not a detail to slip into a retrieval
//! change. The HTTP shape gets the *quality* question right today with **no
//! new third-party dependency at all** (`reqwest` is already in the
//! workspace), and it is not a hosted-only answer: the same adapter points at
//! a local Ollama or llama.cpp server, which is offline operation without
//! vendoring an inference engine. What it gives up is the zero-config path —
//! with nothing configured you get [`HashEmbedder`], which cannot admit a
//! candidate on its own and is labelled as such wherever it is used.

// Every public item here is a seam another crate implements or calls without
// the source in front of it.
#![warn(missing_docs)]

mod hash;
#[cfg(feature = "http")]
mod http;
pub mod rank;
mod seam;

pub use hash::{HashEmbedder, l2_normalize};
#[cfg(feature = "http")]
pub use http::{EmbedderEnv, HttpEmbedder, Resolution, from_env, resolve};
pub use seam::{EmbedError, Embedder, EmbedderFingerprint, Embedding, SimilarityPosture};
