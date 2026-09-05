// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Copying one checkout's code index into another.
//!
//! The index is a pure function of the tree. Two git worktrees cut from one
//! base ref hold the same tree. A worktree that starts empty still parses every
//! file, to build rows it could have been handed. A self-driving work unit paid
//! that on every issue, out of the spend meant for the work.
//!
//! This is not a way around the catch-up pass. The copy is opened, checked, and
//! stripped of its leases. The turn's own pass then covers what the two trees
//! differ by. What it saves is the parse of what they share.
//!
//! # A copy, never a hard link
//!
//! A hard link is one file under two names. Both checkouts would then write one
//! index. That is the shared writable store that wrecks a code graph for good.
//! A copy is cheap anyway. On APFS and btrfs `std::fs::copy` clones the extents
//! rather than the bytes.

use std::path::{Path, PathBuf};

use crate::error::GraphError;
use crate::{lease, store};

/// Seed the index at `dest` from the one at `source`.
///
/// `Ok(true)` when the copy landed. `Ok(false)` when `dest` already holds an
/// index. An index that is already there is never wiped: it describes the
/// checkout it was built for, and nothing here can tell that `source` is
/// better.
///
/// Both paths name a `codegraph.db`. The caller resolves them, so this touches
/// neither the state-root redirect nor the `.stella/private/` layout.
///
/// # A torn copy costs a rebuild, never a bad index
///
/// Another process may hold `source` open. In WAL mode a checkpoint rewrites
/// the main image, so a copy taken across one can be torn. The copy is opened
/// here through the store's verified open. That walks every page with `PRAGMA
/// quick_check` and quarantines a damaged image. So the worst case is an empty
/// index and one cold build, which is what the caller had before.
///
/// The leases come across too, and they have to go. Each row in
/// `code_graph_leases` names a holder in the process that owns `source`. No one
/// will ever release the copy's rows. An uncleared seed would stand its own
/// catch-up pass down for a whole [`lease::LEASE_TTL_SECONDS`]. That is a warm
/// index that cannot see the edits the turn is about to make.
pub fn seed_index(source: &Path, dest: &Path) -> Result<bool, GraphError> {
    // One file cannot seed itself. The removal below would delete it. A caller
    // that resolved both paths from one root lands here, and wants "nothing to
    // do".
    if source == dest {
        return Ok(false);
    }
    if file_len(dest)? > 0 {
        return Ok(false);
    }
    // A path resolver leaves an empty file behind. Remove it rather than write
    // over it: `fs::copy` clones extents only when it creates the file.
    remove_if_present(dest)?;
    for sidecar in sidecars(dest) {
        remove_if_present(&sidecar)?;
    }
    std::fs::copy(source, dest).map_err(|error| GraphError::Seed {
        path: dest.to_path_buf(),
        source: error,
    })?;
    let conn = store::open(dest)?;
    lease::clear_all(&conn)?;
    Ok(true)
}

/// The size of `path`, or `0` when nothing is there.
fn file_len(path: &Path) -> Result<u64, GraphError> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(GraphError::Seed {
            path: path.to_path_buf(),
            source: error,
        }),
    }
}

fn remove_if_present(path: &Path) -> Result<(), GraphError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphError::Seed {
            path: path.to_path_buf(),
            source: error,
        }),
    }
}

/// The WAL and shared-memory files SQLite keeps beside `db`. A sidecar left by
/// an earlier open belongs to the database that was there. Reading it back over
/// a fresh copy turns a good image into a bad one.
fn sidecars(db: &Path) -> [PathBuf; 2] {
    let suffixed = |suffix: &str| {
        let mut name = db.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    };
    [suffixed("-wal"), suffixed("-shm")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CodeGraph;

    /// A workspace with one indexable file, indexed.
    fn indexed_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("workspace");
        std::fs::write(dir.path().join("lib.rs"), "pub fn seeded_symbol() {}\n").expect("source");
        let db = dir.path().join("codegraph.db");
        let graph = CodeGraph::open(dir.path(), &db).expect("open");
        graph.index_all().expect("index");
        (dir, db)
    }

    /// The seeded checkout starts with the rows the parent had. Its own pass
    /// re-parses nothing they share. It holds none of the parent's leases,
    /// which is what lets that pass run at all.
    #[test]
    fn a_seeded_index_carries_the_rows_and_none_of_the_leases() {
        let (parent, parent_db) = indexed_workspace();
        // A pass in flight in the parent, exactly as a live session would have.
        let held = CodeGraph::open(parent.path(), &parent_db).expect("reopen");
        assert!(
            held.acquire_lease(lease::Purpose::IndexWalk).is_some(),
            "the parent must be holding a lease for this test to mean anything"
        );

        let child = tempfile::tempdir().expect("worktree");
        std::fs::write(child.path().join("lib.rs"), "pub fn seeded_symbol() {}\n").expect("source");
        let child_db = child.path().join("codegraph.db");
        assert!(seed_index(&parent_db, &child_db).expect("seed"));

        let graph = CodeGraph::open(child.path(), &child_db).expect("open the seed");
        assert!(
            graph
                .indexes_file(&child.path().join("lib.rs"))
                .expect("ask"),
            "the seeded index must already hold the parent's rows"
        );
        let stats = graph
            .index_all_single_flight()
            .expect("the catch-up pass runs")
            .expect("a copied lease must not stand the child's own pass down");
        assert_eq!(
            stats.files_parsed, 0,
            "an unchanged tree must be recognised, not re-parsed"
        );
    }

    /// An index already at the destination is left alone. It describes the
    /// checkout it was built for.
    #[test]
    fn seeding_never_clobbers_an_index_that_is_already_there() {
        let (_parent, parent_db) = indexed_workspace();
        let (child, child_db) = indexed_workspace();
        std::fs::write(child.path().join("only_here.rs"), "pub fn only_here() {}\n")
            .expect("source");
        CodeGraph::open(child.path(), &child_db)
            .expect("open")
            .index_all()
            .expect("index");

        assert!(!seed_index(&parent_db, &child_db).expect("seed"));
        let graph = CodeGraph::open(child.path(), &child_db).expect("reopen");
        assert!(
            graph
                .indexes_file(&child.path().join("only_here.rs"))
                .expect("ask"),
            "the destination's own rows must survive a refused seed"
        );
    }
}
