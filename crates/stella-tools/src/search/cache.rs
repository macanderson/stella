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
