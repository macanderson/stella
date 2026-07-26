// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-context` — **the context plane**: the single door between the
//! engine and everything the agent knows that isn't already in the prompt. One
//! SQLite file, one engine ([`ContextStore`]) holds a bi-temporal property
//! graph, a fingerprinted embedding index, and episodic memory; on top of it
//! sits a hybrid, budgeted, cited retrieval pipeline ([`ContextStore::recall`])
//! and a bi-temporal write-back path ([`ContextStore::upsert`]). Built-in and
//! external sources register through one seam ([`ContextProvider`] /
//! [`ProviderRegistry`]).
//!
//! # The four jobs this plane does that its providers don't (arch §7)
//!
//! 1. **Routing & fusion** — fan a query to capability-matching sources, fuse
//!    vector + recency + graph-adjacency via reciprocal-rank fusion, dedup by
//!    content hash (`retrieval`).
//! 2. **Budgeting** — every frame carries a `token_cost`; assembly packs to the
//!    caller's budget and reports what was dropped. Silent truncation is banned
//!    (`L-C5`).
//! 3. **Provenance** — every frame carries a human `citation_label` (a frame
//!    without one is a constructor error, `L-C4`) and a source chain.
//! 4. **Consent & isolation** — providers declare `reads`/`writes`/`egress`;
//!    the built-in store is `egress: false` (nothing leaves the machine).
//!
//! # Binding lessons realized here
//!
//! - `L-C1` warm-at-mount: [`ContextStore::open_and_warm`] kicks embedding
//!   catch-up in the background, not lazily on first query.
//! - `L-C2` byte-compat skip: vectors are keyed by `(content_hash,
//!   fingerprint)`; identical content is never re-embedded; retrieval never
//!   mixes fingerprints.
//! - `L-C3` bi-temporal facts: corrections close-and-supersede, never delete;
//!   [`ContextStore::facts_as_of`] answers "what did we believe at T1". It is a
//!   guarantee about **queryability**, not about bytes: [`ContextStore::compact`]
//!   reclaims derived index entries whose owner is already gone (orphaned
//!   embeddings, orphaned domain tags) precisely because deleting them cannot
//!   change that answer. `edge` rows, `memory` revisions and superseded `node`
//!   rows are named exclusions.
//! - `L-C6` coverage gate: weak graph/vector coverage falls back to bounded
//!   lexical search, **labeled as such** rather than dressed up as grounding.
//! - `L-L1` crash consistency: every write batch is one transaction; a kill
//!   mid-index rolls back to a consistent store.
//!
//! # Embedder policy
//!
//! The built-in default is the offline, pure-Rust [`HashEmbedder`]. The product
//! default — a local ONNX bge-small model — is the tracked follow-up
//! (risk R14); [`Embedder`] is the seam that makes
//! swapping it in trivial. Wire types are built on `contextgraph-types`, never
//! duplicated (principle #7 / `L-E1`).

// Every public item here is API a host reads without the source in front of it,
// and the field-level silence this closes was an audit finding (#558). Under
// `make lint` (`-D warnings`) an undocumented public item is a build failure,
// which is the point: the gap cannot reopen one field at a time.
#![warn(missing_docs)]

mod ann;
mod candidates;
mod clock;
#[cfg(test)]
mod cost_counters;
mod embed;
mod error;
mod provider;
mod retrieval;
mod store;
mod warm;
mod writeback;

pub use ann::{AnnIndexPolicy, AnnIndexReport, AnnIndexState};
pub use clock::{Clock, FixedClock, SystemClock, format_rfc3339};
pub use embed::{EmbedError, Embedder, EmbedderFingerprint, Embedding, HashEmbedder};
pub use error::ContextError;
pub use provider::{ContextProvider, ProviderRegistry};
pub use retrieval::{
    DEFAULT_ANN_ENABLED, DEFAULT_ANN_PROBES, DEFAULT_COVERAGE_TOPK, DEFAULT_LEXICAL_LIMIT,
    DEFAULT_MAX_VECTOR_SEEDS, DEFAULT_MIN_COVERAGE, DEFAULT_MMR_CANDIDATE_MULTIPLE,
    DEFAULT_MMR_LAMBDA, DEFAULT_RECENCY_WEIGHT, DEFAULT_RRF_K, DropReason, DroppedFrame,
    RecallResult, RecallTuning, SELECTION_PROVENANCE_KIND, SelectionReason, is_lexical_fallback,
};
pub use store::ledger::{AppendOutcome, LedgerAppend, LedgerRecord};
pub use store::{
    CompactionWatermark, ContextCompactPolicy, ContextCompactReport, ContextStore,
    MemoryLineageStats, MemoryRevision, NodeInput, NodeKind, NodeRow,
};
pub use writeback::{
    ContextDelta, DomainInput, EpisodeInput, EpisodeOutcome, FactAssertion, FactView, MemoryInput,
    MemoryKind, UpsertReceipt,
};

// Re-export the CGP wire types callers pass in/out, so a consumer needs only
// this crate for the common path (they remain `contextgraph-types`' definitions).
pub use contextgraph_types::{
    Capabilities, ContextFrame, ContextQuery, ContextQueryResult, DataFlow, ProviderInfo,
};
