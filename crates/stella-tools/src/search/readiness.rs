// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Reading the index counters off an open code graph.
//!
//! Coverage is advisory; prompts can run at any coverage level.

use stella_graph::CodeGraph;

pub use stella_tool_facts::readiness::IndexReadiness;

/// Read whole-file coverage. Chunk vectors add detail to files the ranker can
/// already see; their pending count must not make those files look absent.
/// A failed read reports unknown coverage. Search discloses that failure.
#[must_use]
pub fn measure(graph: &CodeGraph, fingerprint: &str, settled: bool) -> IndexReadiness {
    let (Ok(total_files), Ok(embedded)) =
        (graph.file_count(), graph.embedded_file_count(fingerprint))
    else {
        return IndexReadiness::unknown();
    };
    IndexReadiness {
        total_files,
        unindexed_files: total_files.saturating_sub(embedded),
        settled,
    }
}
