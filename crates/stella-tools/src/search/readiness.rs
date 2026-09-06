// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Reading the index counters off an open code graph.
//!
//! The policy itself — what the counts mean, and when they hold a prompt —
//! is [`stella_tool_facts::readiness`], because a screen renders a hold and
//! must not link a tool executor to do it. Only the read of a live
//! [`CodeGraph`] stays here, where the graph is.

use stella_graph::CodeGraph;

pub use stella_tool_facts::readiness::{IndexReadiness, MAX_UNINDEXED_FILES};

/// Read the two counters off an open graph.
///
/// `unindexed_files` is the **larger** of the two pending sets, not their sum.
/// They are overlapping sets of the same files — a file that has just been
/// indexed has neither a whole-file vector nor chunk vectors, and would be
/// counted twice — so the max is the tightest bound that is certainly true,
/// and the union is never smaller than it. Both halves are counts of *files*
/// for the reason [`super::engine`]'s coverage note gives: chunk rows dedup on
/// the rendered text's hash, so a count of chunks could never be compared
/// against a count of files.
///
/// A counter that cannot be read reports **zero pending**, which lets prompts
/// through. That is the opposite of the coverage note's direction and is the
/// right one here: the note's failure mode is an unstated caveat, this one's
/// is a locked door.
#[must_use]
pub fn measure(graph: &CodeGraph, fingerprint: &str, settled: bool) -> IndexReadiness {
    let total_files = graph.file_count().unwrap_or(0);
    let embedded = graph
        .embedded_file_count(fingerprint)
        .unwrap_or(total_files);
    let chunks_pending = graph.pending_chunk_file_count(fingerprint).unwrap_or(0);
    IndexReadiness {
        total_files,
        unindexed_files: total_files.saturating_sub(embedded).max(chunks_pending),
        settled,
    }
}
