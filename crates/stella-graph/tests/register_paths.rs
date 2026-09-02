// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Registering one path, and asking whether it became a node (#5034).
//!
//! A producer that has just written a file cannot wait for the debounced
//! watcher or the whole-tree catch-up to make a statement about it true.
//! `register_paths` is that pass narrowed to the paths a caller already
//! knows, and `indexes_file` is the question the pass stats cannot answer —
//! a file whose bytes were already indexed is skipped rather than parsed.

use std::path::Path;

use stella_graph::CodeGraph;

fn opened(root: &Path) -> CodeGraph {
    CodeGraph::open(root, &root.join("graph.db")).expect("the store must open")
}

/// A file the index has never seen becomes a node, and says so.
#[test]
fn registering_a_path_makes_it_a_node() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("fresh.rs"), "pub fn fresh() {}\n").unwrap();

    let graph = opened(root);
    assert!(!graph.indexes_file(&root.join("fresh.rs")).unwrap());
    graph
        .register_paths(std::slice::from_ref(&root.join("fresh.rs")))
        .unwrap();
    assert!(graph.indexes_file(&root.join("fresh.rs")).unwrap());
}

/// The spelling a caller hands in need not be the one the store keys on: the
/// root is canonicalized at `open`, and on macOS a temporary directory
/// reaches a caller as `/var/…` and this handle as `/private/var/…`. Both
/// halves have to agree, or the pass files a node the query cannot find.
#[test]
fn a_path_spelled_through_a_symlinked_root_still_lands_where_queries_look() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("fresh.rs"), "pub fn fresh() {}\n").unwrap();
    let linked = dir.path().join("linked");
    std::os::unix::fs::symlink(&real, &linked).unwrap();

    // Opened through the link, so the handle's root canonicalizes to `real`
    // while the caller keeps saying `linked`.
    let graph = opened(&linked);
    graph
        .register_paths(std::slice::from_ref(&linked.join("fresh.rs")))
        .unwrap();

    assert!(graph.indexes_file(&linked.join("fresh.rs")).unwrap());
    assert_eq!(graph.all_files().unwrap(), vec!["fresh.rs".to_string()]);
}

/// A symlink in the workspace must never get its target's content
/// indexed under the link's own path. `register_paths` calls
/// `store::apply_changes` directly, skipping the walk's own symlink
/// check, so it needs its own guard. Without that guard,
/// `Path::is_file()` and `std::fs::read` both follow a symlink, so
/// `apply_changes` and `index_one` read the outside file's bytes and
/// file its symbols under `notes.rs`.
#[test]
fn a_symlinked_path_never_indexes_its_target() {
    let outside = tempfile::tempdir().unwrap();
    let secret_dir = outside.path().join("outside");
    std::fs::create_dir(&secret_dir).unwrap();
    std::fs::write(
        secret_dir.join("secret.rs"),
        "pub fn leaked_outside_the_workspace() {}\n",
    )
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let link = root.join("notes.rs");
    std::os::unix::fs::symlink(secret_dir.join("secret.rs"), &link).unwrap();

    let graph = opened(root);
    graph.register_paths(std::slice::from_ref(&link)).unwrap();

    assert!(!graph.indexes_file(&link).unwrap());
    assert!(
        graph
            .definitions("leaked_outside_the_workspace")
            .unwrap()
            .is_empty(),
        "no symbol from outside the workspace may enter the graph"
    );
    assert_eq!(graph.all_files().unwrap(), Vec::<String>::new());
}

/// A path that has vanished is pruned, exactly as a watcher event for a
/// removed file is — the two callers share one pass.
#[test]
fn registering_a_vanished_path_drops_its_node() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let gone = root.join("gone.rs");
    std::fs::write(&gone, "pub fn gone() {}\n").unwrap();

    let graph = opened(root);
    graph.register_paths(std::slice::from_ref(&gone)).unwrap();
    assert!(graph.indexes_file(&gone).unwrap());

    std::fs::remove_file(&gone).unwrap();
    graph.register_paths(std::slice::from_ref(&gone)).unwrap();
    assert!(!graph.indexes_file(&gone).unwrap());
}
