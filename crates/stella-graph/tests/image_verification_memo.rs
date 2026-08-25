// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! An unchanged store image is walked once, however many times it is opened —
//! and a changed one is walked again (#4385).
//!
//! `PRAGMA quick_check` walks every page, and that is what makes its verdict
//! worth having. It is also why `search::engine::report_with`, which opens the
//! code graph on every `search` call, was paying a full page walk of a 180 MB
//! image per query before `store::image_check` memoized the verdict.
//!
//! **This file holds exactly one test on purpose.** `stella_graph::image_walks`
//! is a process-global counter, so any other test in the same binary opening
//! any store would race the deltas this one asserts, and both halves of the
//! contract have to be checked in a fixed order anyway. One test per binary
//! makes the counts exact instead of approximately right.

use std::path::Path;

use stella_graph::{CodeGraph, image_walks};

/// Open, then drop. The handle's lifetime is not what is being measured.
fn open_and_close(root: &Path, db: &Path) {
    let graph = CodeGraph::open(root, db).expect("open the store");
    drop(graph);
}

#[test]
fn an_unchanged_image_is_walked_once_and_a_changed_one_is_walked_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(&root).expect("workspace root");
    std::fs::write(root.join("lib.rs"), "pub fn answer() -> u32 { 42 }\n").expect("a source file");
    let db = dir.path().join("codegraph.db");

    let before = image_walks();
    open_and_close(&root, &db);
    assert_eq!(
        image_walks() - before,
        1,
        "the first open of an image must still walk it — the memo is a cost fix, not a decision \
         to stop checking"
    );

    // A second open lets the file settle. Creating and migrating a store
    // writes the main image after the walk that verified it, so that open
    // legitimately sees a stamp it has not checked and walks again. What the
    // memo claims is about an image that is *not* moving, which is the state
    // every open after this one is in.
    open_and_close(&root, &db);
    let settled = image_walks();

    for _ in 0..8 {
        open_and_close(&root, &db);
    }
    assert_eq!(
        image_walks(),
        settled,
        "eight opens of an unchanged image walked {} more time(s); before the memo every one of \
         them paid a full page walk",
        image_walks() - settled
    );

    // The other half of the contract, and the reason the key is the image's
    // own stamp rather than its path: a store that changes underneath a
    // process must be walked again on the next open, because that is where
    // damage arriving after a verification is caught
    // (`store::tests::a_store_corrupt_only_in_its_data_pages_is_quarantined_at_open`).
    let bytes = std::fs::read(&db).expect("read the image");
    std::fs::write(&db, &bytes).expect("rewrite the image");
    open_and_close(&root, &db);
    assert!(
        image_walks() > settled,
        "an image whose stamp moved must be walked again rather than trusted from the memo"
    );
}
