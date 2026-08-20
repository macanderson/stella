// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session-start background pass (#3649, rewritten for #4043).
//!
//! A sibling of the eager-pass tests rather than part of them: `tests.rs`
//! sits against the 1500-line ceiling, and this is its own subject — when
//! embedding happens without a query, now that a query never embeds at all.

use super::*;

/// Witness for #3649, still true: a changed file is re-embedded by the
/// session-start pass, with no search ever run.
///
/// Sharper than it used to be. Nothing else can embed this file any more —
/// the lazy per-query pass `dispatch` ran is deleted — so a vector present at
/// the end got there because the background pass put it there.
#[tokio::test]
async fn the_background_pass_re_embeds_a_changed_file_with_no_search() {
    use crate::search::backfill::{BackfillOutcome, backfill_workspace_vectors};

    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    let warmed = warm_file_vectors(&root, &ConceptEmbedder, NO_FILE_CEILING).await;
    assert!(matches!(warmed, WarmOutcome::Warmed { .. }));

    // Stand in for a commit or merge landing: one file's content moves, and
    // the index pass a session start runs picks it up.
    let changed = root.join(FIXTURE[0].0);
    std::fs::write(&changed, "fn something_entirely_new() {}\n").expect("write");
    let db = stella_store::workspace_private_sqlite_path(&root, "codegraph.db").expect("db path");
    let graph = stella_graph::CodeGraph::open(&root, &db).expect("open");
    graph.index_all().expect("re-index");
    graph.shutdown();

    let outcome = backfill_workspace_vectors(&root, &ConceptEmbedder, &mut |_| {}).await;
    let BackfillOutcome::Ran { files, .. } = outcome else {
        panic!("the pass must run: {outcome:?}");
    };
    assert_eq!(
        files,
        WarmOutcome::Warmed {
            embedded: 1,
            remaining: 0,
            unreadable: 0
        },
        "the changed file must be re-embedded without a query having been asked"
    );
}

/// **The opt-in flip (#4043).** A workspace that has never been embedded is
/// now filled by the background pass rather than left alone.
///
/// This asserts the opposite of what its predecessor did, and the reversal is
/// the point. The old guard — skip a workspace with no vectors, because
/// embedding a tree unasked spends the user's money — was sound while a
/// search would eventually fill the index itself. With the lazy pass deleted,
/// skipping means this workspace never gets semantic search at all and is
/// never told why.
#[tokio::test]
async fn the_background_pass_fills_a_workspace_that_was_never_embedded() {
    use crate::search::backfill::{BackfillOutcome, backfill_workspace_vectors};
    use crate::search::readiness::measure;

    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());
    let db = stella_store::workspace_private_sqlite_path(&root, "codegraph.db").expect("db path");
    let graph = stella_graph::CodeGraph::open(&root, &db).expect("open");
    graph.index_all().expect("index");
    let fingerprint = stella_embed::Embedder::fingerprint(&ConceptEmbedder).id();
    assert_eq!(
        graph.embedded_file_count(&fingerprint).expect("count"),
        0,
        "the fixture must start with no vectors at all, or this proves nothing"
    );
    graph.shutdown();

    let outcome = backfill_workspace_vectors(&root, &ConceptEmbedder, &mut |_| {}).await;
    assert!(
        matches!(outcome, BackfillOutcome::Ran { .. }),
        "{outcome:?}"
    );

    let graph = stella_graph::CodeGraph::open(&root, &db).expect("reopen");
    let readiness = measure(&graph, &fingerprint, true);
    graph.shutdown();
    assert_eq!(
        readiness.unindexed_files, 0,
        "a never-embedded workspace must be filled, not skipped: {readiness:?}"
    );
    assert!(readiness.total_files > 0, "the fixture indexed nothing");
}
