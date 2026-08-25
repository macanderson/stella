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
//! # The precision boundary, and the second key that closes it
//!
//! Symbols, kinds and import *specifiers* are functions of the file's own
//! bytes, so the content identity invalidates them exactly. Two facts in the
//! bundle are drawn from the rest of the tree: the `importers` list, and the
//! resolution of an import specifier to a path. File B gaining an import of A
//! does not change A's bytes, so A's identity alone cannot invalidate them —
//! and until #3196 it did not: a second search in one session served the old
//! `imported by:` list for an untouched file.
//!
//! The cross-file half is keyed by the **index generation** instead
//! ([`stella_graph::CodeGraph::index_generation`]): a digest over every
//! indexed file's `(path, content sha)`, which is equal exactly when no file
//! was added, removed or re-indexed — and therefore when every graph-derived
//! answer is byte-identical. [`GatherCache::observe_generation`] compares the
//! stamp the entries were gathered under against the stamp now and empties the
//! cache when they differ. A whole-cache flush rather than a per-entry one,
//! because the stamp is a fact about the tree: once it moves, no entry can
//! prove its own cross-file half survived.
//!
//! An unknown stamp — the graph could not answer — flushes too. The
//! conservative direction is the one that gathers again.
//!
//! The per-symbol facets (callers, callees, body spans) are **not** cached and
//! stay live independently of both keys.
//!
//! # The bound
//!
//! [`MAX_ENTRIES`] entries, least-recently-used out first. A `Vec` scanned
//! linearly is the right structure at this size: 256 entries is a few pages
//! of pointers, a lookup touches at most one screen of memory, and the
//! workspace already carries no LRU crate to reach for.
//!
//! # Why the cache stops at the session (#3198)
//!
//! #3163 sketched more than this — a tier under `.stella/private/` surviving
//! the process, grouped by `.stella/domains.toml`, with git as the
//! invalidation authority for entries that outlive a content hash's usefulness
//! — and #3198 carried that half as a handoff, asking for the measurement
//! before the build: **graph reads per call, and wall clock**. The
//! measurement was taken on 2026-08-24 against this repository's own index
//! (1 769 indexed files, 28 813 chunk vectors), and it says the tier should
//! not be built.
//!
//! **One gather costs 3.7 ms.** That is the three indexed queries
//! [`stella_graph::CodeGraph::file_neighborhood`] issues — symbols, imports,
//! importers — timed over a random 200-file sample of `crates/**`, driven
//! from Python against the same SQL, which bounds the Rust path from above
//! rather than flattering it. A call enriches whole blocks until the 9 000
//! character budget is spent, so it gathers a handful of files, and the
//! ceiling on what any gather cache can ever save is tens of milliseconds.
//!
//! **The terms it would be saving against are one to two orders of magnitude
//! larger.** Measured on the same index the same day: the chunk ranking scan
//! 220-296 ms (118 MB of stored vectors read and decoded per call), a full
//! `PRAGMA quick_check` page walk 333 ms (paid on every call until #4385
//! removed it), the `index_all` catch-up 78-97 ms warm, and the query's own
//! embedding round trip 111-202 ms. Against those, a persisted gather cache
//! is a rounding error.
//!
//! The cache's own guard is in the same range as the thing it guards:
//! [`stella_graph::CodeGraph::index_generation`] re-hashes every indexed
//! file's `(path, content sha)` once per render, 21.8 ms over 1 808 rows by
//! the same Python bound.
//!
//! A faster machine does not change the answer, because the second half of it
//! is structural. [`GatherCache::observe_generation`] empties the whole cache
//! whenever the index generation moves — which it does the moment **any** file
//! in the tree is re-indexed, because the stamp is a digest over every indexed
//! file's `(path, content sha)`. A persisted tier inherits that key: it has to,
//! since the cross-file half of a neighborhood (`importers`, a resolved import
//! target) is a function of the rest of the tree and no per-file identity can
//! invalidate it. So the cross-session case #3163 wants — "the sessions of a
//! team working the same tree" — is exactly the case where every teammate's
//! commit invalidates every persisted entry. The cache would be coldest where
//! it was meant to pay most.
//!
//! **The decision, not a deferral:** the session-lifetime cache is where this
//! stops. Building the persisted tier is legitimate again if the two heavy
//! terms above are removed first and the generation key is narrowed to the
//! cross-file half alone (#3196's subject), because only then is a gather a
//! large enough share of a call to be worth surviving the process.

