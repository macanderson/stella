// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The per-file gathered-context cache behind `search`'s enrichment (#3163).
//!
//! # What is cached, and against what identity
//!
//! Rendering one hit gathers the file's [`FileNeighborhood`] — symbols,
//! kinds, imports, importers — from the code graph before any facet is
//! written. Within a session the same files rank again and again, and each
//! re-gather re-runs the same SQLite reads to reproduce an answer nothing has
//! invalidated. This cache keeps that bundle keyed by
//! **(path, content identity)**, where the identity is a SHA-256 over the
//! file's bytes as read for rendering.
//!
//! The bytes themselves are the invalidation authority, deliberately, rather
//! than the git blob sha #3163 opens with: a content hash needs no
//! subprocess, works in a workspace that is not a git checkout at all, and
//! makes the dirty-file case the issue calls out disappear instead of needing
//! a bypass — an uncommitted edit simply *is* different bytes, so it can
//! never be served an entry gathered from the old ones. The renderer already
//! reads the file on every call (the source feeds the signature, doc and body
//! facets), so the identity costs one hash over bytes that are in hand, and
//! the source-derived facets are always rendered from the fresh read — only
//! the graph lookups are skipped on a hit.
//!
//! A file whose bytes cannot be read as UTF-8 has no identity here and
//! bypasses the cache entirely: it is gathered fresh every time, which is the
//! conservative direction.
//!
//! # The precision boundary
//!
//! Symbols, kinds and import *specifiers* are functions of the file's own
//! bytes, so the key invalidates them exactly. Two facts in the bundle are
//! drawn from the rest of the tree: the `importers` list, and the resolution
//! of an import specifier to a path. Those can go one index generation stale
//! while the file itself is unchanged — file B gaining an import of A does
//! not change A's bytes, so A's cached entry keeps its old importers until A
//! changes or the entry is evicted. The per-symbol facets (callers, callees,
//! body spans) are **not** cached and stay live for this reason. Tightening
//! the cross-file half — e.g. keying entries additionally by an index
//! generation stamp — is #3196.
//!
//! # The bound
//!
//! [`MAX_ENTRIES`] entries, least-recently-used out first. A `Vec` scanned
//! linearly is the right structure at this size: 256 entries is a few pages
//! of pointers, a lookup touches at most one screen of memory, and the
//! workspace already carries no LRU crate to reach for.

use sha2::{Digest, Sha256};
use stella_graph::FileNeighborhood;

/// The most files whose gathered context one session retains. Past this the
/// least-recently-used entry is evicted; see the module docs for why the
/// bound is a small constant rather than a knob.
pub(crate) const MAX_ENTRIES: usize = 256;

/// One cached gather: the neighborhood as the graph answered it, pinned to
/// the bytes the file had when it was gathered.
#[derive(Debug)]
struct GatherEntry {
    /// Workspace-relative, forward-slash — `Hit::path`'s spelling.
    path: String,
    /// SHA-256 of the file's bytes at gather time ([`content_identity`]).
    identity: [u8; 32],
    neighborhood: FileNeighborhood,
}

/// The session-lifetime cache, owned by the `Search` tool instance. Recency
/// order: the last entry is the most recently used.
#[derive(Debug, Default)]
pub(crate) struct GatherCache {
    entries: Vec<GatherEntry>,
    /// How many times a neighborhood was gathered from the graph — the
    /// witness counter for #3163: unchanged across a repeat search that was
    /// served from cache, incremented again once the file's bytes change.
    #[cfg(test)]
    pub(super) gathered: usize,
}

impl GatherCache {
    /// The cached neighborhood for `path`, if one exists **and** its identity
    /// still matches. An entry whose identity does not match is removed on
    /// the spot — the file changed, and a stale bundle must not survive to
    /// be served by a later buggy caller.
    pub(crate) fn lookup(&mut self, path: &str, identity: &[u8; 32]) -> Option<FileNeighborhood> {
        let position = self.entries.iter().position(|entry| entry.path == path)?;
        if self.entries[position].identity != *identity {
            self.entries.remove(position);
            return None;
        }
        // Move to the back: most recently used, last to be evicted.
        let entry = self.entries.remove(position);
        let neighborhood = entry.neighborhood.clone();
        self.entries.push(entry);
        Some(neighborhood)
    }

