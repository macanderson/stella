// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The reconciliation is *wired*, not merely declared.
//!
//! `reconcile::plan` is a pure decision and is unit-tested as one. That proves
//! the branch table and nothing else: a plan nobody acts on indexes no files
//! and records no commit, and would pass every one of those tests. These
//! exercise [`CodeGraph::reconcile_with`] end to end against a fake oracle —
//! real store, real tree-sitter pass, real rows — so the chain from "HEAD
//! moved" to "these files are in the index and the commit is recorded" is
//! covered by something that fails when the wiring is cut.

use std::fs;
use std::path::Path;

use stella_graph::{CodeGraph, Plan, RepoOracle};
use tempfile::{TempDir, tempdir};

/// An oracle whose answers the test dictates outright.
struct FakeRepo {
    head: Option<String>,
    diff: Option<Vec<String>>,
}

impl RepoOracle for FakeRepo {
    fn head(&self) -> Option<String> {
        self.head.clone()
    }
    fn changed_between(&self, _from: &str, _to: &str) -> Option<Vec<String>> {
        self.diff.clone()
    }
}

fn workspace(files: &[(&str, &str)]) -> TempDir {
    let ws = tempdir().expect("tempdir");
    for (rel, body) in files {
        let path = ws.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&path, body).expect("write");
    }
    ws
}

fn open(ws: &Path) -> CodeGraph {
    CodeGraph::open(ws, &ws.join("codegraph.db")).expect("open")
}

/// The witness: a moved HEAD indexes the commit's files and only those.
///
/// `src/untouched.rs` exists on disk and is perfectly indexable, so a
/// reconciliation that quietly fell back to a whole-tree walk would index it
/// too and the count would be 2. Asserting 1 is what makes this a test of
/// *scoping* rather than of indexing.
#[test]
fn a_moved_head_indexes_the_commits_files_and_records_the_commit() {
    let ws = workspace(&[
        ("src/merged.rs", "fn merged() {}\n"),
        ("src/untouched.rs", "fn untouched() {}\n"),
    ]);
    let graph = open(ws.path());

    // A store that has never recorded a commit plans a full walk, so seed one
    // first — that is the state every session after the first is actually in.
    let seed = FakeRepo {
        head: Some("commit-one".to_string()),
        diff: None,
    };
    let (plan, _) = graph.reconcile_with(&seed).expect("seed");
    assert!(
        matches!(plan, Plan::FullWalk { .. }),
        "a store with no recorded commit must widen, not scope: {plan:?}"
    );
    assert_eq!(
        graph.file_count().expect("count"),
        0,
        "a full-walk plan indexes nothing by itself — index_all is what walks"
    );

    // Now the case under test: HEAD moved, and the range names one file.
    let merged = FakeRepo {
        head: Some("commit-two".to_string()),
        diff: Some(vec!["src/merged.rs".to_string()]),
    };
    let (plan, stats) = graph.reconcile_with(&merged).expect("reconcile");
    assert!(
        matches!(plan, Plan::Scoped { .. }),
        "a moved HEAD with a resolvable range must scope: {plan:?}"
    );
    assert_eq!(
        stats.files_parsed, 1,
        "exactly the commit's file was parsed"
    );
    assert_eq!(
        graph.file_count().expect("count"),
        1,
        "the untouched file must NOT have been indexed — that would be a walk"
    );

    // The commit is recorded, so the next reconciliation has a `from`.
    let again = FakeRepo {
        head: Some("commit-two".to_string()),
        diff: None,
    };
    let (plan, stats) = graph.reconcile_with(&again).expect("re-reconcile");
    assert_eq!(
        plan,
        Plan::Unchanged {
            head: "commit-two".to_string()
        },
        "the commit the last pass acted on must have been recorded"
    );
    assert_eq!(stats.files_parsed, 0, "an unmoved HEAD parses nothing");
}

/// A deletion in the commit range prunes the row rather than being skipped
/// for having no file on disk to parse.
#[test]
fn a_file_deleted_by_the_commit_is_pruned_from_the_index() {
    let ws = workspace(&[("src/gone.rs", "fn gone() {}\n")]);
    let graph = open(ws.path());
    graph.index_all().expect("index");
    assert_eq!(graph.file_count().expect("count"), 1);

    let seed = FakeRepo {
        head: Some("commit-one".to_string()),
        diff: None,
    };
    graph.reconcile_with(&seed).expect("seed");

    // The commit removed it, so by the time we reconcile it is off disk —
    // which is exactly why the change set must include deleted paths.
    fs::remove_file(ws.path().join("src/gone.rs")).expect("remove");
    let deleted = FakeRepo {
        head: Some("commit-two".to_string()),
        diff: Some(vec!["src/gone.rs".to_string()]),
    };
    let (_, stats) = graph.reconcile_with(&deleted).expect("reconcile");
    assert_eq!(stats.files_pruned, 1);
    assert_eq!(graph.file_count().expect("count"), 0);
}

/// No repository is not an error, and records nothing to reconcile against
/// later — a workspace outside git must behave exactly as it did before this
/// existed.
#[test]
fn a_workspace_with_no_repository_reconciles_to_a_no_op() {
    let ws = workspace(&[("src/a.rs", "fn a() {}\n")]);
    let graph = open(ws.path());
    let none = FakeRepo {
        head: None,
        diff: None,
    };
    let (plan, stats) = graph.reconcile_with(&none).expect("reconcile");
    assert_eq!(plan, Plan::NoRepo);
    assert_eq!(stats.files_parsed, 0);
    assert_eq!(graph.file_count().expect("count"), 0);
}