use sha2::{Digest, Sha256};
use stella_graph::FileNeighborhood;

/// The most files whose gathered context one session retains. Past this the
/// least-recently-used entry is evicted; see the module docs for why the
/// bound is a small constant rather than a knob.
pub const MAX_ENTRIES: usize = 256;

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
///
/// A `stella search` CLI process makes one of these, uses it once and drops
/// it — the command is one-shot, so it gets no benefit and pays only an empty
/// `Vec`. The tool is where the cache earns anything, because a session
/// searches the same files repeatedly.
#[derive(Debug, Default)]
pub struct GatherCache {
    entries: Vec<GatherEntry>,
    /// The index generation every entry was gathered under, or `None` before
    /// the first observation. See [`GatherCache::observe_generation`].
    generation: Option<[u8; 32]>,
    /// How many times a neighborhood was gathered from the graph — the
    /// witness counter for #3163: unchanged across a repeat search that was
    /// served from cache, incremented again once the file's bytes change.
    ///
    /// Always compiled, not `#[cfg(test)]`: the counter is the only
    /// observable difference between a working cache and a cache that quietly
    /// never hits, and a field that exists only under `cfg(test)` cannot be
    /// asserted from another crate's integration test.
    pub gathered: usize,
}

impl GatherCache {
    /// Point the cache at the index generation `stamp` names, emptying it if
    /// that is not the generation its entries were gathered under. Returns
    /// whether anything was discarded.
    ///
    /// Called once per search, before any hit is rendered, so a single render
    /// pass can never straddle two generations. `None` means the stamp could
    /// not be read, and flushes: an entry that cannot prove which generation
    /// it belongs to is exactly the entry that must not be served.
    ///
    /// Pure over owned data — the stamp is computed by the caller, which is
    /// what keeps this module free of I/O (invariant 2).
    pub fn observe_generation(&mut self, stamp: Option<[u8; 32]>) -> bool {
        if self.generation == stamp && stamp.is_some() {
            return false;
        }
        self.generation = stamp;
        let flushed = !self.entries.is_empty();
        self.entries.clear();
        flushed
    }

    /// The cached neighborhood for `path`, if one exists **and** its identity
    /// still matches. An entry whose identity does not match is removed on
    /// the spot — the file changed, and a stale bundle must not survive to
    /// be served by a later buggy caller.
    pub fn lookup(&mut self, path: &str, identity: &[u8; 32]) -> Option<FileNeighborhood> {
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
    pub fn store(&mut self, path: String, identity: [u8; 32], neighborhood: FileNeighborhood) {
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
pub fn content_identity(source: &str) -> [u8; 32] {
    Sha256::digest(source.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warmed(stamp: Option<[u8; 32]>) -> GatherCache {
        let mut cache = GatherCache::default();
        cache.observe_generation(stamp);
        cache.store(
            "src/a.rs".to_string(),
            content_identity("pub fn a() {}"),
            FileNeighborhood::default(),
        );
        cache
    }

    /// The same generation keeps the entries: a stamp that has not moved is
    /// the proof every cross-file answer is still the one that was gathered.
    #[test]
    fn an_unmoved_stamp_keeps_the_entries() {
        let mut cache = warmed(Some([7; 32]));
        assert!(!cache.observe_generation(Some([7; 32])));
        assert!(
            cache
                .lookup("src/a.rs", &content_identity("pub fn a() {}"))
                .is_some()
        );
    }

    /// A moved stamp empties the cache whole. Per-entry retention is not
    /// available here: the stamp says the tree changed and says nothing about
    /// which file's importers moved with it.
    #[test]
    fn a_moved_stamp_empties_the_cache() {
        let mut cache = warmed(Some([7; 32]));
        assert!(cache.observe_generation(Some([8; 32])));
        assert!(
            cache
                .lookup("src/a.rs", &content_identity("pub fn a() {}"))
                .is_none()
        );
    }

    /// An unreadable stamp flushes, and keeps flushing. Equality with the
    /// previous unknown would let two unrelated failures agree with each other
    /// and serve entries neither of them can place.
    #[test]
    fn an_unknown_stamp_flushes_every_time() {
        let mut cache = warmed(None);
        assert!(cache.observe_generation(None));
        assert!(
            cache
                .lookup("src/a.rs", &content_identity("pub fn a() {}"))
                .is_none()
        );
    }
}