    /// Retain `neighborhood` for `path` under `identity`, evicting the
    /// least-recently-used entry once the bound is reached.
    pub(crate) fn store(
        &mut self,
        path: String,
        identity: [u8; 32],
        neighborhood: FileNeighborhood,
    ) {
        if let Some(position) = self.entries.iter().position(|entry| entry.path == path) {
            self.entries.remove(position);
        }
        self.entries.push(GatherEntry {
            path,
            identity,
            neighborhood,
        });
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
    }
}

/// The content identity a cache entry is keyed by: SHA-256 over the file's
/// bytes as read for rendering. See the module docs for why this — and not
/// the git blob sha — is the invalidation authority.
pub(crate) fn content_identity(source: &str) -> [u8; 32] {
    Sha256::digest(source.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use stella_core::search::Depth;
    use stella_embed::Resolution;
    use stella_graph::CodeGraph;
    use stella_protocol::tool::ToolOutput;

    use super::super::{Hit, Search, SearchConfig, enrich};
    use super::{GatherCache, MAX_ENTRIES, content_identity};

    /// Two files, one call edge between them, indexed the way the direct
    /// `dispatch`/`render` tests index (`<root>/codegraph.db`).
    fn indexed_workspace(workspace: &Path) -> (std::path::PathBuf, CodeGraph) {
        write_sources(workspace);
        let root = workspace.canonicalize().expect("canonicalize");
        let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
        graph.index_all().expect("index");
        (root, graph)
    }

    fn write_sources(workspace: &Path) {
        let src = workspace.join("src");
        fs::create_dir_all(&src).expect("mkdir");
        fs::write(src.join("callee.rs"), "pub fn cached_target() {}\n").expect("write");
        fs::write(
            src.join("caller.rs"),
            "pub fn calling_site() {\n    cached_target();\n}\n",
        )
        .expect("write");
    }

    fn neighborhood_named(file: &str) -> stella_graph::FileNeighborhood {
        stella_graph::FileNeighborhood {
            file: file.into(),
            ..Default::default()
        }
    }

    /// **The #3163 witness, cache half.** The second render of an unchanged
    /// file must not re-gather from the graph — and must render
    /// byte-identically to the miss that populated the cache, which is the
    /// fidelity contract the issue states.
    #[test]
    fn a_repeat_render_of_an_unchanged_file_is_served_from_cache_byte_identically() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let (root, graph) = indexed_workspace(workspace.path());
        let hit = Hit {
            path: "src/callee.rs".into(),
            why: "fixture".into(),
            focus: None,
        };
        let mut cache = GatherCache::default();

        let first = enrich::render_hit(Some(&graph), &root, &hit, Depth::new(8), &mut cache);
        assert!(
            first.contains("symbols: cached_target"),
            "the fixture must actually enrich, or this test proves nothing: {first}"
        );
        assert_eq!(cache.gathered, 1, "the first render gathers from the graph");

        let second = enrich::render_hit(Some(&graph), &root, &hit, Depth::new(8), &mut cache);
        assert_eq!(
            first, second,
            "a cache hit must render byte-identically to the miss that populated it"
        );
        assert_eq!(
            cache.gathered, 1,
            "the second render of an unchanged file must be served from cache, not re-gathered"
        );

        graph.shutdown();
    }

    /// **The #3163 witness, invalidation half.** Changing the file's bytes
    /// must invalidate its entry: the next render re-gathers and shows the
    /// new content, never the cached snapshot of the old bytes.
    #[test]
    fn a_mutated_file_is_regathered_never_served_stale() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let (root, graph) = indexed_workspace(workspace.path());
        let hit = Hit {
            path: "src/callee.rs".into(),
            why: "fixture".into(),
            focus: None,
        };
        let mut cache = GatherCache::default();
        let stale = enrich::render_hit(Some(&graph), &root, &hit, Depth::new(8), &mut cache);
        assert!(!stale.contains("appended_later"));
        assert_eq!(cache.gathered, 1);

        fs::write(
            root.join("src/callee.rs"),
            "pub fn cached_target() {}\npub fn appended_later() {}\n",
        )
        .expect("write");
        // What every real call does before rendering: `open_or_build`'s
        // catch-up pass, so the graph reflects the new bytes.
        graph.index_all().expect("re-index");

        let fresh = enrich::render_hit(Some(&graph), &root, &hit, Depth::new(8), &mut cache);
        assert!(
            fresh.contains("appended_later"),
            "the changed file was served from a stale cache entry: {fresh}"
        );
        assert_eq!(
            cache.gathered, 2,
            "the changed file must be re-gathered, not served from cache"
        );

        graph.shutdown();
    }

    /// The tool holds one cache for its whole session: a repeat `search`
    /// through the same instance is answered without re-gathering, and the
    /// two answers are byte-identical end to end.
    #[tokio::test]
    async fn one_tool_instance_serves_repeat_searches_from_its_session_cache() {
        let workspace = tempfile::tempdir().expect("tempdir");
        write_sources(workspace.path());
        let root = workspace.path().canonicalize().expect("canonicalize");
        let tool = Search::with_config(SearchConfig::default());

        let first = tool
            .report_cached(&root, "cached_target", Resolution::Unconfigured)
            .await;
        assert_eq!(
            first.hits.first().map(|hit| hit.path.as_str()),
            Some("src/callee.rs"),
            "the exact rung must find the fixture symbol: {:?}",
            first.hits
        );
        let gathered = tool.cache.lock().expect("lock").gathered;
        assert!(gathered > 0, "the first search must gather");

        let second = tool
            .report_cached(&root, "cached_target", Resolution::Unconfigured)
            .await;
        assert_eq!(
            rendered(&first.rendered),
            rendered(&second.rendered),
            "the cached answer must be byte-identical to the one that populated it"
        );
        assert_eq!(
            tool.cache.lock().expect("lock").gathered,
            gathered,
            "the repeat search through the same tool instance re-gathered"
        );
    }

    fn rendered(output: &ToolOutput) -> &str {
        match output {
            ToolOutput::Ok { content } => content,
            ToolOutput::Error { message, .. } => panic!("expected an answer, got: {message}"),
        }
    }

    /// A path whose bytes changed is removed on lookup, and the removal is
    /// final — a later lookup under the *old* identity must not resurrect it.
    #[test]
    fn a_changed_identity_removes_the_entry_instead_of_serving_it() {
        let mut cache = GatherCache::default();
        cache.store("a.rs".into(), [1; 32], neighborhood_named("a.rs"));
        assert!(cache.lookup("a.rs", &[2; 32]).is_none());
        assert!(
            cache.lookup("a.rs", &[1; 32]).is_none(),
            "the stale entry must be flushed, not kept for a caller with older bytes"
        );
    }

    /// The bound holds, and eviction is least-recently-used: a full cache
    /// drops the entry nothing has touched, never the one just served.
    #[test]
    fn the_bound_evicts_the_least_recently_used_entry() {
        let mut cache = GatherCache::default();
        for index in 0..MAX_ENTRIES {
            let path = format!("file{index}.rs");
            cache.store(path.clone(), [7; 32], neighborhood_named(&path));
        }
        // Touch the oldest entry so the *second*-oldest becomes the victim.
        assert!(cache.lookup("file0.rs", &[7; 32]).is_some());

        cache.store(
            "overflow.rs".into(),
            [7; 32],
            neighborhood_named("overflow.rs"),
        );
        assert!(cache.entries.len() <= MAX_ENTRIES, "the bound must hold");
        assert!(
            cache.lookup("file1.rs", &[7; 32]).is_none(),
            "the least-recently-used entry is the one evicted"
        );
        assert!(
            cache.lookup("file0.rs", &[7; 32]).is_some(),
            "a just-served entry must survive the eviction"
        );
        assert!(cache.lookup("overflow.rs", &[7; 32]).is_some());
    }

    /// Storing a path twice replaces the entry rather than duplicating it.
    #[test]
    fn restoring_a_path_replaces_its_entry() {
        let mut cache = GatherCache::default();
        cache.store("a.rs".into(), [1; 32], neighborhood_named("a.rs"));
        cache.store("a.rs".into(), [2; 32], neighborhood_named("a.rs"));
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.lookup("a.rs", &[1; 32]).is_none());
        // The [1; 32] miss above flushed the entry — by design; re-store and
        // confirm the newest identity is the one that serves.
        cache.store("a.rs".into(), [2; 32], neighborhood_named("a.rs"));
        assert!(cache.lookup("a.rs", &[2; 32]).is_some());
    }

    /// The identity is over bytes, so any byte matters — including a
    /// trailing newline a diff viewer would hide.
    #[test]
    fn the_identity_distinguishes_any_byte_change() {
        assert_ne!(
            content_identity("pub fn a() {}"),
            content_identity("pub fn a() {}\n")
        );
        assert_eq!(content_identity("same"), content_identity("same"));
    }
}
