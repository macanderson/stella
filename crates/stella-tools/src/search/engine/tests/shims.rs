// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Fresh-cache wrappers around the three render/report doors.
//!
//! Every test in the parent module asserts what a search *computes*, not what
//! it *remembers*, so each call must gather from the code graph as if it were
//! the first. Wrapping the doors here — rather than threading
//! `&mut GatherCache::default()` through a dozen call sites — keeps the
//! session cache (#3467) invisible to the tests that are not about it.
//!
//! The cache's own witness deliberately does not live here: it is
//! `crates/stella-tools/tests/gather_cache_witness.rs`, an integration test,
//! so it drives the tool through the same public API a session does and
//! cannot accidentally assert against a cache these shims replaced.

use std::path::Path;

use stella_core::search::Depth;
use stella_graph::CodeGraph;
use stella_protocol::tool::ToolOutput;

use crate::search::cache::GatherCache;
use crate::search::engine::{Answer, Hit, SearchConfig, SearchReport};

pub(super) fn render(
    graph: Option<&CodeGraph>,
    root: &Path,
    query: &str,
    answer: &Answer,
    config: SearchConfig,
) -> ToolOutput {
    crate::search::engine::render(
        graph,
        root,
        query,
        answer,
        config,
        &mut GatherCache::default(),
    )
}

pub(super) async fn report_with(
    root: &Path,
    query: &str,
    config: SearchConfig,
    resolution: stella_embed::Resolution,
) -> SearchReport {
    crate::search::engine::report_with(root, query, config, resolution, &mut GatherCache::default())
        .await
}

pub(super) fn render_hit(
    graph: Option<&CodeGraph>,
    root: &Path,
    hit: &Hit,
    depth: Depth,
) -> String {
    crate::search::enrich::render_hit(graph, root, hit, depth, &mut GatherCache::default())
}
