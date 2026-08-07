// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The replacement record carried by [`AgentEvent::Compaction`](crate::AgentEvent::Compaction)'s
//! `rewrites` field — one entry per tool result a compaction pass rewrote in
//! place during that pass.
//!
//! # Why the replacement bytes ride the event at all
//!
//! Compaction rewrites tool results *in place* (eviction, dedup, supersession,
//! aging stubs). Before these entries existed the journal held only the
//! original `tool_result` event under that `call_id`, so byte-exact
//! reconstruction of a later step recovered a real output that was simply not
//! the one the step sent — the pre-compaction bytes, flagged as a digest
//! mismatch (#1667). Each entry journals the post-rewrite preimage under the
//! digest the next step's manifest will record, which is what lets
//! reconstruction resolve a compacted block exactly instead of falling back to
//! the closest preimage under the same `call_id`.
//!
//! # Size discipline
//!
//! Entries are emitted only for blocks rewritten by *this* pass — a form that
//! survives unchanged across later passes is never re-journaled — and are
//! deduplicated by digest within one event, so the constant stub strings
//! collapse to one entry however many results they replaced. Every replacement
//! is also strictly smaller than the bytes it replaced (that is what
//! compaction is for), so the journal grows by less than the context shrank.

use serde::{Deserialize, Serialize};

/// One in-place compaction rewrite: the post-rewrite identity and bytes of a
/// tool result a compaction pass stubbed, deduplicated, superseded, or aged.
///
/// `content` is the canonical serialized form of the replacement tool output —
/// the same `serde_json` serialization the receipts plane hashes — so
/// `content_digest` re-derives from `content` exactly and a consumer can
/// verify the pair without any other context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CompactionRewrite {
    /// The content-addressed id (`blk_…`) of the block the rewrite produced —
    /// the identity the next step's manifest cites for this result.
    pub block_id: String,
    /// `sha256:<hex>` of `content` — the digest the block registry records,
    /// and the key reconstruction resolves this preimage by.
    pub content_digest: String,
    /// The replacement bytes: the serialized post-rewrite tool output.
    pub content: String,
}
