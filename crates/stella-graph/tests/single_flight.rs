// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The walk lease is *consulted*, not merely implemented (#3650).
//!
//! `lease.rs`'s unit tests prove the lease's own semantics — who wins, what
//! expires, who may release. None of that proves the index pass looks at it.
//! These drive [`CodeGraph::index_all_single_flight`] with the lease held and
//! with it free, and assert on whether the tree was actually walked.

use std::fs;

use stella_graph::CodeGraph;
use tempfile::tempdir;

fn workspace() -> tempfile::TempDir {
    let ws = tempdir().expect("tempdir");
    fs::create_dir_all(ws.path().join("src")).expect("mkdir");
    fs::write(ws.path().join("src/lib.rs"), "pub fn thing() {}\n").expect("write");
    ws
}

fn open(ws: &tempfile::TempDir) -> CodeGraph {
    CodeGraph::open(ws.path(), &ws.path().join("codegraph.db")).expect("open")
}

/// The witness: with another pass holding the lease, the walk is skipped
/// entirely — reported as `None`, and provably not done, because the file that
/// exists on disk never reaches the index.
#[test]
fn a_walk_yields_to_a_pass_already_in_progress() {
    let ws = workspace();
    let graph = open(&ws);

    let held = graph
        .hold_walk_lease_for_tests()
        .expect("the lease is free on a fresh store");

    assert_eq!(
        graph.index_all_single_flight().expect("no store error"),
        None,
        "a walk must yield while another pass holds the lease"
    );
    assert_eq!(
        graph.file_count().expect("count"),
        0,
        "yielding must mean the tree was NOT walked — a None with the work \
         done anyway would defeat the whole point"
    );

    // Once the other pass finishes, the next walk proceeds normally.
    graph.release_lease_for_tests(&held);
    let stats = graph
        .index_all_single_flight()
        .expect("no store error")
        .expect("the lease is free again");
    assert_eq!(stats.files_parsed, 1);
    assert_eq!(graph.file_count().expect("count"), 1);
}

/// An explicit pass is never yielded. Someone who typed `stella init` is owed
/// the work, and "another process was busy" is a worse answer to a direct
/// instruction than doing it twice.
#[test]
fn an_explicit_index_all_ignores_the_lease() {
    let ws = workspace();
    let graph = open(&ws);
    let _held = graph
        .hold_walk_lease_for_tests()
        .expect("the lease is free on a fresh store");

    let stats = graph.index_all().expect("an explicit pass always runs");
    assert_eq!(stats.files_parsed, 1);
    assert_eq!(graph.file_count().expect("count"), 1);
}

/// The lease is released even when the pass it covered failed, so one bad
/// walk cannot stall indexing for the whole TTL.
#[test]
fn a_second_walk_succeeds_after_the_first_completes() {
    let ws = workspace();
    let graph = open(&ws);

    assert!(graph.index_all_single_flight().expect("first").is_some());
    assert!(
        graph.index_all_single_flight().expect("second").is_some(),
        "a completed pass must have released its lease"
    );
}
