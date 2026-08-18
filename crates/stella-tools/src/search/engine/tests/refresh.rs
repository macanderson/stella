// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session-start vector refresh (#3649).
//!
//! A sibling of the eager-pass tests rather than part of them: `tests.rs`
//! sits against the 1500-line ceiling, and the refresh is its own subject —
//! when embedding happens without a query, and when it deliberately does not.

use super::*;

/// Witness for #3649: a changed file is re-embedded by the session-start
/// refresh, with no search ever run.
///
/// The lazy per-query pass is the only other thing that would embed this file,
/// and it fires from `dispatch`. Nothing here calls `dispatch`, so a vector
/// present at the end got there because the refresh put it there.
#[tokio::test]
async fn the_session_refresh_re_embeds_a_changed_file_with_no_search() {
    use crate::search::semantic::{RefreshOutcome, refresh_recent_vectors};

    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    // Establish the opt-in the refresh checks for: this workspace uses
    // semantic search, because `stella init`'s eager pass ran.
    let warmed = warm_file_vectors(&root, &ConceptEmbedder, MAX_FILES_PER_EAGER_PASS).await;
    assert!(matches!(warmed, WarmOutcome::Warmed { .. }));

    // Stand in for a commit or merge landing: one file's content moves, and
    // the index pass a session start runs picks it up.
    let changed = root.join(FIXTURE[0].0);
    std::fs::write(&changed, "fn something_entirely_new() {}\n").expect("write");
    let db = stella_store::workspace_private_sqlite_path(&root, "codegraph.db").expect("db path");
    let graph = stella_graph::CodeGraph::open(&root, &db).expect("open");
    graph.index_all().expect("re-index");
    graph.shutdown();

    let outcome =
        refresh_recent_vectors(&root, &ConceptEmbedder, stella_graph::MAX_FILES_PER_PASS).await;
    assert_eq!(
        outcome,
        RefreshOutcome::Refreshed(WarmOutcome::Warmed {
            embedded: 1,
            remaining: 0,
            unreadable: 0
        }),
        "the changed file must be re-embedded without a query having been asked"
    );
}

/// A workspace that never opted into semantic search is left alone. Embedding
/// a whole tree unasked spends the user's API budget on a capability they may
/// not have chosen, and that is `stella init`'s call to make.
#[tokio::test]
async fn the_session_refresh_leaves_an_unembedded_workspace_alone() {
    use crate::search::semantic::{RefreshOutcome, refresh_recent_vectors};

    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());
    let db = stella_store::workspace_private_sqlite_path(&root, "codegraph.db").expect("db path");
    let graph = stella_graph::CodeGraph::open(&root, &db).expect("open");
    graph.index_all().expect("index");
    graph.shutdown();

    assert_eq!(
        refresh_recent_vectors(&root, &ConceptEmbedder, stella_graph::MAX_FILES_PER_PASS).await,
        RefreshOutcome::NotOptedIn
    );

    // And it really embedded nothing — `NotOptedIn` with the work done anyway
    // would be the expensive kind of wrong.
    let graph = stella_graph::CodeGraph::open(&root, &db).expect("reopen");
    let fingerprint = stella_embed::Embedder::fingerprint(&ConceptEmbedder).id();
    let embedded = graph.embedded_file_count(&fingerprint).expect("count");
    graph.shutdown();
    assert_eq!(embedded, 0);
}
