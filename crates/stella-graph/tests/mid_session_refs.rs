// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A HEAD move during a live session is reconciled (#3651).
//!
//! Before this, reconciliation ran only at mount, so a merge or checkout
//! mid-session reached the index solely as working-tree churn the watcher
//! absorbed: the commit range was never consulted, and `head_sha` went stale
//! until the next session did more work than it needed to.
//!
//! These drive the **real** debounce pipeline through the injector seam, so
//! the filter, the batching, and the reconcile call under test are production
//! code — only OS event *delivery* is stood in for.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use stella_graph::CodeGraph;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .expect("git must be runnable in the test environment");
    assert!(status.success(), "git {args:?} failed");
}

fn commit(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", message]);
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "--quiet"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    dir
}

/// The witness: a commit landing mid-session prunes the file it deleted,
/// because the reconcile asked git what the range changed.
///
/// The deletion is the sharp end. A working-tree event for the removed file
/// is deliberately never injected — exactly the case where the OS watcher
/// misses a removal during churn — so the row can only disappear if the
/// commit range was consulted.
#[tokio::test]
async fn a_commit_landing_mid_session_prunes_what_it_deleted() {
    let dir = repo();
    let root = dir.path().canonicalize().expect("canonicalize");
    std::fs::create_dir_all(root.join("src")).expect("mkdir");
    std::fs::write(root.join("src/keep.rs"), "fn keep() {}\n").expect("write");
    std::fs::write(root.join("src/drop.rs"), "fn drop_me() {}\n").expect("write");
    commit(&root, "one");

    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");
    assert_eq!(graph.file_count().expect("count"), 2);

    // Seed the reconciliation baseline, as a session mount would.
    let oracle = stella_graph::GitCli::new(&root);
    graph.reconcile_with(&oracle).expect("seed reconcile");

    let mut injector = graph.watch_pipeline_for_tests(Duration::from_millis(10));
    let seen = injector.batches_applied();

    // The commit lands: one file is gone.
    std::fs::remove_file(root.join("src/drop.rs")).expect("remove");
    commit(&root, "two");

    // Only the ref move is delivered — never an event for the deleted file.
    assert!(
        injector.inject(&root.join(".git/HEAD")),
        "a ref-state path must be accepted by the watcher filter"
    );
    injector
        .applied_past(seen)
        .await
        .expect("the batch must apply");

    assert_eq!(
        graph.file_count().expect("count"),
        1,
        "the commit's deletion must be pruned from the range git reported, \
         not from a filesystem event nobody delivered"
    );
    graph.shutdown();
}

/// A merge that edits `.gitignore` must not leave the watcher enforcing the
/// old rules for the rest of the session.
///
/// The watcher resolves ignore rules once at spawn, so before this a file the
/// merge *un*-ignored stayed filtered out of every event until the next full
/// walk — indefinitely, for a long-running session.
#[tokio::test]
async fn a_ref_move_refreshes_the_watchers_ignore_rules() {
    let dir = repo();
    let root = dir.path().canonicalize().expect("canonicalize");
    std::fs::write(root.join(".gitignore"), "staged/\n").expect("write");
    std::fs::create_dir_all(root.join("staged")).expect("mkdir");
    std::fs::write(root.join("staged/thing.rs"), "fn thing() {}\n").expect("write");
    commit(&root, "one");

    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    let mut injector = graph.watch_pipeline_for_tests(Duration::from_millis(10));
    assert!(
        !injector.inject(&root.join("staged/thing.rs")),
        "the rule is live: an ignored file is filtered out"
    );

    // The merge lands and un-ignores the directory.
    std::fs::write(root.join(".gitignore"), "# nothing ignored now\n").expect("write");
    commit(&root, "two");

    let seen = injector.batches_applied();
    assert!(injector.inject(&root.join(".git/HEAD")));
    injector
        .applied_past(seen)
        .await
        .expect("the ref batch must apply");

    assert!(
        injector.inject(&root.join("staged/thing.rs")),
        "the ref move must have re-resolved the rules — otherwise this file \
         stays invisible to live indexing for the whole session"
    );
    graph.shutdown();
}

/// `.git/HEAD` must reach the pipeline at all. It is rejected by the ordinary
/// relevance filter's dot-prefix rule, so this asserts the ref check runs
/// ahead of it — the exact ordering bug that would make the feature inert.
#[tokio::test]
async fn a_ref_path_is_accepted_though_dotfiles_are_otherwise_ignored() {
    let dir = repo();
    let root = dir.path().canonicalize().expect("canonicalize");
    std::fs::write(root.join("a.rs"), "fn a() {}\n").expect("write");
    commit(&root, "one");

    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    let injector = graph.watch_pipeline_for_tests(Duration::from_millis(10));

    assert!(injector.inject(&root.join(".git/HEAD")));
    assert!(injector.inject(&root.join(".git/packed-refs")));
    assert!(injector.inject(&root.join(".git/refs/heads/main")));
    assert!(
        !injector.inject(&root.join(".git/index")),
        "an ordinary git internal must NOT wake the reconcile — it churns \
         constantly and says nothing about committed history"
    );
    graph.shutdown();
}
